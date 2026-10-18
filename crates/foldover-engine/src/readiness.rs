//! Whether a pair's folders are present, so unattended runs wait for an
//! unmounted disk or share instead of failing.

use foldover_endpoint::{LocalPair, PairRootError};
use foldover_types::{PairConfig, Side};

/// The side whose folder is missing, if either is.
pub fn missing_side(pair: &PairConfig) -> Option<Side> {
    match LocalPair::open(pair.draft().left(), pair.draft().right()) {
        Err(PairRootError::Missing(side)) => Some(side),
        _ => None,
    }
}
