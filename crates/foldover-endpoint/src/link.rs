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
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use foldover_net::peer::{
    DeviceId as NetDeviceId, Identity, Node, PeerConnection, PeerError, PeerMode, PeerRecvStream,
    PeerSendStream, TrustStore, read_message, write_message,
};
use foldover_proto::peer::{PEER_PROTOCOL, PairingMessage};
use foldover_types::DeviceId;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// How long a person has to confirm a code once both Macs show it.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// How long a final pairing message may take to reach the other Mac.
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

/// Records a confirmed device; a device it could not record is not
/// trusted, as it would be forgotten at the next start.
type Paired = Box<dyn Fn(DeviceId, String) -> Result<(), ()> + Send + Sync>;

pub struct PeerLink {
    identity: Arc<Identity>,
    name: String,
    trust: Arc<TrustStore>,
    node: Arc<Node>,
    state: Mutex<State>,
    /// Records a device both people confirmed, e.g. in the state store.
    paired: Paired,
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
        paired: impl IntoIterator<Item = DeviceId>,
        on_paired: impl Fn(DeviceId, String) -> Result<(), ()> + Send + Sync + 'static,
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
        let link = Arc::new(Self {
            identity,
            name,
            trust,
            node: Arc::clone(&node),
            state: Mutex::default(),
            paired: Box::new(on_paired),
        });
        // Shared folders arrive in the next slice; until then a paired
        // device's connection is accepted and closed.
        let accept = tokio::spawn(async move {
            while let Some(connection) = node.accept().await {
                if let Ok(connection) = connection {
                    connection.close();
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
    /// `address`; returns the code both Macs now show.
    pub async fn pair(
        self: &Arc<Self>,
        device: DeviceId,
        address: SocketAddr,
    ) -> Result<PendingPairing, LinkError> {
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

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for PeerLink {
    fn drop(&mut self) {
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
        && (link.paired)(device, pending.name).is_ok()
    {
        link.trust.pin(net_device(device));
    }
    // The decision just sent must reach the other Mac before the close.
    let _ = tokio::time::timeout(DELIVERY_TIMEOUT, send.finish_delivered()).await;
    connection.close();
    link.drain();
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
