//! Exercise engine archive recovery across the real peer framing boundary.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use foldover_engine::{
    Answer, NotRecorded, PairArchive, PeerHost, PeerIdentity, PeerLink, Presence, Reach,
};
use foldover_proto::peer::{
    FolderError, FolderErrorKind, FolderOp, NameCase, NameUnicode, PeerCall, PeerReply, WireItem,
};
use foldover_types::{
    DeviceId, LocalRoot, PairConfig, PairDraft, PairId, PairRoot, PeerRoot, ShareId, Side, SyncMode,
};

const ITEM_COUNT: usize = 2_105;
const PAGE_ITEMS: usize = 1_000;
const PEER_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const RUN: &str = "archived-run";
const PATH_DEPTH: usize = 330;
const FAILED_INDICES: [usize; 4] = [0, PAGE_ITEMS - 1, 2 * PAGE_ITEMS - 1, ITEM_COUNT - 1];
const RESTORE_TIMEOUT: Duration = Duration::from_secs(60);

/// Long valid relative paths make the full listing larger than a peer frame
/// without creating thousands of filesystem entries for a transport test.
fn path(index: usize) -> Vec<u8> {
    format!("{index:04}/{}file", "folder/".repeat(PATH_DEPTH)).into_bytes()
}

#[derive(Default)]
struct ArchiveState {
    remaining: BTreeSet<Vec<u8>>,
    requested: Vec<(Option<Vec<u8>>, u64)>,
    attempted: Vec<Vec<u8>>,
}

struct ArchivedMac(Mutex<ArchiveState>);

impl PeerHost for ArchivedMac {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        let PeerCall::Folder { op, .. } = call else {
            panic!("unexpected share listing")
        };
        let reply = match *op {
            FolderOp::Probe => PeerReply::Done,
            FolderOp::NameMatching => PeerReply::Names {
                case: NameCase::Exact,
                unicode: NameUnicode::Exact,
            },
            FolderOp::ArchivedItems { run, after, limit } => {
                assert_eq!(run, RUN);
                let mut state = self.0.lock().unwrap();
                state.requested.push((after.clone(), limit));
                let mut rest = state
                    .remaining
                    .iter()
                    .filter(|path| after.as_ref().is_none_or(|after| *path > after));
                let items = rest
                    .by_ref()
                    .take(usize::try_from(limit).unwrap_or(usize::MAX))
                    .map(|path| WireItem {
                        path: path.clone(),
                        size: 1,
                    })
                    .collect();
                PeerReply::Items {
                    items,
                    more: rest.next().is_some(),
                }
            }
            FolderOp::PutBack {
                run, path: item, ..
            } => {
                assert_eq!(run, RUN);
                let mut state = self.0.lock().unwrap();
                assert!(state.remaining.contains(&item));
                state.attempted.push(item.clone());
                // Leave failures at page boundaries in the archive: a cursor
                // derived from restored paths would miss or retry these items.
                if FAILED_INDICES.map(path).contains(&item) {
                    PeerReply::Failed {
                        error: FolderError {
                            kind: FolderErrorKind::AccessDenied,
                            path: Some(item),
                            message: "restore denied".into(),
                        },
                    }
                } else {
                    state.remaining.remove(&item);
                    let placed = if item == path(1) {
                        [item, b" (restored)".to_vec()].concat()
                    } else {
                        item
                    };
                    PeerReply::Placed { path: placed }
                }
            }
            _ => panic!("unexpected archive operation"),
        };
        // An unlimited listing used to fail here at the actual transport cap.
        let _ = answer.reply(&reply);
    }
}

#[tokio::test]
async fn restores_a_peer_archive_larger_than_one_message_in_bounded_pages() {
    let items: BTreeSet<_> = (0..ITEM_COUNT).map(path).collect();
    let unpaged = PeerReply::Items {
        items: items
            .iter()
            .map(|path| WireItem {
                path: path.clone(),
                size: 1,
            })
            .collect(),
        more: false,
    };
    assert!(serde_json::to_vec(&unpaged).unwrap().len() > PEER_MESSAGE_BYTES);
    let host = Arc::new(ArchivedMac(Mutex::new(ArchiveState {
        remaining: items,
        ..ArchiveState::default()
    })));
    let identities = [
        PeerIdentity::generate().unwrap(),
        PeerIdentity::generate().unwrap(),
    ];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
    let b = PeerLink::start(
        b_identity,
        "Studio".into(),
        loopback,
        Presence::Unlisted,
        [devices[0]],
        host.clone(),
    )
    .unwrap();
    let a = PeerLink::start(
        a_identity,
        "Laptop".into(),
        loopback,
        Presence::Unlisted,
        [devices[1]],
        host.clone(),
    )
    .unwrap();
    a.reach(devices[1], b.address().unwrap());
    let temporary = tempfile::tempdir().unwrap();
    let draft = PairDraft::new(
        "Archive".into(),
        PairRoot::Peer(PeerRoot::new(
            devices[1],
            ShareId::from_bytes([7; ShareId::BYTE_LEN]),
        )),
        LocalRoot::from_bytes(temporary.path().as_os_str().as_encoded_bytes().to_vec()).unwrap(),
        SyncMode::Sync,
    )
    .unwrap();
    let pair =
        PairConfig::from_stored(PairId::from_bytes([1; PairId::BYTE_LEN]).unwrap(), 1, draft)
            .unwrap();
    let reach = Reach::through(&a);
    let restored = tokio::task::spawn_blocking(move || {
        PairArchive::open(&pair, Side::Left, &reach)
            .unwrap()
            .put_back_all(RUN)
    });
    let report = tokio::time::timeout(RESTORE_TIMEOUT, restored)
        .await
        .expect("peer restore stopped answering")
        .unwrap()
        .expect("the archive must fit across pages");
    assert_eq!(
        report.restored(),
        (ITEM_COUNT - FAILED_INDICES.len() - 1) as u64
    );
    assert_eq!(report.beside(), 1);
    let failures: Vec<_> = report
        .failures()
        .iter()
        .map(|(path, _)| path.as_bytes().to_vec())
        .collect();
    assert_eq!(failures, FAILED_INDICES.map(path));
    let state = host.0.lock().unwrap();
    assert_eq!(
        state.attempted,
        (0..ITEM_COUNT).map(path).collect::<Vec<_>>()
    );
    assert_eq!(state.remaining, failures.into_iter().collect());
    assert_eq!(
        state.requested,
        [
            (None, PAGE_ITEMS as u64),
            (Some(path(PAGE_ITEMS - 1)), PAGE_ITEMS as u64),
            (Some(path(2 * PAGE_ITEMS - 1)), PAGE_ITEMS as u64)
        ]
    );
}
