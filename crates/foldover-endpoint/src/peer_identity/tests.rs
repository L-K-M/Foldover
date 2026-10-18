use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use foldover_net::peer::{Node, PeerMode, TrustStore, read_message, write_message};
use zeroize::Zeroizing;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
enum Outcome {
    #[default]
    Stored,
    ReadFailure,
    CreateFailure,
    ReadBackFailure,
    Disappeared,
    ConcurrentCreator(Zeroizing<Vec<u8>>),
}

#[derive(Default)]
struct Store {
    bytes: Mutex<Option<Zeroizing<Vec<u8>>>>,
    reads: AtomicUsize,
    creates: AtomicUsize,
    outcome: Outcome,
}

impl PeerIdentityStore for Store {
    fn read(&self) -> Result<Option<Zeroizing<Vec<u8>>>, IdentityStoreError> {
        let read = self.reads.fetch_add(1, Ordering::SeqCst);
        match self.outcome {
            Outcome::ReadFailure => Err(IdentityStoreError::Unsupported),
            Outcome::ReadBackFailure if read > 0 => Err(IdentityStoreError::Unsupported),
            Outcome::Disappeared => Ok(None),
            _ => Ok(self.bytes.lock().unwrap().clone()),
        }
    }

    fn create_if_absent(&self, bytes: &[u8]) -> Result<(), IdentityStoreError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        match &self.outcome {
            Outcome::CreateFailure => Err(IdentityStoreError::Unsupported),
            Outcome::ConcurrentCreator(winner) => {
                *self.bytes.lock().unwrap() = Some(winner.clone());
                Ok(())
            }
            _ => {
                self.bytes
                    .lock()
                    .unwrap()
                    .get_or_insert_with(|| Zeroizing::new(bytes.to_vec()));
                Ok(())
            }
        }
    }
}

#[test]
fn reloading_keeps_the_saved_identity_without_creating_another() {
    let store = Store::default();
    let first = load_from(&store).unwrap().id();
    assert_eq!(load_from(&store).unwrap().id(), first);
    assert_eq!(store.creates.load(Ordering::SeqCst), 1);
}

#[test]
fn storage_failures_never_publish_an_unsaved_identity() {
    for outcome in [
        Outcome::ReadFailure,
        Outcome::CreateFailure,
        Outcome::ReadBackFailure,
    ] {
        let store = Store {
            outcome,
            ..Store::default()
        };
        assert!(matches!(
            load_from(&store),
            Err(PeerIdentityError::Storage(_))
        ));
        if matches!(store.outcome, Outcome::ReadFailure) {
            assert_eq!(store.creates.load(Ordering::SeqCst), 0);
        }
    }
    let store = Store {
        outcome: Outcome::Disappeared,
        ..Store::default()
    };
    assert!(matches!(
        load_from(&store),
        Err(PeerIdentityError::MissingAfterCreate)
    ));
}

#[test]
fn a_corrupt_stored_identity_is_never_replaced() {
    let store = Store {
        bytes: Mutex::new(Some(Zeroizing::new(b"corrupt identity".to_vec()))),
        ..Store::default()
    };
    assert!(matches!(
        load_from(&store),
        Err(PeerIdentityError::Identity(_))
    ));
    assert_eq!(store.creates.load(Ordering::SeqCst), 0);
    assert_eq!(
        store.bytes.lock().unwrap().as_ref().unwrap().as_slice(),
        b"corrupt identity"
    );
}

#[test]
fn a_concurrent_creators_identity_is_reloaded_and_validated() {
    let winner = PeerIdentity::generate().unwrap();
    let store = Store {
        outcome: Outcome::ConcurrentCreator(winner.to_secret_bytes().unwrap()),
        ..Store::default()
    };
    assert_eq!(load_from(&store).unwrap().id(), winner.id());

    let corrupt = Store {
        outcome: Outcome::ConcurrentCreator(Zeroizing::new(b"corrupt winner".to_vec())),
        ..Store::default()
    };
    assert!(matches!(
        load_from(&corrupt),
        Err(PeerIdentityError::Identity(_))
    ));
    assert_eq!(corrupt.creates.load(Ordering::SeqCst), 1);
}

#[test]
fn concurrent_loaders_converge_on_one_saved_identity() {
    let store = Store::default();
    let ids = std::thread::scope(|scope| {
        let readers: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| load_from(&store).unwrap().id()))
            .collect();
        readers
            .into_iter()
            .map(|reader| reader.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(load_from(&store).unwrap().id(), ids[0]);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn unsupported_platforms_do_not_fall_back_to_plaintext_storage() {
    assert!(matches!(
        load_peer_identity(),
        Err(PeerIdentityError::Storage(IdentityStoreError::Unsupported))
    ));
}

fn node(store: &Store, trust: Arc<TrustStore>) -> Node {
    Node::bind(
        Arc::new(load_from(store).unwrap()),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        trust,
        PeerMode::Paired,
    )
    .unwrap()
}

async fn exchange(a: &Node, b: &Node) {
    tokio::time::timeout(TIMEOUT, async {
        let (dialed, accepted) = tokio::join!(a.dial(b.address().unwrap(), b.id()), b.accept());
        let dialed = dialed.unwrap();
        let accepted = accepted.unwrap().unwrap();
        let (mut send, _) = dialed.open_bi().await.unwrap();
        write_message(&mut send, b"restored identity")
            .await
            .unwrap();
        send.finish().unwrap();
        let (_, mut recv) = accepted.accept_bi().await.unwrap();
        assert_eq!(read_message(&mut recv).await.unwrap(), b"restored identity");
    })
    .await
    .expect("restored peer could not reconnect");
}

#[tokio::test]
async fn restored_identities_reconnect_with_the_original_certificate_pins() {
    let a_store = Store::default();
    let b_store = Store::default();
    let a_trust = Arc::new(TrustStore::default());
    let b_trust = Arc::new(TrustStore::default());
    let a = node(&a_store, Arc::clone(&a_trust));
    let b = node(&b_store, Arc::clone(&b_trust));
    a_trust.pin(b.id());
    b_trust.pin(a.id());
    exchange(&a, &b).await;
    let expected = (a.id(), b.id());
    a.close();
    b.close();
    drop((a, b));

    let a = node(&a_store, a_trust);
    let b = node(&b_store, b_trust);
    assert_eq!((a.id(), b.id()), expected);
    exchange(&a, &b).await;
    a.close();
    b.close();
}
