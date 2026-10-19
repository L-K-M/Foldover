//! Whether a pair's folders are present, so unattended runs wait for an
//! unmounted disk or share instead of failing.

use foldover_endpoint::{EndpointPair, PairRootError, Reach, mount_share};
use foldover_types::{PairConfig, Side};

/// Mount `side`'s share again from `source`, its recorded mount source;
/// true when the folder is back.
pub fn reconnect(pair: &PairConfig, side: Side, source: &str, reach: &Reach) -> bool {
    mount_share(source).is_ok() && missing_side(pair, reach) != Some(side)
}

/// The side whose folder is missing, if either is.
pub fn missing_side(pair: &PairConfig, reach: &Reach) -> Option<Side> {
    match EndpointPair::open(pair.draft().left(), pair.draft().right(), reach) {
        // An unreachable server is waited for like an unmounted disk.
        Err(PairRootError::Missing(side) | PairRootError::Unreachable(side, _)) => Some(side),
        _ => None,
    }
}
