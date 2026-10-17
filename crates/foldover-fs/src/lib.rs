//! `foldover-fs` — driver layer. Local file I/O relative to an open root
//! handle: enumeration with attributes, atomic writes, clones, links and
//! file metadata. Calls the OS directly and uses shared vocabulary.
//! See PLAN.md §4.4.

#[cfg(unix)]
#[forbid(unsafe_code)]
mod root;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod materialization;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod volume;

#[cfg(target_os = "macos")]
use materialization::prevent_materialization;

#[cfg(not(target_os = "macos"))]
fn prevent_materialization() -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub use root::{
    DataResidency, DirectoryEntry, DirectoryScope, EntryMetadata, FileIdentity, HashError,
    NodeKind, Root, RootRelation,
};
