use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

use foldover_net::peer::Identity;
use foldover_proto::peer::{FolderOp, IncomingKind, PeerCall, PeerReply, VolumeKind};
use foldover_types::{DeviceId, Digest, LocalRoot, PairOptions, PeerRoot, ShareId, SyncPath};

use super::*;
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
        let run = RunScope::new(UNIX_EPOCH, PairOptions::default());
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
            |sink| {
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
    let run = RunScope::new(UNIX_EPOCH, options.clone());
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
    let run = RunScope::new(UNIX_EPOCH, PairOptions::default());
    let inside =
        SyncPath::try_from(b"Library/Application Support/Foldover/pairs.sqlite".as_slice())
            .unwrap();

    // The entry a real scan plans for pairs.sqlite, so the read can only
    // be refused by the state-directory guard, never an entry mismatch.
    // (The share's own endpoint protects what the *agent's* endpoint
    // serves, not what sits on disk.)
    let unprotected = Endpoint::Local(crate::LocalEndpoint::open(shared.path()).unwrap());
    let planned = observe(&unprotected).unwrap().entries()[&inside].clone();
    assert!(peer.read(&inside, &planned, &run, 0).is_err());

    // Case-insensitive volumes (default APFS) resolve this spelling to
    // the same file; the guard compares directory identity, not bytes. On
    // case-sensitive volumes the alias does not exist, so the read would
    // fail as missing rather than as guarded — probe only where it can
    // resolve.
    if shared.path().join("library").exists() {
        let cased =
            SyncPath::try_from(b"library/Application Support/Foldover/pairs.sqlite".as_slice())
                .unwrap();
        assert!(peer.read(&cased, &planned, &run, 0).is_err());
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
            |sink| sink(b"tampered"),
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
    assert!(
        steps <= 2 * MAX_STEPS_PER_REPORT,
        "one report's counters made the scan report {steps} steps"
    );
    assert!(
        observed
            .entries()
            .contains_key(&SyncPath::try_from(b"safe".as_slice()).unwrap())
    );
}
