//! Whether a pair's folders are present, so unattended runs wait for an
//! unmounted disk or share instead of failing.

use foldover_endpoint::{LocalPair, PairRootError, mount_share};
use foldover_types::{PairConfig, Side};

/// Mount `side`'s share again from `source`, its recorded mount source;
/// true when the folder is back.
pub fn reconnect(pair: &PairConfig, side: Side, source: &str) -> bool {
    mount_share(source).is_ok() && missing_side(pair) != Some(side)
}

/// The side whose folder is missing, if either is.
pub fn missing_side(pair: &PairConfig) -> Option<Side> {
    match LocalPair::open(pair.draft().left(), pair.draft().right()) {
        Err(PairRootError::Missing(side)) => Some(side),
        _ => None,
    }
}
