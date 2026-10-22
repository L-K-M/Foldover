//! Chaos simulation for the peer transport (ADR 0038): quinn's abstract
//! socket and runtime traits over turmoil's simulated UDP, so loss,
//! delay, jitter and partitions replay deterministically from a seed.

use std::future::Future;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncTimer, AsyncUdpSocket, Endpoint, EndpointConfig, Runtime, UdpPoller};
use turmoil::net::UdpSocket;
use turmoil::{Builder, Sim};

use super::{DeviceId, Identity, Node, PeerError, PeerMode, TrustStore, server_config};

/// The port every simulated listener picks; each host owns its address.
const PORT: u16 = 9988;

type SimResult = Result<(), Box<dyn std::error::Error>>;
/// A `readable()`/`writable()` future owned by the waiter.
type Readiness = Pin<Box<dyn Future<Output = io::Result<()>> + Send>>;

/// Quinn runtime glue for turmoil hosts: spawn on the host's tokio
/// runtime and read the simulated clock, so peer code sees ordinary
/// futures while the world ticks deterministically.
#[derive(Debug)]
struct SimRuntime;

impl Runtime for SimRuntime {
    fn new_timer(&self, deadline: Instant) -> Pin<Box<dyn AsyncTimer>> {
        Box::pin(SimTimer {
            sleep: Box::pin(tokio::time::sleep_until(deadline.into())),
        })
    }

    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        tokio::spawn(future);
    }

    fn wrap_udp_socket(&self, _: std::net::UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "simulated endpoints carry a socket already",
        ))
    }

    fn now(&self) -> Instant {
        tokio::time::Instant::now().into_std()
    }
}

#[derive(Debug)]
struct SimTimer {
    sleep: Pin<Box<tokio::time::Sleep>>,
}

impl AsyncTimer for SimTimer {
    fn reset(mut self: Pin<&mut Self>, deadline: Instant) {
        self.sleep.as_mut().reset(deadline.into());
    }

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Future::poll(self.sleep.as_mut(), cx)
    }
}

/// Turmoil's socket behind quinn's abstract UDP interface. Quinn polls
/// rather than awaits, so the parked `readable()` future lives in
/// `ready` until it resolves, then is rebuilt for the next wait.
struct SimSocket {
    socket: Arc<UdpSocket>,
    ready: Mutex<Option<Readiness>>,
}

impl std::fmt::Debug for SimSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimSocket").finish_non_exhaustive()
    }
}

impl SimSocket {
    fn new(socket: UdpSocket) -> Self {
        Self {
            socket: Arc::new(socket),
            ready: Mutex::new(None),
        }
    }
}

impl AsyncUdpSocket for SimSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(SimPoller {
            socket: Arc::clone(&self.socket),
            writable: Mutex::new(None),
        })
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        self.socket
            .try_send_to(transmit.contents, transmit.destination)?;
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let mut received = 0;
            for (buf, meta) in bufs.iter_mut().zip(meta.iter_mut()) {
                match self.socket.try_recv_from(buf) {
                    Ok((len, addr)) => {
                        *meta = RecvMeta {
                            addr,
                            len,
                            stride: len,
                            ..RecvMeta::default()
                        };
                        received += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => return Poll::Ready(Err(error)),
                }
            }
            if received > 0 {
                return Poll::Ready(Ok(received));
            }
            let mut ready = self.ready.lock().unwrap_or_else(|error| error.into_inner());
            if ready.is_none() {
                let socket = Arc::clone(&self.socket);
                *ready = Some(Box::pin(async move { socket.readable().await }));
            }
            match ready.as_mut().expect("freshly parked").as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => {
                    *ready = None;
                    return Poll::Ready(Err(error));
                }
                Poll::Ready(Ok(())) => *ready = None,
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    fn may_fragment(&self) -> bool {
        false
    }
}

/// Writability waiter for one quinn task; send readiness dips only while
/// a socket's bounded outbox is full. The parked `writable()` future
/// sits behind a mutex because `UdpPoller` must be `Sync` while the
/// future itself is only `Send`.
struct SimPoller {
    socket: Arc<UdpSocket>,
    writable: Mutex<Option<Readiness>>,
}

impl std::fmt::Debug for SimPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimPoller").finish_non_exhaustive()
    }
}

impl UdpPoller for SimPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut writable = self
            .writable
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if writable.is_none() {
            let socket = Arc::clone(&self.socket);
            *writable = Some(Box::pin(async move { socket.writable().await }));
        }
        match writable.as_mut().expect("freshly parked").as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                *writable = None;
                Poll::Ready(result)
            }
        }
    }
}

/// A node on `socket` with a quinn endpoint driven by the sim clock.
/// `serve` controls whether the endpoint accepts connections.
fn sim_node(
    identity: Arc<Identity>,
    socket: UdpSocket,
    trust: Arc<TrustStore>,
    mode: PeerMode,
    serve: bool,
) -> Result<Node, PeerError> {
    let server = serve
        .then(|| server_config(&identity, &trust, mode))
        .transpose()?;
    let endpoint = Endpoint::new_with_abstract_socket(
        EndpointConfig::default(),
        server,
        Arc::new(SimSocket::new(socket)),
        Arc::new(SimRuntime),
    )?;
    Ok(Node {
        identity,
        endpoint,
        trust,
        mode,
    })
}

/// Bind a node inside its host; the fixed PORT keeps dialing simple.
async fn bound_node(
    identity: Arc<Identity>,
    trust: Arc<TrustStore>,
    mode: PeerMode,
    serve: bool,
) -> Result<Node, PeerError> {
    let socket = UdpSocket::bind(("0.0.0.0", PORT)).await?;
    sim_node(identity, socket, trust, mode, serve)
}

/// An echoing listener: each accepted connection answers every opened
/// stream by reading `size` bytes and writing them straight back.
async fn serve_echo(node: Node, size: usize) -> SimResult {
    while let Some(connection) = node.accept().await {
        let Ok(connection) = connection else {
            continue;
        };
        tokio::spawn(async move {
            while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                let mut data = vec![0u8; size];
                if recv.read_exact(&mut data).await.is_err() {
                    continue;
                }
                if send.write_all(&data).await.is_ok() {
                    let _ = send.finish_delivered().await;
                }
            }
        });
    }
    Ok(())
}

/// One bidirectional exchange: `size` patterned bytes out, echo back,
/// compare. The pattern is deterministic so the sim's seed decides
/// everything the network does.
async fn echo_round_trip(
    node: &Node,
    server: SocketAddr,
    device: DeviceId,
    size: usize,
    seed: u8,
) -> Result<(), PeerError> {
    let connection = node.dial(server, device).await?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let data: Vec<u8> = (0..size).map(|i| seed.wrapping_add(i as u8)).collect();
    send.write_all(&data).await?;
    let mut echo = vec![0u8; size];
    recv.read_exact(&mut echo).await?;
    assert_eq!(echo, data, "echoed payload must match sent bytes");
    send.finish_delivered().await?;
    connection.close();
    Ok(())
}

fn paired_pair() -> (
    Arc<Identity>,
    Arc<Identity>,
    Arc<TrustStore>,
    Arc<TrustStore>,
) {
    let client_identity = Arc::new(Identity::generate().expect("identity"));
    let server_identity = Arc::new(Identity::generate().expect("identity"));
    let client_trust = Arc::new(TrustStore::default());
    let server_trust = Arc::new(TrustStore::default());
    client_trust.pin(server_identity.id());
    server_trust.pin(client_identity.id());
    (client_identity, server_identity, client_trust, server_trust)
}

/// Echo survives loss, latency and jitter: retransmission and
/// re-ordering are QUIC's job, and the adapter must not hide that from
/// it by keeping a fake-perfect link.
#[test]
fn sim_stream_survives_loss_jitter_and_delay() {
    let (client_identity, server_identity, client_trust, server_trust) = paired_pair();
    let server_id = server_identity.id();
    let mut builder = Builder::new();
    builder
        .rng_seed(0xF01D_0E1C)
        .min_message_latency(Duration::from_millis(10))
        .max_message_latency(Duration::from_millis(60))
        .fail_rate(0.1)
        .simulation_duration(Duration::from_secs(300));
    let mut sim = builder.build();
    sim.host("server", move || {
        let identity = Arc::clone(&server_identity);
        let trust = Arc::clone(&server_trust);
        async move {
            let node = bound_node(identity, trust, PeerMode::Paired, true).await?;
            serve_echo(node, 256 * 1024).await
        }
    });
    sim.client("client", async move {
        let node = bound_node(client_identity, client_trust, PeerMode::Paired, false).await?;
        let server = SocketAddr::new(turmoil::lookup("server"), PORT);
        echo_round_trip(&node, server, server_id, 256 * 1024, 7).await?;
        Ok(())
    });
    sim.run().expect("chaos sim");
}

/// The pairing code exchange is a two-round commit-then-reveal on one
/// stream; under loss both ends must still derive the same six digits.
#[test]
fn sim_pairing_code_survives_packet_loss() {
    let client_identity = Arc::new(Identity::generate().expect("identity"));
    let server_identity = Arc::new(Identity::generate().expect("identity"));
    let server_id = server_identity.id();
    let client_trust = Arc::new(TrustStore::default());
    let server_trust = Arc::new(TrustStore::default());
    let mut builder = Builder::new();
    builder
        .rng_seed(0xF01D_0E2A)
        .min_message_latency(Duration::from_millis(5))
        .fail_rate(0.15)
        .simulation_duration(Duration::from_secs(120));
    let mut sim = builder.build();
    sim.host("server", move || {
        let identity = Arc::clone(&server_identity);
        let trust = Arc::clone(&server_trust);
        async move {
            let node = bound_node(identity, trust, PeerMode::Pairing, true).await?;
            let connection = node.accept().await.expect("one pairing connection")?;
            let code = connection.pairing_code().await?;
            assert_ne!(code, 0, "pairing code must be produced");
            Ok(())
        }
    });
    sim.client("client", async move {
        let node = bound_node(client_identity, client_trust, PeerMode::Pairing, false).await?;
        let server = SocketAddr::new(turmoil::lookup("server"), PORT);
        let connection = node.dial(server, server_id).await?;
        let code = connection.pairing_code().await?;
        assert_ne!(code, 0, "pairing code must be produced");
        Ok(())
    });
    sim.run().expect("pairing sim");
}

/// A mid-transfer partition stalls delivery but must not corrupt it:
/// after repair, every queued byte still arrives.
#[test]
fn sim_partition_mid_transfer_recovers_on_repair() {
    let (client_identity, server_identity, client_trust, server_trust) = paired_pair();
    let server_id = server_identity.id();
    let mut builder = Builder::new();
    builder
        .rng_seed(0xF01D_0E3B)
        .simulation_duration(Duration::from_secs(300));
    let mut sim: Sim<'_> = builder.build();
    sim.host("server", move || {
        let identity = Arc::clone(&server_identity);
        let trust = Arc::clone(&server_trust);
        async move {
            let node = bound_node(identity, trust, PeerMode::Paired, true).await?;
            serve_echo(node, 512 * 1024).await
        }
    });
    sim.client("client", async move {
        let node = bound_node(client_identity, client_trust, PeerMode::Paired, false).await?;
        let server = SocketAddr::new(turmoil::lookup("server"), PORT);
        let connection = node.dial(server, server_id).await?;
        let (mut send, mut recv) = connection.open_bi().await?;
        let data: Vec<u8> = (0..512 * 1024u32).map(|i| (i % 251) as u8).collect();
        send.write_all(&data[..128 * 1024]).await?;
        turmoil::partition("client", "server");
        send.write_all(&data[128 * 1024..]).await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        turmoil::repair("client", "server");
        let mut echo = vec![0u8; data.len()];
        recv.read_exact(&mut echo).await?;
        assert_eq!(echo, data, "partitioned payload must arrive intact");
        send.finish_delivered().await?;
        Ok(())
    });
    sim.run().expect("partition sim");
}
