use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use foldover_net::peer::Identity;
use foldover_proto::peer::{
    CatalogOp, FolderError, FolderErrorKind, FolderOp, IncomingKind, NameCase, NameUnicode,
    PeerCall, PeerReply, VolumeKind,
};
use foldover_types::{
    DeviceId, Digest, Durability, LocalRoot, PairId, PairOptions, PeerRoot, ShareId, SyncPath,
};

use super::*;
use crate::CatalogFiles;
use crate::catalog::CATALOG_FILE_LIMIT;
use crate::lease::{LeaseHolder, LeaseTiming, OwnedLease};
use crate::link::{Answer, NotRecorded, PeerHost, PeerLink, Presence};
use crate::local::Carried;
use crate::{Endpoint, Expected, RunScope};

/// Serves one folder as every share, as the sharing Mac's agent does;
/// `state` is that agent's state directory, protected under the root.
struct Sharing {
    root: LocalRoot,
    cache: FolderCache,
    state: Option<std::path::PathBuf>,
}

/// Answers setup calls, then leaves an observation waiting on its stream.
struct StallingObservation(std::sync::Mutex<mpsc::Sender<()>>);

impl PeerHost for StallingObservation {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        let PeerCall::Folder { op, .. } = call else {
            return;
        };
        match *op {
            FolderOp::Probe => {
                let _ = answer.reply(&PeerReply::Done);
            }
            FolderOp::NameMatching => {
                let _ = answer.reply(&PeerReply::Names {
                    case: NameCase::Exact,
                    unicode: NameUnicode::Exact,
                });
            }
            FolderOp::Observe { .. } => {
                let _ = self.0.lock().unwrap().send(());
                let _ = answer.next_data();
            }
            _ => {
                let _ = answer.reply(&PeerReply::Refused {
                    reason: "Not served by this fixture".into(),
                });
            }
        }
    }
}

/// Reads a complete incoming file, then withholds its final `Received` reply.
struct StallingReceive(std::sync::Mutex<mpsc::Sender<()>>);

impl PeerHost for StallingReceive {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        let PeerCall::Folder { op, .. } = call else {
            return;
        };
        match *op {
            FolderOp::Probe => {
                let _ = answer.reply(&PeerReply::Done);
            }
            FolderOp::NameMatching => {
                let _ = answer.reply(&PeerReply::Names {
                    case: NameCase::Exact,
                    unicode: NameUnicode::Exact,
                });
            }
            FolderOp::Receive { .. } => {
                while matches!(answer.next_data(), Ok(Some(_))) {}
                let _ = self.0.lock().unwrap().send(());
                // Keep the accepted stream open without sending its final reply.
                let _ = answer.next_data();
            }
            _ => {
                let _ = answer.reply(&PeerReply::Refused {
                    reason: "Not served by this fixture".into(),
                });
            }
        }
    }
}

/// Starts waiting for file data immediately, while the producer is still
/// checking control and sending transport heartbeats.
struct WaitingForReceiveData(std::sync::Mutex<mpsc::Sender<()>>);

impl PeerHost for WaitingForReceiveData {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        let PeerCall::Folder { op, .. } = call else {
            return;
        };
        match *op {
            FolderOp::Probe => {
                let _ = answer.reply(&PeerReply::Done);
            }
            FolderOp::NameMatching => {
                let _ = answer.reply(&PeerReply::Names {
                    case: NameCase::Exact,
                    unicode: NameUnicode::Exact,
                });
            }
            FolderOp::Receive { .. } => {
                let _ = self.0.lock().unwrap().send(());
                let _ = answer.next_data();
            }
            _ => {
                let _ = answer.reply(&PeerReply::Refused {
                    reason: "Not served by this fixture".into(),
                });
            }
        }
    }
}

/// An already-trusted peer endpoint answered by `host`, with its runtime
/// and answering link kept alive by the returned values.
fn peer_endpoint(
    host: Arc<dyn PeerHost>,
) -> (tokio::runtime::Runtime, PeerEndpoint, Arc<PeerLink>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            host,
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(b"/".to_vec()).unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let endpoint = PeerEndpoint::open(a, &root).unwrap();
    (runtime, endpoint, b)
}

/// Answers setup calls, then leaves a prune request waiting on its stream.
struct StallingPrune(std::sync::Mutex<mpsc::Sender<()>>);

impl PeerHost for StallingPrune {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        let PeerCall::Folder { op, .. } = call else {
            return;
        };
        match *op {
            FolderOp::Probe => {
                let _ = answer.reply(&PeerReply::Done);
            }
            FolderOp::NameMatching => {
                let _ = answer.reply(&PeerReply::Names {
                    case: NameCase::Exact,
                    unicode: NameUnicode::Exact,
                });
            }
            FolderOp::PruneArchive { .. } => {
                let _ = self.0.lock().unwrap().send(());
                // Keep the accepted stream open without sending its reply.
                let _ = answer.next_data();
            }
            _ => {
                let _ = answer.reply(&PeerReply::Refused {
                    reason: "Not served by this fixture".into(),
                });
            }
        }
    }
}

/// A paired endpoint whose prune request is accepted but never answered:
/// Stop while the serving Mac still prunes is `Cancelled`, as for reads
/// and observations, and a finished prune reports what it removed.
#[test]
fn a_peer_prune_answers_progress_and_stopping_its_wait_cancels() {
    let (started_tx, started) = mpsc::channel();
    let (_runtime, endpoint, b) =
        peer_endpoint(Arc::new(StallingPrune(std::sync::Mutex::new(started_tx))));
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result =
            endpoint.prune_archive(Duration::from_secs(30 * 86_400), UNIX_EPOCH, &mut || {
                if worker_stop.load(Ordering::Relaxed) {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            });
        let _ = done_tx.send(result);
    });

    started.recv_timeout(Duration::from_secs(10)).unwrap();
    stopped.store(true, Ordering::Relaxed);
    let result = done.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        // Let the pre-fix blocked reply wait finish before failing the test.
        drop(b);
        let _ = done.recv_timeout(Duration::from_secs(10));
        worker.join().unwrap();
        panic!("Stop did not interrupt the peer prune");
    }
    assert!(matches!(
        result,
        Ok(Err(crate::ApplyError::Scan(ScanError::Cancelled)))
    ));
    worker.join().unwrap();

    // A real shared folder proves the prune itself: the expired run goes
    // and the too-young one stays, by the serving Mac's clock.
    let shared = tempfile::tempdir().unwrap();
    let archive = shared.path().join(".foldover/archive");
    let day = Duration::from_secs(86_400);
    let now = SystemTime::now();
    let old = archive.join(crate::archive_name(now - 40 * day));
    std::fs::create_dir_all(old.join("kept")).unwrap();
    std::fs::write(old.join("kept/data"), b"old").unwrap();
    let recent = archive.join(crate::archive_name(now - 20 * day));
    std::fs::create_dir_all(&recent).unwrap();
    let (_runtime, endpoint, _link) = peer_endpoint(Arc::new(Sharing {
        root: LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap(),
        cache: FolderCache::default(),
        state: None,
    }));
    let endpoint = Endpoint::Peer(endpoint);

    let pruned = endpoint.prune_archive(30 * day, now, crate::Pruning::Everyone, &mut || {
        ScanControl::Continue
    });
    assert_eq!(pruned.ok(), Some(1));
    assert!(!old.exists());
    assert!(recent.exists());
}

/// A paired endpoint whose observation request is accepted but never answered.
#[test]
fn stopping_a_stalled_peer_observation_returns_cancelled() {
    let (started_tx, started) = mpsc::channel();
    let (_runtime, endpoint, b) = peer_endpoint(Arc::new(StallingObservation(
        std::sync::Mutex::new(started_tx),
    )));
    let endpoint = Endpoint::Peer(endpoint);
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let options = PairOptions::default();
        let result = endpoint.observe_from(
            options.metadata(),
            &crate::Filter::none(),
            &std::collections::BTreeMap::new(),
            &mut || {
                if worker_stop.load(Ordering::Relaxed) {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            },
            &mut |_| {},
        );
        let _ = done_tx.send(result);
    });

    started.recv_timeout(Duration::from_secs(10)).unwrap();
    stopped.store(true, Ordering::Relaxed);
    let result = done.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        // Let the pre-fix blocked frame read finish before failing the test.
        drop(b);
        let _ = done.recv_timeout(Duration::from_secs(10));
        worker.join().unwrap();
        panic!("Stop did not interrupt the peer observation");
    }
    assert!(matches!(result, Ok(Err(ScanError::Cancelled))));
    worker.join().unwrap();
}

/// Stop while the destination withholds its final acknowledgement is still
/// cancellation, not a source write/change failure.
#[test]
fn stopping_a_peer_receive_waiting_for_its_final_reply_returns_cancelled() {
    let (started_tx, started) = mpsc::channel();
    let (_runtime, endpoint, b) =
        peer_endpoint(Arc::new(StallingReceive(std::sync::Mutex::new(started_tx))));
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let path = SyncPath::try_from(b"file.bin".as_slice()).unwrap();
        let run = RunScope::new(UNIX_EPOCH, crate::test_pair(), PairOptions::default());
        let carried = Carried {
            metadata: foldover_fs::ForeignMetadata {
                permissions: 0o644,
                modified: UNIX_EPOCH,
            },
            native: None,
        };
        let content = Digest::from_bytes([3; Digest::BYTE_LEN]);
        let mut control = || {
            if worker_stop.load(Ordering::Relaxed) {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        };
        let result = endpoint.receive(
            &path,
            Expected::Absent,
            IncomingKind::File {
                content: content.as_bytes().to_vec(),
                from: 0,
            },
            carried,
            &run,
            &mut control,
            |sink, _| sink(b"complete file"),
        );
        let _ = done_tx.send(result);
    });

    started.recv_timeout(Duration::from_secs(10)).unwrap();
    stopped.store(true, Ordering::Relaxed);
    let result = done.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        drop(b);
        let _ = done.recv_timeout(Duration::from_secs(10));
        worker.join().unwrap();
        panic!("Stop did not interrupt the final peer reply");
    }
    assert!(matches!(
        result,
        Ok(Err(crate::ApplyError::Scan(ScanError::Cancelled)))
    ));
    worker.join().unwrap();
}

/// A slow producer checks Stop before it has data. Its transport heartbeat
/// path must retain cancellation rather than misreport a source write failure.
#[test]
fn stopping_a_peer_receive_while_its_producer_is_still_working_returns_cancelled() {
    let (started_tx, started) = mpsc::channel();
    let (_runtime, endpoint, b) = peer_endpoint(Arc::new(WaitingForReceiveData(
        std::sync::Mutex::new(started_tx),
    )));
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stopped);
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let path = SyncPath::try_from(b"file.bin".as_slice()).unwrap();
        let run = RunScope::new(UNIX_EPOCH, crate::test_pair(), PairOptions::default());
        let carried = Carried {
            metadata: foldover_fs::ForeignMetadata {
                permissions: 0o644,
                modified: UNIX_EPOCH,
            },
            native: None,
        };
        let content = Digest::from_bytes([3; Digest::BYTE_LEN]);
        let mut control = || {
            if worker_stop.load(Ordering::Relaxed) {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        };
        let result = endpoint.receive(
            &path,
            Expected::Absent,
            IncomingKind::File {
                content: content.as_bytes().to_vec(),
                from: 0,
            },
            carried,
            &run,
            &mut control,
            |_, checkpoint| loop {
                if checkpoint() == ScanControl::Cancel {
                    return Err(io::Error::other("cancelled"));
                }
                std::thread::sleep(Duration::from_millis(25));
            },
        );
        let _ = done_tx.send(result);
    });

    started.recv_timeout(Duration::from_secs(10)).unwrap();
    // Cross the heartbeat interval before stopping the source producer.
    std::thread::sleep(Duration::from_millis(250));
    stopped.store(true, Ordering::Relaxed);
    let result = done.recv_timeout(Duration::from_secs(2));
    if result.is_err() {
        drop(b);
        let _ = done.recv_timeout(Duration::from_secs(10));
        worker.join().unwrap();
        panic!("Stop did not interrupt the peer source producer");
    }
    assert!(matches!(
        result,
        Ok(Err(crate::ApplyError::Scan(ScanError::Cancelled)))
    ));
    worker.join().unwrap();
}

/// A producer that ignores a transport-induced Stop must keep seeing Stop,
/// and its later source error must not replace the original link failure.
#[test]
fn a_peer_receive_keeps_a_transport_failure_sticky_for_its_producer() {
    let (server_started_tx, server_started) = mpsc::channel();
    let (_runtime, endpoint, answerer) = peer_endpoint(Arc::new(WaitingForReceiveData(
        std::sync::Mutex::new(server_started_tx),
    )));
    let client_link = Arc::clone(&endpoint.link);
    let server_device = answerer.device();
    let (producer_started_tx, producer_started) = mpsc::channel();
    let (release_tx, release) = mpsc::channel();
    let (done_tx, done) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let path = SyncPath::try_from(b"file.bin".as_slice()).unwrap();
        let run = RunScope::new(UNIX_EPOCH, crate::test_pair(), PairOptions::default());
        let carried = Carried {
            metadata: foldover_fs::ForeignMetadata {
                permissions: 0o644,
                modified: UNIX_EPOCH,
            },
            native: None,
        };
        let content = Digest::from_bytes([3; Digest::BYTE_LEN]);
        let result = endpoint.receive(
            &path,
            Expected::Absent,
            IncomingKind::File {
                content: content.as_bytes().to_vec(),
                from: 0,
            },
            carried,
            &run,
            &mut || ScanControl::Continue,
            |_, checkpoint| {
                producer_started_tx.send(()).unwrap();
                release.recv_timeout(Duration::from_secs(10)).unwrap();
                assert_eq!(checkpoint(), ScanControl::Cancel);
                assert_eq!(checkpoint(), ScanControl::Cancel);
                Err(io::Error::other("producer stopped after the link failed"))
            },
        );
        let _ = done_tx.send(result);
    });

    server_started
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    producer_started
        .recv_timeout(Duration::from_secs(10))
        .unwrap();
    std::thread::sleep(STREAM_HEARTBEAT_INTERVAL + Duration::from_millis(50));
    client_link.revoke(server_device);
    release_tx.send(()).unwrap();

    let result = done.recv_timeout(Duration::from_secs(2));
    assert!(matches!(
        result,
        Ok(Err(crate::ApplyError::Scan(ScanError::MacUnreachable(_))))
    ));
    worker.join().unwrap();
}

impl PeerHost for Sharing {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        match call {
            PeerCall::Folder { op, .. } => {
                serve_folder(&self.root, *op, &self.cache, answer, self.state.as_deref())
            }
            PeerCall::Shares => {
                let _ = answer.reply(&PeerReply::Shares { shares: Vec::new() });
            }
        }
    }
}

/// Review: a copy to another Mac whose local source failed partway kept
/// waiting for that Mac's reply, while that Mac waited for the rest.
#[test]
fn a_copy_whose_source_fails_partway_stops_instead_of_waiting() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let folder = tempfile::tempdir().unwrap();
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, _b) = runtime.block_on(async {
        let sharing = Arc::new(Sharing {
            root: LocalRoot::from_bytes(folder.path().as_os_str().as_encoded_bytes().to_vec())
                .unwrap(),
            cache: FolderCache::default(),
            state: None,
        });
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            sharing,
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(b"/".to_vec()).unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });

    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
        let endpoint = PeerEndpoint::open(a, &root).unwrap();
        let path = SyncPath::try_from(&b"big.bin"[..]).unwrap();
        let run = RunScope::new(UNIX_EPOCH, crate::test_pair(), PairOptions::default());
        let carried = Carried {
            metadata: foldover_fs::ForeignMetadata {
                permissions: 0o644,
                modified: UNIX_EPOCH,
            },
            native: None,
        };
        let content = Digest::from_bytes([3; Digest::BYTE_LEN]);
        let received = endpoint.receive(
            &path,
            Expected::Absent,
            IncomingKind::File {
                content: content.as_bytes().to_vec(),
                from: 0,
            },
            carried,
            &run,
            &mut || ScanControl::Continue,
            |sink, _| {
                sink(&[1; 64 * 1024])?;
                Err(io::Error::other("the source changed"))
            },
        );
        let _ = done.send(received.is_err());
    });
    assert_eq!(
        finished.recv_timeout(Duration::from_secs(20)),
        Ok(true),
        "the copy waited for the other Mac"
    );
    assert!(!folder.path().join("big.bin").exists());
}

/// Serves like `Sharing`, from a folder on one of its network volumes.
struct SharingFromNetwork(Sharing);

impl PeerHost for SharingFromNetwork {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        self.0.paired(device, name)
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        match call {
            PeerCall::Folder { op, .. } if matches!(*op, FolderOp::Volume) => {
                let _ = answer.reply(&PeerReply::Volume {
                    volume: VolumeKind::Network,
                });
            }
            call => self.0.serve(device, call, answer),
        }
    }
}

/// Review of quarantine: a folder another Mac keeps on a network volume
/// counted as local, so runs wrote Gatekeeper's mark to that share.
#[test]
fn a_folder_another_mac_keeps_on_a_network_volume_says_so() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let folder = tempfile::tempdir().unwrap();
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let sharing = |path: &[u8]| Sharing {
        root: LocalRoot::from_bytes(path.to_vec()).unwrap(),
        cache: FolderCache::default(),
        state: None,
    };
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            Arc::new(SharingFromNetwork(sharing(
                folder.path().as_os_str().as_encoded_bytes(),
            ))),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(sharing(b"/")),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });

    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let other = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());
    assert!(other.on_network_volume().unwrap());
}

/// Why `RefusingScans` refuses, as a Mac that paused syncing says it.
const REFUSAL_REASON: &str = "That Mac paused syncing";

/// Serves like `Sharing`, but refuses every scan, as a Mac that paused
/// syncing or stopped sharing the folder after it was opened does.
struct RefusingScans(Sharing);

impl PeerHost for RefusingScans {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        self.0.paired(device, name)
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        match call {
            PeerCall::Folder { op, .. } if matches!(*op, FolderOp::Observe { .. }) => {
                let _ = answer.reply(&PeerReply::Refused {
                    reason: REFUSAL_REASON.into(),
                });
            }
            call => self.0.serve(device, call, answer),
        }
    }
}

/// A scan the other Mac refuses says why, as every other refused call
/// does, instead of reporting that the other Mac answered out of order.
#[test]
fn a_refused_scan_says_why() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let folder = tempfile::tempdir().unwrap();
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let sharing = |path: &[u8]| Sharing {
        root: LocalRoot::from_bytes(path.to_vec()).unwrap(),
        cache: FolderCache::default(),
        state: None,
    };
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            Arc::new(RefusingScans(sharing(
                folder.path().as_os_str().as_encoded_bytes(),
            ))),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(sharing(b"/")),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });

    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let other = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());
    let scanned = observe(&other);
    assert!(
        matches!(&scanned, Err(ScanError::MacUnreachable(reason)) if reason == REFUSAL_REASON),
        "{scanned:?}"
    );
}

/// PLAN.md §4.9: copies to and from another Mac run several at once, each
/// on its own stream, and every file arrives whole.
#[test]
fn copies_to_and_from_another_mac_run_at_once() {
    const FILES: usize = 24;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (local_directory, back_directory, shared) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let sharing = |path: &std::path::Path| {
        Arc::new(Sharing {
            root: LocalRoot::from_bytes(path.as_os_str().as_encoded_bytes().to_vec()).unwrap(),
            cache: FolderCache::default(),
            state: None,
        })
    };
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            sharing(shared.path()),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            sharing(std::path::Path::new("/")),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    let content = |index: usize| format!("file {index} ").repeat(20_000).into_bytes();
    for index in 0..FILES {
        std::fs::write(
            local_directory.path().join(format!("{index}.bin")),
            content(index),
        )
        .unwrap();
    }

    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let other = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());
    assert_eq!(other.workers().get(), 4);
    let local = Endpoint::Local(crate::LocalEndpoint::open(local_directory.path()).unwrap());
    let back = Endpoint::Local(crate::LocalEndpoint::open(back_directory.path()).unwrap());
    let options = PairOptions::default();
    let run = RunScope::new(UNIX_EPOCH, crate::test_pair(), options.clone());
    let copy_all = |to: &Endpoint, from: &Endpoint| {
        let observed = from
            .observe_from(
                options.metadata(),
                &crate::Filter::none(),
                &std::collections::BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |_| {},
            )
            .unwrap();
        let names: Vec<_> = observed.entries().keys().cloned().collect();
        std::thread::scope(|threads| {
            for share in names.chunks(FILES / 4) {
                let (observed, run) = (&observed, &run);
                threads.spawn(move || {
                    for name in share {
                        to.copy_from(
                            from,
                            name,
                            &observed.entries()[name],
                            Expected::Absent,
                            run,
                            &mut || ScanControl::Continue,
                        )
                        .unwrap();
                    }
                });
            }
        });
    };

    copy_all(&other, &local);
    copy_all(&back, &other);
    for index in 0..FILES {
        let name = format!("{index}.bin");
        assert_eq!(
            std::fs::read(shared.path().join(&name)).unwrap(),
            content(index)
        );
        assert_eq!(
            std::fs::read(back_directory.path().join(&name)).unwrap(),
            content(index)
        );
    }
}

fn observe(endpoint: &Endpoint) -> Result<Observation, ScanError> {
    let options = PairOptions::default();
    endpoint.observe_from(
        options.metadata(),
        &crate::Filter::none(),
        &std::collections::BTreeMap::new(),
        &mut || ScanControl::Continue,
        &mut |_| {},
    )
}

/// Reports a chosen unstable directory before serving an ordinary observation.
struct UnstableSharing {
    sharing: Sharing,
    path: std::sync::Mutex<Option<Vec<u8>>>,
}

impl PeerHost for UnstableSharing {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        self.sharing.paired(device, name)
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        if matches!(&call, PeerCall::Folder { op, .. } if matches!(**op, FolderOp::Observe { .. }))
        {
            let path = self.path.lock().unwrap().clone();
            if answer
                .reply(&PeerReply::UnstableDirectory { path })
                .is_err()
            {
                return;
            }
        }
        self.sharing.serve(device, call, answer);
    }
}

#[test]
fn unstable_peer_directories_preserve_entries_and_validate_paths() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let shared = tempfile::tempdir().unwrap();
    std::fs::write(shared.path().join("safe"), b"readable").unwrap();
    let hosting = Arc::new(UnstableSharing {
        sharing: Sharing {
            root: LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec())
                .unwrap(),
            cache: FolderCache::default(),
            state: None,
        },
        path: std::sync::Mutex::new(None),
    });
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            hosting.clone(),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(b"/".to_vec()).unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let endpoint = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());
    for path in [None, Some(b"docs".to_vec())] {
        *hosting.path.lock().unwrap() = path.clone();
        let observed = observe(&endpoint).unwrap();
        let expected = path.map(|path| SyncPath::try_from(path).unwrap());
        assert_eq!(observed.unstable_directories(), &[expected]);
        assert!(observed.unavailable().is_empty());
        assert!(
            observed
                .entries()
                .contains_key(&SyncPath::try_from(b"safe".as_slice()).unwrap())
        );
    }
    *hosting.path.lock().unwrap() = Some(b"../outside".to_vec());
    assert!(matches!(observe(&endpoint), Err(ScanError::InvalidPath(_))));
}

/// A peer that goes away answers errors, never hangs: once its link
/// closes, remote calls on it fail instead of waiting on a dead Mac.
#[test]
fn calls_on_a_link_whose_peer_went_away_fail_instead_of_hanging() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let (shared, a_share) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec())
                    .unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(a_share.path().as_os_str().as_encoded_bytes().to_vec())
                    .unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });

    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let other = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());
    // The link must be proven up, or the calls below could fail for a
    // reason the test does not mean to exercise.
    observe(&other).expect("the link must be proven up before the peer goes away");
    drop(b);

    let (done, result) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = done.send(observe(&other));
    });
    let result = result
        .recv_timeout(std::time::Duration::from_secs(45))
        .expect("a dead peer must answer within the request timeout");
    worker.join().expect("the observe thread must not panic");
    assert!(result.is_err());
}

/// A shared folder that holds the serving agent's state directory never
/// exposes it to the asking Mac: the subtree is missing from scans, and
/// reads and writes inside it are refused.
#[test]
fn a_shared_folder_never_serves_the_agent_state_directory() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let shared = tempfile::tempdir().unwrap();
    let state = shared.path().join("Library/Application Support/Foldover");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("pairs.sqlite"), b"records").unwrap();
    std::fs::write(shared.path().join("mine.txt"), b"mine").unwrap();
    std::fs::write(shared.path().join("._unclassified"), b"metadata").unwrap();
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let a_share = tempfile::tempdir().unwrap();
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec())
                    .unwrap(),
                cache: FolderCache::default(),
                state: Some(state.clone()),
            }),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(a_share.path().as_os_str().as_encoded_bytes().to_vec())
                    .unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });

    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let other = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());
    let seen = observe(&other).unwrap();
    let sidecar = SyncPath::try_from(b"._unclassified".as_slice()).unwrap();
    assert_eq!(seen.issues().len(), 1);
    assert_eq!(seen.issues()[0].path(), &sidecar);
    assert_eq!(seen.unavailable(), &[sidecar]);
    assert!(seen.issues()[0].message().contains("AppleDouble"));
    let prefix = b"Library/Application Support/Foldover";
    assert!(!seen.entries().keys().any(|path| {
        let bytes = path.as_bytes();
        bytes == prefix || bytes.starts_with(b"Library/Application Support/Foldover/")
    }));
    assert!(
        seen.entries()
            .contains_key(&SyncPath::try_from(b"mine.txt".as_slice()).unwrap())
    );

    // A read or write inside the state directory is refused there.
    let Endpoint::Peer(peer) = &other else {
        unreachable!()
    };
    let run = RunScope::new(UNIX_EPOCH, crate::test_pair(), PairOptions::default());
    let inside =
        SyncPath::try_from(b"Library/Application Support/Foldover/pairs.sqlite".as_slice())
            .unwrap();

    // The entry a real scan plans for pairs.sqlite, so the read can only
    // be refused by the state-directory guard, never an entry mismatch.
    // (The share's own endpoint protects what the *agent's* endpoint
    // serves, not what sits on disk.)
    let unprotected = Endpoint::Local(crate::LocalEndpoint::open(shared.path()).unwrap());
    let planned = observe(&unprotected).unwrap().entries()[&inside].clone();
    assert!(
        peer.read(&inside, &planned, &run, 0, &mut || ScanControl::Continue,)
            .is_err()
    );

    // Case-insensitive volumes (default APFS) resolve this spelling to
    // the same file; the guard compares directory identity, not bytes. On
    // case-sensitive volumes the alias does not exist, so the read would
    // fail as missing rather than as guarded — probe only where it can
    // resolve.
    if shared.path().join("library").exists() {
        let cased =
            SyncPath::try_from(b"library/Application Support/Foldover/pairs.sqlite".as_slice())
                .unwrap();
        assert!(
            peer.read(&cased, &planned, &run, 0, &mut || ScanControl::Continue,)
                .is_err()
        );
    }

    // A write inside the state directory is refused there as well. The
    // target does not exist, so `Expected::Absent` is truthful and the
    // guard is the only possible reason for refusal.
    let tampered =
        SyncPath::try_from(b"Library/Application Support/Foldover/tampered.sqlite".as_slice())
            .unwrap();
    let carried = Carried {
        metadata: foldover_fs::ForeignMetadata {
            permissions: 0o644,
            modified: UNIX_EPOCH,
        },
        native: None,
    };
    let content = Digest::from_bytes([2; Digest::BYTE_LEN]);
    assert!(
        peer.receive(
            &tampered,
            Expected::Absent,
            IncomingKind::File {
                content: content.as_bytes().to_vec(),
                from: 0,
            },
            carried,
            &run,
            &mut || ScanControl::Continue,
            |sink, _| sink(b"tampered"),
        )
        .is_err()
    );
    assert!(!state.join("tampered.sqlite").exists());
    assert_eq!(
        std::fs::read(state.join("pairs.sqlite")).unwrap(),
        b"records"
    );
}

/// A counter grows by its own steps, however the other Mac counts; one
/// that stands still or goes back reports nothing new.
#[test]
fn progress_reports_only_a_peers_counters_growth() {
    let mut adopted = 0;
    let mut steps = 0;
    report_steps(&mut adopted, 3, ObserveStep::Listed, &mut |_| steps += 1);
    assert_eq!(steps, 3);
    report_steps(&mut adopted, 2, ObserveStep::Listed, &mut |_| steps += 1);
    report_steps(&mut adopted, 3, ObserveStep::Listed, &mut |_| steps += 1);
    assert_eq!(steps, 3);
    assert_eq!(adopted, 3);
}

/// Whatever the other Mac claims its counter reached, the asking side
/// works a bounded number of steps for it (PLAN.md §4.3).
#[test]
fn a_peers_counter_cannot_make_the_asking_side_iterate() {
    let mut adopted = 0;
    let mut steps = 0;
    report_steps(&mut adopted, u64::MAX, ObserveStep::Checked, &mut |_| {
        steps += 1
    });
    assert_eq!(steps, MAX_STEPS_PER_REPORT);
    report_steps(&mut adopted, u64::MAX, ObserveStep::Checked, &mut |_| {
        steps += 1
    });
    assert_eq!(steps, MAX_STEPS_PER_REPORT);
    assert_eq!(adopted, u64::MAX);
}

/// Once a scan's counter stood for nearly `MAX_STEPS_PER_SCAN` steps,
/// further reports, each within one report's cap, add only the rest:
/// however many reports the other Mac sends, a scan's work stays bounded.
#[test]
fn a_scans_counter_stops_adding_steps_at_the_scan_bound() {
    let mut adopted = MAX_STEPS_PER_SCAN - MAX_STEPS_PER_REPORT / 2;
    let mut steps = 0;
    for _ in 0..3 {
        let now = adopted + MAX_STEPS_PER_REPORT;
        report_steps(&mut adopted, now, ObserveStep::Listed, &mut |_| steps += 1);
    }
    assert_eq!(steps, MAX_STEPS_PER_REPORT / 2);
}

/// A scan counter far past the folder's size, as a buggy or hostile
/// Mac might claim: more than one report's cap of steps.
const HUGE_COUNT: u64 = MAX_STEPS_PER_REPORT + 5;

/// Claims a scan counter far past the folder's size before answering
/// the observation it shares, as a buggy or hostile Mac might.
struct InflatingSharing {
    sharing: Sharing,
}

impl PeerHost for InflatingSharing {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        self.sharing.paired(device, name)
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        if matches!(&call, PeerCall::Folder { op, .. } if matches!(**op, FolderOp::Observe { .. }))
            && answer
                .reply(&PeerReply::Progress {
                    listed: HUGE_COUNT,
                    checked: HUGE_COUNT,
                })
                .is_err()
        {
            return;
        }
        self.sharing.serve(device, call, answer);
    }
}

/// The other Mac's scan counters stay display data end to end: the
/// asking Mac's progress loop stays bounded by one report's cap.
#[test]
fn inflated_scan_counters_report_a_bounded_number_of_steps() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let shared = tempfile::tempdir().unwrap();
    std::fs::write(shared.path().join("safe"), b"readable").unwrap();
    let hosting = Arc::new(InflatingSharing {
        sharing: Sharing {
            root: LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec())
                .unwrap(),
            cache: FolderCache::default(),
            state: None,
        },
    });
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            hosting,
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(b"/".to_vec()).unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let endpoint = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());

    let options = PairOptions::default();
    let mut steps = 0u64;
    let observed = endpoint
        .observe_from(
            options.metadata(),
            &crate::Filter::none(),
            &std::collections::BTreeMap::new(),
            &mut || ScanControl::Continue,
            &mut |_| steps += 1,
        )
        .unwrap();
    // Each of the report's two counters stands for at most one cap.
    assert_eq!(
        steps,
        2 * MAX_STEPS_PER_REPORT,
        "both inflated counters must reach the cap"
    );
    assert!(
        observed
            .entries()
            .contains_key(&SyncPath::try_from(b"safe".as_slice()).unwrap())
    );
}

/// Serves like `Sharing`, after first reporting `partway` of an
/// observation's counts, as a report sent partway through a scan longer
/// than one progress interval would.
struct PartwaySharing {
    sharing: Sharing,
    partway: std::sync::Mutex<Option<(u64, u64)>>,
}

impl PeerHost for PartwaySharing {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        self.sharing.paired(device, name)
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        let partway = *self.partway.lock().unwrap();
        if matches!(&call, PeerCall::Folder { op, .. } if matches!(**op, FolderOp::Observe { .. }))
            && let Some((listed, checked)) = partway
            && answer
                .reply(&PeerReply::Progress { listed, checked })
                .is_err()
        {
            return;
        }
        self.sharing.serve(device, call, answer);
    }
}

/// The asking Mac's count ends on every entry the other Mac scanned.
/// Reports on the way go out at least one progress interval apart, so a
/// scan that ends sooner sends none, and a longer scan's last one can be
/// up to an interval behind.
#[test]
fn a_peer_scans_count_ends_on_every_entry() {
    const ENTRIES: usize = 5;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let shared = tempfile::tempdir().unwrap();
    for index in 0..ENTRIES {
        std::fs::write(shared.path().join(format!("{index}.txt")), b"counted").unwrap();
    }
    let hosting = Arc::new(PartwaySharing {
        sharing: Sharing {
            root: LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec())
                .unwrap(),
            cache: FolderCache::default(),
            state: None,
        },
        partway: std::sync::Mutex::new(None),
    });
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, _b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            hosting.clone(),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(b"/".to_vec()).unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    let root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let endpoint = Endpoint::Peer(PeerEndpoint::open(a, &root).unwrap());

    // A scan of a few files ends long before its first report is due; a
    // longer one has reported part of its count by the time it ends.
    let options = PairOptions::default();
    for partway in [None, Some((2, 1))] {
        *hosting.partway.lock().unwrap() = partway;
        let (mut listed, mut checked) = (0, 0);
        let observed = endpoint
            .observe_from(
                options.metadata(),
                &crate::Filter::none(),
                &std::collections::BTreeMap::new(),
                &mut || ScanControl::Continue,
                &mut |step| match step {
                    ObserveStep::Listed => listed += 1,
                    ObserveStep::Checked => checked += 1,
                },
            )
            .unwrap();
        assert_eq!(observed.entries().len(), ENTRIES);
        assert_eq!(
            (listed, checked),
            (ENTRIES, ENTRIES),
            "reported partway: {partway:?}"
        );
    }
}

/// Starts two paired links, the answering one sharing `root` with the
/// asking one, and returns the asking link and endpoint with both links
/// and the runtime kept alive.
fn catalog_peer(
    root: LocalRoot,
) -> (
    tokio::runtime::Runtime,
    PeerEndpoint,
    Arc<PeerLink>,
    Arc<PeerLink>,
) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let identities = [Identity::generate().unwrap(), Identity::generate().unwrap()];
    let devices = identities
        .each_ref()
        .map(|identity| DeviceId::from_bytes(*identity.id().as_bytes()));
    let [a_identity, b_identity] = identities;
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
    let (a, b) = runtime.block_on(async {
        let b = PeerLink::start(
            b_identity,
            "Studio".into(),
            loopback,
            Presence::Unlisted,
            [devices[0]],
            Arc::new(Sharing {
                root,
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Sharing {
                root: LocalRoot::from_bytes(b"/".to_vec()).unwrap(),
                cache: FolderCache::default(),
                state: None,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    let peer_root = PeerRoot::new(devices[1], ShareId::from_bytes([7; ShareId::BYTE_LEN]));
    let endpoint = PeerEndpoint::open(Arc::clone(&a), &peer_root).unwrap();
    (runtime, endpoint, a, b)
}

/// The pair every catalog test uses.
fn catalog_pair() -> PairId {
    PairId::from_bytes([0x42; PairId::BYTE_LEN]).unwrap()
}

struct MalformedCatalogPage(PeerReply);

impl PeerHost for MalformedCatalogPage {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        let PeerCall::Folder { op, .. } = call else {
            return;
        };
        match *op {
            FolderOp::NameMatching => {
                let _ = answer.reply(&PeerReply::Names {
                    case: NameCase::Exact,
                    unicode: NameUnicode::Exact,
                });
            }
            FolderOp::Catalog {
                op: CatalogOp::List,
                ..
            } => {
                let _ = answer.reply(&self.0);
                let _ = answer.reply(&PeerReply::CatalogListed {
                    names: Vec::new(),
                    more: false,
                });
            }
            _ => {
                let _ = answer.reply(&PeerReply::Done);
            }
        }
    }
}

#[test]
fn malformed_peer_catalog_pages_are_refused() {
    for reply in [
        PeerReply::CatalogListed {
            names: Vec::new(),
            more: true,
        },
        PeerReply::CatalogListed {
            names: vec!["../escape".into()],
            more: false,
        },
        PeerReply::CatalogListed {
            names: vec!["x".repeat(129)],
            more: false,
        },
    ] {
        let (_runtime, endpoint, _server) = peer_endpoint(Arc::new(MalformedCatalogPage(reply)));
        let catalog = endpoint
            .catalog_endpoint(catalog_pair(), Durability::Standard)
            .unwrap();
        assert_eq!(
            catalog.catalog_list().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}

/// A catalog file's bytes travel as data frames, so a file crossing the
/// 1 MiB block boundary still arrives whole, and the catalog's directory,
/// lease and listing all live under the pair's private directory.
#[test]
fn a_peer_catalog_round_trips_through_its_private_directory() {
    let shared = tempfile::tempdir().unwrap();
    let root =
        LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let (_runtime, endpoint, _a, _b) = catalog_peer(root);
    let pair = catalog_pair();
    let catalog = endpoint
        .catalog_endpoint(pair, Durability::Standard)
        .unwrap();

    let private = shared.path().join(".foldover/state").join(pair.to_string());
    assert!(
        private.join("format").exists(),
        "the catalog marker is created"
    );

    // One byte more than four 1 MiB blocks: the file leaves in five frames.
    let bytes = vec![b'x'; 4 * 1024 * 1024 + 1];
    catalog
        .catalog_write("checkpoint-1", &bytes, Durability::Standard)
        .unwrap();
    assert_eq!(
        catalog
            .catalog_read("checkpoint-1", CATALOG_FILE_LIMIT)
            .unwrap()
            .as_deref(),
        Some(bytes.as_slice())
    );
    assert!(private.join("checkpoint-1").exists());

    // A second write of the same name reports it already exists, as the
    // local endpoint does, so a caller can read back and compare.
    assert_eq!(
        catalog
            .catalog_write("checkpoint-1", b"other", Durability::Standard)
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );

    // An empty file round-trips too, distinguished from an absent one.
    catalog
        .catalog_write("empty", b"", Durability::Standard)
        .unwrap();
    assert_eq!(
        catalog.catalog_read("empty", CATALOG_FILE_LIMIT).unwrap(),
        Some(Vec::new())
    );

    // The listing carries the names; removal makes them absent.
    let listed = catalog.catalog_list().unwrap();
    assert!(listed.contains(&"checkpoint-1".to_owned()));
    assert!(listed.contains(&"empty".to_owned()));
    assert!(!listed.contains(&"absent".to_owned()));
    catalog.catalog_remove("checkpoint-1").unwrap();
    assert_eq!(
        catalog
            .catalog_read("checkpoint-1", CATALOG_FILE_LIMIT)
            .unwrap(),
        None
    );
    // Removing an absent file is nothing to do, not an error.
    catalog.catalog_remove("checkpoint-1").unwrap();
    assert!(
        !catalog
            .catalog_list()
            .unwrap()
            .contains(&"checkpoint-1".to_owned())
    );
}

/// A catalog session's lease lives in its private directory, so two
/// pairs targeting one shared folder never contend for the shared
/// folder's own lease.
#[test]
fn a_catalog_lease_lives_in_the_private_directory() {
    let shared = tempfile::tempdir().unwrap();
    let root =
        LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let (_runtime, endpoint, _a, _b) = catalog_peer(root);
    let pair = catalog_pair();
    let catalog = endpoint
        .catalog_endpoint(pair, Durability::Standard)
        .unwrap();
    let endpoint = Endpoint::Peer(catalog);

    let lease = OwnedLease::take(
        Arc::new(endpoint),
        LeaseHolder {
            id: "catalog/test".into(),
            label: "Catalog test".into(),
        },
        LeaseTiming::STANDARD,
        &mut || ScanControl::Continue,
    )
    .unwrap();

    let private = shared.path().join(".foldover/state").join(pair.to_string());
    assert!(
        private.join(".foldover/lock").exists(),
        "the catalog lease is in its private directory"
    );
    assert!(
        !shared.path().join(".foldover/lock").exists(),
        "the shared folder's own lease is untouched"
    );
    // The lease renews and releases there, as any held lease does.
    lease.renew().unwrap();
    drop(lease);
    assert!(!private.join(".foldover/lock").exists());
}

/// Full durability is the sharing Mac's promise to make: the catalog
/// applies it to the marker and to each file, as its local endpoint does.
#[test]
fn a_peer_catalog_applies_full_durability() {
    let shared = tempfile::tempdir().unwrap();
    let root =
        LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let (_runtime, endpoint, _a, _b) = catalog_peer(root);
    let catalog = endpoint
        .catalog_endpoint(catalog_pair(), Durability::Full)
        .unwrap();
    catalog
        .catalog_write("durable", b"bytes", Durability::Full)
        .unwrap();
    assert_eq!(
        catalog.catalog_read("durable", 1024).unwrap().as_deref(),
        Some(&b"bytes"[..])
    );
}

/// Names, limits and blob sizes are validated on both ends: a name that
/// is not a plain catalog file, a read past the limit, and a blob past
/// the 16 MiB bound are all refused before anything is written.
#[test]
fn a_catalog_endpoint_validates_names_limits_and_blobs() {
    let shared = tempfile::tempdir().unwrap();
    let root =
        LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let (_runtime, endpoint, _a, _b) = catalog_peer(root);
    let catalog = endpoint
        .catalog_endpoint(catalog_pair(), Durability::Standard)
        .unwrap();

    for name in ["", ".hidden", "../escape", "a/b", "UPPER", &"x".repeat(129)] {
        assert!(
            catalog.catalog_read(name, 1024).is_err(),
            "read {name:?} should be refused"
        );
        assert!(
            catalog
                .catalog_write(name, b"x", Durability::Standard)
                .is_err(),
            "write {name:?} should be refused"
        );
        assert!(
            catalog.catalog_remove(name).is_err(),
            "remove {name:?} should be refused"
        );
    }

    // A read limit past the catalog's bound is refused here, and so is a
    // blob past it, without a byte crossing the link.
    assert_eq!(
        catalog
            .catalog_read("name", CATALOG_FILE_LIMIT + 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    let huge = vec![b'x'; CATALOG_FILE_LIMIT + 1];
    assert_eq!(
        catalog
            .catalog_write("huge", &huge, Durability::Standard)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert!(
        !shared
            .path()
            .join(".foldover/state")
            .join(catalog_pair().to_string())
            .join("huge")
            .exists()
    );

    // An endpoint that is not scoped to a catalog has no catalog to read.
    for error in [
        endpoint.catalog_read("name", 1024).unwrap_err(),
        endpoint.catalog_list().unwrap_err(),
        endpoint.catalog_remove("name").unwrap_err(),
    ] {
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
    assert!(
        endpoint
            .catalog_write("name", b"x", Durability::Standard)
            .is_err()
    );
}

/// `CatalogFiles` opens a peer target's catalog, takes its lease in the
/// pair's private directory, and reads, writes, lists and removes through
/// it, exactly as it does over a local one.
#[test]
fn catalog_files_open_over_a_peer_catalog_and_lease_privately() {
    let shared = tempfile::tempdir().unwrap();
    let root =
        LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let (_runtime, endpoint, _a, _b) = catalog_peer(root);
    let pair = catalog_pair();
    let files = CatalogFiles::open(
        Arc::new(Endpoint::Peer(endpoint)),
        pair,
        LeaseHolder {
            id: "catalog/test".into(),
            label: "Catalog test".into(),
        },
        Durability::Standard,
        &mut || ScanControl::Continue,
    )
    .unwrap();

    let bytes = b"checkpoint-bytes".to_vec();
    files.write_new("checkpoint", &bytes).unwrap();
    assert_eq!(
        files
            .read("checkpoint", CATALOG_FILE_LIMIT)
            .unwrap()
            .as_deref(),
        Some(bytes.as_slice())
    );
    assert!(files.list().unwrap().contains(&"checkpoint".to_owned()));
    // The lease beats and fences from the private directory.
    files.tick().unwrap();
    files.fence().unwrap();
    files.remove("checkpoint").unwrap();
    assert_eq!(files.read("checkpoint", CATALOG_FILE_LIMIT).unwrap(), None);

    let private = shared.path().join(".foldover/state").join(pair.to_string());
    assert!(
        private.join(".foldover/lock").exists(),
        "the catalog lease is in its private directory"
    );
    assert!(
        !shared.path().join(".foldover/lock").exists(),
        "the shared folder's own lease is untouched"
    );
    drop(files);
    assert!(!private.join(".foldover/lock").exists());
}

/// The answering Mac validates the pair it is handed, so a request that
/// does not name a pair is refused rather than served from the shared
/// folder's own `.foldover`.
#[test]
fn a_catalog_request_with_a_malformed_pair_is_refused() {
    let shared = tempfile::tempdir().unwrap();
    let root =
        LocalRoot::from_bytes(shared.path().as_os_str().as_encoded_bytes().to_vec()).unwrap();
    let (_runtime, _endpoint, a, b) = catalog_peer(root);
    let call = PeerCall::Folder {
        share: ShareId::from_bytes([7; ShareId::BYTE_LEN]).to_string(),
        op: Box::new(FolderOp::Catalog {
            pair: "not-a-pair".into(),
            op: CatalogOp::List,
        }),
    };
    let mut exchange = a.ask(b.device(), call).unwrap();
    let reply = exchange.reply().unwrap();
    assert!(matches!(
        reply,
        PeerReply::Failed {
            error: FolderError {
                kind: FolderErrorKind::Invalid,
                ..
            }
        }
    ));
}
