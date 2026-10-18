//! Mac-to-Mac transport (PLAN.md §4.11, ADR 0004): mutually authenticated
//! QUIC over TLS 1.3 with self-signed Ed25519 device certificates. A
//! device ID is the SHA-256 of its certificate.
//!
//! Both ends require pinned devices unless explicitly in pairing mode.
//! Pairing authenticates a channel, not permission to access a folder;
//! the endpoint and engine must authorize requests separately.
//!
//! Pairing shows six digits on both Macs from a commit-then-reveal
//! exchange bound to the TLS session and both device IDs:
//!
//! ```text
//! dialer                                  listener
//!   | -- C = H(commit label, exporter, Nd) --> |
//!   | <------------------ Nl ----------------- |
//!   | ------------------- Nd ----------------> |  checks C
//!   code = H(code label, exporter, Nd, Nl, both IDs) mod 10^6
//! ```

use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{
    ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig,
    VarInt,
};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

mod identity;

/// ALPN of Foldover's peer protocol, version 1.
const ALPN: &[u8] = b"foldover-peer-1";
const SERVER_NAME: &str = "foldover.local";
/// Exporter label binding the code to this session; also the code
/// hash's domain separator.
const CODE_LABEL: &[u8] = b"foldover-pairing-code-v1";
const COMMIT_LABEL: &[u8] = b"foldover-pairing-commit-v1";
const HASH_BYTES: usize = 32;
const NONCE_BYTES: usize = 32;
const CODE_MODULUS: u32 = 1_000_000;
const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];
const SEND_WINDOW: u64 = 64 * 1024 * 1024;
const RECEIVE_WINDOW: u32 = 64 * 1024 * 1024;
const STREAM_WINDOW: u32 = 16 * 1024 * 1024;
const MAX_BIDI_STREAMS: u32 = 4;
const FRAME_CHUNK_BYTES: usize = 64 * 1024;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(15);
/// The largest framed message accepted.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;

type Nonce = [u8; NONCE_BYTES];

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("Peer TLS: {0}")]
    Tls(String),
    #[error("Peer connection: {0}")]
    Connection(String),
    #[error("Peer I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("The other Mac sent a malformed message")]
    Protocol,
    /// The dialer revealed a nonce other than the one it committed to:
    /// pairing must stop whatever the screens show.
    #[error("Pairing was tampered with; try again on a trusted network")]
    CommitmentMismatch,
    #[error("This device is not paired")]
    Untrusted,
    #[error("The other Mac did not finish pairing in time")]
    PairingTimeout,
    #[error("The stored peer identity is invalid; restore it before connecting")]
    InvalidIdentity,
}

fn connection_error(error: impl fmt::Display) -> PeerError {
    PeerError::Connection(error.to_string())
}

/// A device ID: SHA-256 of the device certificate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId([u8; HASH_BYTES]);

impl DeviceId {
    fn of(cert: &CertificateDer<'_>) -> Self {
        Self(Sha256::digest(cert.as_ref()).into())
    }

    pub fn from_bytes(bytes: [u8; HASH_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; HASH_BYTES] {
        &self.0
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        if text.len() != HASH_BYTES * 2 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let mut bytes = [0; HASH_BYTES];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(bytes))
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// This device's key in a self-signed certificate.
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    id: DeviceId,
}

impl Identity {
    pub fn generate() -> Result<Self, PeerError> {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519).map_err(tls_error)?;
        let params =
            rcgen::CertificateParams::new(vec![SERVER_NAME.to_owned()]).map_err(tls_error)?;
        let cert = params.self_signed(&key).map_err(tls_error)?;
        let cert = cert.der().clone();
        Ok(Self {
            id: DeviceId::of(&cert),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            cert,
        })
    }

    pub fn id(&self) -> DeviceId {
        self.id
    }
}

fn tls_error(error: impl fmt::Display) -> PeerError {
    PeerError::Tls(error.to_string())
}

/// Whether a node is accepting a pairing attempt or already paired peers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerMode {
    Paired,
    /// Only use during a user-initiated pairing window. A code match still
    /// needs confirmation on both devices before either pin is recorded.
    Pairing,
}

/// Confirmed devices and weak handles to their live channels. Sharing this
/// store makes revocation apply to every node using the same pins.
#[derive(Debug, Default)]
pub struct TrustStore {
    state: Mutex<TrustState>,
}

#[derive(Debug, Default)]
struct TrustState {
    pins: BTreeSet<DeviceId>,
    sessions: Vec<Weak<Session>>,
}

impl TrustStore {
    /// Record a device only after the user confirms the pairing code.
    pub fn pin(&self, device: DeviceId) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pins
            .insert(device);
    }

    pub fn is_pinned(&self, device: DeviceId) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pins
            .contains(&device)
    }

    /// Remove the pin and close its channels, including pairing channels.
    pub fn revoke(&self, device: DeviceId) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.pins.remove(&device);
        state.sessions.retain(|session| {
            let Some(session) = session.upgrade() else {
                return false;
            };
            if session.peer != device {
                return true;
            }
            session
                .connection
                .close(VarInt::from_u32(0), b"device revoked");
            false
        });
    }

    fn register(&self, session: Session, mode: PeerMode) -> Result<PeerConnection, PeerError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // Recheck under the revocation lock: a handshake can finish after
        // its certificate passed verification but its pin was removed.
        if mode == PeerMode::Paired && !state.pins.contains(&session.peer) {
            session
                .connection
                .close(VarInt::from_u32(0), b"device revoked");
            return Err(PeerError::Untrusted);
        }
        let session = Arc::new(session);
        state.sessions.retain(|session| session.strong_count() > 0);
        state.sessions.push(Arc::downgrade(&session));
        Ok(PeerConnection(session))
    }
}

/// Which certificates a TLS handshake accepts.
#[derive(Clone, Copy, Debug)]
enum Accept {
    /// Listening: any device that also passes the node's trust policy.
    AnyDevice,
    /// Dialing: exactly this device.
    Device(DeviceId),
}

#[derive(Debug)]
struct Verifier {
    accept: Accept,
    mode: PeerMode,
    trust: Arc<TrustStore>,
    provider: Arc<CryptoProvider>,
}

impl Verifier {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if self.mode == PeerMode::Paired && !self.trust.is_pinned(DeviceId::of(cert)) {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        match self.accept {
            Accept::AnyDevice => Ok(()),
            Accept::Device(expected) if DeviceId::of(cert) == expected => Ok(()),
            Accept::Device(_) => Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            )),
        }
    }

    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        if signature.scheme != SignatureScheme::ED25519 {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ));
        }
        verify_tls13_signature(
            message,
            cert,
            signature,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

fn no_tls12() -> Result<HandshakeSignatureValid, rustls::Error> {
    Err(rustls::Error::PeerIncompatible(
        rustls::PeerIncompatible::Tls12NotOffered,
    ))
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check(cert)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        no_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for Verifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check(cert)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        no_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, signature)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

/// ring with TLS 1.3 suites only: every suite offered is an AEAD.
fn provider() -> Arc<CryptoProvider> {
    let mut provider = rustls::crypto::ring::default_provider();
    provider
        .cipher_suites
        .retain(|suite| suite.version() == &rustls::version::TLS13);
    Arc::new(provider)
}

fn transport() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport.send_window(SEND_WINDOW);
    transport.stream_receive_window(VarInt::from_u32(STREAM_WINDOW));
    transport.receive_window(VarInt::from_u32(RECEIVE_WINDOW));
    transport.max_concurrent_bidi_streams(VarInt::from_u32(MAX_BIDI_STREAMS));
    transport.max_concurrent_uni_streams(VarInt::from_u32(0));
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    Arc::new(transport)
}

/// One device's QUIC socket: it listens for peers and dials them.
#[derive(Clone)]
pub struct Node {
    identity: Arc<Identity>,
    endpoint: Endpoint,
    trust: Arc<TrustStore>,
    mode: PeerMode,
}

impl Node {
    /// Listen on `address`; port 0 picks a free one.
    pub fn bind(
        identity: Arc<Identity>,
        address: SocketAddr,
        trust: Arc<TrustStore>,
        mode: PeerMode,
    ) -> Result<Self, PeerError> {
        let provider = provider();
        let verifier = Verifier {
            accept: Accept::AnyDevice,
            mode,
            trust: Arc::clone(&trust),
            provider: Arc::clone(&provider),
        };
        let mut crypto = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(TLS13_ONLY)
            .map_err(tls_error)?
            .with_client_cert_verifier(Arc::new(verifier))
            .with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())
            .map_err(tls_error)?;
        crypto.alpn_protocols = vec![ALPN.to_vec()];
        let mut server = ServerConfig::with_crypto(Arc::new(
            QuicServerConfig::try_from(crypto).map_err(tls_error)?,
        ));
        server.transport = transport();
        let endpoint = Endpoint::server(server, address)?;
        Ok(Self {
            identity,
            endpoint,
            trust,
            mode,
        })
    }

    pub fn address(&self) -> Result<SocketAddr, PeerError> {
        Ok(self.endpoint.local_addr()?)
    }

    pub fn id(&self) -> DeviceId {
        self.identity.id
    }

    /// Connect to `device` at `address`; any other certificate fails.
    pub async fn dial(
        &self,
        address: SocketAddr,
        device: DeviceId,
    ) -> Result<PeerConnection, PeerError> {
        let provider = provider();
        let verifier = Verifier {
            accept: Accept::Device(device),
            mode: self.mode,
            trust: Arc::clone(&self.trust),
            provider: Arc::clone(&provider),
        };
        let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(TLS13_ONLY)
            .map_err(tls_error)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_client_auth_cert(
                vec![self.identity.cert.clone()],
                self.identity.key.clone_key(),
            )
            .map_err(tls_error)?;
        crypto.alpn_protocols = vec![ALPN.to_vec()];
        let mut config = ClientConfig::new(Arc::new(
            QuicClientConfig::try_from(crypto).map_err(tls_error)?,
        ));
        config.transport_config(transport());
        let connecting = self
            .endpoint
            .connect_with(config, address, SERVER_NAME)
            .map_err(connection_error)?;
        let connection = connecting.await.map_err(connection_error)?;
        self.register(connection, PairingRole::Dialer)
    }

    /// The next connection and the device behind it; `None` once closed.
    pub async fn accept(&self) -> Option<Result<PeerConnection, PeerError>> {
        let incoming = self.endpoint.accept().await?;
        Some(
            async {
                let connection = incoming.await.map_err(connection_error)?;
                self.register(connection, PairingRole::Listener)
            }
            .await,
        )
    }

    fn register(
        &self,
        connection: Connection,
        role: PairingRole,
    ) -> Result<PeerConnection, PeerError> {
        let peer = peer_device(&connection)?;
        self.trust.register(
            Session {
                connection,
                local: self.id(),
                peer,
                role,
            },
            self.mode,
        )
    }

    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(0), b"closed");
    }
}

/// The device at the other end of a connection.
fn peer_device(connection: &Connection) -> Result<DeviceId, PeerError> {
    let identity = connection.peer_identity().ok_or(PeerError::Protocol)?;
    let chain = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| PeerError::Protocol)?;
    Ok(DeviceId::of(chain.first().ok_or(PeerError::Protocol)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PairingRole {
    Dialer,
    Listener,
}

#[derive(Debug)]
struct Session {
    connection: Connection,
    local: DeviceId,
    peer: DeviceId,
    role: PairingRole,
}

/// A channel whose identity and revocation handle stay together. Streams
/// retain the same session, so revocation reaches them after this is dropped.
#[derive(Clone, Debug)]
pub struct PeerConnection(Arc<Session>);

impl PeerConnection {
    pub fn device(&self) -> DeviceId {
        self.0.peer
    }

    pub async fn open_bi(&self) -> Result<(PeerSendStream, PeerRecvStream), PeerError> {
        let streams = self
            .0
            .connection
            .open_bi()
            .await
            .map_err(connection_error)?;
        Ok(self.streams(streams))
    }

    pub async fn accept_bi(&self) -> Result<(PeerSendStream, PeerRecvStream), PeerError> {
        let streams = self
            .0
            .connection
            .accept_bi()
            .await
            .map_err(connection_error)?;
        Ok(self.streams(streams))
    }

    fn streams(&self, (send, recv): (SendStream, RecvStream)) -> (PeerSendStream, PeerRecvStream) {
        (
            PeerSendStream {
                stream: send,
                _session: Arc::clone(&self.0),
            },
            PeerRecvStream {
                stream: recv,
                _session: Arc::clone(&self.0),
            },
        )
    }

    pub fn close(&self) {
        self.0.connection.close(VarInt::from_u32(0), b"closed");
    }

    /// Compute the six-digit code on a dedicated stream. A match is not
    /// approval; pin only after confirmation on both devices.
    pub async fn pairing_code(&self) -> Result<u32, PeerError> {
        let exchange = async {
            let (mut send, mut recv) = match self.0.role {
                PairingRole::Dialer => self.open_bi().await?,
                PairingRole::Listener => self.accept_bi().await?,
            };
            let code = pairing_code(&self.0, &mut send, &mut recv).await?;
            send.finish()?;
            Ok(code)
        };
        let result = tokio::time::timeout(PAIRING_TIMEOUT, exchange)
            .await
            .unwrap_or(Err(PeerError::PairingTimeout));
        if result.is_err() {
            self.close();
        }
        result
    }
}

/// The sending half of a stream, retaining its session for revocation.
pub struct PeerSendStream {
    stream: SendStream,
    _session: Arc<Session>,
}

impl PeerSendStream {
    pub async fn write_all(&mut self, bytes: &[u8]) -> Result<(), PeerError> {
        self.stream.write_all(bytes).await.map_err(connection_error)
    }

    pub fn finish(&mut self) -> Result<(), PeerError> {
        self.stream.finish().map_err(connection_error)
    }
}

/// The receiving half of a stream, retaining its session for revocation.
pub struct PeerRecvStream {
    stream: RecvStream,
    _session: Arc<Session>,
}

impl PeerRecvStream {
    pub async fn read_exact(&mut self, bytes: &mut [u8]) -> Result<(), PeerError> {
        self.stream
            .read_exact(bytes)
            .await
            .map_err(connection_error)
    }
}

/// The six-digit code both Macs show, from the commit-then-reveal
/// exchange on `send`/`recv`. Both ends must run it with their roles.
async fn pairing_code(
    session: &Session,
    send: &mut PeerSendStream,
    recv: &mut PeerRecvStream,
) -> Result<u32, PeerError> {
    let mut exported = [0; HASH_BYTES];
    session
        .connection
        .export_keying_material(&mut exported, CODE_LABEL, &[])
        .map_err(|_| PeerError::Tls("keying material unavailable".into()))?;
    let nonce = random_nonce()?;
    let (dialer, listener) = match session.role {
        PairingRole::Dialer => {
            send.write_all(&commitment(&exported, &nonce)).await?;
            let mut theirs = [0; NONCE_BYTES];
            recv.read_exact(&mut theirs).await?;
            send.write_all(&nonce).await?;
            (nonce, theirs)
        }
        PairingRole::Listener => {
            let mut committed = [0; HASH_BYTES];
            recv.read_exact(&mut committed).await?;
            send.write_all(&nonce).await?;
            let mut theirs = [0; NONCE_BYTES];
            recv.read_exact(&mut theirs).await?;
            if commitment(&exported, &theirs) != committed {
                return Err(PeerError::CommitmentMismatch);
            }
            (theirs, nonce)
        }
    };
    let (low, high) = if session.local <= session.peer {
        (session.local, session.peer)
    } else {
        (session.peer, session.local)
    };
    let digest: [u8; HASH_BYTES] = Sha256::new()
        .chain_update(CODE_LABEL)
        .chain_update(exported)
        .chain_update(dialer)
        .chain_update(listener)
        .chain_update(low.0)
        .chain_update(high.0)
        .finalize()
        .into();
    Ok(u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % CODE_MODULUS)
}

fn random_nonce() -> Result<Nonce, PeerError> {
    let mut nonce = [0; NONCE_BYTES];
    provider()
        .secure_random
        .fill(&mut nonce)
        .map_err(|_| PeerError::Tls("random source failed".into()))?;
    Ok(nonce)
}

fn commitment(exported: &[u8; HASH_BYTES], nonce: &Nonce) -> [u8; HASH_BYTES] {
    Sha256::new()
        .chain_update(COMMIT_LABEL)
        .chain_update(exported)
        .chain_update(nonce)
        .finalize()
        .into()
}

/// Write one length-prefixed message.
pub async fn write_message(send: &mut PeerSendStream, message: &[u8]) -> Result<(), PeerError> {
    if message.len() > MAX_MESSAGE {
        return Err(PeerError::Protocol);
    }
    let length = u32::try_from(message.len()).map_err(|_| PeerError::Protocol)?;
    send.write_all(&length.to_be_bytes()).await?;
    send.write_all(message).await
}

/// Read one length-prefixed message of at most `MAX_MESSAGE` bytes.
/// Drop the stream after an error; its frame boundary is no longer known.
pub async fn read_message(recv: &mut PeerRecvStream) -> Result<Vec<u8>, PeerError> {
    let mut length = [0; 4];
    recv.read_exact(&mut length).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_MESSAGE {
        return Err(PeerError::Protocol);
    }
    // Grow only as bytes arrive: a length header alone must not allocate
    // a full frame on every stream a hostile peer opens.
    let mut message = Vec::new();
    while message.len() < length {
        let remaining = length - message.len();
        let chunk = recv
            .stream
            .read_chunk(remaining.min(FRAME_CHUNK_BYTES), true)
            .await
            .map_err(connection_error)?
            .ok_or(PeerError::Protocol)?;
        message.extend_from_slice(&chunk.bytes);
    }
    Ok(message)
}

#[cfg(test)]
mod tests;
