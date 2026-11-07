use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use foldover_types::ScanControl;
use tokio::sync::mpsc;

use super::*;

const WAIT: Duration = Duration::from_secs(10);
// Preserve a generous gap between heartbeat cadence and expiry on busy CI.
const HEARTBEAT_INACTIVITY: Duration = Duration::from_secs(2);
const HEARTBEAT_INTERVAL: Duration = Duration::from_millis(60);

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

/// Answers waiting for data that never comes; says when one starts.
struct Waiting(std::sync::Mutex<std::sync::mpsc::Sender<()>>);

impl PeerHost for Waiting {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, _: PeerCall, answer: &mut Answer) {
        let _ = self.0.lock().unwrap().send(());
        let _ = answer.next_data();
    }
}

/// Leaves its first call unanswered until the asker retires that stream;
/// later calls prove the connection itself remains usable.
struct FirstCallWaits {
    calls: AtomicUsize,
    started: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
}

/// Keeps a request alive beyond one inactivity period, then really answers.
struct HeartbeatsThenDone;

impl PeerHost for HeartbeatsThenDone {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, _: PeerCall, answer: &mut Answer) {
        let started = std::time::Instant::now();
        while started.elapsed() < HEARTBEAT_INACTIVITY + Duration::from_millis(500) {
            answer.heartbeat().unwrap();
            std::thread::sleep(HEARTBEAT_INTERVAL);
        }
        answer.reply(&PeerReply::Done).unwrap();
    }
}

/// Reads data around client heartbeats and reports exactly what arrived.
struct ReadsPastHeartbeats(std::sync::Mutex<std::sync::mpsc::Sender<(Vec<u8>, bool)>>);

impl PeerHost for ReadsPastHeartbeats {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, _: PeerCall, answer: &mut Answer) {
        let data = answer.next_data().unwrap().unwrap();
        let ended = answer.next_data().unwrap().is_none();
        let _ = self.0.lock().unwrap().send((data, ended));
        answer.reply(&PeerReply::Done).unwrap();
    }
}

/// Emits heartbeats until the test requests silence, then keeps the
/// stream open until the test has observed inactivity rather than closure.
struct WatchHeartbeatsThenSilence {
    beats: std::sync::Mutex<std::sync::mpsc::Sender<Result<usize, LinkError>>>,
    quiet: AtomicBool,
    quiet_started: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
    released: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl PeerHost for WatchHeartbeatsThenSilence {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, _: PeerCall, answer: &mut Answer) {
        let mut index = 0;
        while !self.quiet.load(Ordering::Relaxed) {
            let heartbeat = answer.heartbeat();
            let failed = heartbeat.is_err();
            let _ = self.beats.lock().unwrap().send(heartbeat.map(|()| index));
            if failed {
                return;
            }
            index += 1;
            std::thread::sleep(HEARTBEAT_INTERVAL);
        }
        let _ = self.quiet_started.lock().unwrap().send(());
        // Dropping the sender also releases this wait if an assertion fails.
        let _ = self.released.lock().unwrap().recv_timeout(WAIT);
    }
}

impl PeerHost for FirstCallWaits {
    fn paired(&self, _: DeviceId, _: String) -> Result<(), NotRecorded> {
        Ok(())
    }

    fn serve(&self, _: DeviceId, _: PeerCall, answer: &mut Answer) {
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            let _ = self.started.lock().unwrap().send(());
            let _ = answer.next_data();
            return;
        }
        let _ = answer.reply(&PeerReply::Done);
    }
}

/// A pair of already-trusted links on one runtime, with `host` answering B.
fn trusted_links(
    host: Arc<dyn PeerHost>,
) -> (tokio::runtime::Runtime, Arc<PeerLink>, Arc<PeerLink>) {
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
        let (recorded, _) = mpsc::unbounded_channel();
        let a = PeerLink::start(
            a_identity,
            "Laptop".into(),
            loopback,
            Presence::Unlisted,
            [devices[1]],
            Arc::new(Host {
                recorded,
                store: Store::Working,
            }),
        )
        .unwrap();
        a.reach(devices[1], b.address().unwrap());
        (a, b)
    });
    (runtime, a, b)
}

/// A stopped job must retire only its request stream. Its checkpoint can
/// itself use the peer link; it therefore cannot run inside `Handle::block_on`.
#[test]
fn a_stalled_stream_stops_and_its_checkpoint_can_use_the_link() {
    let (started_tx, started) = std::sync::mpsc::channel();
    let host = Arc::new(FirstCallWaits {
        calls: AtomicUsize::new(0),
        started: std::sync::Mutex::new(started_tx),
    });
    let (runtime, asker, answerer) = trusted_links(host);
    let device = answerer.device();
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_link = Arc::clone(&asker);
    let worker_stop = Arc::clone(&stopped);
    let (done_tx, done) = std::sync::mpsc::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut exchange = worker_link.ask(device, PeerCall::Shares).unwrap();
        release.recv_timeout(WAIT).unwrap();
        let mut checked_link = false;
        let result = exchange.reply_unless(&mut || {
            if !checked_link {
                checked_link = true;
                let mut check = worker_link.ask(device, PeerCall::Shares).unwrap();
                assert_eq!(check.reply().unwrap(), PeerReply::Done);
            }
            if worker_stop.load(Ordering::Relaxed) {
                ScanControl::Cancel
            } else {
                ScanControl::Continue
            }
        });
        let _ = done_tx.send(result);
    });

    started.recv_timeout(WAIT).unwrap();
    release_tx.send(()).unwrap();
    stopped.store(true, Ordering::Relaxed);
    assert!(matches!(
        done.recv_timeout(Duration::from_secs(2)),
        Ok(Err(LinkError::Cancelled))
    ));
    worker.join().unwrap();

    // Cancelling one stream does not discard the healthy connection.
    assert_eq!(
        runtime
            .block_on(asker.request(device, PeerCall::Shares))
            .unwrap(),
        PeerReply::Done
    );
    drop(answerer);
    drop(asker);
    drop(runtime);
}

#[test]
fn an_inactive_stream_times_out_without_poisoning_its_connection() {
    let (started_tx, started) = std::sync::mpsc::channel();
    let host = Arc::new(FirstCallWaits {
        calls: AtomicUsize::new(0),
        started: std::sync::Mutex::new(started_tx),
    });
    let (runtime, asker, answerer) = trusted_links(host);
    let device = answerer.device();
    let mut exchange = asker
        .ask_unless_with_inactivity(
            device,
            PeerCall::Shares,
            Duration::from_millis(150),
            &mut || ScanControl::Continue,
        )
        .unwrap();
    started.recv_timeout(WAIT).unwrap();

    assert!(matches!(exchange.reply(), Err(LinkError::TimedOut)));
    assert!(matches!(exchange.reply(), Err(LinkError::Closed)));
    assert_eq!(
        runtime
            .block_on(asker.request(device, PeerCall::Shares))
            .unwrap(),
        PeerReply::Done
    );
}

#[test]
fn transport_heartbeats_reset_the_frame_inactivity_deadline() {
    let (runtime, asker, answerer) = trusted_links(Arc::new(HeartbeatsThenDone));
    let mut exchange = asker
        .ask_unless_with_inactivity(
            answerer.device(),
            PeerCall::Shares,
            HEARTBEAT_INACTIVITY,
            &mut || ScanControl::Continue,
        )
        .unwrap();

    assert_eq!(exchange.reply().unwrap(), PeerReply::Done);
    drop(answerer);
    drop(asker);
    drop(runtime);
}

#[test]
fn an_answer_reads_data_and_end_past_client_heartbeats() {
    let (read_tx, read) = std::sync::mpsc::channel();
    let (runtime, asker, answerer) = trusted_links(Arc::new(ReadsPastHeartbeats(
        std::sync::Mutex::new(read_tx),
    )));
    let mut exchange = asker.ask(answerer.device(), PeerCall::Shares).unwrap();

    exchange
        .heartbeat_unless(&mut || ScanControl::Continue)
        .unwrap();
    exchange.send_data(b"payload").unwrap();
    exchange
        .heartbeat_unless(&mut || ScanControl::Continue)
        .unwrap();
    exchange.end_data().unwrap();
    assert_eq!(exchange.reply().unwrap(), PeerReply::Done);
    assert_eq!(
        read.recv_timeout(WAIT).unwrap(),
        (b"payload".to_vec(), true)
    );
    drop(answerer);
    drop(asker);
    drop(runtime);
}

#[test]
fn a_watch_stays_live_on_heartbeats_then_reports_silence_as_lost() {
    let (beat_tx, beats) = std::sync::mpsc::channel();
    let (quiet_tx, quiet_started) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let host = Arc::new(WatchHeartbeatsThenSilence {
        beats: std::sync::Mutex::new(beat_tx),
        quiet: AtomicBool::new(false),
        quiet_started: std::sync::Mutex::new(quiet_tx),
        released: std::sync::Mutex::new(released),
    });
    let (runtime, asker, answerer) = trusted_links(host.clone());
    let (report_tx, reports) = std::sync::mpsc::channel();
    // A busy runner can stall longer than the old 200 ms test budget.
    // Production keeps its 120 s budget; this still tests expiry promptly.
    let inactivity = HEARTBEAT_INACTIVITY;
    let handle = asker.watch_with_timing(
        answerer.device(),
        PeerCall::Shares,
        move |report| {
            let _ = report_tx.send(report);
        },
        inactivity,
        Duration::from_secs(10),
    );

    beats.recv_timeout(WAIT).unwrap().unwrap();
    let started = std::time::Instant::now();
    // Keep receiving beyond an entire inactivity period. Ignoring heartbeat
    // frames would lose the watch before the peer is told to go quiet.
    while started.elapsed() < inactivity + Duration::from_millis(500) {
        beats.recv_timeout(WAIT).unwrap().unwrap();
    }
    assert!(reports.try_recv().is_err());
    host.quiet.store(true, Ordering::Relaxed);
    quiet_started.recv_timeout(WAIT).unwrap();
    assert_eq!(reports.recv_timeout(WAIT), Ok(Watched::Lost));
    drop(handle);
    drop(release);
    drop(answerer);
    drop(asker);
    drop(runtime);
}

/// Review: an answer waiting on its stream as its link went away kept
/// waiting on a runtime that was stopping, so the runtime never stopped.
#[test]
fn a_link_going_away_ends_answers_that_wait() {
    for _ in 0..20 {
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
        let (started_tx, started) = std::sync::mpsc::channel();
        let (asker, answerer) = runtime.block_on(async {
            let b = PeerLink::start(
                b_identity,
                "Studio".into(),
                loopback,
                Presence::Unlisted,
                [devices[0]],
                Arc::new(Waiting(std::sync::Mutex::new(started_tx))),
            )
            .unwrap();
            let (recorded, _) = mpsc::unbounded_channel();
            let a = PeerLink::start(
                a_identity,
                "Laptop".into(),
                loopback,
                Presence::Unlisted,
                [devices[1]],
                Arc::new(Host {
                    recorded,
                    store: Store::Working,
                }),
            )
            .unwrap();
            a.reach(devices[1], b.address().unwrap());
            (a, b)
        });
        let exchange = asker.ask(devices[1], PeerCall::Shares).unwrap();
        started.recv_timeout(WAIT).unwrap();

        let (stopped_tx, stopped) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(answerer);
            drop(runtime);
            let _ = stopped_tx.send(());
        });
        assert_eq!(
            stopped.recv_timeout(WAIT),
            Ok(()),
            "the runtime never stopped"
        );
        drop(exchange);
        drop(asker);
    }
}
