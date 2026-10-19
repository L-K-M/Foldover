//! Keep both versions of a conflict: Folder B's version is saved beside
//! the entry, then a "keep A" choice makes A's version win at the path.
//! The next run carries both to both folders.

use foldover_endpoint::{ApplyError, EndpointPair, PairRootError, Reach};
use foldover_types::{EntryVersion, PairConfig, ScanControl, Side, SyncPath};

/// How the app names the right folder; saved copies carry it.
const RIGHT_FOLDER: &str = "Folder B";

#[derive(Debug, thiserror::Error)]
pub enum KeepBothError {
    #[error("{0:?} changed since the preview; preview again")]
    Changed(SyncPath),
    #[error(transparent)]
    Roots(#[from] PairRootError),
    #[error(transparent)]
    Copy(ApplyError),
}

/// Save the right folder's version of `path`, which must still be
/// `right`, beside it, and return the copy's path.
pub fn keep_both(
    pair: &PairConfig,
    reach: &Reach,
    path: &SyncPath,
    right: EntryVersion,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<SyncPath, KeepBothError> {
    let draft = pair.draft();
    let endpoints = EndpointPair::open(draft.left(), draft.right(), reach)?;
    endpoints
        .endpoint(Side::Right)
        .keep_copy(path, right, RIGHT_FOLDER, draft.options(), control)
        .map_err(|error| match error {
            ApplyError::SourceChanged(path) => KeepBothError::Changed(path),
            error => KeepBothError::Copy(error),
        })
}
