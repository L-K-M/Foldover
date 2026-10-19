//! `foldover-service` — service layer. The agent's public API: sessions,
//! `handle` request dispatch, validation and the event bus. Depends on
//! foldover-engine. See PLAN.md §4.4.

mod archive;
mod automation;
mod history;
mod peer;
mod schedule;
mod share;
mod sync;

use std::sync::{Arc, Mutex};

use foldover_engine::{Approval, Engine, ErrorKind, StateLocation};
use foldover_proto::local::{
    ARCHIVE_ITEMS, ARCHIVE_PUT_BACK, ARCHIVE_PUT_BACK_ALL, ARCHIVE_RUNS, AUTOMATION_PAUSE,
    AUTOMATION_RESUME, AUTOMATION_STATUS, HISTORY_LIST, PAIR_REMOVE, PEER_CANCEL, PEER_CONFIRM,
    PEER_PAIR, PEER_PAIRING_CLOSE, PEER_PAIRING_OPEN, PEER_REVOKE, PEER_SHARES, PEER_START,
    PEER_STATUS, PairRevision, SHARE_ADD, SHARE_LIST, SHARE_REMOVE,
};
use foldover_proto::local::{
    AgentCapability, DAEMON_STATUS, DaemonStatus, Hello, INTERNAL_ERROR, INVALID_PARAMS,
    METHOD_NOT_FOUND, PAIR_CREATE, PAIR_GET, PAIR_LIST, PAIR_NOT_FOUND, PAIR_PAGE_SIZE,
    PAIR_UPDATE, PROTOCOL_VERSION, PairInput, PairKey, PairList, PairPage, PairRecord, PairUpdate,
    ROOT_OVERLAP, ROOT_UNAVAILABLE, Request, RpcError, SESSION_HELLO, STALE_REVISION,
    VERSION_MISMATCH,
};
use foldover_proto::local::{
    ConflictKeep, SYNC_CANCEL, SYNC_ITEMS, SYNC_PREVIEW, SYNC_RESOLVE, SYNC_RUN, SYNC_STATUS,
    SYNC_UNDO, SyncItemQuery, SyncKey, SyncResolve, SyncRun,
};
use foldover_types::{PairDraft, PairId, ReviewToken, Side};
use schedule::Scheduler;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sync::{Jobs, Work};
use tokio::sync::Semaphore;

/// The workspace version reported by health methods and `foldoverd --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The cloneable API façade owns exactly one engine and persistent writer.
/// IPC authenticates private requests before passing them to this handler.
#[derive(Clone)]
pub struct Service {
    engine: Arc<Mutex<Engine>>,
    commands: Arc<Semaphore>,
    jobs: Arc<Jobs>,
    scheduler: Arc<Scheduler>,
    peers: peer::Peers,
}

impl Service {
    pub fn open() -> Result<Self, foldover_engine::Error> {
        Ok(Self::new(Engine::open(&StateLocation::Default)?))
    }

    fn new(engine: Engine) -> Self {
        Self {
            engine: Arc::new(Mutex::new(engine)),
            commands: Arc::new(Semaphore::new(1)),
            jobs: Arc::new(Jobs::default()),
            scheduler: Arc::new(Scheduler::new(std::time::Instant::now())),
            peers: Arc::default(),
        }
    }

    /// Listen for paired Macs (ADR 0031), if this Mac has any. Otherwise
    /// the first `peer.*` request starts listening.
    pub async fn start_peers(&self) -> Result<(), RpcError> {
        peer::start_if_paired(&self.engine, &self.peers).await
    }

    pub async fn handle(&self, request: Request) -> Result<Value, RpcError> {
        if matches!(request.method(), SESSION_HELLO | DAEMON_STATUS) {
            return health(request).await;
        }
        if matches!(
            request.method(),
            SYNC_PREVIEW | SYNC_RUN | SYNC_STATUS | SYNC_CANCEL | SYNC_RESOLVE | SYNC_UNDO
        ) {
            return self.sync(&request).await;
        }
        if request.method() == SYNC_ITEMS {
            let params: SyncItemQuery = parameters(&request)?;
            let id = params
                .id
                .parse::<PairId>()
                .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))?;
            let token = review_token(params.review_token.as_deref())?;
            // Saves and snapshot writes can hold the engine for seconds;
            // wait for it off the runtime so other requests still answer.
            let (engine, jobs) = (Arc::clone(&self.engine), Arc::clone(&self.jobs));
            let page = tokio::task::spawn_blocking(move || {
                jobs.items(&engine, id, token, params.after.as_deref(), params.kind)
            })
            .await
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
            return encode(page);
        }
        if matches!(
            request.method(),
            ARCHIVE_RUNS | ARCHIVE_ITEMS | ARCHIVE_PUT_BACK | ARCHIVE_PUT_BACK_ALL
        ) {
            return archive::handle(&self.engine, &self.jobs, &request).await;
        }
        if matches!(
            request.method(),
            AUTOMATION_STATUS | AUTOMATION_PAUSE | AUTOMATION_RESUME
        ) {
            return automation::handle(&self.engine, &request).await;
        }
        if matches!(
            request.method(),
            PEER_STATUS
                | PEER_START
                | PEER_PAIRING_OPEN
                | PEER_PAIRING_CLOSE
                | PEER_PAIR
                | PEER_CONFIRM
                | PEER_CANCEL
                | PEER_REVOKE
                | PEER_SHARES
        ) {
            return peer::handle(&self.engine, &self.peers, &request).await;
        }
        if matches!(request.method(), SHARE_LIST | SHARE_ADD | SHARE_REMOVE) {
            return share::handle(&self.engine, &request).await;
        }
        if request.method() == HISTORY_LIST {
            return history::handle(&self.engine, &request).await;
        }
        let command = Command::parse(&request)?;
        let permit = Arc::clone(&self.commands)
            .acquire_owned()
            .await
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "Configuration worker is unavailable"))?;
        let engine = Arc::clone(&self.engine);
        let jobs = Arc::clone(&self.jobs);
        tokio::task::spawn_blocking(move || {
            // Retain ownership even if a client disconnects while its command commits.
            let _permit = permit;
            // The reservation and the busy check are one lock acquisition,
            // so a job cannot start in between; the guard outlives the
            // write below. The jobs lock itself is never held across the
            // engine lock, keeping the two lock orders disjoint.
            let _mutation = if let Some(id) = command.mutation_target() {
                let mutation = jobs.mutation(id)?;
                #[cfg(test)]
                jobs.wait_mutation_gate();
                Some(mutation)
            } else {
                None
            };
            let mut engine = engine
                .lock()
                .map_err(|_| RpcError::new(INTERNAL_ERROR, "Configuration worker failed"))?;
            command.execute(&mut engine)
        })
        .await
        .map_err(|_| RpcError::new(INTERNAL_ERROR, "Configuration worker failed"))?
    }
}

impl Service {
    /// Sync jobs bypass the configuration queue: they hold the engine only to
    /// read inputs and commit progress.
    async fn sync(&self, request: &Request) -> Result<Value, RpcError> {
        let pair = |id: &str| {
            id.parse::<PairId>()
                .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))
        };
        let status = match request.method() {
            SYNC_RESOLVE => {
                let params: SyncResolve = parameters(request)?;
                let id = pair(&params.id)?;
                let path = foldover_types::SyncPath::try_from(params.path)
                    .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))?;
                let token = review_token(params.review_token.as_deref())?;
                let (jobs, engine) = (Arc::clone(&self.jobs), Arc::clone(&self.engine));
                // Resolutions record to the engine and keep-both copies
                // files; keep them off the runtime.
                tokio::task::spawn_blocking(move || match params.keep {
                    ConflictKeep::Left => jobs.resolve(&engine, id, &path, Side::Left, token),
                    ConflictKeep::Right => jobs.resolve(&engine, id, &path, Side::Right, token),
                    ConflictKeep::Both => jobs.keep_both(&engine, id, &path, token),
                    ConflictKeep::Skip => jobs.skip(id, &path, token),
                })
                .await
                .map_err(|_| RpcError::new(INTERNAL_ERROR, "The sync engine failed"))??;
                return encode(self.jobs.status(id));
            }
            SYNC_RUN => {
                let params: SyncRun = parameters(request)?;
                let token = review_token(params.review_token.as_deref())?;
                let approval = Approval::new(
                    params.approved_left_deletions,
                    params.approved_right_deletions,
                );
                self.jobs
                    .start(&self.engine, pair(&params.id)?, Work::Run(approval, token))?
            }
            method => {
                let id = pair(&parameters::<SyncKey>(request)?.id)?;
                match method {
                    SYNC_PREVIEW => self.jobs.start(&self.engine, id, Work::Preview)?,
                    SYNC_UNDO => self.jobs.start(&self.engine, id, Work::Undo)?,
                    SYNC_CANCEL => {
                        self.jobs.cancel(id);
                        self.jobs.status(id)
                    }
                    _ => self.jobs.status(id),
                }
            }
        };
        encode(status)
    }
}

enum Command {
    List(Option<PairId>),
    Get(PairId),
    Create(PairDraft),
    Update {
        id: PairId,
        revision: u64,
        configuration: PairDraft,
    },
    Remove {
        id: PairId,
        revision: u64,
    },
}

impl Command {
    fn parse(request: &Request) -> Result<Self, RpcError> {
        let invalid =
            |error: foldover_types::InvalidPair| RpcError::new(INVALID_PARAMS, error.to_string());
        match request.method() {
            PAIR_LIST => {
                let params: PairList = parameters(request)?;
                Ok(Self::List(
                    params
                        .after
                        .map(|id| id.parse())
                        .transpose()
                        .map_err(invalid)?,
                ))
            }
            PAIR_GET => {
                let params: PairKey = parameters(request)?;
                Ok(Self::Get(params.id.parse().map_err(invalid)?))
            }
            PAIR_CREATE => {
                let params: PairInput = parameters(request)?;
                Ok(Self::Create(params.try_into().map_err(invalid)?))
            }
            PAIR_UPDATE => {
                let params: PairUpdate = parameters(request)?;
                Ok(Self::Update {
                    id: params.id.parse().map_err(invalid)?,
                    revision: expected_revision(params.revision)?,
                    configuration: params.configuration.try_into().map_err(invalid)?,
                })
            }
            PAIR_REMOVE => {
                let params: PairRevision = parameters(request)?;
                Ok(Self::Remove {
                    id: params.id.parse().map_err(invalid)?,
                    revision: expected_revision(params.revision)?,
                })
            }
            _ => Err(RpcError::new(METHOD_NOT_FOUND, "Method not found")),
        }
    }

    /// The existing pair whose configuration this command replaces.
    fn mutation_target(&self) -> Option<PairId> {
        match self {
            Self::Update { id, .. } | Self::Remove { id, .. } => Some(*id),
            Self::List(_) | Self::Get(_) | Self::Create(_) => None,
        }
    }

    fn execute(self, engine: &mut Engine) -> Result<Value, RpcError> {
        match self {
            Self::List(after) => {
                let mut pairs = engine
                    .pairs_after(after, PAIR_PAGE_SIZE + 1)
                    .map_err(engine_error)?;
                let next = if pairs.len() > PAIR_PAGE_SIZE {
                    pairs.pop();
                    pairs.last().map(|pair| pair.id().to_string())
                } else {
                    None
                };
                encode(PairPage {
                    pairs: pairs.iter().map(PairRecord::from).collect(),
                    next,
                })
            }
            Self::Get(id) => encode(PairRecord::from(&engine.pair(id).map_err(engine_error)?)),
            Self::Create(draft) => encode(PairRecord::from(
                &engine.create_pair(&draft).map_err(engine_error)?,
            )),
            Self::Update {
                id,
                revision,
                configuration,
            } => encode(PairRecord::from(
                &engine
                    .update_pair(id, revision, &configuration)
                    .map_err(engine_error)?,
            )),
            Self::Remove { id, revision } => encode(PairRecord::from(
                &engine.remove_pair(id, revision).map_err(engine_error)?,
            )),
        }
    }
}

fn expected_revision(revision: u64) -> Result<u64, RpcError> {
    if revision == 0 || revision > i64::MAX as u64 {
        return Err(RpcError::new(
            INVALID_PARAMS,
            foldover_types::InvalidPair::Revision.to_string(),
        ));
    }
    Ok(revision)
}

/// Parse a presented review token; a malformed one is a parameter error
/// before any job or engine work, while `None` is handled by the callee.
fn review_token(value: Option<&str>) -> Result<Option<ReviewToken>, RpcError> {
    value
        .map(|token| token.parse::<ReviewToken>())
        .transpose()
        .map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))
}

fn parameters<T: DeserializeOwned>(request: &Request) -> Result<T, RpcError> {
    serde_json::from_value(
        request
            .params()
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .map_err(|_| RpcError::new(INVALID_PARAMS, "Invalid method parameters"))
}

fn encode(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|_| RpcError::new(INTERNAL_ERROR, "Cannot encode response"))
}

pub(crate) fn engine_error(error: foldover_engine::Error) -> RpcError {
    let code = match error.kind() {
        ErrorKind::Storage => INTERNAL_ERROR,
        ErrorKind::NotFound => PAIR_NOT_FOUND,
        ErrorKind::Changed => STALE_REVISION,
        ErrorKind::Unavailable => ROOT_UNAVAILABLE,
        ErrorKind::Overlap => ROOT_OVERLAP,
        ErrorKind::Invalid => INVALID_PARAMS,
    };
    RpcError::new(code, error.to_string())
}

async fn health(request: Request) -> Result<Value, RpcError> {
    match request.method() {
        SESSION_HELLO => {
            let protocol = request
                .params()
                .and_then(Value::as_object)
                .and_then(|params| params.get("protocol_version"))
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    RpcError::new(INVALID_PARAMS, "session.hello needs protocol_version")
                })?;
            if protocol != u64::from(PROTOCOL_VERSION) {
                return Err(RpcError::new(
                    VERSION_MISMATCH,
                    "Unsupported agent protocol version",
                ));
            }
            serde_json::to_value(Hello {
                protocol_version: PROTOCOL_VERSION,
                core_version: version().to_owned(),
            })
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "Cannot encode hello"))
        }
        DAEMON_STATUS => {
            if request.params().is_some_and(|params| {
                !matches!(params, Value::Object(object) if object.is_empty())
                    && !matches!(params, Value::Array(array) if array.is_empty())
            }) {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "daemon.status takes no parameters",
                ));
            }
            serde_json::to_value(DaemonStatus {
                core_version: version().to_owned(),
                capability: AgentCapability::Sync,
            })
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "Cannot encode status"))
        }
        _ => Err(RpcError::new(METHOD_NOT_FOUND, "Method not found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldover_engine::Completion;
    use foldover_proto::local::SyncProgress;
    use foldover_proto::local::{
        SyncCounts, SyncDeletions, SyncJob, SyncPreview, SyncResult, SyncStatus,
    };
    use serde_json::json;
    use std::{fs, os::unix::ffi::OsStrExt, path::Path};

    fn fixture() -> (tempfile::TempDir, Service) {
        let directory = tempfile::tempdir().unwrap();
        let engine =
            Engine::open(&StateLocation::Directory(directory.path().join("state"))).unwrap();
        (directory, Service::new(engine))
    }

    fn input(directory: &Path, name: &str) -> Value {
        let left = directory.join(format!("{name}-left"));
        let right = directory.join(format!("{name}-right"));
        fs::create_dir(&left).unwrap();
        fs::create_dir(&right).unwrap();
        json!({"name":name, "left_root":left.as_os_str().as_bytes(), "right_root":right.as_os_str().as_bytes(), "mode":{"kind":"sync"}, "options":{"modification_times":"sync","extended_attributes":"sync","permissions":"ignore","durability":"standard","schedule":{"kind":"manual"},"filters":[]}})
    }

    #[tokio::test]
    async fn concurrent_creation_is_serialized_and_updates_require_the_current_revision() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Photos");
        let (first, second) = tokio::join!(
            service.handle(Request::call(1, PAIR_CREATE, Some(input.clone()))),
            service.handle(Request::call(2, PAIR_CREATE, Some(input.clone())))
        );
        let (Ok(created), Err(rejected)) = (if first.is_ok() {
            (first, second)
        } else {
            (second, first)
        }) else {
            panic!("Exactly one reservation must succeed");
        };
        assert_eq!(rejected.code(), ROOT_OVERLAP);
        let id = created["id"].clone();
        let mut updated = input.clone();
        updated["name"] = json!("Renamed");
        let params = json!({"id":id, "revision":created["revision"], "configuration":updated});
        let saved = service
            .handle(Request::call(3, PAIR_UPDATE, Some(params.clone())))
            .await
            .unwrap();
        assert_eq!(saved["revision"], 2);
        assert_eq!(saved["configuration"]["name"], "Renamed");
        assert_eq!(
            service
                .handle(Request::call(4, PAIR_UPDATE, Some(params)))
                .await
                .unwrap_err()
                .code(),
            STALE_REVISION
        );
        assert_eq!(
            service
                .handle(Request::call(5, PAIR_GET, Some(json!({"id":id}))))
                .await
                .unwrap(),
            saved
        );
        let page = service
            .handle(Request::call(6, PAIR_LIST, None))
            .await
            .unwrap();
        assert_eq!(page["pairs"], json!([saved]));
        assert!(page["next"].is_null());
    }

    #[tokio::test]
    async fn malformed_private_parameters_never_mutate_the_catalog() {
        let (directory, service) = fixture();
        let valid = input(directory.path(), "Photos");
        let mut unknown_field = valid.clone();
        unknown_field["run_now"] = json!(true);
        let mut relative_root = valid.clone();
        relative_root["left_root"] = json!([b'a']);
        for params in [json!({}), json!(null), unknown_field, relative_root] {
            assert_eq!(
                service
                    .handle(Request::call(1, PAIR_CREATE, Some(params)))
                    .await
                    .unwrap_err()
                    .code(),
                INVALID_PARAMS
            );
        }
        for (method, params) in [
            (PAIR_LIST, json!({"after":"invalid"})),
            (PAIR_GET, json!({"id":"invalid"})),
            (
                PAIR_UPDATE,
                json!({"id":"01010101010101010101010101010101", "revision":0, "configuration":valid}),
            ),
        ] {
            assert_eq!(
                service
                    .handle(Request::call(1, method, Some(params)))
                    .await
                    .unwrap_err()
                    .code(),
                INVALID_PARAMS
            );
        }
        assert_eq!(
            service
                .handle(Request::call(
                    1,
                    PAIR_GET,
                    Some(json!({"id":"01010101010101010101010101010101"}))
                ))
                .await
                .unwrap_err()
                .code(),
            PAIR_NOT_FOUND
        );
        assert_eq!(
            service
                .handle(Request::call(1, "run.start", None))
                .await
                .unwrap_err()
                .code(),
            METHOD_NOT_FOUND
        );
        assert!(
            service
                .handle(Request::call(1, PAIR_LIST, None))
                .await
                .unwrap()["pairs"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn catalog_pages_have_stable_identity_order_and_no_duplicates() {
        let (directory, service) = fixture();
        let count = PAIR_PAGE_SIZE + 2;
        for index in 0..count {
            service
                .handle(Request::call(
                    1,
                    PAIR_CREATE,
                    Some(input(directory.path(), &format!("Pair {index}"))),
                ))
                .await
                .unwrap();
        }
        let first = service
            .handle(Request::call(1, PAIR_LIST, None))
            .await
            .unwrap();
        assert_eq!(first["pairs"].as_array().unwrap().len(), PAIR_PAGE_SIZE);
        let second = service
            .handle(Request::call(
                1,
                PAIR_LIST,
                Some(json!({"after":first["next"]})),
            ))
            .await
            .unwrap();
        assert!(second["next"].is_null());
        let ids: Vec<_> = first["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .chain(second["pairs"].as_array().unwrap())
            .map(|pair| pair["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), count);
        assert!(ids.windows(2).all(|ids| ids[0] < ids[1]));
    }

    #[tokio::test]
    async fn removal_is_scoped_to_the_validated_pair_revision() {
        let (directory, service) = fixture();
        let pair = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Photos")),
            ))
            .await
            .unwrap();
        for params in [
            json!({"id":pair["id"], "revision":0}),
            json!({"id":pair["id"], "revision":1, "root":"/elsewhere"}),
        ] {
            assert_eq!(
                service
                    .handle(Request::call(1, PAIR_REMOVE, Some(params)))
                    .await
                    .unwrap_err()
                    .code(),
                INVALID_PARAMS
            );
        }
        assert_eq!(
            service
                .handle(Request::call(
                    1,
                    PAIR_REMOVE,
                    Some(json!({"id":pair["id"], "revision":2}))
                ))
                .await
                .unwrap_err()
                .code(),
            STALE_REVISION
        );
        assert_eq!(
            service
                .handle(Request::call(1, PAIR_GET, Some(json!({"id":pair["id"]}))))
                .await
                .unwrap(),
            pair
        );
        assert_eq!(
            service
                .handle(Request::call(
                    1,
                    PAIR_REMOVE,
                    Some(json!({"id":pair["id"], "revision":1}))
                ))
                .await
                .unwrap(),
            pair
        );
        assert_eq!(
            service
                .handle(Request::call(
                    1,
                    PAIR_REMOVE,
                    Some(json!({"id":pair["id"], "revision":1}))
                ))
                .await
                .unwrap_err()
                .code(),
            PAIR_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn health_dispatch_validates_parameters_and_has_no_private_state() {
        let status = health(Request::call(1, DAEMON_STATUS, None)).await.unwrap();
        let status: DaemonStatus = serde_json::from_value(status).unwrap();
        assert_eq!(status.capability, AgentCapability::Sync);
        assert_eq!(status.core_version, version());
        assert_eq!(
            health(Request::call(1, "pair.list", None))
                .await
                .unwrap_err()
                .code(),
            METHOD_NOT_FOUND
        );
        assert_eq!(
            health(Request::call(
                1,
                DAEMON_STATUS,
                Some(json!({"root":"private"}))
            ))
            .await
            .unwrap_err()
            .code(),
            INVALID_PARAMS
        );
        assert_eq!(
            health(Request::call(1, SESSION_HELLO, None))
                .await
                .unwrap_err()
                .code(),
            INVALID_PARAMS
        );
        assert_eq!(
            health(Request::call(
                1,
                SESSION_HELLO,
                Some(json!({"protocol_version": PROTOCOL_VERSION + 1}))
            ))
            .await
            .unwrap_err()
            .code(),
            VERSION_MISMATCH
        );
        let hello = health(Request::call(
            1,
            SESSION_HELLO,
            Some(json!({"protocol_version": PROTOCOL_VERSION})),
        ))
        .await
        .unwrap();
        assert_eq!(hello["core_version"], version());
    }

    #[test]
    fn version_is_the_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::version().is_empty());
    }

    async fn settled(service: &Service, key: &Value) -> Value {
        // Generous: runs wait out the racy-timestamp window on slow hosts.
        settle(service, key, 3_000).await
    }

    /// Runs that copy or archive dozens of entries take longer to finish.
    async fn settled_slow(service: &Service, key: &Value) -> Value {
        settle(service, key, 10_000).await
    }

    async fn settle(service: &Service, key: &Value, polls: usize) -> Value {
        for _ in 0..polls {
            let status = service
                .handle(Request::call(9, SYNC_STATUS, Some(key.clone())))
                .await
                .unwrap();
            if status["state"] != "working" {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("The sync job did not finish");
    }

    /// Wait for every readiness probe the last tick dispatched.
    async fn probes_idle(service: &Service) {
        for _ in 0..3_000 {
            if service.scheduler.probes_settled() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("A readiness probe did not finish");
    }

    /// Tick, wait for the probes it dispatched, and tick again so their
    /// results can start runs; returns every pair started.
    async fn run_due_started(service: &Service, at: std::time::Instant) -> Vec<PairId> {
        let mut started = service.run_due(at);
        probes_idle(service).await;
        started.extend(service.run_due(at));
        started
    }

    /// A pair's first run must be reviewed by a person (PLAN.md §3.2):
    /// preview it, then run that plan back with its token. Returns the
    /// run's settled status.
    async fn first_sync(service: &Service, key: &Value) -> Value {
        first_run(service, key, 3_000).await
    }

    /// `first_sync` for plans that copy or archive dozens of entries.
    async fn first_sync_slow(service: &Service, key: &Value) -> Value {
        first_run(service, key, 10_000).await
    }

    async fn first_run(service: &Service, key: &Value, polls: usize) -> Value {
        service
            .handle(Request::call(8, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settle(service, key, polls).await;
        let run = json!({"id": key["id"], "approved_left_deletions": 0, "approved_right_deletions": 0, "review_token": previewed["preview"]["review_token"]});
        service
            .handle(Request::call(9, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        settle(service, key, polls).await
    }

    #[tokio::test]
    async fn previews_and_runs_execute_as_background_jobs() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let left = directory.path().join("Docs-left");
        let right = directory.path().join("Docs-right");
        fs::write(left.join("file"), b"data").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        assert_eq!(
            service
                .handle(Request::call(2, SYNC_STATUS, Some(key.clone())))
                .await
                .unwrap()["state"],
            "idle"
        );

        let started = service
            .handle(Request::call(3, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        assert_eq!(started["state"], "working");
        assert_eq!(started["job"], "preview");
        let previewed = settled(&service, &key).await;
        assert_eq!(previewed["state"], "previewed", "{previewed}");
        assert_eq!(previewed["preview"]["counts"]["copies"], 1);
        assert_eq!(
            previewed["preview"]["items"][0]["action"],
            json!({"kind": "copy", "to": "right"})
        );
        assert!(!right.join("file").exists());

        // PLAN.md §3.2: a pair's first run is always reviewed. Unreviewed
        // it stops with the plan a person must see; nothing is written.
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(4, SYNC_RUN, Some(run.clone())))
            .await
            .unwrap();
        let held = settled(&service, &key).await;
        assert_eq!(held["state"], "needsApproval", "{held}");
        assert_eq!(held["preview"]["counts"]["copies"], 1);
        assert!(!right.join("file").exists());
        assert!(!right.join(".foldover/id").exists());

        // The same plan named by the held preview's token is reviewed
        // and runs.
        let mut reviewed = run.clone();
        reviewed["review_token"] = held["preview"]["review_token"].clone();
        service
            .handle(Request::call(4, SYNC_RUN, Some(reviewed)))
            .await
            .unwrap();
        let finished = settled(&service, &key).await;
        assert_eq!(finished["state"], "finished", "{finished}");
        assert_eq!(finished["result"]["copied"], 1);
        assert_eq!(finished["result"]["failure_count"], 0);
        assert_eq!(fs::read(right.join("file")).unwrap(), b"data");
        assert!(right.join(".foldover/id").exists());

        // Configuration requests stay available, and the capability says so.
        let status = service
            .handle(Request::call(5, DAEMON_STATUS, None))
            .await
            .unwrap();
        assert_eq!(status["capability"], "sync");
    }

    #[tokio::test]
    async fn sync_requests_validate_their_parameters() {
        let (_directory, service) = fixture();
        for (method, params) in [
            (SYNC_PREVIEW, json!({"id": "bad"})),
            (SYNC_STATUS, json!({})),
            (SYNC_RUN, json!({"id": "01010101010101010101010101010101"})),
        ] {
            assert_eq!(
                service
                    .handle(Request::call(1, method, Some(params)))
                    .await
                    .unwrap_err()
                    .code(),
                INVALID_PARAMS
            );
        }
        let missing = json!({"id": "01010101010101010101010101010101"});
        service
            .handle(Request::call(2, SYNC_PREVIEW, Some(missing.clone())))
            .await
            .unwrap();
        let failed = settled(&service, &missing).await;
        assert_eq!(failed["state"], "failed");
    }

    #[tokio::test]
    async fn scheduled_pairs_run_unattended_once_due_after_a_first_sync() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Notes");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let left = directory.path().join("Notes-left");
        fs::write(left.join("a"), b"a").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        assert_eq!(
            created["configuration"]["options"]["schedule"]["minutes"],
            1
        );
        let key = json!({"id": created["id"]});
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        // A first run is always reviewed by a person.
        assert!(service.run_due(at(120)).is_empty());
        assert_eq!(first_sync(&service, &key).await["result"]["copied"], 1);

        fs::write(left.join("b"), b"b").unwrap();
        assert!(service.run_due(at(30)).is_empty());
        assert_eq!(run_due_started(&service, at(61)).await.len(), 1);
        let finished = settled(&service, &key).await;
        assert_eq!(finished["result"]["copied"], 1, "{finished}");
        assert!(service.run_due(at(90)).is_empty());
        assert_eq!(run_due_started(&service, at(122)).await.len(), 1);
        settled(&service, &key).await;
    }

    #[tokio::test]
    async fn conflicts_are_resolved_from_the_latest_preview() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Draft");
        let (left, right) = (
            directory.path().join("Draft-left"),
            directory.path().join("Draft-right"),
        );
        fs::write(left.join("text"), b"base").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        first_sync(&service, &key).await;
        fs::write(left.join("text"), b"left").unwrap();
        fs::write(right.join("text"), b"right").unwrap();

        let resolve = json!({"id": created["id"], "path": b"text", "keep": "right"});
        assert_eq!(
            service
                .handle(Request::call(3, SYNC_RESOLVE, Some(resolve.clone())))
                .await
                .unwrap_err()
                .code(),
            foldover_proto::local::PREVIEW_REQUIRED
        );
        service
            .handle(Request::call(4, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;
        assert_eq!(previewed["preview"]["counts"]["conflicts"], 1);
        // The choice must name the preview it was made from.
        let mut resolve = resolve.clone();
        resolve["review_token"] = previewed["preview"]["review_token"].clone();
        service
            .handle(Request::call(5, SYNC_RESOLVE, Some(resolve)))
            .await
            .unwrap();

        service
            .handle(Request::call(6, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;
        assert_eq!(
            previewed["preview"]["counts"]["conflicts"], 0,
            "{previewed}"
        );
        assert_eq!(previewed["preview"]["counts"]["copies"], 1);
        service
            .handle(Request::call(7, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 1);
        assert_eq!(fs::read(left.join("text")).unwrap(), b"right");
    }

    #[tokio::test]
    async fn changed_pairs_run_once_quiet_and_hourly_without_changes() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Live");
        input["options"]["schedule"] = json!({"kind": "onChange"});
        fs::write(directory.path().join("Live-left/a"), b"a").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let id: PairId = created["id"].as_str().unwrap().parse().unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        assert!(service.run_due(at(10)).is_empty());
        service.mark_changed(id, at(20));
        assert!(service.run_due(at(22)).is_empty());
        assert_eq!(run_due_started(&service, at(26)).await, [id]);
        settled(&service, &key).await;
        assert!(service.run_due(at(40)).is_empty());
        assert_eq!(run_due_started(&service, at(26 + 3_600)).await, [id]);
        settled(&service, &key).await;
    }

    #[tokio::test]
    async fn invalid_filter_rules_are_rejected_when_saved() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Rules");
        input["options"]["filters"] = json!(["a{"]);
        assert_eq!(
            service
                .handle(Request::call(1, PAIR_CREATE, Some(input.clone())))
                .await
                .unwrap_err()
                .code(),
            INVALID_PARAMS
        );
        input["options"]["filters"] = json!(["*.tmp", "!keep.tmp"]);
        let created = service
            .handle(Request::call(2, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        assert_eq!(
            created["configuration"]["options"]["filters"],
            json!(["*.tmp", "!keep.tmp"])
        );
    }

    #[tokio::test]
    async fn archived_items_are_listed_and_put_back_on_their_side() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        let left = directory.path().join("Docs-left");
        let right = directory.path().join("Docs-right");
        let key = json!({"id": created["id"]});
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        fs::write(left.join("file"), b"data").unwrap();
        first_sync(&service, &key).await;
        fs::remove_file(left.join("file")).unwrap();
        // The emptied left side archives on the right only after review.
        service
            .handle(Request::call(3, SYNC_RUN, Some(run.clone())))
            .await
            .unwrap();
        let stopped = settled(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        let mut reviewed = run.clone();
        reviewed["review_token"] = stopped["preview"]["review_token"].clone();
        service
            .handle(Request::call(3, SYNC_RUN, Some(reviewed)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["archived"], 1);

        let side = json!({"id": created["id"], "side": "right"});
        let runs = service
            .handle(Request::call(4, ARCHIVE_RUNS, Some(side)))
            .await
            .unwrap();
        assert_eq!(runs["more"], false);
        let name = runs["runs"][0]["name"].clone();
        let query = json!({"id": created["id"], "side": "right", "run": name, "after": null});
        let items = service
            .handle(Request::call(5, ARCHIVE_ITEMS, Some(query)))
            .await
            .unwrap();
        assert_eq!(items["items"], json!([{"path": b"file", "size": 4}]));

        let put_back = json!({"id": created["id"], "side": "right", "run": name, "path": b"file"});
        let placed = service
            .handle(Request::call(6, ARCHIVE_PUT_BACK, Some(put_back.clone())))
            .await
            .unwrap();
        assert_eq!(placed["path"], json!(b"file"));
        assert_eq!(fs::read(right.join("file")).unwrap(), b"data");
        let gone = service
            .handle(Request::call(7, ARCHIVE_PUT_BACK, Some(put_back)))
            .await
            .unwrap_err();
        assert_eq!(gone.code(), foldover_proto::local::ARCHIVE_NOT_FOUND);

        service
            .handle(Request::call(8, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 1);
        assert_eq!(fs::read(left.join("file")).unwrap(), b"data");
    }

    #[tokio::test]
    async fn runs_but_not_previews_enter_the_history() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        fs::write(directory.path().join("Docs-left/file"), b"data").unwrap();
        let key = json!({"id": created["id"]});
        service
            .handle(Request::call(2, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0, "review_token": previewed["preview"]["review_token"]});
        service
            .handle(Request::call(3, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        settled(&service, &key).await;

        let history = service
            .handle(Request::call(4, HISTORY_LIST, Some(key)))
            .await
            .unwrap();
        let runs = history["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1, "{history}");
        assert_eq!(runs[0]["trigger"], "manual");
        assert_eq!(runs[0]["outcome"], "completed");
        assert_eq!(runs[0]["copied"], 1);
        assert!(runs[0]["finished_ms"].as_u64() >= runs[0]["started_ms"].as_u64());
    }

    #[tokio::test]
    async fn undo_reverts_the_newest_sync_and_holds_automatic_runs() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Docs");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let (left, right) = (
            directory.path().join("Docs-left"),
            directory.path().join("Docs-right"),
        );
        fs::write(left.join("keep"), b"k").unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        fs::write(left.join("new"), b"n").unwrap();
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(2, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 1);

        let started = service
            .handle(Request::call(3, SYNC_UNDO, Some(key.clone())))
            .await
            .unwrap();
        assert_eq!(started["job"], "undo");
        let undone = settled(&service, &key).await;
        assert_eq!(undone["state"], "undone", "{undone}");
        assert_eq!(undone["result"]["removed"], 1);
        assert_eq!(undone["result"]["not_undone_count"], 0);
        assert!(!right.join("new").exists());
        assert!(right.join("keep").exists());

        // An automatic run would redo the undo unseen; it waits for a person.
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);
        assert!(run_due_started(&service, at(600)).await.is_empty());
        assert_eq!(settled(&service, &key).await["state"], "undone");

        let history = service
            .handle(Request::call(4, HISTORY_LIST, Some(key.clone())))
            .await
            .unwrap();
        assert_eq!(history["runs"][0]["trigger"], "undo", "{history}");
        assert_eq!(history["runs"][0]["archived"], 1);

        // A restart forgets the undone status, not the hold.
        drop(service);
        let engine =
            Engine::open(&StateLocation::Directory(directory.path().join("state"))).unwrap();
        let service = Service::new(engine);
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);
        assert!(run_due_started(&service, at(600)).await.is_empty());

        // The undo spent the journal.
        service
            .handle(Request::call(5, SYNC_UNDO, Some(key.clone())))
            .await
            .unwrap();
        let again = settled(&service, &key).await;
        assert_eq!(again["state"], "failed");
        assert!(
            again["message"]
                .as_str()
                .unwrap()
                .contains("no sync to undo"),
            "{again}"
        );

        // Once a person syncs again, automatic runs resume.
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(6, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 1);
        assert_eq!(run_due_started(&service, at(600)).await.len(), 1);
    }

    #[tokio::test]
    async fn previewed_changes_can_go_the_other_way_or_wait_one_run() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        let (left, right) = (
            directory.path().join("Docs-left"),
            directory.path().join("Docs-right"),
        );
        fs::write(left.join("keep"), b"k").unwrap();
        fs::write(left.join("gone"), b"g").unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        fs::remove_file(left.join("gone")).unwrap();
        fs::write(left.join("later"), b"l").unwrap();
        fs::write(left.join("now"), b"n").unwrap();

        service
            .handle(Request::call(2, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;
        let token = previewed["preview"]["review_token"].clone();
        let choose = |path: &[u8], keep: &str| json!({"id": created["id"], "path": path, "keep": keep, "review_token": token});
        // Keeping B's file copies it back to A instead of archiving it.
        service
            .handle(Request::call(
                3,
                SYNC_RESOLVE,
                Some(choose(b"gone", "right")),
            ))
            .await
            .unwrap();
        service
            .handle(Request::call(
                4,
                SYNC_RESOLVE,
                Some(choose(b"later", "skip")),
            ))
            .await
            .unwrap();

        service
            .handle(Request::call(5, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let chosen = settled(&service, &key).await;
        assert_eq!(chosen["preview"]["counts"]["archives"], 0, "{chosen}");
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0, "review_token": chosen["preview"]["review_token"]});
        service
            .handle(Request::call(6, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 2);
        assert_eq!(fs::read(left.join("gone")).unwrap(), b"g");
        assert!(right.join("now").exists());
        assert!(!right.join("later").exists());

        // A skip lasts one run.
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(7, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 1);
        assert!(right.join("later").exists());
    }

    /// Opening the Mac-to-Mac view must not start the network, which
    /// would show the Local Network prompt before anything explains it.
    #[tokio::test]
    async fn peer_status_reports_mac_to_mac_off_without_starting_it() {
        let (_directory, service) = fixture();
        let status = service
            .handle(Request::call(1, PEER_STATUS, None))
            .await
            .unwrap();
        assert_eq!(status["device"], Value::Null);
        assert_eq!(status["devices"], json!([]));
        assert!(service.peers.get().is_none());
    }

    /// Two agents already paired with each other over loopback, as
    /// (A, B, A's device, B's device).
    async fn paired_agents() -> (
        (tempfile::TempDir, Service),
        (tempfile::TempDir, Service),
        foldover_types::DeviceId,
        foldover_types::DeviceId,
    ) {
        let loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let (a, b) = (fixture(), fixture());
        let identities = [
            foldover_engine::PeerIdentity::generate().unwrap(),
            foldover_engine::PeerIdentity::generate().unwrap(),
        ];
        let devices = identities
            .each_ref()
            .map(|identity| foldover_types::DeviceId::from_bytes(*identity.id().as_bytes()));
        let [a_identity, b_identity] = identities;
        for (service, other, name) in [(&a.1, devices[1], "Studio"), (&b.1, devices[0], "Laptop")] {
            service
                .engine
                .lock()
                .unwrap()
                .add_device(other, name.into())
                .unwrap();
        }
        for (service, identity) in [(&a.1, a_identity), (&b.1, b_identity)] {
            let link = peer::open(
                &service.engine,
                identity,
                loopback,
                foldover_engine::Presence::Unlisted,
            )
            .await
            .unwrap();
            assert!(service.peers.set(link).is_ok());
        }
        let b_address = b.1.peers.get().unwrap().address().unwrap();
        a.1.peers.get().unwrap().reach(devices[1], b_address);
        (a, b, devices[0], devices[1])
    }

    /// P5.2: a pair's side can be a folder another Mac shares; that Mac
    /// observes and writes it, archiving what syncs replace or remove.
    #[tokio::test]
    async fn a_folder_another_mac_shares_syncs_through_it() {
        let ((a_directory, a), (_b_directory, b), a_device, b_device) = paired_agents().await;
        let shared = tempfile::tempdir().unwrap();
        fs::create_dir(shared.path().join("album")).unwrap();
        fs::write(shared.path().join("album/one.jpg"), b"one").unwrap();
        fs::write(shared.path().join("two.jpg"), b"two").unwrap();
        let share = b
            .handle(Request::call(
                1,
                SHARE_ADD,
                Some(json!({"device": a_device.to_string(), "root": shared.path().as_os_str().as_bytes(), "name": "Photos"})),
            ))
            .await
            .unwrap();

        let local = a_directory.path().join("photos");
        fs::create_dir(&local).unwrap();
        let mut mac = input(a_directory.path(), "Mac");
        mac["left_root"] = json!(local.as_os_str().as_bytes());
        mac["right_root"] =
            json!(format!("peer://{b_device}/{}", share["id"].as_str().unwrap()).into_bytes());
        let created = a
            .handle(Request::call(2, PAIR_CREATE, Some(mac)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        a.handle(Request::call(3, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&a, &key).await;
        assert_eq!(previewed["state"], "previewed", "{previewed}");
        assert_eq!(previewed["preview"]["counts"]["copies"], 3, "{previewed}");
        assert!(
            previewed["preview"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["action"] == json!({"kind": "copy", "to": "left"}))
        );

        // The first sync copies B's folder to A through B.
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0, "review_token": previewed["preview"]["review_token"]});
        a.handle(Request::call(5, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        let ran = settled(&a, &key).await;
        assert_eq!(ran["result"]["failure_count"], 0, "{ran}");
        assert_eq!(ran["result"]["copied"], 3, "{ran}");
        assert_eq!(fs::read(local.join("album/one.jpg")).unwrap(), b"one");
        assert_eq!(fs::read(local.join("two.jpg")).unwrap(), b"two");

        // Changes on A reach B's folder; a deletion is archived there.
        fs::write(local.join("three.jpg"), b"three").unwrap();
        fs::write(local.join("two.jpg"), b"two, edited").unwrap();
        fs::remove_file(local.join("album/one.jpg")).unwrap();
        a.handle(Request::call(6, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let reviewed = settled(&a, &key).await;
        assert_eq!(reviewed["preview"]["right"]["deletions"], 1, "{reviewed}");
        let again = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 1, "review_token": reviewed["preview"]["review_token"]});
        a.handle(Request::call(7, SYNC_RUN, Some(again)))
            .await
            .unwrap();
        let ran = settled(&a, &key).await;
        assert_eq!(ran["result"]["failure_count"], 0, "{ran}");
        assert_eq!(fs::read(shared.path().join("three.jpg")).unwrap(), b"three");
        assert_eq!(
            fs::read(shared.path().join("two.jpg")).unwrap(),
            b"two, edited"
        );
        assert!(!shared.path().join("album/one.jpg").exists());
        let archive = shared
            .path()
            .join(".foldover/archive")
            .join(ran["result"]["archive"].as_str().unwrap());
        assert_eq!(fs::read(archive.join("album/one.jpg")).unwrap(), b"one");
        assert_eq!(fs::read(archive.join("two.jpg")).unwrap(), b"two");

        // A folder not shared with A is refused by B.
        let mut other = input(a_directory.path(), "Other");
        other["right_root"] = json!(format!("peer://{b_device}/{}", "ab".repeat(16)).into_bytes());
        let refused = a
            .handle(Request::call(4, PAIR_CREATE, Some(other)))
            .await
            .unwrap_err();
        assert!(
            refused.message().contains("not shared"),
            "{}",
            refused.message()
        );
    }

    /// P5.3: a paired Mac reports changes in a folder it shares, so a
    /// pair with that folder runs when it changes.
    #[tokio::test]
    async fn changes_in_a_folder_another_mac_shares_are_reported() {
        let ((a_directory, a), (_b_directory, b), a_device, b_device) = paired_agents().await;
        let shared = tempfile::tempdir().unwrap();
        let share = b
            .handle(Request::call(
                1,
                SHARE_ADD,
                Some(json!({"device": a_device.to_string(), "root": shared.path().as_os_str().as_bytes(), "name": "Photos"})),
            ))
            .await
            .unwrap();
        let mut mac = input(a_directory.path(), "Mac");
        mac["right_root"] =
            json!(format!("peer://{b_device}/{}", share["id"].as_str().unwrap()).into_bytes());
        let created = a
            .handle(Request::call(2, PAIR_CREATE, Some(mac)))
            .await
            .unwrap();
        let pair = a.engine.lock().unwrap().pairs().unwrap()[0].clone();
        assert_eq!(pair.id().to_string(), created["id"]);

        let (reported, mut reports) = tokio::sync::mpsc::unbounded_channel();
        let mut watcher = foldover_engine::PairWatcher::new(move |id| {
            let _ = reported.send(id);
        })
        .unwrap();
        let reach = a.engine.lock().unwrap().reach();
        assert!(
            watcher
                .watch(std::slice::from_ref(&pair), &reach)
                .is_empty()
        );
        // Without the link, the folder cannot be watched and says so.
        let unwatched = foldover_engine::PairWatcher::new(|_| {}).unwrap().watch(
            std::slice::from_ref(&pair),
            &foldover_engine::Reach::default(),
        );
        assert_eq!(unwatched.len(), 1);

        // Changes keep being reported once B watches.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut index = 0;
        loop {
            fs::write(shared.path().join(format!("{index}.jpg")), b"new").unwrap();
            index += 1;
            match tokio::time::timeout(std::time::Duration::from_millis(1_500), reports.recv())
                .await
            {
                Ok(Some(id)) => {
                    assert_eq!(id, pair.id());
                    break;
                }
                _ => assert!(std::time::Instant::now() < deadline, "no change reported"),
            }
        }
    }

    /// Two agents pair over loopback; the pin survives a restart and
    /// revoking forgets it.
    #[tokio::test]
    async fn two_agents_pair_only_after_both_people_confirm() {
        let loopback = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let (a_directory, a) = fixture();
        let (_b_directory, b) = fixture();
        // Each agent gets its own identity; the Keychain's is shared.
        let a_identity = foldover_engine::PeerIdentity::generate().unwrap();
        let a_secret = a_identity.to_secret_bytes().unwrap();
        let start = |service: &Service, identity| {
            let service = service.clone();
            async move {
                let link = peer::open(
                    &service.engine,
                    identity,
                    loopback,
                    foldover_engine::Presence::Unlisted,
                )
                .await
                .unwrap();
                assert!(service.peers.set(link).is_ok());
            }
        };
        start(&a, a_identity).await;
        start(&b, foldover_engine::PeerIdentity::generate().unwrap()).await;
        let call = |service: &Service, method: &'static str, params: Option<Value>| {
            let service = service.clone();
            async move { service.handle(Request::call(1, method, params)).await }
        };

        let b_status = call(&b, PEER_PAIRING_OPEN, Some(json!({"minutes": 5})))
            .await
            .unwrap();
        let window = b_status["pairing"].as_str().unwrap().to_owned();
        let a_device = call(&a, PEER_STATUS, None).await.unwrap()["device"].clone();
        let b_device = b_status["device"].clone();
        let shown = call(
            &a,
            PEER_PAIR,
            Some(json!({"device": b_device, "address": window})),
        )
        .await
        .unwrap();

        let mut waiting = Value::Null;
        for _ in 0..1_000 {
            waiting = call(&b, PEER_STATUS, None).await.unwrap()["pending"][0].clone();
            if !waiting.is_null() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(waiting["code"], shown["code"]);
        assert_eq!(waiting["device"], a_device);

        call(&a, PEER_CONFIRM, Some(json!({"device": b_device})))
            .await
            .unwrap();
        call(&b, PEER_CONFIRM, Some(json!({"device": a_device})))
            .await
            .unwrap();
        let mut devices = Value::Null;
        for _ in 0..1_000 {
            devices = call(&a, PEER_STATUS, None).await.unwrap()["devices"].clone();
            if devices
                .as_array()
                .is_some_and(|devices| !devices.is_empty())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(devices[0]["device"], b_device);

        // B shares a folder with A only; A sees it by asking B.
        let photos = tempfile::tempdir().unwrap();
        let root = photos.path().as_os_str().as_bytes().to_vec();
        let shared = call(
            &b,
            SHARE_ADD,
            Some(json!({"device": a_device, "root": root, "name": " Photos "})),
        )
        .await
        .unwrap();
        assert_eq!(shared["name"], "Photos");
        let stranger = "ab".repeat(32);
        let refused = call(
            &b,
            SHARE_ADD,
            Some(json!({"device": stranger, "root": root, "name": "Photos"})),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code(), PAIR_NOT_FOUND);
        let b_link = b.peers.get().unwrap();
        a.peers.get().unwrap().reach(
            b_device.as_str().unwrap().parse().unwrap(),
            b_link.address().unwrap(),
        );
        let offered = call(&a, PEER_SHARES, Some(json!({"device": b_device})))
            .await
            .unwrap();
        assert_eq!(
            offered["shares"],
            json!([{"id": shared["id"], "name": "Photos"}])
        );

        // Forgetting A ends what B shared with it.
        call(&b, PEER_REVOKE, Some(json!({"device": a_device})))
            .await
            .unwrap();
        let listed = call(&b, SHARE_LIST, None).await.unwrap();
        assert_eq!(listed["shares"], json!([]));

        // A restarted agent still knows the device.
        drop(a);
        let engine =
            Engine::open(&StateLocation::Directory(a_directory.path().join("state"))).unwrap();
        let a = Service::new(engine);
        start(
            &a,
            foldover_engine::PeerIdentity::from_secret_bytes(&a_secret).unwrap(),
        )
        .await;
        let status = call(&a, PEER_STATUS, None).await.unwrap();
        assert_eq!(status["device"], a_device);
        assert_eq!(status["devices"][0]["device"], b_device);

        let revoked = call(&a, PEER_REVOKE, Some(json!({"device": b_device})))
            .await
            .unwrap();
        assert_eq!(revoked["devices"], json!([]));
        let unknown = call(&a, PEER_CONFIRM, Some(json!({"device": b_device})))
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), INVALID_PARAMS);
    }

    #[tokio::test]
    async fn scheduled_runs_wait_for_a_missing_folder_without_failing() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Disk");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        // Unmounting the disk removes its mount point.
        let right = directory.path().join("Disk-right");
        let unmounted = directory.path().join("unmounted");
        fs::rename(&right, &unmounted).unwrap();
        assert!(service.run_due(at(61)).is_empty());
        probes_idle(&service).await;
        let status = service
            .handle(Request::call(3, SYNC_STATUS, Some(key.clone())))
            .await
            .unwrap();
        assert_eq!(status, json!({"state": "waiting", "side": "right"}));

        fs::rename(&unmounted, &right).unwrap();
        // A missing folder is rechecked a cadence after its result was
        // first seen, not every tick.
        assert!(service.run_due(at(66)).is_empty());
        let id: PairId = created["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            run_due_started(&service, at(66 + crate::schedule::PROBE.as_secs())).await,
            [id]
        );
        assert_eq!(settled(&service, &key).await["state"], "finished");
        let history = service
            .handle(Request::call(4, HISTORY_LIST, Some(key)))
            .await
            .unwrap();
        assert_eq!(history["runs"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_missing_folder_is_probed_again_on_a_cadence_not_every_tick() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Away");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;

        let probes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        service.scheduler.set_probe({
            let probes = std::sync::Arc::clone(&probes);
            move |_, _| {
                probes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Some(Side::Left)
            }
        });
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        assert!(service.run_due(at(61)).is_empty());
        probes_idle(&service).await;
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The result is stamped when the next tick first sees it, so the
        // cadence counts from there, not from dispatch.
        assert!(service.run_due(at(66)).is_empty());
        assert!(service.run_due(at(90)).is_empty());
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            service
                .run_due(at(66 + crate::schedule::PROBE.as_secs()))
                .is_empty()
        );
        probes_idle(&service).await;
        assert_eq!(probes.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_parked_folder_check_does_not_hold_another_pair() {
        let (directory, service) = fixture();
        let mut ids = Vec::new();
        for name in ["Parked", "Ready"] {
            let mut input = input(directory.path(), name);
            input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
            let created = service
                .handle(Request::call(1, PAIR_CREATE, Some(input)))
                .await
                .unwrap();
            let key = json!({"id": created["id"]});
            first_sync(&service, &key).await;
            ids.push(created["id"].as_str().unwrap().parse::<PairId>().unwrap());
        }
        let (parked_id, ready_id) = (ids[0], ids[1]);

        let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release =
            std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        service.scheduler.set_probe({
            let entered = std::sync::Arc::clone(&entered);
            let release = std::sync::Arc::clone(&release);
            move |pair, _| {
                if pair.id() != parked_id {
                    return None;
                }
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                // Park until released, like a server that never answers.
                let (lock, changed) = &*release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
                Some(Side::Left)
            }
        });
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        assert!(service.run_due(at(61)).is_empty());
        // Wait until the first pair's check is parked on the blocking pool.
        for _ in 0..3_000 {
            if entered.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 1);

        // The other pair schedules while the first's check is parked.
        let mut started = Vec::new();
        for _ in 0..3_000 {
            started = service.run_due(at(62));
            if !started.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(started, [ready_id]);
        assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 1);
        let key = json!({"id": ready_id.to_string()});
        assert_eq!(settled(&service, &key).await["state"], "finished");

        // The parked check ran once; releasing it shows the missing side.
        let (lock, changed) = &*release;
        *lock.lock().unwrap() = true;
        changed.notify_all();
        probes_idle(&service).await;
        let status = service
            .handle(Request::call(
                3,
                SYNC_STATUS,
                Some(json!({"id": parked_id.to_string()})),
            ))
            .await
            .unwrap();
        assert_eq!(status, json!({"state": "waiting", "side": "left"}));
    }

    #[tokio::test]
    async fn a_slow_folder_check_still_starts_the_pair_once_it_lands() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Slow");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        let id = created["id"].as_str().unwrap().parse::<PairId>().unwrap();

        let entered = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let release =
            std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        service.scheduler.set_probe({
            let entered = std::sync::Arc::clone(&entered);
            let release = std::sync::Arc::clone(&release);
            move |_, _| {
                entered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (lock, changed) = &*release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = changed.wait(released).unwrap();
                }
                None
            }
        });
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        assert!(service.run_due(at(61)).is_empty());
        for _ in 0..3_000 {
            if entered.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Between pool queueing and a slow server the result lands over a
        // cadence after its tick — it must still count, not be born stale.
        let (lock, changed) = &*release;
        *lock.lock().unwrap() = true;
        changed.notify_all();
        probes_idle(&service).await;

        let started =
            service.run_due(at(61) + crate::schedule::PROBE + std::time::Duration::from_secs(5));
        assert_eq!(started, [id]);
        assert_eq!(entered.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn keeping_both_saves_folder_b_beside_folder_a() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Both")),
            ))
            .await
            .unwrap();
        let (left, right) = (
            directory.path().join("Both-left"),
            directory.path().join("Both-right"),
        );
        let key = json!({"id": created["id"]});
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        fs::write(left.join("text.md"), b"base").unwrap();
        first_sync(&service, &key).await;
        fs::write(left.join("text.md"), b"left").unwrap();
        fs::write(right.join("text.md"), b"right").unwrap();
        service
            .handle(Request::call(3, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;

        let both = json!({"id": created["id"], "path": b"text.md", "keep": "both", "review_token": previewed["preview"]["review_token"]});
        service
            .handle(Request::call(4, SYNC_RESOLVE, Some(both)))
            .await
            .unwrap();
        service
            .handle(Request::call(5, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        let finished = settled(&service, &key).await;
        assert_eq!(finished["result"]["copied"], 2, "{finished}");

        for folder in [&left, &right] {
            assert_eq!(fs::read(folder.join("text.md")).unwrap(), b"left");
            let copies: Vec<_> = fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| name.starts_with("text (from Folder B, "))
                .collect();
            assert_eq!(copies.len(), 1, "{copies:?}");
            assert_eq!(fs::read(folder.join(&copies[0])).unwrap(), b"right");
        }
    }

    #[tokio::test]
    async fn every_previewed_item_can_be_paged_by_kind() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Many")),
            ))
            .await
            .unwrap();
        let left = directory.path().join("Many-left");
        for index in 0..150 {
            fs::write(left.join(format!("file{index:03}")), b"x").unwrap();
        }
        let key = json!({"id": created["id"]});
        let query = |token: &Value, after: Value, kind: Value| json!({"id": created["id"], "after": after, "kind": kind, "review_token": token});
        assert_eq!(
            service
                .handle(Request::call(
                    2,
                    SYNC_ITEMS,
                    Some(query(&Value::Null, Value::Null, Value::Null))
                ))
                .await
                .unwrap_err()
                .code(),
            foldover_proto::local::PREVIEW_REQUIRED
        );
        service
            .handle(Request::call(3, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;
        let token = previewed["preview"]["review_token"].clone();

        // Paging without the held preview's token is refused.
        assert_eq!(
            service
                .handle(Request::call(
                    3,
                    SYNC_ITEMS,
                    Some(json!({"id": created["id"], "after": null, "kind": null}))
                ))
                .await
                .unwrap_err()
                .code(),
            foldover_proto::local::PREVIEW_REQUIRED
        );

        let first = service
            .handle(Request::call(
                4,
                SYNC_ITEMS,
                Some(query(&token, Value::Null, json!("copy"))),
            ))
            .await
            .unwrap();
        let items = first["items"].as_array().unwrap();
        assert_eq!((items.len(), first["more"].clone()), (100, json!(true)));
        let last = items[99]["path"].clone();
        let rest = service
            .handle(Request::call(
                5,
                SYNC_ITEMS,
                Some(query(&token, last, Value::Null)),
            ))
            .await
            .unwrap();
        assert_eq!(rest["items"].as_array().unwrap().len(), 50);
        assert_eq!(rest["more"], false);
        assert_eq!(rest["items"][49]["path"], json!(b"file149"));
        let archives = service
            .handle(Request::call(
                6,
                SYNC_ITEMS,
                Some(query(&token, Value::Null, json!("archive"))),
            ))
            .await
            .unwrap();
        assert_eq!(archives, json!({"items": [], "more": false}));

        // A newer preview of a changed plan replaces the held plan; the
        // old token pages nothing anymore.
        fs::write(left.join("file150"), b"x").unwrap();
        service
            .handle(Request::call(7, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        settled(&service, &key).await;
        assert_eq!(
            service
                .handle(Request::call(
                    8,
                    SYNC_ITEMS,
                    Some(query(&token, Value::Null, Value::Null))
                ))
                .await
                .unwrap_err()
                .code(),
            foldover_proto::local::PREVIEW_REQUIRED
        );
    }

    #[tokio::test]
    async fn a_pause_holds_automatic_syncs_until_resumed() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Paused");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        let at = |seconds| service.scheduler.started() + std::time::Duration::from_secs(seconds);

        let invalid = service
            .handle(Request::call(
                3,
                AUTOMATION_PAUSE,
                Some(json!({"minutes": 0})),
            ))
            .await
            .unwrap_err();
        assert_eq!(invalid.code(), INVALID_PARAMS);
        let paused = service
            .handle(Request::call(
                4,
                AUTOMATION_PAUSE,
                Some(json!({"minutes": null})),
            ))
            .await
            .unwrap();
        assert_eq!(paused, json!({"paused": true, "until_ms": null}));
        assert!(service.run_due(at(61)).is_empty());

        let resumed = service
            .handle(Request::call(5, AUTOMATION_RESUME, None))
            .await
            .unwrap();
        assert_eq!(resumed["paused"], false);
        assert_eq!(run_due_started(&service, at(62)).await.len(), 1);
        settled(&service, &key).await;

        let timed = service
            .handle(Request::call(
                6,
                AUTOMATION_PAUSE,
                Some(json!({"minutes": 30})),
            ))
            .await
            .unwrap();
        assert!(timed["until_ms"].as_u64().is_some());
        assert_eq!(
            service
                .handle(Request::call(7, AUTOMATION_STATUS, None))
                .await
                .unwrap(),
            timed
        );
    }

    fn sync_preview() -> SyncPreview {
        let deletions = || SyncDeletions {
            deletions: 0,
            paths: 0,
            needs_approval: false,
        };
        SyncPreview {
            items: Vec::new(),
            counts: SyncCounts::default(),
            left: deletions(),
            right: deletions(),
            review_token: "ab".repeat(32),
            pair_revision: 1,
        }
    }

    fn sync_finished(completion: Completion) -> SyncStatus {
        SyncStatus::Finished {
            result: SyncResult {
                cancelled: completion == Completion::Cancelled,
                copied: 0,
                updated: 0,
                archived: 0,
                recovered: 0,
                conflicts: 0,
                failures: Vec::new(),
                failure_count: 0,
                archive: String::new(),
            },
        }
    }

    #[tokio::test]
    async fn unattended_runs_skip_pairs_whose_result_waits_for_review() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Docs");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        fs::write(directory.path().join("Docs-left/file"), b"data").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let id: PairId = created["id"].as_str().unwrap().parse().unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        // States a person must see are never replaced by the scheduler.
        for status in [
            SyncStatus::Previewed {
                preview: sync_preview(),
            },
            SyncStatus::NeedsApproval {
                preview: sync_preview(),
            },
            SyncStatus::Failed {
                message: "read error".into(),
            },
            SyncStatus::Working {
                job: SyncJob::Run,
                cancelling: false,
                progress: SyncProgress::default(),
            },
        ] {
            service.jobs.force_status(id, status.clone());
            assert!(service.run_due(at(120)).is_empty(), "{status:?}");
            assert_eq!(service.jobs.status(id), status);
        }

        // Finished runs, cancelled or not, keep the schedule recurring.
        for (completion, later) in [(Completion::Finished, 120), (Completion::Cancelled, 240)] {
            service.jobs.force_status(id, sync_finished(completion));
            assert_eq!(
                run_due_started(&service, at(later)).await,
                [id],
                "{completion:?}"
            );
            settled(&service, &key).await;
        }
    }

    #[tokio::test]
    async fn a_stopped_unattended_run_stays_until_a_person_syncs() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Notes");
        input["options"]["schedule"] = json!({"kind": "every", "minutes": 1});
        let left = directory.path().join("Notes-left");
        fs::write(left.join("file"), b"data").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let id: PairId = created["id"].as_str().unwrap().parse().unwrap();
        let key = json!({"id": created["id"]});
        first_sync(&service, &key).await;
        fs::remove_file(left.join("file")).unwrap();
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        // Deleting one entry out of one exceeds the unattended fuse (ADR 0013).
        assert_eq!(run_due_started(&service, at(61)).await, [id]);
        let stopped = settled(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");

        // ADR 0022: the stop waits for a person; it is not retried or replaced.
        for later in [122, 600] {
            assert!(service.run_due(at(later)).is_empty());
            assert!(matches!(
                service.jobs.status(id),
                SyncStatus::NeedsApproval { .. }
            ));
        }

        // A person's run releases the latch and the schedule resumes; the
        // approval echoes the stopped preview's token.
        let approved = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 10, "review_token": stopped["preview"]["review_token"]});
        service
            .handle(Request::call(3, SYNC_RUN, Some(approved)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["state"], "finished");
        assert_eq!(run_due_started(&service, at(661)).await, [id]);
        settled(&service, &key).await;
    }

    #[tokio::test]
    async fn a_first_mirror_run_on_a_nonempty_target_waits_for_review() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Photos");
        input["mode"] = json!({"kind": "mirror", "source": "left"});
        let right = directory.path().join("Photos-right");
        fs::write(right.join("extra"), b"keep me").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        let run = |right_deletions| json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": right_deletions});

        // PLAN.md §3.6: a non-empty target needs explicit confirmation on the
        // first run, even below the deletion fuse. Nothing may be archived yet.
        service
            .handle(Request::call(2, SYNC_RUN, Some(run(0))))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        assert_eq!(stopped["preview"]["right"]["deletions"], 1);
        assert_eq!(fs::read(right.join("extra")).unwrap(), b"keep me");
        assert!(!right.join(".foldover/archive").exists());

        // Approving the held, reviewed result runs it; the token names the
        // preview the approval was read from.
        let mut approved = run(1);
        approved["review_token"] = stopped["preview"]["review_token"].clone();
        service
            .handle(Request::call(3, SYNC_RUN, Some(approved)))
            .await
            .unwrap();
        let finished = settled_slow(&service, &key).await;
        assert_eq!(finished["state"], "finished", "{finished}");
        assert_eq!(finished["result"]["archived"], 1);
        assert!(!right.join("extra").exists());
        assert!(right.join(".foldover/archive").exists());
    }

    #[tokio::test]
    async fn a_first_mirror_run_that_overwrites_target_content_waits_for_review() {
        let (directory, service) = fixture();
        let mut input = input(directory.path(), "Photos");
        input["mode"] = json!({"kind": "mirror", "source": "left"});
        let left = directory.path().join("Photos-left");
        let right = directory.path().join("Photos-right");
        fs::write(left.join("shared"), b"new").unwrap();
        fs::write(right.join("shared"), b"old").unwrap();
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});

        // The first run replaces existing target content: a non-empty target
        // needs explicit confirmation even when nothing is deleted (§3.6).
        service
            .handle(Request::call(2, SYNC_RUN, Some(run.clone())))
            .await
            .unwrap();
        let stopped = settled(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        assert_eq!(fs::read(right.join("shared")).unwrap(), b"old");
        assert!(!right.join(".foldover/archive").exists());

        // A reviewed retry of the unchanged plan runs it; the token from
        // the presented preview is what makes it reviewed, even at 0/0.
        let mut reviewed = run.clone();
        reviewed["review_token"] = stopped["preview"]["review_token"].clone();
        service
            .handle(Request::call(3, SYNC_RUN, Some(reviewed)))
            .await
            .unwrap();
        let finished = settled(&service, &key).await;
        assert_eq!(finished["state"], "finished", "{finished}");
        assert_eq!(fs::read(right.join("shared")).unwrap(), b"new");
    }

    #[tokio::test]
    async fn an_approval_does_not_authorize_a_changed_deletion_set() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let left = directory.path().join("Docs-left");
        let right = directory.path().join("Docs-right");
        for index in 1..=102_u32 {
            fs::write(left.join(format!("file{index:03}")), b"data").unwrap();
        }
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        let run = |right_deletions| json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": right_deletions});
        assert_eq!(first_sync_slow(&service, &key).await["state"], "finished");

        // The reviewed plan deletes file001..file051 from the right folder.
        for index in 1..=51_u32 {
            fs::remove_file(left.join(format!("file{index:03}"))).unwrap();
        }
        service
            .handle(Request::call(3, SYNC_RUN, Some(run(0))))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        assert_eq!(stopped["preview"]["right"]["deletions"], 51);

        // The same count now covers a different set: file052..file102.
        for index in 1..=51_u32 {
            fs::write(left.join(format!("file{index:03}")), b"data").unwrap();
            fs::remove_file(left.join(format!("file{:03}", index + 51))).unwrap();
        }
        let mut approved = run(51);
        approved["review_token"] = stopped["preview"]["review_token"].clone();
        service
            .handle(Request::call(4, SYNC_RUN, Some(approved)))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        // The unreviewed set must be untouched: nothing was archived.
        for index in 52..=102_u32 {
            assert_eq!(
                fs::read(right.join(format!("file{index:03}"))).unwrap(),
                b"data"
            );
        }
        assert!(!right.join(".foldover/archive").exists());
    }

    #[tokio::test]
    async fn an_approval_does_not_follow_a_newer_preview() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let left = directory.path().join("Docs-left");
        let right = directory.path().join("Docs-right");
        for index in 1..=102_u32 {
            fs::write(left.join(format!("file{index:03}")), b"data").unwrap();
        }
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        let run = |right_deletions| json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": right_deletions});
        assert_eq!(first_sync_slow(&service, &key).await["state"], "finished");

        // The reviewed plan deletes file001..file051 from the right folder.
        for index in 1..=51_u32 {
            fs::remove_file(left.join(format!("file{index:03}"))).unwrap();
        }
        service
            .handle(Request::call(3, SYNC_RUN, Some(run(0))))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        assert_eq!(stopped["preview"]["right"]["deletions"], 51);

        // The same count now covers a different set: file052..file102.
        for index in 1..=51_u32 {
            fs::write(left.join(format!("file{index:03}")), b"data").unwrap();
            fs::remove_file(left.join(format!("file{:03}", index + 51))).unwrap();
        }
        // A second client's preview replaces the held plan with the new set.
        service
            .handle(Request::call(4, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled_slow(&service, &key).await;
        assert_eq!(previewed["state"], "previewed", "{previewed}");
        assert_eq!(previewed["preview"]["right"]["deletions"], 51);
        // Equal plans have one token; A and B differ, so each client's
        // token names the plan it actually reviewed.
        assert_ne!(
            previewed["preview"]["review_token"],
            stopped["preview"]["review_token"]
        );

        // The first client's approval names preview A: it must not follow
        // the newer preview it never saw, equal deletion counts or not.
        let mut approved = run(51);
        approved["review_token"] = stopped["preview"]["review_token"].clone();
        service
            .handle(Request::call(5, SYNC_RUN, Some(approved)))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        // The unreviewed set must be untouched: nothing was archived.
        for index in 52..=102_u32 {
            assert_eq!(
                fs::read(right.join(format!("file{index:03}"))).unwrap(),
                b"data"
            );
        }
        assert!(!right.join(".foldover/archive").exists());
    }

    #[tokio::test]
    async fn a_review_token_binds_to_its_pair_and_revision() {
        let (directory, service) = fixture();
        let first = input(directory.path(), "Docs");
        let second = input(directory.path(), "Other");
        let left = directory.path().join("Docs-left");
        for index in 1..=60_u32 {
            fs::write(left.join(format!("file{index:03}")), b"data").unwrap();
        }
        fs::write(directory.path().join("Other-left/file"), b"data").unwrap();
        let first_pair = service
            .handle(Request::call(1, PAIR_CREATE, Some(first.clone())))
            .await
            .unwrap();
        let second_pair = service
            .handle(Request::call(2, PAIR_CREATE, Some(second)))
            .await
            .unwrap();
        let first_key = json!({"id": first_pair["id"]});
        let second_key = json!({"id": second_pair["id"]});
        let run = |id: &Value, right_deletions: u64, token: &Value| json!({"id": id, "approved_left_deletions": 0, "approved_right_deletions": right_deletions, "review_token": token});
        first_sync(&service, &second_key).await;
        assert_eq!(
            first_sync_slow(&service, &first_key).await["state"],
            "finished"
        );

        // Deleting 51 of 60 trips the fuse; the stopped run's token names
        // exactly this pair's reviewed plan.
        for index in 1..=51_u32 {
            fs::remove_file(left.join(format!("file{index:03}"))).unwrap();
        }
        service
            .handle(Request::call(
                4,
                SYNC_RUN,
                Some(run(&first_pair["id"], 0, &Value::Null)),
            ))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &first_key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        let token = stopped["preview"]["review_token"].clone();
        assert_eq!(token.as_str().unwrap().len(), 64);

        // The first pair's token is meaningless for the second pair.
        service
            .handle(Request::call(
                5,
                SYNC_RUN,
                Some(run(&second_pair["id"], 0, &token)),
            ))
            .await
            .unwrap();
        assert_eq!(
            settled(&service, &second_key).await["state"],
            "needsApproval"
        );
        assert_eq!(
            fs::read(directory.path().join("Other-right/file")).unwrap(),
            b"data"
        );

        // A configuration change makes the held preview obsolete: its token
        // commits to the revision, so the retry stops for a fresh review.
        let mut renamed = first.clone();
        renamed["name"] = json!("Renamed");
        service
            .handle(Request::call(
                6,
                PAIR_UPDATE,
                Some(json!({"id": first_pair["id"], "revision": 1, "configuration": renamed})),
            ))
            .await
            .unwrap();
        service
            .handle(Request::call(
                7,
                SYNC_RUN,
                Some(run(&first_pair["id"], 51, &token)),
            ))
            .await
            .unwrap();
        assert_eq!(
            settled_slow(&service, &first_key).await["state"],
            "needsApproval"
        );
        assert!(
            !directory
                .path()
                .join("Docs-right/.foldover/archive")
                .exists()
        );
    }

    #[tokio::test]
    async fn malformed_review_tokens_fail_before_any_job_or_page() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        for (method, params) in [
            (
                SYNC_RUN,
                json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 1, "review_token": "nothex"}),
            ),
            (
                SYNC_RUN,
                json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0, "review_token": "AB".repeat(32)}),
            ),
            (
                SYNC_RESOLVE,
                json!({"id": created["id"], "path": b"text", "keep": "left", "review_token": "00"}),
            ),
            (
                SYNC_ITEMS,
                json!({"id": created["id"], "review_token": &"0".repeat(63)}),
            ),
        ] {
            assert_eq!(
                service
                    .handle(Request::call(2, method, Some(params)))
                    .await
                    .unwrap_err()
                    .code(),
                INVALID_PARAMS,
                "{method}"
            );
        }
    }

    /// Copy a folder's files preserving mtimes, so a pair update can point
    /// a root at it while its entries keep the previewed versions.
    fn clone_root(source: &Path, target: &Path) {
        fs::create_dir(target).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                continue;
            }
            let destination = target.join(entry.file_name());
            fs::copy(entry.path(), &destination).unwrap();
            let modified = entry.metadata().unwrap().modified().unwrap();
            fs::File::open(&destination)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_keep_both_after_a_configuration_change_writes_nothing() {
        let (directory, service) = fixture();
        let (created, token) = conflicted_pair(directory.path(), &service).await;
        let right = directory.path().join("Both-right");
        // The new right root holds the same versions: only the revision
        // check, not the content one, can stop the stale choice.
        let fresh = directory.path().join("Both-fresh");
        clone_root(&right, &fresh);
        let mut moved = input(directory.path(), "Temp");
        moved["name"] = json!("Both");
        moved["left_root"] = json!(directory.path().join("Both-left").as_os_str().as_bytes());
        moved["right_root"] = json!(fresh.as_os_str().as_bytes());
        service
            .handle(Request::call(
                2,
                PAIR_UPDATE,
                Some(json!({"id": created["id"], "revision": 1, "configuration": moved})),
            ))
            .await
            .unwrap();

        let both =
            json!({"id": created["id"], "path": b"text.md", "keep": "both", "review_token": token});
        assert_eq!(
            service
                .handle(Request::call(3, SYNC_RESOLVE, Some(both)))
                .await
                .unwrap_err()
                .code(),
            foldover_proto::local::PREVIEW_REQUIRED
        );
        // Nothing may have been written into the new root.
        let names: Vec<_> = fs::read_dir(&fresh)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["text.md"], "{names:?}");
    }

    #[tokio::test]
    async fn a_resolution_cannot_follow_a_newer_preview() {
        let (directory, service) = fixture();
        let (created, token) = conflicted_pair(directory.path(), &service).await;
        let key = json!({"id": created["id"]});
        // A second client previews a changed plan; its conflicts differ.
        fs::write(directory.path().join("Both-right/more"), b"new").unwrap();
        service
            .handle(Request::call(4, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let previewed = settled(&service, &key).await;
        assert_ne!(previewed["preview"]["review_token"], token);

        let resolve =
            json!({"id": created["id"], "path": b"text.md", "keep": "left", "review_token": token});
        assert_eq!(
            service
                .handle(Request::call(5, SYNC_RESOLVE, Some(resolve)))
                .await
                .unwrap_err()
                .code(),
            foldover_proto::local::PREVIEW_REQUIRED
        );
        // The newer preview's own token resolves its conflict.
        let resolve = json!({"id": created["id"], "path": b"text.md", "keep": "left", "review_token": previewed["preview"]["review_token"]});
        service
            .handle(Request::call(6, SYNC_RESOLVE, Some(resolve)))
            .await
            .unwrap();
    }

    /// A held engine lock — a long pair save or snapshot write — must not
    /// park the runtime: paging a preview waits off it, so other requests
    /// still answer.
    #[tokio::test]
    async fn paging_items_waits_for_the_engine_off_the_runtime() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();

        let engine = Arc::clone(&service.engine);
        let (locked, held) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let _engine = engine.lock().unwrap();
            locked.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
        });
        held.recv().unwrap();

        let started = std::time::Instant::now();
        let query = json!({"id": created["id"], "review_token": "ab".repeat(32)});
        let service = Arc::new(service);
        let paging = tokio::spawn({
            let service = Arc::clone(&service);
            async move {
                service
                    .handle(Request::call(2, SYNC_ITEMS, Some(query)))
                    .await
            }
        });
        // Let the paging request run first on this single-threaded runtime.
        tokio::task::yield_now().await;
        service
            .handle(Request::call(3, DAEMON_STATUS, None))
            .await
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(1));

        assert!(paging.await.unwrap().is_err());
        holder.join().unwrap();
    }

    #[tokio::test]
    async fn a_preview_token_covers_items_beyond_the_first_page() {
        let (directory, service) = fixture();
        let pair_input = input(directory.path(), "Many");
        let left = directory.path().join("Many-left");
        for index in 0..150_u32 {
            fs::write(left.join(format!("file{index:03}")), b"x").unwrap();
        }
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(pair_input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        service
            .handle(Request::call(2, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let first = settled_slow(&service, &key).await;
        assert_eq!(first["preview"]["items"].as_array().unwrap().len(), 100);

        // A change beyond the response's 100-item page must change the
        // token: approval cannot bind to a truncated view of the plan.
        fs::write(left.join("file149"), b"edited").unwrap();
        service
            .handle(Request::call(3, SYNC_PREVIEW, Some(key.clone())))
            .await
            .unwrap();
        let second = settled_slow(&service, &key).await;
        assert_eq!(first["preview"]["items"], second["preview"]["items"]);
        assert_ne!(
            first["preview"]["review_token"],
            second["preview"]["review_token"]
        );
    }

    #[tokio::test]
    async fn a_run_cannot_approve_deletions_it_never_reviewed() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let left = directory.path().join("Docs-left");
        let right = directory.path().join("Docs-right");
        for index in 1..=60_u32 {
            fs::write(left.join(format!("file{index:03}")), b"data").unwrap();
        }
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input)))
            .await
            .unwrap();
        let key = json!({"id": created["id"]});
        let run = |right_deletions| json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": right_deletions});
        assert_eq!(first_sync_slow(&service, &key).await["state"], "finished");

        // Deleting 51 of 60 trips the fuse; approving without ever holding
        // the reviewed plan must not authorize it.
        for index in 1..=51_u32 {
            fs::remove_file(left.join(format!("file{index:03}"))).unwrap();
        }
        service
            .handle(Request::call(3, SYNC_RUN, Some(run(51))))
            .await
            .unwrap();
        let stopped = settled_slow(&service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        for index in 1..=60_u32 {
            assert_eq!(
                fs::read(right.join(format!("file{index:03}"))).unwrap(),
                b"data"
            );
        }
        assert!(!right.join(".foldover/archive").exists());
    }

    #[tokio::test]
    async fn a_pair_with_a_running_job_cannot_be_removed() {
        let (directory, service) = fixture();
        let pair = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        let id: PairId = pair["id"].as_str().unwrap().parse().unwrap();
        let remove = || {
            Request::call(
                2,
                PAIR_REMOVE,
                Some(json!({"id": pair["id"], "revision": 1})),
            )
        };

        service.jobs.force_status(
            id,
            SyncStatus::Working {
                job: SyncJob::Run,
                cancelling: false,
                progress: SyncProgress::default(),
            },
        );
        assert_eq!(
            service.handle(remove()).await.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY
        );
        service
            .handle(Request::call(3, PAIR_GET, Some(json!({"id": pair["id"]}))))
            .await
            .unwrap();

        service.jobs.force_status(
            id,
            SyncStatus::Failed {
                message: "read error".into(),
            },
        );
        service.handle(remove()).await.unwrap();
    }

    #[tokio::test]
    async fn a_removal_in_progress_rejects_new_job_claims() {
        let (directory, service) = fixture();
        let pair = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        fs::write(directory.path().join("Docs-left/file"), b"data").unwrap();
        let (reached, release) = service.jobs.gate_mutation();
        let removing = service.clone();
        let id = pair["id"].clone();
        let remover = tokio::spawn(async move {
            removing
                .handle(Request::call(
                    2,
                    PAIR_REMOVE,
                    Some(json!({"id": id, "revision": 1})),
                ))
                .await
        });
        // The removal passed its busy check and waits before the delete.
        tokio::task::spawn_blocking(move || reached.recv().unwrap())
            .await
            .unwrap();
        // A claim landing in that window must not win.
        let attempt = service
            .handle(Request::call(
                3,
                SYNC_PREVIEW,
                Some(json!({"id": pair["id"]})),
            ))
            .await;
        release.send(()).unwrap();
        let removed = remover.await.unwrap();
        assert_eq!(
            attempt.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY
        );
        removed.unwrap();
        assert_eq!(
            service
                .handle(Request::call(4, PAIR_GET, Some(json!({"id": pair["id"]}))))
                .await
                .unwrap_err()
                .code(),
            PAIR_NOT_FOUND
        );
    }

    #[tokio::test]
    async fn a_pair_with_a_running_job_cannot_be_updated() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let pair = service
            .handle(Request::call(1, PAIR_CREATE, Some(input.clone())))
            .await
            .unwrap();
        let id: PairId = pair["id"].as_str().unwrap().parse().unwrap();
        let mut renamed = input.clone();
        renamed["name"] = json!("Renamed");
        let update = || {
            Request::call(
                2,
                PAIR_UPDATE,
                Some(
                    json!({"id": pair["id"], "revision": pair["revision"], "configuration": renamed.clone()}),
                ),
            )
        };

        service.jobs.force_status(
            id,
            SyncStatus::Working {
                job: SyncJob::Run,
                cancelling: false,
                progress: SyncProgress::default(),
            },
        );
        assert_eq!(
            service.handle(update()).await.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY
        );
        // The rejected update changed nothing.
        assert_eq!(
            service
                .handle(Request::call(3, PAIR_GET, Some(json!({"id": pair["id"]}))))
                .await
                .unwrap()["configuration"]["name"],
            "Docs"
        );

        service.jobs.force_status(
            id,
            SyncStatus::Failed {
                message: "read error".into(),
            },
        );
        assert_eq!(
            service.handle(update()).await.unwrap()["configuration"]["name"],
            "Renamed"
        );
    }

    #[tokio::test]
    async fn an_update_in_progress_rejects_new_job_claims() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let pair = service
            .handle(Request::call(1, PAIR_CREATE, Some(input.clone())))
            .await
            .unwrap();
        fs::write(directory.path().join("Docs-left/file"), b"data").unwrap();
        let mut renamed = input.clone();
        renamed["name"] = json!("Renamed");
        let (reached, release) = service.jobs.gate_mutation();
        let updating = service.clone();
        let id = pair["id"].clone();
        let updater = tokio::spawn(async move {
            updating
                .handle(Request::call(
                    2,
                    PAIR_UPDATE,
                    Some(json!({"id": id, "revision": 1, "configuration": renamed})),
                ))
                .await
        });
        // The update waits before its engine write.
        tokio::task::spawn_blocking(move || reached.recv().unwrap())
            .await
            .unwrap();
        // A claim landing in that window must not win.
        let attempt = service
            .handle(Request::call(
                3,
                SYNC_PREVIEW,
                Some(json!({"id": pair["id"]})),
            ))
            .await;
        release.send(()).unwrap();
        let updated = updater.await.unwrap();
        assert_eq!(
            attempt.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY
        );
        updated.unwrap();
        // With the mutation done, the pair takes jobs again.
        assert_eq!(
            service
                .handle(Request::call(
                    4,
                    SYNC_PREVIEW,
                    Some(json!({"id": pair["id"]})),
                ))
                .await
                .unwrap()["state"],
            "working"
        );
    }

    #[tokio::test]
    async fn a_late_folder_probe_never_replaces_a_result_waiting_for_review() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        let id: PairId = created["id"].as_str().unwrap().parse().unwrap();

        // A probe result landing late must not hide a state a person is
        // reviewing, an active job, or a pair mid-configuration.
        for status in [
            SyncStatus::Previewed {
                preview: sync_preview(),
            },
            SyncStatus::NeedsApproval {
                preview: sync_preview(),
            },
            SyncStatus::Failed {
                message: "read error".into(),
            },
            SyncStatus::Working {
                job: SyncJob::Run,
                cancelling: false,
                progress: SyncProgress::default(),
            },
        ] {
            service.jobs.force_status(id, status.clone());
            service.jobs.wait_for(id, Side::Left);
            assert_eq!(service.jobs.status(id), status);
        }

        service.jobs.force_status(id, SyncStatus::Idle);
        {
            let _mutation = service.jobs.mutation(id).unwrap();
            service.jobs.wait_for(id, Side::Right);
            assert_eq!(service.jobs.status(id), SyncStatus::Idle);
        }

        // Finished and unstarted pairs still show the wait.
        service
            .jobs
            .force_status(id, sync_finished(Completion::Finished));
        service.jobs.wait_for(id, Side::Left);
        assert!(matches!(
            service.jobs.status(id),
            SyncStatus::Waiting { .. }
        ));
        service
            .jobs
            .force_status(id, sync_finished(Completion::Cancelled));
        service.jobs.wait_for(id, Side::Right);
        assert!(matches!(
            service.jobs.status(id),
            SyncStatus::Waiting { .. }
        ));
    }

    /// Run two syncs so `file` lands in the pair's right-side archive;
    /// returns the archive run name.
    async fn archived_run(
        directory: &Path,
        service: &Service,
        created: &Value,
        folder: &str,
    ) -> Value {
        let left = directory.join(format!("{folder}-left"));
        let key = json!({"id": created["id"]});
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        fs::write(left.join("file"), b"data").unwrap();
        first_sync(service, &key).await;
        fs::remove_file(left.join("file")).unwrap();
        service
            .handle(Request::call(3, SYNC_RUN, Some(run.clone())))
            .await
            .unwrap();
        // The emptied left side archives on the right only after review.
        let stopped = settled(service, &key).await;
        assert_eq!(stopped["state"], "needsApproval", "{stopped}");
        let mut reviewed = run.clone();
        reviewed["review_token"] = stopped["preview"]["review_token"].clone();
        service
            .handle(Request::call(3, SYNC_RUN, Some(reviewed)))
            .await
            .unwrap();
        assert_eq!(settled(service, &key).await["result"]["archived"], 1);
        let runs = service
            .handle(Request::call(
                4,
                ARCHIVE_RUNS,
                Some(json!({"id": created["id"], "side": "right"})),
            ))
            .await
            .unwrap();
        runs["runs"][0]["name"].clone()
    }

    #[tokio::test]
    async fn a_put_back_in_progress_rejects_new_job_claims() {
        let (directory, service) = fixture();
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory.path(), "Docs")),
            ))
            .await
            .unwrap();
        let run_name = archived_run(directory.path(), &service, &created, "Docs").await;
        let put_back =
            json!({"id": created["id"], "side": "right", "run": run_name, "path": b"file"});

        let (reached, release) = service.jobs.gate_mutation();
        let restoring = service.clone();
        let put = tokio::spawn(async move {
            restoring
                .handle(Request::call(5, ARCHIVE_PUT_BACK, Some(put_back)))
                .await
        });
        // The put-back passed its idle check and waits before writing.
        tokio::task::spawn_blocking(move || reached.recv().unwrap())
            .await
            .unwrap();
        // Cancellation and claims landing in that window: cancel stays a
        // cheap no-op, but a claim must not win.
        service
            .handle(Request::call(
                6,
                SYNC_CANCEL,
                Some(json!({"id": created["id"]})),
            ))
            .await
            .unwrap();
        let attempt = service
            .handle(Request::call(
                7,
                SYNC_PREVIEW,
                Some(json!({"id": created["id"]})),
            ))
            .await;
        release.send(()).unwrap();
        put.await.unwrap().unwrap();
        assert_eq!(
            attempt.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY
        );
        // With the put-back done, the pair takes jobs again.
        assert_eq!(
            service
                .handle(Request::call(
                    8,
                    SYNC_PREVIEW,
                    Some(json!({"id": created["id"]}))
                ))
                .await
                .unwrap()["state"],
            "working"
        );
    }

    #[tokio::test]
    async fn a_configuration_change_in_progress_rejects_put_backs() {
        let (directory, service) = fixture();
        let input = input(directory.path(), "Docs");
        let created = service
            .handle(Request::call(1, PAIR_CREATE, Some(input.clone())))
            .await
            .unwrap();
        let run_name = archived_run(directory.path(), &service, &created, "Docs").await;
        let mut renamed = input.clone();
        renamed["name"] = json!("Renamed");

        let (reached, release) = service.jobs.gate_mutation();
        let updating = service.clone();
        let (id, revision) = (created["id"].clone(), created["revision"].clone());
        let update = tokio::spawn(async move {
            updating
                .handle(Request::call(
                    5,
                    PAIR_UPDATE,
                    Some(json!({"id": id, "revision": revision, "configuration": renamed})),
                ))
                .await
        });
        // The update holds the pair and parks before its engine write.
        tokio::task::spawn_blocking(move || reached.recv().unwrap())
            .await
            .unwrap();
        // A put-back must not write while the configuration changes.
        let put_back = service
            .handle(Request::call(
                6,
                ARCHIVE_PUT_BACK,
                Some(
                    json!({"id": created["id"], "side": "right", "run": run_name, "path": b"file"}),
                ),
            ))
            .await;
        release.send(()).unwrap();
        update.await.unwrap().unwrap();
        assert_eq!(
            put_back.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY
        );
    }

    /// Create a pair, sync `text.md`, edit it on both sides and preview,
    /// leaving a both-changed conflict in the pair's latest preview.
    /// Returns the created pair and the preview's review token.
    async fn conflicted_pair(directory: &Path, service: &Service) -> (Value, Value) {
        let created = service
            .handle(Request::call(
                1,
                PAIR_CREATE,
                Some(input(directory, "Both")),
            ))
            .await
            .unwrap();
        let (left, right) = (directory.join("Both-left"), directory.join("Both-right"));
        let key = json!({"id": created["id"]});
        fs::write(left.join("text.md"), b"base").unwrap();
        first_sync(service, &key).await;
        fs::write(left.join("text.md"), b"left").unwrap();
        fs::write(right.join("text.md"), b"right").unwrap();
        service
            .handle(Request::call(3, SYNC_PREVIEW, Some(key)))
            .await
            .unwrap();
        let previewed = settled(service, &json!({"id": created["id"]})).await;
        let token = previewed["preview"]["review_token"].clone();
        (created, token)
    }

    // A resolution that passed its conflict check and parks before writing
    // must still own the pair: a claim landing in that window is refused.
    async fn claim_during_resolution(
        service: &Service,
        created: &Value,
        keep: &str,
        token: &Value,
    ) -> Value {
        let (reached, release) = service.jobs.gate_mutation();
        let resolving = service.clone();
        let request = Request::call(
            4,
            SYNC_RESOLVE,
            Some(
                json!({"id": created["id"], "path": b"text.md", "keep": keep, "review_token": token}),
            ),
        );
        let resolver = tokio::spawn(async move { resolving.handle(request).await });
        tokio::task::spawn_blocking(move || reached.recv().unwrap())
            .await
            .unwrap();
        let attempt = service
            .handle(Request::call(
                5,
                SYNC_PREVIEW,
                Some(json!({"id": created["id"]})),
            ))
            .await;
        release.send(()).unwrap();
        let resolved = resolver.await.unwrap();
        assert_eq!(
            attempt.unwrap_err().code(),
            foldover_proto::local::SYNC_BUSY,
            "keep={keep}"
        );
        resolved.unwrap()
    }

    #[tokio::test]
    async fn a_keep_both_in_progress_rejects_new_job_claims() {
        let (directory, service) = fixture();
        let (created, token) = conflicted_pair(directory.path(), &service).await;
        let status = claim_during_resolution(&service, &created, "both", &token).await;
        assert_eq!(status["state"], "previewed");
    }

    #[tokio::test]
    async fn a_resolve_in_progress_rejects_new_job_claims() {
        let (directory, service) = fixture();
        let (created, token) = conflicted_pair(directory.path(), &service).await;
        let status = claim_during_resolution(&service, &created, "left", &token).await;
        assert_eq!(status["state"], "previewed");
    }
}
