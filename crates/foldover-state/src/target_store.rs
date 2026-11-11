//! Immutable, checksummed target-side tables. Endpoint adapters supply the I/O.
//! No filesystem or network mechanics belong in this module.

pub(crate) mod catalog;
mod table;
pub use catalog::{CHECKPOINT_LIMIT, Catalog, Checkpoint};
pub use table::{Entry, TableRoot};

/// A private catalog directory. Implementations fence writes with its lease,
/// bound reads, refuse symlinks, and durably create immutable files.
pub trait BlobStore {
    fn read(&self, name: &str, limit: usize) -> std::io::Result<Option<Vec<u8>>>;
    fn write_new(&self, name: &str, bytes: &[u8]) -> std::io::Result<()>;
}
