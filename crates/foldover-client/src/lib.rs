//! `foldover-client` — client layer. The typed async client for the
//! agent's local API: requests, subscriptions and reconnect, shared by
//! the CLI, the FFI crate and later the Linux app. Depends on
//! foldover-ipc. See PLAN.md §4.4.

pub use foldover_ipc::{Error, SocketLocation};
pub use foldover_proto::local::NetworkServer;
pub use foldover_proto::local::PairWarning;
use foldover_proto::local::{
    ARCHIVE_ITEMS, ARCHIVE_PUT_BACK, ARCHIVE_PUT_BACK_ALL, ARCHIVE_RUNS, ArchiveItemQuery,
    ArchiveKey, ArchivePlaced, ArchivePutBack, ArchiveRunKey,
};
use foldover_proto::local::{
    AUTOMATION_PAUSE, AUTOMATION_RESUME, AUTOMATION_SETTINGS, AUTOMATION_STATUS, AutomationPause,
    AutomationSettings, HISTORY_LIST, PAIR_REMOVE, PairRevision,
};
pub use foldover_proto::local::{
    AgentCapability, ArchiveItem, ArchiveItems, ArchiveRestored, ArchiveRun, ArchiveRuns,
    DaemonStatus, History, HistoryOutcome, HistoryRecord, HistoryTrigger, PairSide, SyncAction,
    SyncConflict, SyncCounts, SyncDeletions, SyncFailure, SyncItem, SyncJob, SyncPreview,
    SyncProgress, SyncResult, SyncStatus, UndoResult,
};
pub use foldover_proto::local::{
    AutomationHold, AutomationStatus, LowPowerSyncs, SyncItemKind, SyncItemPage,
};
use foldover_proto::local::{
    ConflictKeep, SYNC_CANCEL, SYNC_ITEMS, SYNC_PREVIEW, SYNC_RESOLVE, SYNC_RUN, SYNC_STATUS,
    SYNC_UNDO, SyncItemQuery, SyncKey, SyncResolve, SyncRun,
};
use foldover_proto::local::{DAEMON_STATUS, Hello, PROTOCOL_VERSION, Request, SESSION_HELLO};
use foldover_proto::local::{NETWORK_SERVERS, NetworkServers};
use foldover_proto::local::{
    PAIR_CHECK, PAIR_CREATE, PAIR_GET, PAIR_LIST, PAIR_PAGE_SIZE, PAIR_UPDATE, PairInput, PairKey,
    PairList, PairRecord, PairUpdate, PairWarnings,
};
use foldover_proto::local::{
    PEER_CANCEL, PEER_CONFIRM, PEER_PAIR, PEER_PAIRING_CLOSE, PEER_PAIRING_OPEN, PEER_REVOKE,
    PEER_START, PEER_STATUS, PeerKey, PeerPair, PeerPairingOpen,
};
use foldover_proto::local::{PEER_SHARES, SHARE_ADD, SHARE_LIST, SHARE_REMOVE, ShareAdd, ShareKey};
pub use foldover_proto::local::{
    PeerDeviceRecord, PeerNearbyRecord, PeerPendingRecord, PeerStatus,
};
pub use foldover_proto::local::{PeerShareList, PeerShareRecord, ShareList, ShareRecord};
pub use foldover_types::{
    DailySchedule, Durability, Filters, InvalidPair, InvalidReviewToken, LocalRoot, MetadataPolicy,
    PairConfig, PairDraft, PairId, PairOptions, PairRoot, Propagation, ReviewToken, Schedule,
    ScheduleInterval, Side, SyncMode, SyncPath, TimeOfDay, Weekday, Weekdays,
};
pub use foldover_types::{DeviceId, ShareId};
use serde_json::Value;
use serde_json::json;
use std::net::SocketAddr;

/// The workspace version expected from the agent handshake and exposed to clients.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub async fn agent_status(location: &SocketLocation) -> Result<DaemonStatus, Error> {
    Ok(serde_json::from_value(
        call(location, DAEMON_STATUS, None).await?,
    )?)
}

/// Start a background preview; poll `sync_status` for its result.
pub async fn start_preview(location: &SocketLocation, id: PairId) -> Result<SyncStatus, Error> {
    sync_call(location, SYNC_PREVIEW, id).await
}

/// Start a background run that may make at most the approved deletions on
/// each side beyond the automatic limits. `review_token` names the
/// `SyncPreview` a person saw; any approval or presented token that does
/// not match the held and freshly planned preview stops for review again.
pub async fn start_run(
    location: &SocketLocation,
    id: PairId,
    approved_left_deletions: u64,
    approved_right_deletions: u64,
    review_token: Option<ReviewToken>,
) -> Result<SyncStatus, Error> {
    let params = SyncRun {
        id: id.to_string(),
        approved_left_deletions,
        approved_right_deletions,
        review_token: review_token.map(|token| token.to_string()),
    };
    Ok(serde_json::from_value(
        call(location, SYNC_RUN, Some(serde_json::to_value(params)?)).await?,
    )?)
}

pub async fn sync_status(location: &SocketLocation, id: PairId) -> Result<SyncStatus, Error> {
    sync_call(location, SYNC_STATUS, id).await
}

/// Keep one side's version of a change or conflict in the pair's latest
/// preview; for a change, that copies it the other way when it is the side
/// the change would overwrite. `review_token` must be that preview's; a
/// stale one fails.
pub async fn resolve_conflict(
    location: &SocketLocation,
    id: PairId,
    path: &[u8],
    keep: Side,
    review_token: ReviewToken,
) -> Result<SyncStatus, Error> {
    let params = SyncResolve {
        id: id.to_string(),
        path: path.to_vec(),
        keep: keep.into(),
        review_token: Some(review_token.to_string()),
    };
    Ok(serde_json::from_value(
        call(location, SYNC_RESOLVE, Some(serde_json::to_value(params)?)).await?,
    )?)
}

/// A page of the latest preview's items after `after`, optionally of one
/// kind. `review_token` must be that preview's; a stale one fails.
pub async fn sync_items(
    location: &SocketLocation,
    id: PairId,
    review_token: ReviewToken,
    after: Option<&[u8]>,
    kind: Option<SyncItemKind>,
) -> Result<SyncItemPage, Error> {
    let params = SyncItemQuery {
        id: id.to_string(),
        after: after.map(<[u8]>::to_vec),
        kind,
        review_token: Some(review_token.to_string()),
    };
    Ok(serde_json::from_value(
        call(location, SYNC_ITEMS, Some(serde_json::to_value(params)?)).await?,
    )?)
}

/// Keep both versions of a both-changed conflict in the latest preview:
/// the right side's is saved beside the entry, the left side's wins.
/// `review_token` must be that preview's; a stale one fails.
pub async fn keep_both(
    location: &SocketLocation,
    id: PairId,
    path: &[u8],
    review_token: ReviewToken,
) -> Result<SyncStatus, Error> {
    let params = SyncResolve {
        id: id.to_string(),
        path: path.to_vec(),
        keep: ConflictKeep::Both,
        review_token: Some(review_token.to_string()),
    };
    Ok(serde_json::from_value(
        call(location, SYNC_RESOLVE, Some(serde_json::to_value(params)?)).await?,
    )?)
}

/// Leave a change in the pair's latest preview, and everything inside it,
/// alone in the next run. `review_token` must be that preview's.
pub async fn skip_once(
    location: &SocketLocation,
    id: PairId,
    path: &[u8],
    review_token: ReviewToken,
) -> Result<SyncStatus, Error> {
    let params = SyncResolve {
        id: id.to_string(),
        path: path.to_vec(),
        keep: ConflictKeep::Skip,
        review_token: Some(review_token.to_string()),
    };
    Ok(serde_json::from_value(
        call(location, SYNC_RESOLVE, Some(serde_json::to_value(params)?)).await?,
    )?)
}

pub async fn cancel_sync(location: &SocketLocation, id: PairId) -> Result<SyncStatus, Error> {
    sync_call(location, SYNC_CANCEL, id).await
}

/// Start undoing the pair's newest sync that changed anything; poll
/// `sync_status` for the result.
pub async fn undo_sync(location: &SocketLocation, id: PairId) -> Result<SyncStatus, Error> {
    sync_call(location, SYNC_UNDO, id).await
}

/// Whether automatic syncs are paused.
pub async fn automation_status(location: &SocketLocation) -> Result<AutomationStatus, Error> {
    Ok(serde_json::from_value(
        call(location, AUTOMATION_STATUS, None).await?,
    )?)
}

/// Pause automatic syncs for `minutes`, or until resumed.
pub async fn pause_automation(
    location: &SocketLocation,
    minutes: Option<u32>,
) -> Result<AutomationStatus, Error> {
    let params = serde_json::to_value(AutomationPause { minutes })?;
    Ok(serde_json::from_value(
        call(location, AUTOMATION_PAUSE, Some(params)).await?,
    )?)
}

pub async fn resume_automation(location: &SocketLocation) -> Result<AutomationStatus, Error> {
    Ok(serde_json::from_value(
        call(location, AUTOMATION_RESUME, None).await?,
    )?)
}

/// Set the person's choice for automatic syncs in Low Power Mode.
pub async fn set_low_power_syncs(
    location: &SocketLocation,
    syncs: LowPowerSyncs,
) -> Result<AutomationStatus, Error> {
    let params = serde_json::to_value(AutomationSettings { low_power: syncs })?;
    Ok(serde_json::from_value(
        call(location, AUTOMATION_SETTINGS, Some(params)).await?,
    )?)
}

/// The pair's finished runs, newest first.
pub async fn history(location: &SocketLocation, id: PairId) -> Result<History, Error> {
    let params = serde_json::to_value(SyncKey { id: id.to_string() })?;
    Ok(serde_json::from_value(
        call(location, HISTORY_LIST, Some(params)).await?,
    )?)
}

/// One side's archived runs, newest first.
pub async fn archive_runs(
    location: &SocketLocation,
    id: PairId,
    side: Side,
) -> Result<ArchiveRuns, Error> {
    let params = ArchiveKey {
        id: id.to_string(),
        side: side.into(),
    };
    Ok(serde_json::from_value(
        call(location, ARCHIVE_RUNS, Some(serde_json::to_value(params)?)).await?,
    )?)
}

/// A page of one archived run's items, in path order after `after`.
pub async fn archive_items(
    location: &SocketLocation,
    id: PairId,
    side: Side,
    run: &str,
    after: Option<&[u8]>,
) -> Result<ArchiveItems, Error> {
    let params = ArchiveItemQuery {
        id: id.to_string(),
        side: side.into(),
        run: run.to_owned(),
        after: after.map(<[u8]>::to_vec),
    };
    Ok(serde_json::from_value(
        call(location, ARCHIVE_ITEMS, Some(serde_json::to_value(params)?)).await?,
    )?)
}

/// Put an archived item back and return the path it went to: the original
/// one, or a `(restored)` name beside it when that was taken.
pub async fn put_back(
    location: &SocketLocation,
    id: PairId,
    side: Side,
    run: &str,
    path: &[u8],
) -> Result<Vec<u8>, Error> {
    let params = ArchivePutBack {
        id: id.to_string(),
        side: side.into(),
        run: run.to_owned(),
        path: path.to_vec(),
    };
    let placed: ArchivePlaced = serde_json::from_value(
        call(
            location,
            ARCHIVE_PUT_BACK,
            Some(serde_json::to_value(params)?),
        )
        .await?,
    )?;
    Ok(placed.path)
}

/// Put back everything one run archived on one side.
pub async fn put_back_all(
    location: &SocketLocation,
    id: PairId,
    side: Side,
    run: &str,
) -> Result<ArchiveRestored, Error> {
    let params = ArchiveRunKey {
        id: id.to_string(),
        side: side.into(),
        run: run.to_owned(),
    };
    Ok(serde_json::from_value(
        call(
            location,
            ARCHIVE_PUT_BACK_ALL,
            Some(serde_json::to_value(params)?),
        )
        .await?,
    )?)
}

async fn sync_call(
    location: &SocketLocation,
    method: &str,
    id: PairId,
) -> Result<SyncStatus, Error> {
    let params = serde_json::to_value(SyncKey { id: id.to_string() })?;
    Ok(serde_json::from_value(
        call(location, method, Some(params)).await?,
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

/// What will not work as a person might expect with `draft`'s folders
/// (PLAN.md §3.6). Warnings never block saving.
pub async fn check_pair(
    location: &SocketLocation,
    draft: &PairDraft,
) -> Result<Vec<PairWarning>, Error> {
    let warnings: PairWarnings = serde_json::from_value(
        call(
            location,
            PAIR_CHECK,
            Some(serde_json::to_value(PairInput::from(draft))?),
        )
        .await?,
    )?;
    Ok(warnings.warnings)
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

/// SMB file servers seen on the local network (PLAN.md §3.8); waits
/// about two seconds while the agent browses.
pub async fn network_servers(location: &SocketLocation) -> Result<Vec<NetworkServer>, Error> {
    let listed: NetworkServers =
        serde_json::from_value(call(location, NETWORK_SERVERS, None).await?)?;
    Ok(listed.servers)
}

/// This Mac's device, its pairing window and the Macs paired with it;
/// never turns Mac-to-Mac on.
pub async fn peer_status(location: &SocketLocation) -> Result<PeerStatus, Error> {
    Ok(serde_json::from_value(
        call(location, PEER_STATUS, None).await?,
    )?)
}

/// Turn Mac-to-Mac on: listen, and look for Macs on the network.
pub async fn start_peers(location: &SocketLocation) -> Result<PeerStatus, Error> {
    Ok(serde_json::from_value(
        call(location, PEER_START, None).await?,
    )?)
}

/// Let unpaired Macs reach this one for `minutes` (10 when absent).
pub async fn open_pairing(
    location: &SocketLocation,
    minutes: Option<u32>,
) -> Result<PeerStatus, Error> {
    let params = serde_json::to_value(PeerPairingOpen { minutes })?;
    Ok(serde_json::from_value(
        call(location, PEER_PAIRING_OPEN, Some(params)).await?,
    )?)
}

pub async fn close_pairing(location: &SocketLocation) -> Result<PeerStatus, Error> {
    Ok(serde_json::from_value(
        call(location, PEER_PAIRING_CLOSE, None).await?,
    )?)
}

/// Connect to `device`'s open pairing window at `address`, or where the
/// local network says when None. Both Macs then show the returned code; a
/// person confirms it on each.
pub async fn pair_device(
    location: &SocketLocation,
    device: DeviceId,
    address: Option<SocketAddr>,
) -> Result<PeerPendingRecord, Error> {
    let params = serde_json::to_value(PeerPair {
        device: device.to_string(),
        address: address.map(|address| address.to_string()),
    })?;
    Ok(serde_json::from_value(
        call(location, PEER_PAIR, Some(params)).await?,
    )?)
}

/// Confirm that `device`'s code matches the one the other Mac shows.
pub async fn confirm_pairing(
    location: &SocketLocation,
    device: DeviceId,
) -> Result<PeerStatus, Error> {
    peer_call(location, PEER_CONFIRM, device).await
}

pub async fn cancel_pairing(
    location: &SocketLocation,
    device: DeviceId,
) -> Result<PeerStatus, Error> {
    peer_call(location, PEER_CANCEL, device).await
}

/// Forget a paired Mac and close its connections.
pub async fn revoke_device(
    location: &SocketLocation,
    device: DeviceId,
) -> Result<PeerStatus, Error> {
    peer_call(location, PEER_REVOKE, device).await
}

/// Folders this Mac shares with paired Macs.
pub async fn shares(location: &SocketLocation) -> Result<ShareList, Error> {
    Ok(serde_json::from_value(
        call(location, SHARE_LIST, None).await?,
    )?)
}

/// Share the folder at `root` with paired `device`, which shows it as
/// `name`.
pub async fn add_share(
    location: &SocketLocation,
    device: DeviceId,
    root: &[u8],
    name: &str,
) -> Result<ShareRecord, Error> {
    let params = serde_json::to_value(ShareAdd {
        device: device.to_string(),
        root: root.to_vec(),
        name: name.to_owned(),
    })?;
    Ok(serde_json::from_value(
        call(location, SHARE_ADD, Some(params)).await?,
    )?)
}

pub async fn remove_share(location: &SocketLocation, id: ShareId) -> Result<ShareList, Error> {
    let params = serde_json::to_value(ShareKey { id: id.to_string() })?;
    Ok(serde_json::from_value(
        call(location, SHARE_REMOVE, Some(params)).await?,
    )?)
}

/// The folders paired `device` shares with this Mac; asks that Mac.
pub async fn peer_shares(
    location: &SocketLocation,
    device: DeviceId,
) -> Result<PeerShareList, Error> {
    let params = serde_json::to_value(PeerKey {
        device: device.to_string(),
    })?;
    Ok(serde_json::from_value(
        call(location, PEER_SHARES, Some(params)).await?,
    )?)
}

async fn peer_call(
    location: &SocketLocation,
    method: &str,
    device: DeviceId,
) -> Result<PeerStatus, Error> {
    let params = serde_json::to_value(PeerKey {
        device: device.to_string(),
    })?;
    Ok(serde_json::from_value(
        call(location, method, Some(params)).await?,
    )?)
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
