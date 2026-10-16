//! Integration tests for the S9 peer-pairing spike. Each test maps to
//! one pass criterion of PLAN.md §8 S9; run.sh records their results.

use std::sync::Arc;
use std::time::Duration;

use s9_peer_pairing::{
    Error, Fingerprint, Identity, PeerNode, TrustStore, VerifierMode, offered_cipher_suites,
    open_connection, peer_fingerprint, session_sas, session_secret,
};

/// Generous bound: every handshake here is local, so anything slower
/// means the harness hung, not that a criterion failed.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Six decimal digits.
const SAS_MODULUS: u32 = 1_000_000;

/// [`open_connection`] with a hard deadline, so a wedged handshake
/// fails the test instead of hanging CI.
async fn connect(
    client: &PeerNode,
    server: &PeerNode,
    client_mode: VerifierMode,
) -> Result<(quinn::Connection, quinn::Connection), Error> {
    match tokio::time::timeout(TIMEOUT, open_connection(client, server, client_mode)).await {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout),
    }
}

/// Assert a connection attempt failed through the TLS layer (not by
/// timing out); `context` names the direction under test.
fn assert_rejected(result: Result<(quinn::Connection, quinn::Connection), Error>, context: &str) {
    let Err(error) = result else {
        panic!("{context}: connection unexpectedly succeeded");
    };
    assert!(
        !matches!(error, Error::Timeout),
        "{context}: handshake hung instead of being rejected: {error}"
    );
}

/// Devices A and B go through pairing: both are in pairing mode, A
/// dials B, both compute the SAS. Returns the fingerprint each side saw
/// and the two connections, ready for SAS comparison.
async fn pairing_exchange(
    a: &PeerNode,
    b: &PeerNode,
) -> (
    quinn::Connection,
    quinn::Connection,
    Fingerprint,
    Fingerprint,
) {
    let (conn_a, conn_b) = connect(a, b, VerifierMode::Pairing)
        .await
        .expect("pairing connection");
    let seen_by_a = peer_fingerprint(&conn_a).expect("B presented a certificate");
    let seen_by_b = peer_fingerprint(&conn_b).expect("A presented a certificate");
    (conn_a, conn_b, seen_by_a, seen_by_b)
}

/// One pairing round: fresh identities, matching SAS, then both sides
/// pin each other. Returns identity handles and the two trust stores.
async fn pair_devices() -> (
    Arc<Identity>,
    Arc<Identity>,
    Arc<TrustStore>,
    Arc<TrustStore>,
    u32,
) {
    let identity_a = Arc::new(Identity::generate().expect("identity A"));
    let identity_b = Arc::new(Identity::generate().expect("identity B"));
    let a = PeerNode::bind(identity_a.clone(), VerifierMode::Pairing).expect("bind A");
    let b = PeerNode::bind(identity_b.clone(), VerifierMode::Pairing).expect("bind B");

    let (conn_a, conn_b, seen_by_a, seen_by_b) = pairing_exchange(&a, &b).await;
    assert_eq!(seen_by_a, b.fingerprint(), "A must see B's device ID");
    assert_eq!(seen_by_b, a.fingerprint(), "B must see A's device ID");

    let sas_a = session_sas(&conn_a, a.fingerprint(), seen_by_a).expect("exporter A");
    let sas_b = session_sas(&conn_b, b.fingerprint(), seen_by_b).expect("exporter B");
    assert_eq!(sas_a, sas_b, "both sides must show the same code");
    assert!(sas_a < SAS_MODULUS, "SAS must be six decimal digits");

    // The user confirmed the code: each side pins the other.
    let store_a = TrustStore::new();
    let store_b = TrustStore::new();
    store_a.pin(b.fingerprint());
    store_b.pin(a.fingerprint());
    a.close();
    b.close();
    (identity_a, identity_b, store_a, store_b, sas_a)
}

/// (1) Honest pairing: same SAS on both sides, then both pin each other
/// and a pinned-mode reconnect succeeds.
#[tokio::test]
async fn honest_pairing_shows_matching_sas_and_pins() {
    let (identity_a, identity_b, store_a, store_b, _sas) = pair_devices().await;

    let a =
        PeerNode::bind(identity_a, VerifierMode::Pinned(store_a.clone())).expect("bind A pinned");
    let b = PeerNode::bind(identity_b, VerifierMode::Pinned(store_b)).expect("bind B pinned");
    let (conn_a, _conn_b) = connect(&a, &b, VerifierMode::Pinned(store_a))
        .await
        .expect("pinned reconnect after confirming the SAS");
    assert_eq!(
        peer_fingerprint(&conn_a).expect("B presented a certificate"),
        b.fingerprint(),
        "pinned connection must show B's device ID"
    );
    a.close();
    b.close();
}

/// (2) Pinned reconnect carries data both ways over one stream.
#[tokio::test]
async fn pinned_reconnect_succeeds() {
    let (identity_a, identity_b, store_a, store_b, _sas) = pair_devices().await;
    let a =
        PeerNode::bind(identity_a, VerifierMode::Pinned(store_a.clone())).expect("bind A pinned");
    let b = PeerNode::bind(identity_b, VerifierMode::Pinned(store_b)).expect("bind B pinned");

    let (conn_a, conn_b) = connect(&a, &b, VerifierMode::Pinned(store_a))
        .await
        .expect("pinned reconnect");

    // B echoes one message: proves the secured channel moves data. The
    // task gets a cloned handle so dropping it cannot close the
    // connection before A has read the echo.
    let server_conn = conn_b.clone();
    let echo = tokio::spawn(async move {
        let (mut send, mut recv) = server_conn.accept_bi().await.expect("accept stream");
        let mut buf = [0u8; 8];
        recv.read_exact(&mut buf).await.expect("read request");
        send.write_all(&buf).await.expect("echo");
        send.finish().expect("close echo");
    });
    let (mut send, mut recv) = conn_a.open_bi().await.expect("open stream");
    send.write_all(b"foldover").await.expect("write request");
    send.finish().expect("finish request");
    let mut buf = [0u8; 8];
    recv.read_exact(&mut buf).await.expect("read echo");
    assert_eq!(&buf, b"foldover", "echo must survive the round trip");
    echo.await.expect("echo task");
    a.close();
    b.close();
}

/// (3) A device that is not pinned gets no connection, in either
/// direction: the pinned side's verifier rejects the unknown peer.
#[tokio::test]
async fn unknown_device_rejected_in_both_directions() {
    let identity_a = Arc::new(Identity::generate().expect("identity A"));
    let identity_b = Arc::new(Identity::generate().expect("identity B"));
    let empty_a = TrustStore::new();
    let empty_b = TrustStore::new();

    // B dials A's pinned listener: A rejects B's client certificate.
    let a = PeerNode::bind(identity_a, VerifierMode::Pinned(empty_a)).expect("bind A pinned");
    let b = PeerNode::bind(identity_b.clone(), VerifierMode::Pairing).expect("bind B pairing");
    let result = connect(&b, &a, VerifierMode::Pairing).await;
    assert_rejected(result, "pinned listener refusing an unknown client");
    a.close();
    b.close();

    // A dials B's pinned listener: A accepts B's certificate, but B
    // rejects A's client certificate and the handshake fails.
    let identity_a2 = Arc::new(Identity::generate().expect("identity A dialer"));
    let a2 = PeerNode::bind(identity_a2, VerifierMode::Pairing).expect("bind A pairing");
    let b2 = PeerNode::bind(identity_b, VerifierMode::Pinned(empty_b)).expect("bind B pinned");
    let result = connect(&a2, &b2, VerifierMode::Pairing).await;
    assert_rejected(
        result,
        "pinned listener refusing an unknown server-side peer",
    );
    a2.close();
    b2.close();
}

/// (4) A man in the middle that terminates both legs with its own
/// identity lands on two different TLS sessions, so the two SAS codes
/// differ and at least one user sees the wrong code. Detection, not
/// prevention: that is the pairing protocol's job.
#[tokio::test]
async fn mitm_relay_makes_sas_differ() {
    let identity_a = Arc::new(Identity::generate().expect("identity A"));
    let identity_b = Arc::new(Identity::generate().expect("identity B"));
    let identity_m = Arc::new(Identity::generate().expect("identity M (attacker)"));
    let a = PeerNode::bind(identity_a, VerifierMode::Pairing).expect("bind A");
    let m = PeerNode::bind(identity_m.clone(), VerifierMode::Pairing).expect("bind M");
    let b = PeerNode::bind(identity_b, VerifierMode::Pairing).expect("bind B");

    // Leg 1: A dials the attacker.
    let (conn_a, _conn_m1) = connect(&a, &m, VerifierMode::Pairing)
        .await
        .expect("A to attacker leg");
    // Leg 2: the attacker dials B.
    let (_conn_m2, conn_b) = connect(&m, &b, VerifierMode::Pairing)
        .await
        .expect("attacker to B leg");

    let seen_by_a = peer_fingerprint(&conn_a).expect("M presented a certificate");
    let seen_by_b = peer_fingerprint(&conn_b).expect("M presented a certificate");
    assert_eq!(seen_by_a, m.fingerprint(), "A sees the attacker, not B");
    assert_eq!(seen_by_b, m.fingerprint(), "B sees the attacker, not A");

    let sas_a = session_sas(&conn_a, a.fingerprint(), seen_by_a).expect("exporter A");
    let sas_b = session_sas(&conn_b, b.fingerprint(), seen_by_b).expect("exporter B");
    assert_ne!(
        sas_a, sas_b,
        "a relay terminating both legs must make the codes differ"
    );
    a.close();
    m.close();
    b.close();
}

/// (5) Revocation in one step: removing B's pin from A's trust store
/// makes the very next connection attempt fail.
#[tokio::test]
async fn revocation_breaks_next_connection() {
    let (identity_a, identity_b, store_a, store_b, _sas) = pair_devices().await;
    let a =
        PeerNode::bind(identity_a, VerifierMode::Pinned(store_a.clone())).expect("bind A pinned");
    let b =
        PeerNode::bind(identity_b, VerifierMode::Pinned(store_b.clone())).expect("bind B pinned");

    connect(&a, &b, VerifierMode::Pinned(store_a.clone()))
        .await
        .expect("connection works while B is pinned");

    store_a.revoke(&b.fingerprint());
    let result = connect(&a, &b, VerifierMode::Pinned(store_a)).await;
    assert_rejected(result, "revoked peer reconnecting");
    a.close();
    b.close();
}

/// (6) No plaintext: only TLS 1.3 AEAD suites are offered, the TLS 1.3
/// certificate-verification path actually ran on both ends, and the
/// exporter gives per-session keys (same on both ends of one session,
/// different across sessions).
#[tokio::test]
async fn negotiated_tls13_with_aead_cipher() {
    let suites = offered_cipher_suites();
    assert!(!suites.is_empty(), "the provider must offer suites");
    for suite in &suites {
        assert!(
            matches!(
                suite.as_str(),
                "TLS13_AES_128_GCM_SHA256"
                    | "TLS13_AES_256_GCM_SHA384"
                    | "TLS13_CHACHA20_POLY1305_SHA256"
            ),
            "offered suite must be TLS 1.3 AEAD: {suite}"
        );
    }

    let identity_a = Arc::new(Identity::generate().expect("identity A"));
    let identity_b = Arc::new(Identity::generate().expect("identity B"));
    let a = PeerNode::bind(identity_a, VerifierMode::Pairing).expect("bind A");
    let b = PeerNode::bind(identity_b, VerifierMode::Pairing).expect("bind B");

    let (conn_a1, conn_b1) = connect(&a, &b, VerifierMode::Pairing)
        .await
        .expect("first session");
    // rustls runs verify_tls13_signature only on TLS 1.3 handshakes, so
    // these counts are dynamic proof of the negotiated version.
    assert!(a.tls13_verifications() >= 1, "A verified via TLS 1.3");
    assert!(b.tls13_verifications() >= 1, "B verified via TLS 1.3");

    let secret_a = session_secret(&conn_a1).expect("exporter A");
    let secret_b = session_secret(&conn_b1).expect("exporter B");
    assert_eq!(secret_a, secret_b, "both ends share the session secret");

    let (conn_a2, _conn_b2) = connect(&a, &b, VerifierMode::Pairing)
        .await
        .expect("second session");
    let secret_a2 = session_secret(&conn_a2).expect("exporter A, session two");
    assert_ne!(
        secret_a, secret_a2,
        "each session must get fresh keying material"
    );
    a.close();
    b.close();
}
