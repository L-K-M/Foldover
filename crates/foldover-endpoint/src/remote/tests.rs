use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc;
use std::time::{Duration, UNIX_EPOCH};

use foldover_net::peer::Identity;
use foldover_proto::peer::{IncomingKind, PeerCall, PeerReply};
use foldover_types::{DeviceId, Digest, LocalRoot, PairOptions, PeerRoot, ShareId, SyncPath};

use super::*;
use crate::link::{Answer, NotRecorded, PeerHost, PeerLink, Presence};
use crate::local::Carried;
use crate::{Endpoint, Expected, RunScope};

/// Serves one folder as every share, as the sharing Mac's agent does.
struct Sharing {
    root: LocalRoot,
    cache: FolderCache,
}

impl PeerHost for Sharing {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, call: PeerCall, answer: &mut Answer) {
        match call {
            PeerCall::Folder { op, .. } => serve_folder(&self.root, *op, &self.cache, answer),
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
