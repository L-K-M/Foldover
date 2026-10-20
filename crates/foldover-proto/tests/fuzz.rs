//! Messages from clients and other Macs never panic their parsers (PLAN.md
//! §5.2): JSON-RPC frames and the peer protocol. Values are built from the
//! protocol's own field names, so deserializers get past the first key.
//! The nightly job runs these with many more cases (`PROPTEST_CASES`).

use foldover_proto::local::{PeerStatus, Request, Response, ShareAdd, SyncStatus};
use foldover_proto::peer::{PairingMessage, PeerReply, PeerRequest};
use proptest::prelude::*;
use serde_json::{Map, Value};

const NAMES: &[&str] = &[
    "jsonrpc",
    "id",
    "method",
    "params",
    "result",
    "error",
    "code",
    "message",
    "protocol",
    "call",
    "kind",
    "op",
    "share",
    "shares",
    "path",
    "entry",
    "run",
    "name",
    "options",
    "scan",
    "rules",
    "current",
    "content",
    "target",
    "version",
    "durability",
    "metadata",
    "attributes",
    "value",
    "flags",
    "device",
    "reason",
    "identity",
    "folder",
    "observe",
    "read",
    "receive",
    "watch",
    "hello",
    "confirm",
    "cancel",
    "state",
    "preview",
    "items",
    "counts",
];

fn name() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::sample::select(NAMES).prop_map(str::to_owned),
        "[a-zA-Z_]{0,12}",
    ]
}

fn json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        name().prop_map(Value::String),
        "[ -~]{0,24}".prop_map(Value::String),
    ];
    leaf.prop_recursive(5, 96, 8, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
            prop::collection::vec((name(), inner), 0..8)
                .prop_map(|entries| Value::Object(entries.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

fn parse_everything(value: &Value) {
    let _ = Request::parse(value.clone());
    let _ = Response::parse(value.clone());
    let _ = serde_json::from_value::<PeerRequest>(value.clone());
    let _ = serde_json::from_value::<PeerReply>(value.clone());
    let _ = serde_json::from_value::<PairingMessage>(value.clone());
    let _ = serde_json::from_value::<PeerStatus>(value.clone());
    let _ = serde_json::from_value::<SyncStatus>(value.clone());
    let _ = serde_json::from_value::<ShareAdd>(value.clone());
}

proptest! {
    #[test]
    fn fuzz_structured_messages(value in json()) {
        parse_everything(&value);
    }

    #[test]
    fn fuzz_raw_frames(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
            parse_everything(&value);
        }
        let _ = serde_json::from_slice::<PeerRequest>(&bytes);
        let _ = serde_json::from_slice::<PeerReply>(&bytes);
    }

    /// Valid requests and replies survive a round trip unchanged.
    #[test]
    fn fuzz_peer_messages_round_trip(value in json()) {
        if let Ok(request) = serde_json::from_value::<PeerRequest>(value.clone()) {
            let again = serde_json::to_value(&request).unwrap();
            prop_assert_eq!(serde_json::from_value::<PeerRequest>(again).unwrap(), request);
        }
        if let Ok(reply) = serde_json::from_value::<PeerReply>(value) {
            let again = serde_json::to_value(&reply).unwrap();
            prop_assert_eq!(serde_json::from_value::<PeerReply>(again).unwrap(), reply);
        }
    }
}
