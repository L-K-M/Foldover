//! `foldover-service` — service layer. The agent's public API: sessions,
//! `handle` request dispatch, validation and the event bus. Depends on
//! foldover-engine. See PLAN.md §4.4.

mod schedule;
mod sync;

use std::sync::{Arc, Mutex};

use foldover_engine::{Approval, Engine, ErrorKind, StateLocation};
use foldover_proto::local::{
    AgentCapability, DAEMON_STATUS, DaemonStatus, Hello, INTERNAL_ERROR, INVALID_PARAMS,
    METHOD_NOT_FOUND, PAIR_CREATE, PAIR_GET, PAIR_LIST, PAIR_NOT_FOUND, PAIR_PAGE_SIZE,
    PAIR_UPDATE, PROTOCOL_VERSION, PairInput, PairKey, PairList, PairPage, PairRecord, PairUpdate,
    ROOT_OVERLAP, ROOT_UNAVAILABLE, Request, RpcError, SESSION_HELLO, STALE_REVISION,
    VERSION_MISMATCH,
};
use foldover_proto::local::{PAIR_REMOVE, PairRevision};
use foldover_proto::local::{
    SYNC_CANCEL, SYNC_PREVIEW, SYNC_RESOLVE, SYNC_RUN, SYNC_STATUS, SyncKey, SyncResolve, SyncRun,
};
use foldover_types::{PairDraft, PairId};
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
        }
    }

    pub async fn handle(&self, request: Request) -> Result<Value, RpcError> {
        if matches!(request.method(), SESSION_HELLO | DAEMON_STATUS) {
            return health(request).await;
        }
        if matches!(
            request.method(),
            SYNC_PREVIEW | SYNC_RUN | SYNC_STATUS | SYNC_CANCEL | SYNC_RESOLVE
        ) {
            return self.sync(&request);
        }
        let command = Command::parse(&request)?;
        let permit = Arc::clone(&self.commands)
            .acquire_owned()
            .await
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "Configuration worker is unavailable"))?;
        let engine = Arc::clone(&self.engine);
        tokio::task::spawn_blocking(move || {
            // Retain ownership even if a client disconnects while its command commits.
            let _permit = permit;
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
    fn sync(&self, request: &Request) -> Result<Value, RpcError> {
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
                self.jobs
                    .resolve(&self.engine, id, &path, params.keep.into())?;
                self.jobs.status(id)
            }
            SYNC_RUN => {
                let params: SyncRun = parameters(request)?;
                let approval = Approval::new(
                    params.approved_left_deletions,
                    params.approved_right_deletions,
                );
                self.jobs
                    .start(&self.engine, pair(&params.id)?, Work::Run(approval))?
            }
            method => {
                let id = pair(&parameters::<SyncKey>(request)?.id)?;
                match method {
                    SYNC_PREVIEW => self.jobs.start(&self.engine, id, Work::Preview)?,
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
        for _ in 0..1_000 {
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

        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(4, SYNC_RUN, Some(run)))
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
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(2, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        assert_eq!(settled(&service, &key).await["result"]["copied"], 1);

        fs::write(left.join("b"), b"b").unwrap();
        assert!(service.run_due(at(30)).is_empty());
        assert_eq!(service.run_due(at(61)).len(), 1);
        let finished = settled(&service, &key).await;
        assert_eq!(finished["result"]["copied"], 1, "{finished}");
        assert!(service.run_due(at(90)).is_empty());
        assert_eq!(service.run_due(at(122)).len(), 1);
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
        service
            .handle(Request::call(2, SYNC_RUN, Some(run.clone())))
            .await
            .unwrap();
        settled(&service, &key).await;
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
        assert_eq!(
            settled(&service, &key).await["preview"]["counts"]["conflicts"],
            1
        );
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
        let run = json!({"id": created["id"], "approved_left_deletions": 0, "approved_right_deletions": 0});
        service
            .handle(Request::call(2, SYNC_RUN, Some(run)))
            .await
            .unwrap();
        settled(&service, &key).await;
        let start = service.scheduler.started();
        let at = |seconds| start + std::time::Duration::from_secs(seconds);

        assert!(service.run_due(at(10)).is_empty());
        service.mark_changed(id, at(20));
        assert!(service.run_due(at(22)).is_empty());
        assert_eq!(service.run_due(at(26)), [id]);
        settled(&service, &key).await;
        assert!(service.run_due(at(40)).is_empty());
        assert_eq!(service.run_due(at(26 + 3_600)), [id]);
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
}
