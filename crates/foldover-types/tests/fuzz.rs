//! Parsers of untrusted text and bytes never panic (PLAN.md §5.2): paths
//! and locators from clients and other Macs, identifiers and filter rules.
//! The nightly job runs these with many more cases (`PROPTEST_CASES`).

use foldover_types::{
    DeviceId, EndpointId, Filters, LocalRoot, PairId, PairRoot, ReviewToken, ShareId, SyncPath,
};
use proptest::prelude::*;

proptest! {
    #[test]
    fn fuzz_paths_and_roots_from_bytes(bytes in prop::collection::vec(any::<u8>(), 0..600)) {
        if let Ok(path) = SyncPath::try_from(bytes.as_slice()) {
            prop_assert_eq!(path.as_bytes(), bytes.as_slice());
        }
        if let Ok(root) = PairRoot::from_bytes(bytes.clone()) {
            prop_assert_eq!(root.as_bytes(), bytes.as_slice());
        }
        let _ = LocalRoot::from_bytes(bytes.clone());
        let _ = EndpointId::from_hex(&bytes);
    }

    #[test]
    fn fuzz_locators(text in "(sftp|peer)://[ -~]{0,200}") {
        if let Ok(root) = PairRoot::from_bytes(text.clone().into_bytes()) {
            prop_assert_eq!(root.as_bytes(), text.as_bytes());
        }
    }

    #[test]
    fn fuzz_identifiers(text in "[0-9a-fA-F/ -]{0,80}") {
        if let Ok(device) = text.parse::<DeviceId>() {
            prop_assert_eq!(device.to_string(), text.clone());
        }
        if let Ok(share) = text.parse::<ShareId>() {
            prop_assert_eq!(share.to_string(), text.clone());
        }
        let _ = text.parse::<PairId>();
        let _ = text.parse::<ReviewToken>();
    }

    #[test]
    fn fuzz_filter_rules(rules in prop::collection::vec("[ -~\n]{0,40}", 0..8)) {
        let _ = Filters::new(rules);
    }
}
