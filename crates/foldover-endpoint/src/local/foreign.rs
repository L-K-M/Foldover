//! A local root's half of copies to and from other kinds of endpoint,
//! such as SFTP servers. Those carry data, mode and modification time
//! only; the same verification and staging rules apply.

use foldover_fs::{Chunks, ForeignMetadata, Placement, WriteError};
use foldover_types::{Digest, EntryContent, EntryVersion, SyncPath};

use super::{
    ApplyError, Expected, LocalEndpoint, LocalEntry, ObservedEntry, Payload, RunScope, ScanControl,
    ScanError, entry_version, metadata_version, observed,
};

impl LocalEndpoint {
    /// Prove `path` still holds the planned source entry.
    pub(crate) fn source_entry(
        &self,
        path: &SyncPath,
        expected: &ObservedEntry,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<LocalEntry, ApplyError> {
        self.verified(path, expected, run, control)
            .map_err(|error| match error {
                ApplyError::DestinationChanged(_) | ApplyError::Scan(ScanError::Changed(_)) => {
                    ApplyError::SourceChanged(path.clone())
                }
                error => error,
            })
    }

    pub(crate) fn foreign_metadata(entry: &LocalEntry) -> ForeignMetadata {
        ForeignMetadata {
            permissions: entry.metadata.permissions(),
            modified: entry.metadata.modified(),
        }
    }

    /// Push a verified file's data to `sink` and return its digest.
    pub(crate) fn read_entry(
        &self,
        entry: &LocalEntry,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> Result<Digest, ApplyError> {
        let mut checkpoint = |_| control();
        self.root
            .read_observed(&entry.path, &entry.metadata, &mut checkpoint, sink)
            .map_err(|_| ApplyError::SourceChanged(entry.path.clone()))
    }

    /// What `path` holds before receiving: nothing, or the verified entry
    /// the plan expected.
    pub(crate) fn receiving(
        &self,
        path: &SyncPath,
        current: Expected<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<Option<LocalEntry>, ApplyError> {
        match current {
            Expected::Absent => match self.entry_at(path)? {
                None => Ok(None),
                Some(_) => Err(ApplyError::DestinationChanged(path.clone())),
            },
            Expected::Present(current) => self
                .verified(path, current, run, control)
                .map(Some)
                .map_err(|error| match error {
                    ApplyError::Scan(ScanError::Changed(_)) => {
                        ApplyError::DestinationChanged(path.clone())
                    }
                    error => error,
                }),
        }
    }

    /// Receive a file from pushed data. Its content must come out as
    /// `expected`; anything it replaces is archived for the run.
    pub(crate) fn receive_file(
        &self,
        path: &SyncPath,
        destination: Option<&LocalEntry>,
        metadata: ForeignMetadata,
        expected: EntryContent,
        run: &RunScope,
        produce: impl FnOnce(Chunks<'_>) -> std::io::Result<()>,
    ) -> Result<EntryVersion, ApplyError> {
        let options = run.options();
        let staged = self
            .root
            .stage_stream(path, metadata, options, produce)
            .map_err(|source| write_error(path, source))?;
        let staged_metadata = staged
            .metadata()
            .map_err(|source| write_error(path, source.into()))?;
        // The staged file is ours and small in metadata; no checkpoints.
        let extended = staged
            .extended_metadata(&mut |_| ScanControl::Continue)
            .map_err(|source| super::metadata_error(Some(path.clone()), source))?;
        let version = entry_version(
            Payload::File(staged.data_digest()),
            &observed(&staged_metadata, Some(&extended)),
            options.metadata(),
        );
        if version.content() != expected {
            return Err(ApplyError::ContentNotStored(path.clone()));
        }
        self.place_received(staged.into_staged(), path, destination, run)?;
        Ok(version)
    }

    /// Receive a link with exactly `target`.
    pub(crate) fn receive_link(
        &self,
        path: &SyncPath,
        destination: Option<&LocalEntry>,
        target: &[u8],
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let staged = self
            .root
            .stage_symlink(path, target)
            .map_err(|source| write_error(path, source))?;
        self.place_received(staged, path, destination, run)?;
        let placed = self
            .entry_at(path)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        Ok(self.entry_version(&placed, run.options().metadata(), control)?)
    }

    pub(crate) fn receive_directory(
        &self,
        path: &SyncPath,
        metadata: ForeignMetadata,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        self.root
            .create_foreign_directory(path, metadata, run.options())
            .map_err(|source| match source {
                WriteError::Exists => ApplyError::DestinationChanged(path.clone()),
                source => write_error(path, source),
            })?;
        let created = self
            .entry_at(path)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        Ok(self.entry_version(&created, run.options().metadata(), control)?)
    }

    /// Give `path` foreign synced metadata, leaving its content.
    pub(crate) fn receive_metadata(
        &self,
        path: &SyncPath,
        current: &ObservedEntry,
        metadata: ForeignMetadata,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let destination = self
            .receiving(path, Expected::Present(current), run, control)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        if destination.metadata.kind() != super::NodeKind::Symlink {
            self.root
                .update_foreign_metadata(path, &destination.metadata, metadata, run.options())
                .map_err(|source| match source {
                    WriteError::Changed => ApplyError::DestinationChanged(path.clone()),
                    source => write_error(path, source),
                })?;
        }
        let updated = self
            .entry_at(path)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        let mut checkpoint = |_| control();
        let extended = self.optional_extended_metadata(&updated, &mut checkpoint)?;
        Ok(metadata_version(
            current.version.content(),
            &observed(&updated.metadata, extended.as_ref()),
            run.options().metadata(),
        ))
    }

    /// Put a staged entry at `path`: create, or replace the verified
    /// `destination` and archive it.
    fn place_received(
        &self,
        staged: foldover_fs::Staged,
        path: &SyncPath,
        destination: Option<&LocalEntry>,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
        let placement = match destination {
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
        let durability = run.options().durability();
        let displaced = self
            .root
            .place(staged, path, placement, durability)
            .map_err(|source| match source {
                WriteError::Exists | WriteError::Changed => {
                    ApplyError::DestinationChanged(path.clone())
                }
                source => write_error(path, source),
            })?;
        if let Some(displaced) = displaced {
            self.root
                .archive_displaced(displaced, &run.archive_path(path)?, durability)
                .map_err(|source| write_error(path, source))?;
        }
        Ok(())
    }
}

fn write_error(path: &SyncPath, source: WriteError) -> ApplyError {
    ApplyError::Write {
        path: path.clone(),
        source,
    }
}
