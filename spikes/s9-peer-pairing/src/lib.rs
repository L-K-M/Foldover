//! S9 spike harness: does QUIC (quinn) + rustls with self-signed Ed25519
//! device certificates, a six-digit short authentication string (SAS) at
//! pairing time and pinned fingerprints afterwards give the security
//! PLAN.md §4.11 promises?
//!
//! Model: every device has one Ed25519 key inside a self-signed
//! certificate; the device ID is the SHA-256 fingerprint of the
//! certificate DER. Connections are mutually authenticated QUIC over
//! TLS 1.3 only. [`VerifierMode`] picks the peer-certificate policy:
//! [`VerifierMode::Pairing`] accepts any peer certificate (the caller
//! records the fingerprint and asks the user to confirm the SAS), while
//! [`VerifierMode::Pinned`] accepts only fingerprints already in a
//! [`TrustStore`]. The verifier reads the store at verify time, so
//! `revoke` takes effect on the next handshake.

use std::collections::BTreeSet;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig, TransportConfig, VarInt};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use sha2::{Digest, Sha256};

/// ALPN identifier for the spike's peer protocol.
pub const ALPN: &[u8] = b"foldover-peer-s9";

/// RFC 5705 exporter label binding the SAS to this TLS session.
const SAS_LABEL: &[u8] = b"foldover-sas-v1";

/// SAS space: six decimal digits.
const SAS_MODULUS: u32 = 1_000_000;

/// TLS 1.3 only. QUIC needs nothing older and the spike must show there
/// is no downgrade path.
const TLS13_ONLY: &[&rustls::SupportedProtocolVersion] = &[&rustls::version::TLS13];

/// Failure modes of the pairing harness.
#[derive(Debug)]
pub enum Error {
    Rcgen(rcgen::Error),
    Rustls(rustls::Error),
    Io(std::io::Error),
    Connect(quinn::ConnectError),
    Connection(quinn::ConnectionError),
    Write(quinn::WriteError),
    Read(quinn::ReadError),
    /// A stream finished by `finish()` could not be closed because the
    /// connection was already gone.
    ClosedStream(quinn::ClosedStream),
    /// The rustls config offered no usable initial cipher suite.
    /// Cannot happen here: `crypto_provider` always offers TLS 1.3
    /// suites.
    InitialCipherSuite(quinn::crypto::rustls::NoInitialCipherSuite),
    Join(tokio::task::JoinError),
    /// A connecting peer never finished the handshake, or the endpoint
    /// closed while accepting.
    EndpointClosed,
    /// The peer presented no certificate chain we could read. Cannot
    /// happen in this harness: both directions are mandatory.
    NoPeerCertificate,
    /// The TLS keying-material exporter rejected the request.
    KeyExport,
    /// A connect or accept exceeded the test timeout.
    Timeout,
    /// Catch-all for harness-internal failures.
    Other(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Rcgen(e) => write!(f, "certificate generation: {e}"),
            Error::Rustls(e) => write!(f, "tls: {e}"),
            Error::Io(e) => write!(f, "io: {e}"),
            Error::Connect(e) => write!(f, "connect: {e}"),
            Error::Connection(e) => write!(f, "connection: {e}"),
            Error::Write(e) => write!(f, "stream write: {e}"),
            Error::Read(e) => write!(f, "stream read: {e}"),
            Error::ClosedStream(e) => write!(f, "stream closed: {e}"),
            Error::InitialCipherSuite(e) => {
                write!(f, "no usable initial cipher suite: {e}")
            }
            Error::Join(e) => write!(f, "task: {e}"),
            Error::EndpointClosed => write!(f, "endpoint closed while accepting"),
            Error::NoPeerCertificate => write!(f, "peer presented no certificate"),
            Error::KeyExport => write!(f, "keying-material export unavailable"),
            Error::Timeout => write!(f, "timed out"),
            Error::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Rcgen(e) => Some(e),
            Error::Rustls(e) => Some(e),
            Error::Io(e) => Some(e),
            Error::Connect(e) => Some(e),
            Error::Connection(e) => Some(e),
            Error::Write(e) => Some(e),
            Error::Read(e) => Some(e),
            Error::ClosedStream(e) => Some(e),
            Error::InitialCipherSuite(e) => Some(e),
            Error::Join(e) => Some(e),
            _ => None,
        }
    }
}

macro_rules! from_err {
    ($variant:ident, $ty:ty) => {
        impl From<$ty> for Error {
            fn from(e: $ty) -> Self {
                Error::$variant(e)
            }
        }
    };
}

from_err!(Rcgen, rcgen::Error);
from_err!(Rustls, rustls::Error);
from_err!(Io, std::io::Error);
from_err!(Connect, quinn::ConnectError);
from_err!(Connection, quinn::ConnectionError);
from_err!(Write, quinn::WriteError);
from_err!(Read, quinn::ReadError);
from_err!(ClosedStream, quinn::ClosedStream);
from_err!(
    InitialCipherSuite,
    quinn::crypto::rustls::NoInitialCipherSuite
);
from_err!(Join, tokio::task::JoinError);

impl From<quinn::crypto::ExportKeyingMaterialError> for Error {
    fn from(_: quinn::crypto::ExportKeyingMaterialError) -> Self {
        Error::KeyExport
    }
}

/// Device ID: SHA-256 of the device certificate's DER encoding. This is
/// what gets pinned at pairing time and what a trust store holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// Hash a certificate's DER bytes.
    pub fn of(cert: &CertificateDer<'_>) -> Self {
        Self(Sha256::digest(cert.as_ref()).into())
    }

    /// Lowercase hex, for logs and the JSON report.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

/// An Ed25519 device key wrapped in a self-signed certificate, like the
/// Keychain-resident device key §4.11 describes (minus Keychain, which
/// is irrelevant to the wire-security question).
pub struct Identity {
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    fingerprint: Fingerprint,
}

impl Identity {
    /// Generate a fresh device identity.
    pub fn generate() -> Result<Self, Error> {
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let params = rcgen::CertificateParams::new(vec!["foldover-device".to_string()])?;
        let cert = params.self_signed(&key_pair)?;
        let cert_der = cert.der().clone();
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_pair.serialize_der()));
        let fingerprint = Fingerprint::of(&cert_der);
        Ok(Self {
            cert: cert_der,
            key: key_der,
            fingerprint,
        })
    }

    /// The DER-encoded device certificate sent in the handshake.
    pub fn cert(&self) -> &CertificateDer<'static> {
        &self.cert
    }

    /// SHA-256 of `cert()`: this device's ID.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }
}

/// Device IDs the user has confirmed at pairing time. Read by the
/// verifier at every handshake, so `revoke` takes effect on the next
/// connection — the one-step revocation S9 must demonstrate.
#[derive(Debug, Default)]
pub struct TrustStore {
    pins: Mutex<BTreeSet<Fingerprint>>,
}

impl TrustStore {
    /// An empty store, shared by `Arc` so the same pins apply to every
    /// connection and listener of the device.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record a confirmed peer fingerprint.
    pub fn pin(&self, fingerprint: Fingerprint) {
        self.pins.lock().unwrap().insert(fingerprint);
    }

    /// Remove a peer; its next connection fails.
    pub fn revoke(&self, fingerprint: &Fingerprint) {
        self.pins.lock().unwrap().remove(fingerprint);
    }

    /// Whether the fingerprint is pinned.
    pub fn is_pinned(&self, fingerprint: &Fingerprint) -> bool {
        self.pins.lock().unwrap().contains(fingerprint)
    }
}

/// Peer-certificate policy for one listener or connection.
#[derive(Debug)]
pub enum VerifierMode {
    /// Pairing window: accept any peer certificate. The caller records
    /// the peer fingerprint and must confirm the SAS before pinning.
    Pairing,
    /// Accept only device IDs already pinned in this trust store.
    Pinned(Arc<TrustStore>),
}

/// One verifier for both roles: `ServerCertVerifier` when this node
/// dials, `ClientCertVerifier` when it listens. `tls13_verified` counts
/// TLS 1.3 CertificateVerify checks — observable proof that the TLS 1.3
/// signature path ran, since rustls calls it only for TLS 1.3.
#[derive(Debug)]
struct PeerVerifier {
    mode: VerifierMode,
    provider: Arc<CryptoProvider>,
    tls13_verified: Arc<AtomicUsize>,
}

impl PeerVerifier {
    /// Apply the trust policy to the peer's leaf certificate.
    fn check_leaf(&self, end_entity: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        let fingerprint = Fingerprint::of(end_entity);
        let allowed = match &self.mode {
            VerifierMode::Pairing => true,
            VerifierMode::Pinned(store) => store.is_pinned(&fingerprint),
        };
        if allowed {
            Ok(())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    /// Shared TLS 1.3 CertificateVerify check for both directions.
    fn verify13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.tls13_verified.fetch_add(1, Ordering::SeqCst);
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    /// Unreachable under [`TLS13_ONLY`]; fail closed rather than pretend.
    fn tls12_disabled() -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::PeerIncompatible(
            rustls::PeerIncompatible::Tls12NotOffered,
        ))
    }
}

impl ServerCertVerifier for PeerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        self.check_leaf(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Self::tls12_disabled()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

impl ClientCertVerifier for PeerVerifier {
    /// No root hints: trust is by pinned fingerprint, not by CA.
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        self.check_leaf(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Self::tls12_disabled()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
    // offer_client_auth defaults to true; client_auth_mandatory
    // defaults to it, so a client certificate is required.
}

/// ring is the one provider pinned for this spike (quinn `rustls-ring`).
/// The suite list is filtered down to TLS 1.3 suites, so every suite the
/// config can ever offer is an AEAD cipher; together with
/// [`TLS13_ONLY`] there is no non-AEAD or TLS 1.2 path at all.
fn crypto_provider() -> Arc<CryptoProvider> {
    let mut provider = rustls::crypto::ring::default_provider();
    provider.cipher_suites = provider
        .cipher_suites
        .iter()
        .copied()
        .filter(|suite| suite.version() == &rustls::version::TLS13)
        .collect();
    Arc::new(provider)
}

/// The cipher suites this spike offers, by rustls enum name. All of
/// them are TLS 1.3 AEAD suites; tests and the JSON report record this
/// as the "no plaintext" evidence at the configuration level.
pub fn offered_cipher_suites() -> Vec<String> {
    crypto_provider()
        .cipher_suites
        .iter()
        .map(|suite| format!("{:?}", suite.suite()))
        .collect()
}

/// The rustls server config behind every listener: own device
/// certificate plus a mandatory, policy-checked client certificate.
pub fn rustls_server_config(
    identity: &Identity,
    mode: VerifierMode,
    tls13_verified: &Arc<AtomicUsize>,
) -> Result<rustls::ServerConfig, Error> {
    let provider = crypto_provider();
    let verifier = PeerVerifier {
        mode,
        provider: provider.clone(),
        tls13_verified: tls13_verified.clone(),
    };
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(TLS13_ONLY)?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(config)
}

/// The rustls client config behind every dial: own device certificate
/// plus a policy-checked server certificate.
pub fn rustls_client_config(
    identity: &Identity,
    mode: VerifierMode,
    tls13_verified: &Arc<AtomicUsize>,
) -> Result<rustls::ClientConfig, Error> {
    let provider = crypto_provider();
    let verifier = PeerVerifier {
        mode,
        provider: provider.clone(),
        tls13_verified: tls13_verified.clone(),
    };
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(TLS13_ONLY)?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(vec![identity.cert.clone()], identity.key.clone_key())?;
    config.alpn_protocols = vec![ALPN.to_vec()];
    Ok(config)
}

/// QUIC flow-control windows shared by tests and the bench, sized so a
/// 1 GiB stream does not stall on loopback.
fn transport_config() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport.send_window(64 << 20);
    transport.stream_receive_window(VarInt::from_u32(32 << 20));
    transport.receive_window(VarInt::from_u32(128 << 20));
    transport.max_concurrent_uni_streams(VarInt::from_u32(16));
    Arc::new(transport)
}

/// A Foldover device on loopback: one endpoint that both accepts and
/// dials mutually authenticated QUIC connections, like the `peer`
/// backend of `foldover-net` will.
#[derive(Clone)]
pub struct PeerNode {
    identity: Arc<Identity>,
    endpoint: Endpoint,
    tls13_verified: Arc<AtomicUsize>,
}

impl PeerNode {
    /// Bind a loopback UDP socket that accepts peers under `server_mode`.
    pub fn bind(
        identity: impl Into<Arc<Identity>>,
        server_mode: VerifierMode,
    ) -> Result<Self, Error> {
        let identity = identity.into();
        let tls13_verified = Arc::new(AtomicUsize::new(0));
        let crypto = rustls_server_config(&identity, server_mode, &tls13_verified)?;
        let mut server = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto)?));
        server.transport = transport_config();
        let endpoint = Endpoint::server(server, SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))?;
        Ok(Self {
            identity,
            endpoint,
            tls13_verified,
        })
    }

    /// The bound loopback address peers dial.
    pub fn addr(&self) -> SocketAddr {
        self.endpoint.local_addr().expect("endpoint is bound")
    }

    /// This device's ID.
    pub fn fingerprint(&self) -> Fingerprint {
        self.identity.fingerprint()
    }

    /// TLS 1.3 CertificateVerify checks this node's verifiers ran.
    pub fn tls13_verifications(&self) -> usize {
        self.tls13_verified.load(Ordering::SeqCst)
    }

    /// Dial a peer under `client_mode`.
    pub async fn connect(
        &self,
        addr: SocketAddr,
        client_mode: VerifierMode,
    ) -> Result<Connection, Error> {
        let crypto = rustls_client_config(&self.identity, client_mode, &self.tls13_verified)?;
        let mut config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto)?));
        config.transport_config(transport_config());
        let connecting = self.endpoint.connect_with(config, addr, "foldover.local")?;
        Ok(connecting.await?)
    }

    /// Accept the next peer that completes the handshake.
    pub async fn accept(&self) -> Result<Connection, Error> {
        let incoming = self.endpoint.accept().await.ok_or(Error::EndpointClosed)?;
        let connecting = incoming.accept()?;
        Ok(connecting.await?)
    }

    /// Stop the endpoint, releasing pending accepts and sends.
    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(0), b"done");
    }
}

/// Establish a mutually authenticated connection: `client` dials
/// `server` under `client_mode` while `server` accepts. Returns the
/// client-side and server-side connection ends.
pub async fn open_connection(
    client: &PeerNode,
    server: &PeerNode,
    client_mode: VerifierMode,
) -> Result<(Connection, Connection), Error> {
    let server_node = server.clone();
    let accept = tokio::spawn(async move { server_node.accept().await });
    let client_conn = client.connect(server.addr(), client_mode).await?;
    let server_conn = accept.await??;
    Ok((client_conn, server_conn))
}

/// The peer's leaf-certificate fingerprint on an established connection.
/// Always available in this harness: both directions are mandatory.
pub fn peer_fingerprint(connection: &Connection) -> Result<Fingerprint, Error> {
    let identity = connection.peer_identity().ok_or(Error::NoPeerCertificate)?;
    let chain = identity
        .downcast::<Vec<CertificateDer<'static>>>()
        .map_err(|_| Error::NoPeerCertificate)?;
    let leaf = chain.first().ok_or(Error::NoPeerCertificate)?;
    Ok(Fingerprint::of(leaf))
}

/// The raw exporter output the SAS is derived from; exposed so tests can
/// assert two sessions differ without depending on digit formatting.
pub fn session_secret(connection: &Connection) -> Result<[u8; 32], Error> {
    let mut exported = [0u8; 32];
    connection.export_keying_material(&mut exported, SAS_LABEL, &[])?;
    Ok(exported)
}

/// The six-digit code both devices show during pairing. Derived from the
/// TLS keying-material exporter — so a relay that terminates both legs
/// lands on different session keys — and hashed with both device
/// fingerprints in canonical order, so it also binds the identities.
pub fn session_sas(
    connection: &Connection,
    local: Fingerprint,
    peer: Fingerprint,
) -> Result<u32, Error> {
    let exported = session_secret(connection)?;
    let (lo, hi) = if local <= peer {
        (local, peer)
    } else {
        (peer, local)
    };
    let digest = Sha256::new()
        .chain_update(exported)
        .chain_update(lo.0)
        .chain_update(hi.0)
        .finalize();
    Ok(u32::from_be_bytes(digest[..4].try_into().unwrap()) % SAS_MODULUS)
}
