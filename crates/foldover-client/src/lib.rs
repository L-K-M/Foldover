//! `foldover-client` — client layer. The typed async client for the
//! agent's local API: requests, subscriptions and reconnect, shared by
//! the CLI, the FFI crate and later the Linux app. Depends on
//! foldover-ipc. See PLAN.md §4.4.

pub use foldover_ipc::{Error, SocketLocation};
pub use foldover_proto::local::{AgentCapability, DaemonStatus};
use foldover_proto::local::{DAEMON_STATUS, Hello, PROTOCOL_VERSION, Request, SESSION_HELLO};
use foldover_proto::local::{
    PAIR_CREATE, PAIR_GET, PAIR_LIST, PAIR_PAGE_SIZE, PAIR_UPDATE, PairInput, PairKey, PairList,
    PairRecord, PairUpdate,
};
use foldover_proto::local::{PAIR_REMOVE, PairRevision};
pub use foldover_types::{InvalidPair, LocalRoot, PairConfig, PairDraft, PairId, Side, SyncMode};
use serde_json::Value;
use serde_json::json;

/// The workspace version expected from the agent handshake and exposed to clients.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub async fn agent_status(location: &SocketLocation) -> Result<DaemonStatus, Error> {
    Ok(serde_json::from_value(
        call(location, DAEMON_STATUS, None).await?,
    )?)
}

pub struct PairPage {
    pub pairs: Vec<PairConfig>,
    pub next: Option<PairId>,
}

pub async fn pairs(location: &SocketLocation, after: Option<PairId>) -> Result<PairPage, Error> {
    let page = call(
        location,
        PAIR_LIST,
        Some(serde_json::to_value(PairList {
            after: after.map(|id| id.to_string()),
        })?),
    )
    .await?;
    decode_page(page, after)
}

pub async fn pair(location: &SocketLocation, id: PairId) -> Result<PairConfig, Error> {
    decode_pair(
        call(
            location,
            PAIR_GET,
            Some(serde_json::to_value(PairKey { id: id.to_string() })?),
        )
        .await?,
    )
}

pub async fn create_pair(
    location: &SocketLocation,
    draft: &PairDraft,
) -> Result<PairConfig, Error> {
    decode_pair(
        call(
            location,
            PAIR_CREATE,
            Some(serde_json::to_value(PairInput::from(draft))?),
        )
        .await?,
    )
}

pub async fn update_pair(
    location: &SocketLocation,
    id: PairId,
    revision: u64,
    draft: &PairDraft,
) -> Result<PairConfig, Error> {
    decode_pair(
        call(
            location,
            PAIR_UPDATE,
            Some(serde_json::to_value(PairUpdate {
                id: id.to_string(),
                revision,
                configuration: draft.into(),
            })?),
        )
        .await?,
    )
}

pub async fn remove_pair(
    location: &SocketLocation,
    id: PairId,
    revision: u64,
) -> Result<PairConfig, Error> {
    let removed = decode_pair(
        call(
            location,
            PAIR_REMOVE,
            Some(serde_json::to_value(PairRevision {
                id: id.to_string(),
                revision,
            })?),
        )
        .await?,
    )?;
    if removed.id() != id || removed.revision() != revision {
        return Err(Error::Protocol(
            "Removed pair does not match the requested revision",
        ));
    }
    Ok(removed)
}

fn decode_pair(value: Value) -> Result<PairConfig, Error> {
    let record: PairRecord = serde_json::from_value(value)?;
    record
        .try_into()
        .map_err(|_| Error::Protocol("Invalid pair configuration from agent"))
}

fn decode_page(value: Value, after: Option<PairId>) -> Result<PairPage, Error> {
    let page: foldover_proto::local::PairPage = serde_json::from_value(value)?;
    let invalid = || Error::Protocol("Invalid pair page from agent");
    if page.pairs.len() > PAIR_PAGE_SIZE {
        return Err(invalid());
    }
    let mut previous = after;
    let mut pairs = Vec::with_capacity(page.pairs.len());
    for record in page.pairs {
        let pair: PairConfig = record.try_into().map_err(|_| invalid())?;
        if previous.is_some_and(|previous| pair.id() <= previous) {
            return Err(invalid());
        }
        previous = Some(pair.id());
        pairs.push(pair);
    }
    let next = page
        .next
        .map(|id| id.parse())
        .transpose()
        .map_err(|_| invalid())?;
    if next.is_some() && (next != previous || pairs.len() != PAIR_PAGE_SIZE) {
        return Err(invalid());
    }
    Ok(PairPage { pairs, next })
}

async fn call(
    location: &SocketLocation,
    method: &str,
    params: Option<Value>,
) -> Result<Value, Error> {
    let mut connection = foldover_ipc::connect(location).await?;
    if method != DAEMON_STATUS {
        connection.authenticate().await?;
    }
    let hello: Hello = serde_json::from_value(
        connection
            .call(Request::call(
                1,
                SESSION_HELLO,
                Some(json!({"protocol_version": PROTOCOL_VERSION})),
            ))
            .await?,
    )?;
    if hello.protocol_version != PROTOCOL_VERSION || hello.core_version != version() {
        return Err(Error::Protocol(
            "Agent version mismatch; restart the background agent",
        ));
    }
    connection.call(Request::call(2, method, params)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_pages_cannot_create_invalid_pairs_or_loop_a_client() {
        let id = PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap();
        let draft = PairDraft::new(
            "Pair".into(),
            LocalRoot::from_bytes(b"/a".to_vec()).unwrap(),
            LocalRoot::from_bytes(b"/b".to_vec()).unwrap(),
            SyncMode::Sync,
        )
        .unwrap();
        let pair = PairConfig::from_stored(id, 1, draft).unwrap();
        let record = serde_json::to_value(PairRecord::from(&pair)).unwrap();
        let page = json!({"pairs":[record.clone()], "next":null});
        assert_eq!(decode_page(page.clone(), None).unwrap().pairs, vec![pair]);
        assert!(decode_page(page, Some(id)).is_err());
        assert!(
            decode_page(
                json!({"pairs":[record.clone(), record.clone()], "next":null}),
                None
            )
            .is_err()
        );
        assert!(
            decode_page(
                json!({"pairs":[record.clone()], "next":id.to_string()}),
                None
            )
            .is_err()
        );
        let mut invalid = record;
        invalid["revision"] = json!(0);
        assert!(decode_pair(invalid).is_err());
    }

    #[test]
    fn version_is_the_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::version().is_empty());
    }
}
