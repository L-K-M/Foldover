use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr};

use super::*;

const TIMEOUT: Duration = Duration::from_secs(10);

async fn bounded<T>(work: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, work)
        .await
        .expect("loopback operation timed out")
}

fn node(mode: PeerMode) -> Node {
    Node::bind(
        Arc::new(Identity::generate().unwrap()),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Arc::new(TrustStore::default()),
        mode,
    )
    .unwrap()
}

async fn connect(a: &Node, b: &Node) -> (PeerConnection, PeerConnection) {
    let (dialed, accepted) =
        bounded(async { tokio::join!(a.dial(b.address().unwrap(), b.id()), b.accept()) }).await;
    (dialed.unwrap(), accepted.unwrap().unwrap())
}

fn pin(a: &Node, b: &Node) {
    a.trust.pin(b.id());
    b.trust.pin(a.id());
}

#[test]
fn device_ids_round_trip_and_reject_malformed_hex() {
    let id = Identity::generate().unwrap().id();
    assert_eq!(DeviceId::from_hex(&id.to_string()), Some(id));
    assert_eq!(DeviceId::from_bytes(*id.as_bytes()), id);
    assert_eq!(DeviceId::from_hex(""), None);
    assert_eq!(DeviceId::from_hex(&"x".repeat(64)), None);
    assert_eq!(DeviceId::from_hex(&"é".repeat(32)), None);
    assert_eq!(DeviceId::from_hex(&"+0".repeat(32)), None);
}

fn ecdsa_identity() -> Identity {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let cert = rcgen::CertificateParams::new(vec![SERVER_NAME.to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap()
        .der()
        .clone();
    Identity {
        id: DeviceId::of(&cert),
        key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        cert,
    }
}

#[tokio::test]
async fn pairing_refuses_non_ed25519_devices() {
    let a = node(PeerMode::Pairing);
    let b = Node::bind(
        Arc::new(ecdsa_identity()),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Arc::new(TrustStore::default()),
        PeerMode::Pairing,
    )
    .unwrap();
    let (dialed, _) =
        bounded(async { tokio::join!(a.dial(b.address().unwrap(), b.id()), b.accept()) }).await;
    assert!(dialed.is_err());
    a.close();
    b.close();
}

#[test]
fn a_valid_but_unadvertised_signature_scheme_is_rejected() {
    use rustls::internal::msgs::codec::Codec;

    let identity = ecdsa_identity();
    let crypto = provider();
    let key = crypto
        .key_provider
        .load_private_key(identity.key.clone_key())
        .unwrap();
    let scheme = SignatureScheme::ECDSA_NISTP256_SHA256;
    let message = b"certificate verification";
    let signature = key.choose_scheme(&[scheme]).unwrap().sign(message).unwrap();
    let mut encoded = u16::from(scheme).to_be_bytes().to_vec();
    encoded.extend_from_slice(&u16::try_from(signature.len()).unwrap().to_be_bytes());
    encoded.extend_from_slice(&signature);
    let signed = DigitallySignedStruct::read_bytes(&encoded).unwrap();
    assert!(
        verify_tls13_signature(
            message,
            &identity.cert,
            &signed,
            &crypto.signature_verification_algorithms
        )
        .is_ok()
    );

    let verifier = Verifier {
        accept: Accept::AnyDevice,
        mode: PeerMode::Pairing,
        trust: Arc::new(TrustStore::default()),
        provider: crypto,
    };
    assert!(verifier.verify13(message, &identity.cert, &signed).is_err());
}

#[tokio::test]
async fn pairing_matches_then_confirmed_devices_reconnect() {
    let a = node(PeerMode::Pairing);
    let b = node(PeerMode::Pairing);
    let (dialed, accepted) = connect(&a, &b).await;
    assert_eq!(dialed.device(), b.id());
    assert_eq!(accepted.device(), a.id());

    let (a_code, b_code) =
        bounded(async { tokio::join!(dialed.pairing_code(), accepted.pairing_code()) }).await;
    let code = a_code.unwrap();
    assert_eq!(code, b_code.unwrap());
    assert!(code < CODE_MODULUS);
    assert!(!a.trust.is_pinned(b.id()), "a code is not confirmation");
    assert!(!b.trust.is_pinned(a.id()));

    // Confirmation persists in the store, not in one socket's TLS config.
    pin(&a, &b);
    a.close();
    b.close();
    let a_paired = Node::bind(
        Arc::clone(&a.identity),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Arc::clone(&a.trust),
        PeerMode::Paired,
    );
    let a_paired = a_paired.unwrap();
    let b_paired = Node::bind(
        Arc::clone(&b.identity),
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        Arc::clone(&b.trust),
        PeerMode::Paired,
    )
    .unwrap();
    let (dialed, accepted) = connect(&a_paired, &b_paired).await;
    assert_eq!(dialed.device(), b.id());
    assert_eq!(accepted.device(), a.id());
    a_paired.close();
    b_paired.close();
}

#[tokio::test]
async fn unknown_devices_are_refused_in_both_tls_roles() {
    let paired = node(PeerMode::Paired);
    let stranger = node(PeerMode::Pairing);

    // The listener refuses a client missing from its pins.
    let (dialed, accepted) = bounded(async {
        tokio::join!(
            stranger.dial(paired.address().unwrap(), paired.id()),
            paired.accept(),
        )
    })
    .await;
    assert!(accepted.unwrap().is_err());
    if let Ok(connection) = dialed {
        assert!(bounded(connection.accept_bi()).await.is_err());
    }

    // The dialer refuses the server even when its advertised ID is right.
    let (dialed, _) = bounded(async {
        tokio::join!(
            paired.dial(stranger.address().unwrap(), stranger.id()),
            stranger.accept(),
        )
    })
    .await;
    assert!(dialed.is_err());
    paired.close();
    stranger.close();
}

#[tokio::test]
async fn pairing_still_requires_a_client_certificate() {
    let a = node(PeerMode::Pairing);
    let b = node(PeerMode::Pairing);
    let provider = provider();
    let verifier = Verifier {
        accept: Accept::Device(b.id()),
        mode: PeerMode::Pairing,
        trust: Arc::clone(&a.trust),
        provider: Arc::clone(&provider),
    };
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(TLS13_ONLY)
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()));
    let dial = a
        .endpoint
        .connect_with(config, b.address().unwrap(), SERVER_NAME)
        .unwrap();
    let (dialed, accepted) = bounded(async { tokio::join!(dial, b.accept()) }).await;
    assert!(accepted.unwrap().is_err());
    if let Ok(connection) = dialed {
        assert!(bounded(connection.accept_bi()).await.is_err());
    }
    a.close();
    b.close();
}

#[tokio::test]
async fn the_advertised_device_id_must_match_even_when_pairing() {
    let a = node(PeerMode::Pairing);
    let b = node(PeerMode::Pairing);
    let wrong = Identity::generate().unwrap().id();
    let (dialed, _) =
        bounded(async { tokio::join!(a.dial(b.address().unwrap(), wrong), b.accept()) }).await;
    assert!(dialed.is_err());
    a.close();
    b.close();
}

#[tokio::test]
async fn revocation_closes_clones_and_refuses_new_handshakes() {
    let a = node(PeerMode::Paired);
    let b = node(PeerMode::Paired);
    let other = node(PeerMode::Paired);
    pin(&a, &b);
    pin(&a, &other);
    let (dialed, accepted) = connect(&a, &b).await;
    let clone = dialed.clone();
    let (unaffected, _unaffected_peer) = connect(&a, &other).await;
    let (mut send, _recv) = dialed.open_bi().await.unwrap();
    write_message(&mut send, b"before revoke").await.unwrap();
    let (_send, mut recv) = bounded(accepted.accept_bi()).await.unwrap();
    assert_eq!(read_message(&mut recv).await.unwrap(), b"before revoke");

    a.trust.revoke(b.id());
    assert!(!a.trust.is_pinned(b.id()));
    assert!(clone.open_bi().await.is_err());
    assert!(bounded(accepted.accept_bi()).await.is_err());
    assert!(write_message(&mut send, b"after revoke").await.is_err());
    assert!(bounded(read_message(&mut recv)).await.is_err());
    assert!(unaffected.open_bi().await.is_ok());

    let (dialed_again, _) =
        bounded(async { tokio::join!(a.dial(b.address().unwrap(), b.id()), b.accept()) }).await;
    assert!(dialed_again.is_err());
    drop(dialed);
    a.close();
    b.close();
    other.close();
}

#[tokio::test]
async fn a_revoked_handshake_cannot_publish_a_late_session() {
    let a = node(PeerMode::Paired);
    let b = node(PeerMode::Paired);
    pin(&a, &b);
    let (dialed, _) = connect(&a, &b).await;
    a.trust.revoke(b.id());

    // Model verification passing before revoke, registration after it.
    let late = Session {
        connection: dialed.0.connection.clone(),
        local: a.id(),
        peer: b.id(),
        role: PairingRole::Dialer,
    };
    assert!(matches!(
        a.trust.register(late, PeerMode::Paired),
        Err(PeerError::Untrusted)
    ));
    a.close();
    b.close();
}

#[tokio::test]
async fn revocation_reaches_streams_after_connection_handles_are_dropped() {
    let a = node(PeerMode::Paired);
    let b = node(PeerMode::Paired);
    pin(&a, &b);
    let (dialed, accepted) = connect(&a, &b).await;
    let (mut send, _recv) = dialed.open_bi().await.unwrap();
    write_message(&mut send, b"before revoke").await.unwrap();
    let (_send, mut recv) = bounded(accepted.accept_bi()).await.unwrap();
    assert_eq!(read_message(&mut recv).await.unwrap(), b"before revoke");
    drop(dialed);
    drop(accepted);

    a.trust.revoke(b.id());
    assert!(write_message(&mut send, b"after revoke").await.is_err());
    assert!(bounded(read_message(&mut recv)).await.is_err());
    a.close();
    b.close();
}

#[tokio::test]
async fn a_false_nonce_commitment_aborts_pairing() {
    let a = node(PeerMode::Pairing);
    let b = node(PeerMode::Pairing);
    let (dialed, accepted) = connect(&a, &b).await;

    let cheat = async {
        let (mut send, mut recv) = dialed.open_bi().await.unwrap();
        send.write_all(&[0; NONCE_BYTES]).await.unwrap();
        let mut nonce = [0; NONCE_BYTES];
        recv.read_exact(&mut nonce).await.unwrap();
        send.write_all(&[1; NONCE_BYTES]).await.unwrap();
        send.finish().unwrap();
        let _ = recv.read_exact(&mut [0]).await;
    };
    let (_, verdict) = bounded(async { tokio::join!(cheat, accepted.pairing_code()) }).await;
    assert!(matches!(verdict, Err(PeerError::CommitmentMismatch)));
    assert!(!b.trust.is_pinned(a.id()));
    assert!(accepted.open_bi().await.is_err());
    a.close();
    b.close();
}

#[tokio::test]
async fn frame_limits_apply_in_both_directions_without_losing_alignment() {
    let a = node(PeerMode::Paired);
    let b = node(PeerMode::Paired);
    pin(&a, &b);
    let (dialed, accepted) = connect(&a, &b).await;

    let send_frames = async {
        let (mut send, mut recv) = dialed.open_bi().await.unwrap();
        assert!(matches!(
            write_message(&mut send, &vec![0; MAX_MESSAGE + 1]).await,
            Err(PeerError::Protocol)
        ));
        write_message(&mut send, &[]).await.unwrap();
        write_message(&mut send, &vec![7; MAX_MESSAGE])
            .await
            .unwrap();
        send.finish().unwrap();
        assert_eq!(read_message(&mut recv).await.unwrap(), b"received");
    };
    let receive_frames = async {
        let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
        assert!(read_message(&mut recv).await.unwrap().is_empty());
        let message = read_message(&mut recv).await.unwrap();
        assert_eq!(message.len(), MAX_MESSAGE);
        assert!(message.iter().all(|byte| *byte == 7));
        write_message(&mut send, b"received").await.unwrap();
        send.finish().unwrap();
    };
    bounded(async { tokio::join!(send_frames, receive_frames) }).await;
    a.close();
    b.close();
}

#[tokio::test]
async fn hostile_lengths_and_truncated_frames_fail() {
    let a = node(PeerMode::Paired);
    let b = node(PeerMode::Paired);
    pin(&a, &b);
    let (dialed, accepted) = connect(&a, &b).await;

    let send_frames = async {
        for (length, payload) in [(u32::MAX, b"".as_slice()), (3, b"x".as_slice())] {
            let (mut send, mut recv) = dialed.open_bi().await.unwrap();
            send.write_all(&length.to_be_bytes()).await.unwrap();
            send.write_all(payload).await.unwrap();
            send.finish().unwrap();
            // Wait for the reader before opening the next stream.
            read_message(&mut recv).await.unwrap();
        }
    };
    let receive_frames = async {
        for oversized in [true, false] {
            let (mut send, mut recv) = accepted.accept_bi().await.unwrap();
            let result = read_message(&mut recv).await;
            assert!(result.is_err());
            if oversized {
                assert!(matches!(result, Err(PeerError::Protocol)));
            }
            write_message(&mut send, &[]).await.unwrap();
            send.finish().unwrap();
        }
    };
    bounded(async { tokio::join!(send_frames, receive_frames) }).await;
    a.close();
    b.close();
}

#[tokio::test]
async fn connections_bound_parallel_streams_and_disable_unused_unidirectional_streams() {
    let a = node(PeerMode::Paired);
    let b = node(PeerMode::Paired);
    pin(&a, &b);
    let (dialed, accepted) = connect(&a, &b).await;
    let mut held = Vec::new();
    for _ in 0..MAX_BIDI_STREAMS {
        held.push(bounded(dialed.open_bi()).await.unwrap());
    }

    let mut extra = std::pin::pin!(dialed.open_bi());
    let mut uni = std::pin::pin!(dialed.0.connection.open_uni());
    std::future::poll_fn(|context| {
        assert!(extra.as_mut().poll(context).is_pending());
        assert!(uni.as_mut().poll(context).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    accepted.close();
    assert!(bounded(extra).await.is_err());
    assert!(bounded(uni).await.is_err());
    drop(held);
    a.close();
    b.close();
}

#[tokio::test]
async fn tls_is_session_bound_and_only_offers_tls13_aead() {
    let crypto = provider();
    assert!(!crypto.cipher_suites.is_empty());
    for suite in &crypto.cipher_suites {
        assert_eq!(suite.version(), &rustls::version::TLS13);
        assert!(matches!(
            suite.suite(),
            rustls::CipherSuite::TLS13_AES_128_GCM_SHA256
                | rustls::CipherSuite::TLS13_AES_256_GCM_SHA384
                | rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        ));
    }
    assert!(no_tls12().is_err());

    let a = node(PeerMode::Pairing);
    let b = node(PeerMode::Pairing);
    let (first, first_peer) = connect(&a, &b).await;
    let (second, _) = connect(&a, &b).await;
    let export = |connection: &PeerConnection| {
        let mut material = [0; NONCE_BYTES];
        connection
            .0
            .connection
            .export_keying_material(&mut material, CODE_LABEL, &[])
            .unwrap();
        material
    };
    assert_eq!(export(&first), export(&first_peer));
    assert_ne!(export(&first), export(&second));
    a.close();
    b.close();
}

#[tokio::test]
async fn closing_a_node_unblocks_accept() {
    let a = node(PeerMode::Paired);
    a.close();
    assert!(bounded(a.accept()).await.is_none());
}

#[tokio::test]
async fn an_unresponsive_pairing_peer_times_out_and_is_closed() {
    let a = node(PeerMode::Pairing);
    let b = node(PeerMode::Pairing);
    let (_dialed, accepted) = connect(&a, &b).await;
    let verdict = tokio::time::timeout(PAIRING_TIMEOUT + TIMEOUT, accepted.pairing_code())
        .await
        .expect("pairing deadline did not fire");
    assert!(matches!(verdict, Err(PeerError::PairingTimeout)));
    assert!(accepted.open_bi().await.is_err());
    a.close();
    b.close();
}
