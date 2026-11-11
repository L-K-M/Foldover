use std::io;
use std::sync::Arc;

use foldover_net::sftp::FileKind;
use foldover_types::{Durability, PairId};

use super::{PRIVATE_DIRECTORY, PRIVATE_FILE, SftpEndpoint, read_small_on};
use crate::catalog::{CATALOG_ENTRIES_LIMIT, DIRECTORY_MARKER, invalid, other};

impl SftpEndpoint {
    pub(crate) fn catalog_endpoint(
        &self,
        pair: PairId,
        durability: Durability,
    ) -> io::Result<Self> {
        if durability == Durability::Full {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SFTP cannot guarantee full durability",
            ));
        }
        let directory = format!(".foldover/state/{pair}");
        self.make_directories(directory.as_bytes(), PRIVATE_DIRECTORY)?;
        let connect = self.connect.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Catalog requires a reconnectable SFTP endpoint",
            )
        })?;
        let mut child =
            Self::with_session(connect()?, &self.remote(directory.as_bytes())).map_err(other)?;
        child.connect = Some(Arc::clone(connect));
        child.server = self.server.clone();
        child.effort = self.effort;
        let format = match child.catalog_read("format", 64)? {
            Some(bytes) => Some(bytes),
            None => {
                if let Err(error) = child.catalog_write("format", DIRECTORY_MARKER, durability)
                    && error.kind() != io::ErrorKind::AlreadyExists
                {
                    return Err(error);
                }
                child.catalog_read("format", 64)?
            }
        };
        if format.as_deref() != Some(DIRECTORY_MARKER) {
            return Err(invalid("Unrecognized target catalog directory"));
        }
        Ok(child)
    }

    pub(crate) fn catalog_read(&self, name: &str, limit: usize) -> io::Result<Option<Vec<u8>>> {
        crate::catalog::validate_name(name)?;
        let remote = self.remote(name.as_bytes());
        let mut session = self.sessions.borrow_once(self.connect.as_ref())?;
        match session.lstat(&remote) {
            Ok(attributes)
                if attributes.kind() == FileKind::File
                    && attributes.size.is_some_and(|size| size <= limit as u64) => {}
            Ok(_) => return Err(invalid("Expected a bounded regular catalog file")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        read_small_on(&mut session, &remote, limit)
    }

    pub(crate) fn catalog_write(
        &self,
        name: &str,
        bytes: &[u8],
        durability: Durability,
    ) -> io::Result<()> {
        crate::catalog::validate_name(name)?;
        if durability == Durability::Full {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SFTP cannot guarantee full durability",
            ));
        }
        let remote = self.remote(name.as_bytes());
        let mut session = self.sessions.borrow_once(self.connect.as_ref())?;
        match session.lstat(&remote) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Catalog file already exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let pending =
            self.remote(format!("pending-{}.tmp", uuid::Uuid::new_v4().simple()).as_bytes());
        let handle = session.create_new(&pending, PRIVATE_FILE)?;
        let written = session.write_all(&handle, &mut &bytes[..]).and_then(|_| {
            // Standard SFTP durability uses fsync where the server offers it.
            // No SFTP v3 operation can flush the containing directory.
            match session.sync(&handle) {
                Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(()),
                result => result,
            }
        });
        let closed = session.close(handle);
        if let Err(error) = written.and(closed) {
            let _ = session.remove_file(&pending);
            return Err(error);
        }
        // The final name must not exist. No atomic-replace extension is needed.
        session.rename(&pending, &remote)
    }

    pub(crate) fn catalog_list(&self) -> io::Result<Vec<String>> {
        let entries = self
            .sessions
            .borrow_once(self.connect.as_ref())?
            .read_dir_bounded(
                &self.root,
                CATALOG_ENTRIES_LIMIT,
                CATALOG_ENTRIES_LIMIT * 128,
            )?;
        Ok(entries
            .into_iter()
            .filter_map(|(name, attributes)| {
                if attributes.kind() != FileKind::File {
                    return None;
                }
                let name = String::from_utf8(name).ok()?;
                crate::catalog::validate_name(&name).ok()?;
                Some(name)
            })
            .collect())
    }

    pub(crate) fn catalog_remove(&self, name: &str) -> io::Result<()> {
        crate::catalog::validate_name(name)?;
        let remote = self.remote(name.as_bytes());
        let mut session = self.sessions.borrow_once(self.connect.as_ref())?;
        match session.lstat(&remote) {
            Ok(attributes) if attributes.kind() == FileKind::File => session.remove_file(&remote),
            Ok(_) => Err(invalid("Catalog cleanup refuses non-regular files")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
