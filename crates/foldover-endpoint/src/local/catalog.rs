use std::io;

use foldover_fs::{DirectoryScope, NodeKind, WriteError};
use foldover_types::{Durability, PairId, SyncPath};

use super::LocalEndpoint;
use crate::catalog::{CATALOG_ENTRIES_LIMIT, DIRECTORY_MARKER, invalid, other};

fn path(name: &str) -> io::Result<SyncPath> {
    crate::catalog::validate_name(name)?;
    SyncPath::try_from(name.as_bytes()).map_err(other)
}

fn write_error(error: WriteError) -> io::Error {
    match error {
        WriteError::Exists => {
            io::Error::new(io::ErrorKind::AlreadyExists, "Catalog file already exists")
        }
        error => other(error),
    }
}

impl LocalEndpoint {
    pub(crate) fn catalog_endpoint(
        &self,
        pair: PairId,
        durability: Durability,
    ) -> io::Result<Self> {
        let directory =
            SyncPath::try_from(format!(".foldover/state/{pair}").as_bytes()).map_err(other)?;
        let marker = SyncPath::try_from(format!(".foldover/state/{pair}/format").as_bytes())
            .map_err(other)?;
        let format = match self.root.read_private_file(&marker, 64)? {
            Some(bytes) => Some(bytes),
            None => {
                match self
                    .root
                    .create_private_file(&marker, DIRECTORY_MARKER, durability)
                {
                    Ok(()) | Err(WriteError::Exists) => {}
                    Err(error) => return Err(write_error(error)),
                }
                self.root.read_private_file(&marker, 64)?
            }
        };
        if format.as_deref() != Some(DIRECTORY_MARKER) {
            return Err(invalid("Unrecognized target catalog directory"));
        }
        let root = self.root.open_directory(&directory)?;
        let identity = root.root_metadata()?.identity();
        Ok(Self {
            root,
            identity,
            state: None,
            effort: self.effort,
        })
    }

    pub(crate) fn catalog_read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        self.root.read_private_file(&path(name)?, limit as u64)
    }

    pub(crate) fn catalog_write(
        &self,
        name: &str,
        bytes: &[u8],
        durability: Durability,
    ) -> io::Result<()> {
        self.root
            .create_private_file(&path(name)?, bytes, durability)
            .map_err(write_error)
    }

    pub(crate) fn catalog_list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in self.root.entries(DirectoryScope::Root)? {
            let entry = entry?;
            // Private lease directories and foreign entries are never garbage.
            if entry.metadata().kind() != NodeKind::File {
                continue;
            }
            if names.len() >= CATALOG_ENTRIES_LIMIT {
                return Err(invalid("Too many catalog files"));
            }
            if let Ok(name) = std::str::from_utf8(entry.name())
                && crate::catalog::validate_name(name).is_ok()
            {
                names.push(name.to_owned());
            }
        }
        Ok(names)
    }

    pub(crate) fn catalog_remove(&self, name: &str) -> io::Result<()> {
        let path = path(name)?;
        match self.root.metadata(&path) {
            Ok(metadata) if metadata.kind() == NodeKind::File => {
                self.root.remove_private_file(&path).map_err(write_error)
            }
            Ok(_) => Err(invalid("Catalog cleanup refuses non-regular files")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
