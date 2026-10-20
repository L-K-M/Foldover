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
    let (recorded, paired) = mpsc::unbounded_channel();
    let link = PeerLink::start(
        Identity::generate().unwrap(),
        name.into(),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        [],
        move |device, name| {
            let _ = recorded.send((device, name));
            match store {
                Store::Working => Ok(()),
                Store::Failing => Err(()),
            }
        },
    )
    .unwrap();
    (link, paired)
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

    let shown = tokio::time::timeout(WAIT, a.pair(b.device(), window))
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
    assert!(a.is_paired(b.device()) && b.is_paired(a.device()));
    assert!(a.pending().is_empty() && b.pending().is_empty());

    a.revoke(b.device());
    assert!(!a.is_paired(b.device()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancel_on_either_side_pins_nothing() {
    for cancel_on_dialer in [true, false] {
        let (a, mut a_paired) = link("Laptop");
        let (b, mut b_paired) = link("Studio");
        let window = b.open_pairing(Duration::from_secs(60)).unwrap();
        tokio::time::timeout(WAIT, a.pair(b.device(), window))
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
    let result = tokio::time::timeout(WAIT, a.pair(b.device(), b.address().unwrap()))
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(b.pending().is_empty());
    assert!(matches!(
        a.decide(b.device(), Decision::Confirm),
        Err(LinkError::NoSuchPairing)
    ));
}

/// Review: closing a window closed its node, and with it the connections
/// of codes still on screen.
#[tokio::test(flavor = "multi_thread")]
async fn a_code_shown_before_the_window_closed_can_still_be_confirmed() {
    let (a, mut a_paired) = link("Laptop");
    let (b, mut b_paired) = link("Studio");
    let window = b.open_pairing(Duration::from_secs(60)).unwrap();
    tokio::time::timeout(WAIT, a.pair(b.device(), window))
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
    assert!(a.is_paired(b.device()) && b.is_paired(a.device()));
}

/// Review: a device the store failed to record was trusted until the
/// agent restarted, then silently refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_device_that_could_not_be_recorded_is_not_trusted() {
    let (a, mut a_paired) = link("Laptop");
    let (b, mut b_paired) = link_with("Studio", Store::Failing);
    let window = b.open_pairing(Duration::from_secs(60)).unwrap();
    tokio::time::timeout(WAIT, a.pair(b.device(), window))
        .await
        .unwrap()
        .unwrap();
    pending_on(&b, a.device()).await;

    a.decide(b.device(), Decision::Confirm).unwrap();
    b.decide(a.device(), Decision::Confirm).unwrap();
    tokio::time::timeout(WAIT, a_paired.recv()).await.unwrap();
    tokio::time::timeout(WAIT, b_paired.recv()).await.unwrap();
    settled(&b).await;
    assert!(a.is_paired(b.device()));
    assert!(!b.is_paired(a.device()));
}
