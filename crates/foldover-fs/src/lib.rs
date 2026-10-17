//! `foldover-fs` — driver layer. Local file I/O relative to an open root
//! handle: enumeration with attributes, atomic writes, clones, links and
//! file metadata. Calls the OS directly and uses shared vocabulary.
//! See PLAN.md §4.4.

#[cfg(unix)]
mod root;

#[cfg(unix)]
pub use root::{
    DirectoryEntry, DirectoryScope, EntryMetadata, FileIdentity, HashError, NodeKind, Root,
    RootRelation,
};
