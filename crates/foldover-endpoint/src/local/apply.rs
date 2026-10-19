//! Executing one planned change on a local endpoint. Every operation first
//! proves that both sides still hold what the plan observed, then writes
//! through the driver's staged, no-loss primitives (ADR 0019).
//!
//! ```text
//! verify source ─▶ verify destination ─▶ stage ─▶ check staged content
//!                                      ─▶ recheck destination ─▶ place
//!                                      ─▶ archive what was displaced
//! ```

use std::time::SystemTime;

use foldover_fs::{Placement, WriteError};
use foldover_types::{
    EntryContent, EntryKind, EntryVersion, InvalidSyncPath, PairOptions, SyncPath,
};

use super::archive::{ARCHIVE_ROOT, archive_name};
use super::{
    LocalEndpoint, LocalEntry, NodeKind, ObservedEntry, Payload, ScanControl, ScanError,
    entry_version, metadata_version, observed,
};

/// What the plan observed on one side of a path.
#[derive(Clone, Copy, Debug)]
pub enum Expected<'a> {
    Absent,
    Present(&'a ObservedEntry),
}

/// Where one run's displaced and removed entries go, and how it writes.
#[derive(Clone, Debug)]
pub struct RunScope {
    name: String,
    archive: SyncPath,
    options: PairOptions,
}

impl RunScope {
    /// The archive folder is named after `started`.
    pub fn new(started: SystemTime, options: PairOptions) -> Self {
        let name = archive_name(started);
        let mut archive = ARCHIVE_ROOT.to_vec();
        archive.push(b'/');
        archive.extend_from_slice(name.as_bytes());
        Self {
            name,
            archive: SyncPath::try_from(archive).expect("run names are single path components"),
            options,
        }
    }

    /// The run's folder under `.foldover/archive`.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn options(&self) -> &PairOptions {
        &self.options
    }

    pub(crate) fn archive_path(&self, path: &SyncPath) -> Result<SyncPath, InvalidSyncPath> {
        let mut bytes = self.archive.as_bytes().to_vec();
        bytes.push(b'/');
        bytes.extend_from_slice(path.as_bytes());
        SyncPath::try_from(bytes)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error("Write failed at {path:?}: {source}")]
    Write {
        path: SyncPath,
        #[source]
        source: WriteError,
    },
    #[error("Source changed since the preview: {0:?}")]
    SourceChanged(SyncPath),
    #[error("Destination changed since the preview: {0:?}")]
    DestinationChanged(SyncPath),
    #[error("Destination did not store the content exactly, e.g. a resource fork: {0:?}")]
    ContentNotStored(SyncPath),
    #[error("Folder gained entries since the preview: {0:?}")]
    NotEmpty(SyncPath),
    #[error("No free name was found to put {0:?} back beside")]
    Occupied(SyncPath),
    #[error("The archive no longer holds {0:?}")]
    NotArchived(SyncPath),
    #[error("Operation does not apply to this entry kind: {0:?}")]
    Unsupported(SyncPath),
    #[error("Full durability cannot be guaranteed on this folder: {0:?}")]
    DurabilityUnsupported(SyncPath),
    #[error(transparent)]
    InvalidPath(#[from] InvalidSyncPath),
}

impl LocalEndpoint {
    /// Make `path` hold the source's entry and return the version stored
    /// here. Anything replaced is archived for this run.
    pub fn copy_from(
        &self,
        source: &LocalEndpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: Expected<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let policy = run.options.metadata();
        let source_entry = source
            .verified(path, from, run, control)
            .map_err(|error| changed_as(error, ApplyError::SourceChanged(path.clone())))?;
        let destination = match current {
            Expected::Absent => {
                if self.entry_at(path)?.is_some() {
                    return Err(ApplyError::DestinationChanged(path.clone()));
                }
                None
            }
            Expected::Present(current) => Some(
                self.verified(path, current, run, control)
                    .map_err(|error| {
                        changed_as(error, ApplyError::DestinationChanged(path.clone()))
                    })?,
            ),
        };

        let (staged, version) = match from.version.kind() {
            EntryKind::Directory => {
                if destination.is_some() {
                    return Err(ApplyError::Unsupported(path.clone()));
                }
                self.root
                    .create_directory(path, &source.root, source_entry.metadata(), &run.options)
                    .map_err(|error| write_error(path, error))?;
                let created = self
                    .entry_at(path)?
                    .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
                return Ok(self.entry_version(&created, policy, control)?);
            }
            EntryKind::File => {
                let mut checkpoint = |_| control();
                let staged = self
                    .root
                    .stage_file(
                        path,
                        &source.root,
                        source_entry.metadata(),
                        &run.options,
                        &mut checkpoint,
                    )
                    .map_err(|error| match error {
                        WriteError::Source(_) => ApplyError::SourceChanged(path.clone()),
                        error => write_error(path, error),
                    })?;
                let metadata = staged
                    .metadata()
                    .map_err(|error| write_error(path, error.into()))?;
                let extended = staged
                    .extended_metadata(&mut checkpoint)
                    .map_err(|source| super::metadata_error(Some(path.clone()), source))?;
                let version = entry_version(
                    Payload::File(staged.data_digest()),
                    &observed(&metadata, Some(&extended)),
                    policy,
                );
                (staged.into_staged(), Some(version))
            }
            EntryKind::Symlink => {
                let target = source_entry
                    .link_target()
                    .ok_or_else(|| ApplyError::Unsupported(path.clone()))?;
                let staged = self
                    .root
                    .stage_symlink(path, target)
                    .map_err(|error| write_error(path, error))?;
                (staged, None)
            }
        };
        if let Some(version) = version
            && version.content() != from.version.content()
        {
            return Err(ApplyError::ContentNotStored(path.clone()));
        }

        // The cheap stat recheck narrows the window since verification.
        let placement = match &destination {
            None => Placement::Create,
            Some(destination) => {
                if self.entry_at(path)?.map(|entry| entry.metadata)
                    != Some(destination.metadata.clone())
                {
                    return Err(ApplyError::DestinationChanged(path.clone()));
                }
                Placement::Replace
            }
        };
        let durability = run.options.durability();
        let displaced = self
            .root
            .place(staged, path, placement, durability)
            .map_err(|error| match error {
                WriteError::Exists | WriteError::Changed => {
                    ApplyError::DestinationChanged(path.clone())
                }
                error => write_error(path, error),
            })?;
        if let Some(displaced) = displaced {
            self.root
                .archive_displaced(displaced, &run.archive_path(path)?, durability)
                .map_err(|error| write_error(path, error))?;
        }

        match version {
            Some(version) => Ok(version),
            // Links carry no copied metadata; record what was placed.
            None => {
                let placed = self
                    .entry_at(path)?
                    .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
                Ok(self.entry_version(&placed, policy, control)?)
            }
        }
    }

    /// Give `path` the source's synced metadata, leaving content alone, and
    /// return the version stored here.
    pub fn update_metadata_from(
        &self,
        source: &LocalEndpoint,
        path: &SyncPath,
        from: &ObservedEntry,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        if from.version.content() != current.version.content() {
            return Err(ApplyError::Unsupported(path.clone()));
        }
        let source_entry = source
            .verified(path, from, run, control)
            .map_err(|error| changed_as(error, ApplyError::SourceChanged(path.clone())))?;
        let destination = self
            .verified(path, current, run, control)
            .map_err(|error| changed_as(error, ApplyError::DestinationChanged(path.clone())))?;
        if destination.metadata.kind() == NodeKind::Symlink {
            // Link metadata is not copied; the content already matches.
            return Ok(current.version);
        }
        self.root
            .update_metadata(
                path,
                destination.metadata(),
                &source.root,
                source_entry.metadata(),
                &run.options,
            )
            .map_err(|error| match error {
                WriteError::Changed => ApplyError::DestinationChanged(path.clone()),
                error => write_error(path, error),
            })?;
        let updated = self
            .entry_at(path)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        let mut checkpoint = |_| control();
        let extended = self.optional_extended_metadata(&updated, &mut checkpoint)?;
        Ok(metadata_version(
            current.version.content(),
            &observed(&updated.metadata, extended.as_ref()),
            run.options.metadata(),
        ))
    }

    /// Archive the entry at `path` for this run; folders are removed only
    /// when empty.
    pub fn remove(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<(), ApplyError> {
        let entry = self
            .verified(path, current, run, control)
            .map_err(|error| changed_as(error, ApplyError::DestinationChanged(path.clone())))?;
        let durability = run.options.durability();
        let result = match current.version.content() {
            EntryContent::Directory => {
                self.root
                    .remove_directory(path, entry.metadata(), durability)
            }
            _ => self
                .root
                .archive(path, entry.metadata(), &run.archive_path(path)?, durability),
        };
        result.map_err(|error| match error {
            WriteError::NotEmpty => ApplyError::NotEmpty(path.clone()),
            WriteError::Changed => ApplyError::DestinationChanged(path.clone()),
            error => write_error(path, error),
        })
    }

    /// Archive a temporary an interrupted write left behind. It may hold a
    /// displaced original, so it is never deleted.
    pub fn recover_leftover(&self, leftover: &SyncPath, run: &RunScope) -> Result<(), ApplyError> {
        let metadata = self
            .root
            .metadata(leftover)
            .map_err(|source| ScanError::Io {
                path: Some(leftover.clone()),
                source,
            })?;
        self.root
            .archive(
                leftover,
                &metadata,
                &run.archive_path(leftover)?,
                run.options.durability(),
            )
            .map_err(|error| write_error(leftover, error))
    }

    /// Re-observe `path` and prove it still holds the planned entry. An
    /// unchanged fingerprint proves it without rereading data.
    pub(crate) fn verified(
        &self,
        path: &SyncPath,
        expected: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<LocalEntry, ApplyError> {
        let changed = || ApplyError::DestinationChanged(path.clone());
        let entry = self.entry_at(path)?.ok_or_else(changed)?;
        if expected.fingerprint.proves_unchanged(&entry.metadata) {
            return Ok(entry);
        }
        if self.entry_version(&entry, run.options.metadata(), control)? != expected.version {
            return Err(changed());
        }
        Ok(entry)
    }
}

fn changed_as(error: ApplyError, changed: ApplyError) -> ApplyError {
    match error {
        ApplyError::DestinationChanged(_) | ApplyError::Scan(ScanError::Changed(_)) => changed,
        error => error,
    }
}

fn write_error(path: &SyncPath, source: WriteError) -> ApplyError {
    ApplyError::Write {
        path: path.clone(),
        source,
    }
}

#[cfg(test)]
mod tests;
