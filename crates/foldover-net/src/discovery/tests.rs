use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use super::*;

const DEVICE: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b";

fn advert<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<&'a str> {
    |key| {
        pairs
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| *value)
    }
}

#[test]
fn adverts_name_the_device_and_where_it_listens_network_addresses_first() {
    let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5));
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let v6 = "fe80::1".parse().unwrap();
    let properties = [
        ("v", "1"),
        ("id", DEVICE),
        ("name", "Studio\u{1b}[2J"),
        ("pairing", "50124"),
    ];
    let seen = sighting(&[loopback, v6, lan], 50123, advert(&properties)).unwrap();
    assert_eq!(seen.device, DeviceId::from_hex(DEVICE).unwrap());
    assert_eq!(seen.name, "Studio[2J");
    assert_eq!(
        seen.addresses,
        [
            SocketAddr::new(lan, 50123),
            SocketAddr::new(loopback, 50123)
        ]
    );
    assert_eq!(seen.pairing[0], SocketAddr::new(lan, 50124));

    // Closed pairing, and a missing name.
    let seen = sighting(&[lan], 50123, advert(&[("v", "1"), ("id", DEVICE)])).unwrap();
    assert!(seen.pairing.is_empty());
    assert_eq!(seen.name, "Mac");
}

#[test]
fn other_versions_and_malformed_adverts_are_ignored() {
    let lan = [IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))];
    for properties in [
        &[("v", "2"), ("id", DEVICE)][..],
        &[("id", DEVICE)],
        &[("v", "1"), ("id", "abc")],
        &[("v", "1"), ("id", DEVICE), ("pairing", "lots")],
    ] {
        assert_eq!(
            sighting(&lan, 50123, advert(properties)),
            None,
            "{properties:?}"
        );
    }
    let valid = [("v", "1"), ("id", DEVICE)];
    assert_eq!(sighting(&lan, 0, advert(&valid)), None);
    assert_eq!(
        sighting(&["::1".parse().unwrap()], 50123, advert(&valid)),
        None
    );
}

/// Live multicast on this host; set FOLDOVER_MDNS_TESTS=1 to run it.
#[test]
fn two_agents_on_one_network_see_each_other_and_pairing_windows() {
    if std::env::var_os("FOLDOVER_MDNS_TESTS").is_none() {
        return;
    }
    let a_id = DeviceId::from_bytes([0xa1; 32]);
    let b_id = DeviceId::from_bytes([0xb2; 32]);
    let a = Discovery::start(a_id).unwrap();
    let b = Discovery::start(b_id).unwrap();
    let advert = |name: &str, pairing| Advert {
        name: name.into(),
        port: 50123,
        pairing,
    };
    a.advertise(&advert("Laptop", None)).unwrap();
    b.advertise(&advert("Studio", Some(50124))).unwrap();

    let until = |what: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !what() {
            assert!(Instant::now() < deadline, "never seen");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    until(&|| {
        a.seen()
            .iter()
            .any(|seen| seen.device == b_id && !seen.pairing.is_empty())
    });
    until(&|| {
        b.seen()
            .iter()
            .any(|seen| seen.device == a_id && seen.name == "Laptop")
    });
    assert!(a.seen().iter().all(|seen| seen.device != a_id));

    // Closing the window re-advertises without it.
    b.advertise(&advert("Studio", None)).unwrap();
    until(&|| {
        a.seen()
            .iter()
            .any(|seen| seen.device == b_id && seen.pairing.is_empty())
    });
}
