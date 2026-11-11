//! Private target-side catalog files, separated from the endpoint's content
//! lease. Every catalog session owns a lease on its own private directory.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use foldover_types::{Durability, EndpointId, PairId, ScanControl};

use crate::lease::OwnedLease;
use crate::{Endpoint, LeaseHolder, LeaseTiming};

pub const CATALOG_FILE_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const CATALOG_ENTRIES_LIMIT: usize = 250_000;
pub(crate) const DIRECTORY_MARKER: &[u8] = b"Foldover target catalog 1\n";

pub struct CatalogFiles {
    parent: Arc<Endpoint>,
    identity: EndpointId,
    endpoint: Arc<Endpoint>,
    lease: Mutex<OwnedLease>,
    durability: Durability,
}

impl CatalogFiles {
    pub fn open(
        parent: Arc<Endpoint>,
        pair: PairId,
        holder: LeaseHolder,
        durability: Durability,
        control: &mut impl FnMut() -> ScanControl,
    ) -> io::Result<Self> {
        let identity = parent.ensure_identity(durability).map_err(other)?;
        let endpoint = Arc::new(match parent.as_ref() {
            Endpoint::Local(endpoint) => {
                Endpoint::Local(endpoint.catalog_endpoint(pair, durability)?)
            }
            Endpoint::Sftp(endpoint) => {
                Endpoint::Sftp(endpoint.catalog_endpoint(pair, durability)?)
            }
            Endpoint::Peer(endpoint) => {
                Endpoint::Peer(endpoint.catalog_endpoint(pair, durability)?)
            }
            #[cfg(feature = "simulation")]
            Endpoint::Memory(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Memory catalogs use their own test store",
                ));
            }
        });
        let lease = OwnedLease::take(
            Arc::clone(&endpoint),
            holder,
            LeaseTiming::STANDARD,
            control,
        )
        .map_err(other)?;
        Ok(Self {
            parent,
            identity,
            endpoint,
            lease: Mutex::new(lease),
            durability,
        })
    }

    pub fn identity(&self) -> EndpointId {
        self.identity
    }

    /// Keep a long scan's catalog lease alive without touching its tables.
    pub fn tick(&self) -> io::Result<()> {
        self.lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .renew_if_due()
            .map_err(other)
    }

    /// Fence publication against both a lease takeover and target replacement.
    pub fn fence(&self) -> io::Result<()> {
        self.lease
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .renew()
            .map_err(other)?;
        if self.parent.identity().map_err(other)? != Some(self.identity) {
            return Err(io::Error::other(
                "The backup target changed while its catalog was open",
            ));
        }
        Ok(())
    }

    pub fn read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        validate_name(name)?;
        if limit > CATALOG_FILE_LIMIT {
            return Err(invalid("Catalog read exceeds its limit"));
        }
        self.tick()?;
        match self.endpoint.as_ref() {
            Endpoint::Local(endpoint) => endpoint.catalog_read(name, limit),
            Endpoint::Sftp(endpoint) => endpoint.catalog_read(name, limit),
            Endpoint::Peer(endpoint) => endpoint.catalog_read(name, limit),
            #[cfg(feature = "simulation")]
            _ => Err(invalid("Unsupported catalog endpoint")),
        }
    }

    pub fn write_new(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        validate_name(name)?;
        if bytes.len() > CATALOG_FILE_LIMIT {
            return Err(invalid("Catalog file exceeds its limit"));
        }
        self.tick()?;
        match self.endpoint.as_ref() {
            Endpoint::Local(endpoint) => endpoint.catalog_write(name, bytes, self.durability),
            Endpoint::Sftp(endpoint) => endpoint.catalog_write(name, bytes, self.durability),
            Endpoint::Peer(endpoint) => endpoint.catalog_write(name, bytes, self.durability),
            #[cfg(feature = "simulation")]
            _ => Err(invalid("Unsupported catalog endpoint")),
        }
    }

    pub fn list(&self) -> io::Result<Vec<String>> {
        self.tick()?;
        match self.endpoint.as_ref() {
            Endpoint::Local(endpoint) => endpoint.catalog_list(),
            Endpoint::Sftp(endpoint) => endpoint.catalog_list(),
            Endpoint::Peer(endpoint) => endpoint.catalog_list(),
            #[cfg(feature = "simulation")]
            _ => Err(invalid("Unsupported catalog endpoint")),
        }
    }

    pub fn remove(&self, name: &str) -> io::Result<()> {
        validate_name(name)?;
        self.tick()?;
        match self.endpoint.as_ref() {
            Endpoint::Local(endpoint) => endpoint.catalog_remove(name),
            Endpoint::Sftp(endpoint) => endpoint.catalog_remove(name),
            Endpoint::Peer(endpoint) => endpoint.catalog_remove(name),
            #[cfg(feature = "simulation")]
            _ => Err(invalid("Unsupported catalog endpoint")),
        }
    }
}

pub(crate) fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return Err(invalid("Invalid catalog filename"));
    }
    Ok(())
}

pub(crate) fn other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
pub(crate) fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
