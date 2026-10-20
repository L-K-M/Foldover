//! This Mac's presence for other Macs (ADR 0031): a node that accepts only
//! paired devices, and a pairing node that listens only while a person has
//! a pairing window open. Pairing shows both Macs one six-digit code; each
//! person confirms, and only when both have does either Mac pin the other.
//!
//! ```text
//! Mac A                                  Mac B (pairing window open)
//! pair(B, address) ─ QUIC, pairing mode ─▶ accept
//!   code 123456  ◀── commit, then reveal ──▶ code 123456
//!   hello{name}  ◀──────────────────────────▶ hello{name}
//!   confirm ────────────────────────────────▶ (person on B confirms)
//!                ◀──────────────────────────── confirm
//!   pin B, record it                         pin A, record it
//! ```

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use foldover_net::discovery::{Advert, Discovery};
use foldover_net::peer::{
    DeviceId as NetDeviceId, Identity, Node, PeerConnection, PeerError, PeerMode, PeerRecvStream,
    PeerSendStream, TrustStore, read_message, write_message,
};
use foldover_proto::peer::{PEER_PROTOCOL, PairingMessage, PeerCall, PeerReply, PeerRequest};
use foldover_types::DeviceId;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

mod stream;
pub use stream::{Answer, Exchange, Incoming};

/// How long a broken watch waits before asking again.
const WATCH_RETRY: Duration = Duration::from_secs(30);

/// How long a person has to confirm a code once both Macs show it.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long a final pairing message may take to reach the other Mac.
/// Longest wait for a paired Mac to answer a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest name a peer may announce, in bytes.
const MAX_NAME_BYTES: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error(transparent)]
    Peer(#[from] PeerError),
    #[error("That Mac is not waiting for this pairing")]
    NoSuchPairing,
    #[error("That Mac runs another Foldover version; update both Macs and pair again")]
    Protocol,
    #[error("This Mac's identity for other Macs is unavailable: {0}")]
    Identity(String),
    #[error(
        "That Mac is not open for pairing on this network; open pairing on it, or give its address"
    )]
    NotNearby,
    #[error("{0}")]
    Discovery(String),
    #[error("That Mac is not paired with this one")]
    NotPaired,
    #[error("That Mac is not on this network; check that it is awake and runs Foldover")]
    Unreachable,
}

/// Whether this Mac announces itself to other Macs on the local network.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presence {
    /// Advertise and browse with multicast DNS.
    Announced,
    /// Reachable only at an address given by hand, e.g. in tests.
    Unlisted,
}

/// Another Mac running Foldover on the local network.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NearbyMac {
    pub device: DeviceId,
    /// The name it announced, e.g. "Studio iMac".
    pub name: String,
    /// Where paired Macs reach it.
    pub address: SocketAddr,
    /// Where it accepts pairing, while a window is open.
    pub pairing: Option<SocketAddr>,
}

/// What a person must compare on both Macs before confirming.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingPairing {
    pub device: DeviceId,
    /// The name the other Mac announced, e.g. "Studio iMac".
    pub name: String,
    pub code: u32,
}

/// A person's answer to a pairing code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Confirm,
    Cancel,
}

/// The agent could not record a device both people confirmed.
#[derive(Debug)]
pub struct NotRecorded;

/// What this Mac's agent does for the link: record pairings and answer
/// paired Macs' requests. Requests are answered off the async runtime's
/// workers, so `serve` may block.
pub trait PeerHost: Send + Sync + 'static {
    /// Both people confirmed `device`'s pairing code. A device that could
    /// not be recorded is not trusted, as it would be forgotten at the
    /// next start.
    fn paired(&self, device: DeviceId, name: String) -> Result<(), NotRecorded>;

    /// Answer a request from paired `device`: at least one reply, and any
    /// data the call moves.
    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer);
}

pub struct PeerLink {
    identity: Arc<Identity>,
    name: String,
    trust: Arc<TrustStore>,
    node: Arc<Node>,
    neighbourhood: Neighbourhood,
    state: Mutex<State>,
    host: Arc<dyn PeerHost>,
    /// Where blocking callers run their streams.
    runtime: Handle,
    /// Set as the link goes away, so answers that wait on their own, such
    /// as folder watches, stop before the runtime does.
    closing: Arc<AtomicBool>,
}

/// How this Mac finds, and is found by, others on the network.
enum Neighbourhood {
    Unlisted,
    Browsing(Discovery),
    /// Multicast DNS failed to start; pairing by address still works.
    Unavailable(String),
}

#[derive(Default)]
struct State {
    window: Option<Window>,
    /// Bumped per window, so a late expiry never closes a newer one.
    windows: u64,
    pending: BTreeMap<DeviceId, Pending>,
    /// Nodes of closed windows, kept until the codes they showed settle.
    draining: Vec<Arc<Node>>,
    accept: Option<JoinHandle<()>>,
    /// Open connections to paired devices, reused across requests.
    connections: BTreeMap<DeviceId, PeerConnection>,
    /// Where to reach devices the network does not advertise.
    addresses: BTreeMap<DeviceId, SocketAddr>,
}

struct Window {
    generation: u64,
    node: Arc<Node>,
    tasks: Vec<JoinHandle<()>>,
}

struct Pending {
    name: String,
    code: u32,
    decide: Option<oneshot::Sender<Decision>>,
}

impl PeerLink {
    /// Listen for paired devices on `address`; port 0 picks a free one.
    /// `paired` are the devices recorded earlier.
    pub fn start(
        identity: Identity,
        name: String,
        address: SocketAddr,
        presence: Presence,
        paired: impl IntoIterator<Item = DeviceId>,
        host: Arc<dyn PeerHost>,
    ) -> Result<Arc<Self>, LinkError> {
        let identity = Arc::new(identity);
        let trust = Arc::new(TrustStore::default());
        for device in paired {
            trust.pin(net_device(device));
        }
        let node = Arc::new(Node::bind(
            Arc::clone(&identity),
            address,
            Arc::clone(&trust),
            PeerMode::Paired,
        )?);
        let neighbourhood = match presence {
            Presence::Unlisted => Neighbourhood::Unlisted,
            Presence::Announced => match Discovery::start(identity.id()) {
                Ok(discovery) => Neighbourhood::Browsing(discovery),
                Err(error) => Neighbourhood::Unavailable(error.to_string()),
            },
        };
        let link = Arc::new(Self {
            identity,
            name,
            trust,
            node: Arc::clone(&node),
            neighbourhood,
            state: Mutex::default(),
            host,
            runtime: Handle::current(),
            closing: Arc::default(),
        });
        link.announce();
        let answering = Arc::downgrade(&link);
        let accept = tokio::spawn(async move {
            while let Some(connection) = node.accept().await {
                if let Ok(connection) = connection {
                    tokio::spawn(answer(answering.clone(), connection));
                }
            }
        });
        link.state().accept = Some(accept);
        Ok(link)
    }

    pub fn device(&self) -> DeviceId {
        DeviceId::from_bytes(*self.identity.id().as_bytes())
    }

    pub fn address(&self) -> Result<SocketAddr, LinkError> {
        Ok(self.node.address()?)
    }

    pub fn is_paired(&self, device: DeviceId) -> bool {
        self.trust.is_pinned(net_device(device))
    }

    /// Other Macs seen on the local network; none when unlisted.
    pub fn nearby(&self) -> Result<Vec<NearbyMac>, LinkError> {
        let discovery = match &self.neighbourhood {
            Neighbourhood::Unlisted => return Ok(Vec::new()),
            Neighbourhood::Unavailable(error) => return Err(LinkError::Discovery(error.clone())),
            Neighbourhood::Browsing(discovery) => discovery,
        };
        Ok(discovery
            .seen()
            .into_iter()
            .filter_map(|seen| {
                Some(NearbyMac {
                    device: DeviceId::from_bytes(*seen.device.as_bytes()),
                    name: seen.name,
                    address: *seen.addresses.first()?,
                    pairing: seen.pairing.first().copied(),
                })
            })
            .collect())
    }

    /// Let other Macs pair with this one for `open_for`; returns where.
    pub fn open_pairing(self: &Arc<Self>, open_for: Duration) -> Result<SocketAddr, LinkError> {
        if let Some(address) = self.pairing_address() {
            return Ok(address);
        }
        let mut bind = self.address()?;
        bind.set_port(0);
        let node = Arc::new(Node::bind(
            Arc::clone(&self.identity),
            bind,
            Arc::clone(&self.trust),
            PeerMode::Pairing,
        )?);
        let address = node.address()?;
        let mut state = self.state();
        // Another caller may have opened one while this node was bound.
        if let Some(existing) = state
            .window
            .as_ref()
            .and_then(|window| window.node.address().ok())
        {
            node.close();
            return Ok(existing);
        }
        let link = Arc::downgrade(self);
        let listening = Arc::clone(&node);
        let accept = tokio::spawn(async move {
            while let Some(connection) = listening.accept().await {
                let (Ok(connection), Some(link)) = (connection, link.upgrade()) else {
                    continue;
                };
                tokio::spawn(async move {
                    let _ = link.handshake(connection, Role::Listener).await;
                });
            }
        });
        state.windows += 1;
        let generation = state.windows;
        let link = Arc::downgrade(self);
        let expiry = tokio::spawn(async move {
            tokio::time::sleep(open_for).await;
            if let Some(link) = link.upgrade() {
                link.close_window(generation);
            }
        });
        state.window = Some(Window {
            generation,
            node,
            tasks: vec![accept, expiry],
        });
        drop(state);
        self.announce();
        Ok(address)
    }

    pub fn pairing_address(&self) -> Option<SocketAddr> {
        self.state()
            .window
            .as_ref()
            .and_then(|window| window.node.address().ok())
    }

    /// Stop accepting new pairings; codes already shown can still settle.
    pub fn close_pairing(&self) {
        let generation = self.state().window.as_ref().map(|window| window.generation);
        if let Some(generation) = generation {
            self.close_window(generation);
        }
    }

    /// Start pairing with `device`, whose pairing window listens at
    /// `address`, or where the network says when None; returns the code
    /// both Macs now show.
    pub async fn pair(
        self: &Arc<Self>,
        device: DeviceId,
        address: Option<SocketAddr>,
    ) -> Result<PendingPairing, LinkError> {
        let address = match address {
            Some(address) => address,
            None => self
                .nearby()?
                .into_iter()
                .find(|nearby| nearby.device == device)
                .and_then(|nearby| nearby.pairing)
                .ok_or(LinkError::NotNearby)?,
        };
        // Dialing an unpaired device needs a node in pairing mode.
        self.open_pairing(CONFIRM_TIMEOUT)?;
        let node = self
            .state()
            .window
            .as_ref()
            .map(|window| Arc::clone(&window.node))
            .ok_or(LinkError::NoSuchPairing)?;
        let connection = node.dial(address, net_device(device)).await?;
        self.handshake(connection, Role::Dialer).await
    }

    /// Codes waiting for this Mac's person.
    pub fn pending(&self) -> Vec<PendingPairing> {
        self.state()
            .pending
            .iter()
            .map(|(device, pending)| PendingPairing {
                device: *device,
                name: pending.name.clone(),
                code: pending.code,
            })
            .collect()
    }

    /// Record this Mac's person's answer for `device`'s code.
    pub fn decide(&self, device: DeviceId, decision: Decision) -> Result<(), LinkError> {
        let sender = self
            .state()
            .pending
            .get_mut(&device)
            .and_then(|pending| pending.decide.take())
            .ok_or(LinkError::NoSuchPairing)?;
        sender.send(decision).map_err(|_| LinkError::NoSuchPairing)
    }

    /// Forget `device` and close its connections, pairing ones included.
    pub fn revoke(&self, device: DeviceId) {
        self.trust.revoke(net_device(device));
        let mut state = self.state();
        state.connections.remove(&device);
        state.addresses.remove(&device);
    }

    /// Reach paired `device` at `address` when the network does not
    /// advertise it, e.g. across subnets.
    pub fn reach(&self, device: DeviceId, address: SocketAddr) {
        self.state().addresses.insert(device, address);
    }

    /// Ask paired `device` something that changes nothing there,
    /// connecting first if needed. A connection that went stale is
    /// replaced once.
    pub async fn request(&self, device: DeviceId, call: PeerCall) -> Result<PeerReply, LinkError> {
        if !self.is_paired(device) {
            return Err(LinkError::NotPaired);
        }
        let request = PeerRequest {
            protocol: PEER_PROTOCOL,
            call,
        };
        let attempt = async || {
            let (mut send, mut recv) = self.open(device, &request).await?;
            send.finish()?;
            stream::receive_json(&mut recv).await
        };
        // A connection that died after the stream opened, as when the
        // other agent restarted, shows only in the reply. Requests here
        // change nothing, so one is sent again on a new connection.
        // A Mac that never answers counts as unreachable, so a caller
        // such as the app's folder picker does not wait forever.
        let reply = match tokio::time::timeout(REQUEST_TIMEOUT, attempt()).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => {
                self.state().connections.remove(&device);
                tokio::time::timeout(REQUEST_TIMEOUT, attempt())
                    .await
                    .map_err(|_| LinkError::Unreachable)??
            }
            Err(_) => {
                // A connection that stalled is not reused.
                self.state().connections.remove(&device);
                return Err(LinkError::Unreachable);
            }
        };
        serde_json::from_slice(&reply).map_err(|_| LinkError::Protocol)
    }

    /// Send `call` to paired `device` on a new stream, for a blocking
    /// caller to read the replies; never call it from the runtime's own
    /// workers.
    pub fn ask(&self, device: DeviceId, call: PeerCall) -> Result<Exchange, LinkError> {
        if !self.is_paired(device) {
            return Err(LinkError::NotPaired);
        }
        let request = PeerRequest {
            protocol: PEER_PROTOCOL,
            call,
        };
        let (send, recv) = self.runtime.block_on(self.open(device, &request))?;
        Ok(Exchange::new(self.runtime.clone(), send, recv))
    }

    /// Keep `call`, a request answered with reports, open to `device`:
    /// `report` runs for each reply, and when the request breaks. A broken
    /// request is asked again after a pause; dropping the handle stops it.
    pub fn watch(
        self: &Arc<Self>,
        device: DeviceId,
        call: PeerCall,
        report: impl Fn(Watched) + Send + Sync + 'static,
    ) -> WatchHandle {
        let link = Arc::downgrade(self);
        let request = PeerRequest {
            protocol: PEER_PROTOCOL,
            call,
        };
        WatchHandle(self.runtime.spawn(async move {
            loop {
                let Some(asking) = link.upgrade() else {
                    return;
                };
                let opened = asking.open(device, &request).await;
                drop(asking);
                if let Ok((mut send, mut recv)) = opened {
                    let _ = send.finish();
                    while let Ok(bytes) = stream::receive_json(&mut recv).await {
                        match serde_json::from_slice(&bytes) {
                            Ok(reply) => report(Watched::Reply(reply)),
                            Err(_) => break,
                        }
                    }
                }
                report(Watched::Lost);
                tokio::time::sleep(WATCH_RETRY).await;
            }
        }))
    }

    /// A stream to `device` carrying `request`. A connection that went
    /// stale is replaced once.
    async fn open(
        &self,
        device: DeviceId,
        request: &PeerRequest,
    ) -> Result<(PeerSendStream, PeerRecvStream), LinkError> {
        let attempt = async |connection: PeerConnection| {
            let (mut send, recv) = connection.open_bi().await?;
            stream::send_json(&mut send, request).await?;
            Ok::<_, LinkError>((send, recv))
        };
        match attempt(self.connection(device).await?).await {
            Ok(streams) => Ok(streams),
            Err(_) => {
                self.state().connections.remove(&device);
                attempt(self.connection(device).await?).await
            }
        }
    }

    async fn connection(&self, device: DeviceId) -> Result<PeerConnection, LinkError> {
        if let Some(connection) = self.state().connections.get(&device) {
            return Ok(connection.clone());
        }
        let advertised = self
            .nearby()
            .unwrap_or_default()
            .into_iter()
            .find(|nearby| nearby.device == device)
            .map(|nearby| nearby.address);
        let address = advertised
            .or_else(|| self.state().addresses.get(&device).copied())
            .ok_or(LinkError::Unreachable)?;
        let connection = self
            .node
            .dial(address, net_device(device))
            .await
            .map_err(|_| LinkError::Unreachable)?;
        self.state().connections.insert(device, connection.clone());
        Ok(connection)
    }

    /// Derive the code, exchange names, then wait for both decisions.
    async fn handshake(
        self: &Arc<Self>,
        connection: PeerConnection,
        role: Role,
    ) -> Result<PendingPairing, LinkError> {
        let code = connection.pairing_code().await?;
        let (mut send, mut recv) = match role {
            Role::Dialer => connection.open_bi().await?,
            Role::Listener => connection.accept_bi().await?,
        };
        let hello = PairingMessage::Hello {
            protocol: PEER_PROTOCOL,
            name: self.name.clone(),
        };
        send_message(&mut send, &hello).await?;
        let PairingMessage::Hello { protocol, name } = receive(&mut recv).await? else {
            return Err(LinkError::Protocol);
        };
        if protocol != PEER_PROTOCOL {
            connection.close();
            return Err(LinkError::Protocol);
        }
        let name: String = name.chars().filter(|c| !c.is_control()).collect();
        let name = if name.trim().is_empty() || name.len() > MAX_NAME_BYTES {
            String::from("Mac")
        } else {
            name
        };

        let device = DeviceId::from_bytes(*connection.device().as_bytes());
        let (decide, decision) = oneshot::channel();
        self.state().pending.insert(
            device,
            Pending {
                name: name.clone(),
                code,
                decide: Some(decide),
            },
        );
        let link = Arc::downgrade(self);
        tokio::spawn(settle(link, device, connection, send, recv, decision));
        Ok(PendingPairing { device, name, code })
    }

    fn close_window(&self, generation: u64) {
        let mut state = self.state();
        if state.window.as_ref().map(|window| window.generation) != Some(generation) {
            return;
        }
        let Some(window) = state.window.take() else {
            return;
        };
        for task in window.tasks {
            task.abort();
        }
        // Codes already shown still settle over the node's connections.
        window.node.stop_accepting();
        if state.pending.is_empty() {
            window.node.close();
        } else {
            state.draining.push(window.node);
        }
        drop(state);
        self.announce();
    }

    /// Close the nodes of closed windows once no code is left to settle.
    fn drain(&self) {
        let drained = {
            let mut state = self.state();
            if !state.pending.is_empty() {
                return;
            }
            std::mem::take(&mut state.draining)
        };
        for node in drained {
            node.close();
        }
    }

    /// Tell the network where this Mac listens, and whether it pairs.
    fn announce(&self) {
        let Neighbourhood::Browsing(discovery) = &self.neighbourhood else {
            return;
        };
        let Ok(port) = self.address().map(|address| address.port()) else {
            return;
        };
        let advert = Advert {
            name: self.name.clone(),
            port,
            pairing: self.pairing_address().map(|address| address.port()),
        };
        // Best effort: an advert that fails to go out leaves pairing by
        // address, and nearby() still reports what this Mac sees.
        let _ = discovery.advertise(&advert);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for PeerLink {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
        let state = self.state.get_mut().unwrap_or_else(PoisonError::into_inner);
        if let Some(window) = state.window.take() {
            window.node.close();
            for task in window.tasks {
                task.abort();
            }
        }
        for node in state.draining.drain(..) {
            node.close();
        }
        if let Some(accept) = state.accept.take() {
            accept.abort();
        }
        self.node.close();
    }
}

#[derive(Clone, Copy)]
enum Role {
    Dialer,
    Listener,
}

/// Wait for this side's person and the other side's message; pin only if
/// both confirmed in time.
async fn settle(
    link: Weak<PeerLink>,
    device: DeviceId,
    connection: PeerConnection,
    mut send: PeerSendStream,
    mut recv: PeerRecvStream,
    decision: oneshot::Receiver<Decision>,
) {
    // Reading in its own task: cancelling a read midway would lose part
    // of a frame.
    let (remote_tx, remote) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let _ = remote_tx.send(receive(&mut recv).await);
    });
    let both = async {
        let (mut decision, mut remote) = (decision, remote);
        let (mut local, mut other) = (None, None);
        loop {
            tokio::select! {
                answer = &mut decision, if local.is_none() => {
                    let answer = answer.unwrap_or(Decision::Cancel);
                    let message = match answer {
                        Decision::Confirm => PairingMessage::Confirm,
                        Decision::Cancel => PairingMessage::Cancel,
                    };
                    if send_message(&mut send, &message).await.is_err()
                        || answer == Decision::Cancel
                    {
                        return false;
                    }
                    local = Some(answer);
                }
                message = &mut remote, if other.is_none() => {
                    // The other person cancelled, or the Mac went away.
                    if !matches!(message, Ok(Ok(PairingMessage::Confirm))) {
                        return false;
                    }
                    other = Some(Decision::Confirm);
                }
            }
            if local.is_some() && other.is_some() {
                return true;
            }
        }
    };
    let confirmed = tokio::time::timeout(CONFIRM_TIMEOUT, both)
        .await
        .unwrap_or(false);
    reader.abort();
    let Some(link) = link.upgrade() else {
        connection.close();
        return;
    };
    let pending = link.state().pending.remove(&device);
    if confirmed
        && let Some(pending) = pending
        && link.host.paired(device, pending.name).is_ok()
    {
        link.trust.pin(net_device(device));
    }
    // The decision just sent must reach the other Mac before the close.
    let _ = tokio::time::timeout(DELIVERY_TIMEOUT, send.finish_delivered()).await;
    connection.close();
    link.drain();
}

/// What a kept-open request reports.
#[derive(Debug, Eq, PartialEq)]
pub enum Watched {
    Reply(PeerReply),
    /// The request broke, e.g. the other Mac slept; it is asked again.
    Lost,
}

/// A request kept open by `PeerLink::watch`; dropping it closes it.
pub struct WatchHandle(JoinHandle<()>);

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Answer each request a paired device sends on `connection`.
async fn answer(link: Weak<PeerLink>, connection: PeerConnection) {
    let device = DeviceId::from_bytes(*connection.device().as_bytes());
    while let Ok((send, recv)) = connection.accept_bi().await {
        tokio::spawn(answer_one(link.clone(), device, send, recv));
    }
}

async fn answer_one(
    link: Weak<PeerLink>,
    device: DeviceId,
    mut send: PeerSendStream,
    mut recv: PeerRecvStream,
) {
    let Ok(bytes) = stream::receive_json(&mut recv).await else {
        return;
    };
    let request = match serde_json::from_slice::<PeerRequest>(&bytes) {
        Ok(request) if request.protocol == PEER_PROTOCOL => request,
        // A newer request, or another version's.
        _ => {
            let refused = PeerReply::Refused {
                reason: "That Mac runs another Foldover version; update both Macs".into(),
            };
            if stream::send_json(&mut send, &refused).await.is_ok() {
                let _ = tokio::time::timeout(DELIVERY_TIMEOUT, send.finish_delivered()).await;
            }
            return;
        }
    };
    let Some((host, runtime, closing)) = link.upgrade().map(|link| {
        (
            Arc::clone(&link.host),
            link.runtime.clone(),
            Arc::clone(&link.closing),
        )
    }) else {
        return;
    };
    let answered = tokio::task::spawn_blocking(move || {
        let mut answer = Answer::new(runtime, send, recv, closing);
        host.serve(device, request.call, &mut answer);
        answer.into_send()
    })
    .await;
    if let Ok(mut send) = answered {
        let _ = tokio::time::timeout(DELIVERY_TIMEOUT, send.finish_delivered()).await;
    }
}

async fn send_message(
    send: &mut PeerSendStream,
    message: &PairingMessage,
) -> Result<(), LinkError> {
    let bytes = serde_json::to_vec(message).map_err(|_| LinkError::Protocol)?;
    Ok(write_message(send, &bytes).await?)
}

async fn receive(recv: &mut PeerRecvStream) -> Result<PairingMessage, LinkError> {
    let bytes = read_message(recv).await?;
    serde_json::from_slice(&bytes).map_err(|_| LinkError::Protocol)
}

fn net_device(device: DeviceId) -> NetDeviceId {
    NetDeviceId::from_bytes(*device.as_bytes())
}

#[cfg(test)]
mod tests;
