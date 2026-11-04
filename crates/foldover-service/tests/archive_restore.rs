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
const REPEATED_PAGES: usize = 3;

/// Long valid relative paths make the full listing larger than a peer frame
/// without creating thousands of filesystem entries for a transport test.
fn path(index: usize) -> Vec<u8> {
    format!("{index:04}/{}file", "folder/".repeat(PATH_DEPTH)).into_bytes()
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PageShape {
    #[default]
    Ordered,
    Repeated,
    Unsorted,
    Duplicate,
    EmptyWithMore,
    BeforeCursor,
    OverLimit,
}

#[derive(Default)]
struct ArchiveState {
    shape: PageShape,
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
                self.0.lock().unwrap().page(after, limit)
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

impl ArchiveState {
    fn page(&mut self, after: Option<Vec<u8>>, limit: u64) -> PeerReply {
        self.requested.push((after.clone(), limit));
        if self.shape == PageShape::Ordered {
            let mut rest = self
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
            return PeerReply::Items {
                items,
                more: rest.next().is_some(),
            };
        }

        // Bound the bad peer so a failing regression cannot hang its runtime.
        let (indices, more) = match self.shape {
            PageShape::Repeated if self.requested.len() <= REPEATED_PAGES => (vec![0], true),
            PageShape::Repeated => (vec![], false),
            PageShape::Unsorted => (vec![1, 0], false),
            PageShape::Duplicate => (vec![0, 0], false),
            PageShape::EmptyWithMore => (vec![], true),
            PageShape::BeforeCursor => (vec![0], false),
            PageShape::OverLimit => (vec![0, 1], false),
            PageShape::Ordered => unreachable!(),
        };
        PeerReply::Items {
            items: indices
                .into_iter()
                .map(|index| WireItem {
                    path: path(index),
                    size: 1,
                })
                .collect(),
            more,
        }
    }
}

struct PeerFixture {
    pair: PairConfig,
    reach: Reach,
    _client: Arc<PeerLink>,
    _server: Arc<PeerLink>,
    _local: tempfile::TempDir,
}

fn peer_fixture(host: Arc<ArchivedMac>) -> PeerFixture {
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
    PeerFixture {
        pair,
        reach,
        _client: a,
        _server: b,
        _local: temporary,
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
    let fixture = peer_fixture(host.clone());
    let pair = fixture.pair.clone();
    let reach = fixture.reach.clone();
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

#[tokio::test]
async fn a_repeated_peer_page_is_rejected_before_restoring_any_item_twice() {
    let host = Arc::new(ArchivedMac(Mutex::new(ArchiveState {
        shape: PageShape::Repeated,
        remaining: [path(0)].into_iter().collect(),
        ..ArchiveState::default()
    })));
    let fixture = peer_fixture(host.clone());
    let pair = fixture.pair.clone();
    let reach = fixture.reach.clone();
    let restoring = tokio::task::spawn_blocking(move || {
        PairArchive::open(&pair, Side::Left, &reach)
            .unwrap()
            .put_back_all(RUN)
    });
    let result = tokio::time::timeout(RESTORE_TIMEOUT, restoring)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err(), "a peer ignored the page cursor");
    let state = host.0.lock().unwrap();
    assert_eq!(state.requested.len(), 2);
    assert_eq!(state.attempted, [path(0)]);
}

#[tokio::test]
async fn malformed_peer_pages_never_reach_archive_consumers() {
    for shape in [
        PageShape::Unsorted,
        PageShape::Duplicate,
        PageShape::EmptyWithMore,
        PageShape::BeforeCursor,
        PageShape::OverLimit,
    ] {
        let host = Arc::new(ArchivedMac(Mutex::new(ArchiveState {
            shape,
            ..ArchiveState::default()
        })));
        let fixture = peer_fixture(host);
        let pair = fixture.pair.clone();
        let reach = fixture.reach.clone();
        let reading = tokio::task::spawn_blocking(move || {
            let after = (shape == PageShape::BeforeCursor)
                .then(|| foldover_types::SyncPath::try_from(path(1)).unwrap());
            let limit = if shape == PageShape::OverLimit {
                1
            } else {
                PAGE_ITEMS
            };
            PairArchive::open(&pair, Side::Left, &reach)
                .unwrap()
                .items(RUN, after.as_ref(), limit)
        });
        let result = tokio::time::timeout(RESTORE_TIMEOUT, reading)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "accepted malformed page: {shape:?}");
    }
}

#[tokio::test]
async fn zero_limit_peer_queries_can_report_that_items_exist() {
    let host = Arc::new(ArchivedMac(Mutex::new(ArchiveState {
        remaining: [path(0)].into_iter().collect(),
        ..ArchiveState::default()
    })));
    let fixture = peer_fixture(host);
    let pair = fixture.pair.clone();
    let reach = fixture.reach.clone();
    let reading = tokio::task::spawn_blocking(move || {
        PairArchive::open(&pair, Side::Left, &reach)
            .unwrap()
            .items(RUN, None, 0)
    });
    let (items, more) = tokio::time::timeout(RESTORE_TIMEOUT, reading)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(items.is_empty());
    assert!(more);
}
