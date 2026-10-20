use std::net::{Ipv4Addr, SocketAddr};

use tokio::sync::mpsc;

use super::*;

const WAIT: Duration = Duration::from_secs(10);

/// Whether a link's store takes the devices it is asked to record.
#[derive(Clone, Copy)]
enum Store {
    Working,
    Failing,
}

/// A link on loopback and the devices it records as paired.
fn link(name: &str) -> (Arc<PeerLink>, mpsc::UnboundedReceiver<(DeviceId, String)>) {
    link_with(name, Store::Working)
}

fn link_with(
    name: &str,
    store: Store,
) -> (Arc<PeerLink>, mpsc::UnboundedReceiver<(DeviceId, String)>) {
    link_at(
        name,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Presence::Unlisted,
        store,
    )
}

fn link_at(
    name: &str,
    address: SocketAddr,
    presence: Presence,
    store: Store,
) -> (Arc<PeerLink>, mpsc::UnboundedReceiver<(DeviceId, String)>) {
    let (recorded, paired) = mpsc::unbounded_channel();
    let link = PeerLink::start(
        Identity::generate().unwrap(),
        name.into(),
        address,
        presence,
        [],
        Arc::new(Host { recorded, store }),
    )
    .unwrap();
    (link, paired)
}

/// Records pairings; shares "Photos" with every paired Mac.
struct Host {
    recorded: mpsc::UnboundedSender<(DeviceId, String)>,
    store: Store,
}

impl PeerHost for Host {
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded> {
        let _ = self.recorded.send((device, name));
        match self.store {
            Store::Working => Ok(()),
            Store::Failing => Err(NotRecorded),
        }
    }

    fn serve(&self, _device: DeviceId, call: PeerCall, answer: &mut Answer) {
        let reply = match call {
            PeerCall::Shares => PeerReply::Shares {
                shares: vec![foldover_proto::peer::OfferedShare {
                    id: "ab".repeat(16),
                    name: "Photos".into(),
                }],
            },
            PeerCall::Folder { .. } => PeerReply::Refused {
                reason: "No folders here".into(),
            },
        };
        let _ = answer.reply(&reply);
    }
}

async fn pending_on(link: &PeerLink, device: DeviceId) -> PendingPairing {
    for _ in 0..1_000 {
        if let Some(pending) = link.pending().into_iter().find(|p| p.device == device) {
            return pending;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pairing never reached this Mac");
}

/// Wait until `link` trusts `device`. A pairing is recorded just before
/// it is pinned, so the record alone does not prove the pin.
async fn pinned(link: &PeerLink, device: DeviceId) {
    for _ in 0..1_000 {
        if link.is_paired(device) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pairing was never pinned");
}

async fn settled(link: &PeerLink) {
    for _ in 0..1_000 {
        if link.pending().is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pairing never settled");
}

#[tokio::test(flavor = "multi_thread")]
async fn both_confirmations_pin_both_macs_and_one_pins_nothing() {
    let (a, mut a_paired) = link("Laptop");
    let (b, mut b_paired) = link("Studio");
    let window = b.open_pairing(Duration::from_secs(60)).unwrap();

    let shown = tokio::time::timeout(WAIT, a.pair(b.device(), Some(window)))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(shown.name, "Studio");
    let other = pending_on(&b, a.device()).await;
    assert_eq!((other.code, other.name.as_str()), (shown.code, "Laptop"));

    a.decide(b.device(), Decision::Confirm).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!a.is_paired(b.device()) && !b.is_paired(a.device()));

    b.decide(a.device(), Decision::Confirm).unwrap();
    let recorded = tokio::time::timeout(WAIT, a_paired.recv()).await.unwrap();
    assert_eq!(recorded, Some((b.device(), "Studio".into())));
    let recorded = tokio::time::timeout(WAIT, b_paired.recv()).await.unwrap();
    assert_eq!(recorded, Some((a.device(), "Laptop".into())));
    pinned(&a, b.device()).await;
    pinned(&b, a.device()).await;
    assert!(a.pending().is_empty() && b.pending().is_empty());

    // Paired Macs answer each other's requests.
    a.reach(b.device(), b.address().unwrap());
    let reply = tokio::time::timeout(WAIT, a.request(b.device(), PeerCall::Shares))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(reply, PeerReply::Shares { shares } if shares[0].name == "Photos"));

    a.revoke(b.device());
    assert!(!a.is_paired(b.device()));
    assert!(matches!(
        a.request(b.device(), PeerCall::Shares).await,
        Err(LinkError::NotPaired)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_on_either_side_pins_nothing() {
    for cancel_on_dialer in [true, false] {
        let (a, mut a_paired) = link("Laptop");
        let (b, mut b_paired) = link("Studio");
        let window = b.open_pairing(Duration::from_secs(60)).unwrap();
        tokio::time::timeout(WAIT, a.pair(b.device(), Some(window)))
            .await
            .unwrap()
            .unwrap();
        pending_on(&b, a.device()).await;

        let (cancelling, device, confirming, other) = if cancel_on_dialer {
            (&a, b.device(), &b, a.device())
        } else {
            (&b, a.device(), &a, b.device())
        };
        confirming.decide(other, Decision::Confirm).unwrap();
        cancelling.decide(device, Decision::Cancel).unwrap();
        settled(&a).await;
        settled(&b).await;
        assert!(!a.is_paired(b.device()) && !b.is_paired(a.device()));
        assert!(a_paired.try_recv().is_err() && b_paired.try_recv().is_err());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mac_without_an_open_window_refuses_to_pair() {
    let (a, _) = link("Laptop");
    let (b, _) = link("Studio");
    // B's paired-only node takes no unpaired device.
    let result = tokio::time::timeout(WAIT, a.pair(b.device(), Some(b.address().unwrap())))
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(b.pending().is_empty());
    assert!(matches!(
        a.decide(b.device(), Decision::Confirm),
        Err(LinkError::NoSuchPairing)
    ));

    // Unlisted Macs see nobody, so pairing needs an address.
    assert_eq!(a.nearby().unwrap(), []);
    assert!(matches!(
        a.pair(b.device(), None).await,
        Err(LinkError::NotNearby)
    ));
}

/// Live multicast on this host; set FOLDOVER_MDNS_TESTS=1 to run it.
#[tokio::test(flavor = "multi_thread")]
async fn a_mac_open_for_pairing_is_found_on_the_network_and_paired_without_an_address() {
    if std::env::var_os("FOLDOVER_MDNS_TESTS").is_none() {
        return;
    }
    let everywhere = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
    let (a, _) = link_at("Laptop", everywhere, Presence::Announced, Store::Working);
    let (b, _) = link_at("Studio", everywhere, Presence::Announced, Store::Working);
    b.open_pairing(Duration::from_secs(60)).unwrap();

    let mut found = None;
    for _ in 0..400 {
        found = a
            .nearby()
            .unwrap()
            .into_iter()
            .find(|nearby| nearby.device == b.device() && nearby.pairing.is_some());
        if found.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let found = found.expect("B never appeared open for pairing");
    assert_eq!(found.name, "Studio");
    assert_eq!(found.address.port(), b.address().unwrap().port());

    let shown = tokio::time::timeout(WAIT, a.pair(b.device(), None))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending_on(&b, a.device()).await.code, shown.code);
}

/// Review: closing a window closed its node, and with it the connections
/// of codes still on screen.
#[tokio::test(flavor = "multi_thread")]
async fn a_code_shown_before_the_window_closed_can_still_be_confirmed() {
    let (a, mut a_paired) = link("Laptop");
    let (b, mut b_paired) = link("Studio");
    let window = b.open_pairing(Duration::from_secs(60)).unwrap();
    tokio::time::timeout(WAIT, a.pair(b.device(), Some(window)))
        .await
        .unwrap()
        .unwrap();
    pending_on(&b, a.device()).await;

    b.close_pairing();
    assert!(b.pairing_address().is_none());
    a.decide(b.device(), Decision::Confirm).unwrap();
    b.decide(a.device(), Decision::Confirm).unwrap();
    tokio::time::timeout(WAIT, a_paired.recv()).await.unwrap();
    tokio::time::timeout(WAIT, b_paired.recv()).await.unwrap();
    pinned(&a, b.device()).await;
    pinned(&b, a.device()).await;
}

/// Review: a device the store failed to record was trusted until the
/// agent restarted, then silently refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_could_not_be_recorded_is_not_trusted() {
    let (a, mut a_paired) = link("Laptop");
    let (b, mut b_paired) = link_with("Studio", Store::Failing);
    let window = b.open_pairing(Duration::from_secs(60)).unwrap();
    tokio::time::timeout(WAIT, a.pair(b.device(), Some(window)))
        .await
        .unwrap()
        .unwrap();
    pending_on(&b, a.device()).await;

    a.decide(b.device(), Decision::Confirm).unwrap();
    b.decide(a.device(), Decision::Confirm).unwrap();
    tokio::time::timeout(WAIT, a_paired.recv()).await.unwrap();
    tokio::time::timeout(WAIT, b_paired.recv()).await.unwrap();
    settled(&b).await;
    pinned(&a, b.device()).await;
    // B decided not to pin once its record failed; nothing pins it later.
    assert!(!b.is_paired(a.device()));
}
