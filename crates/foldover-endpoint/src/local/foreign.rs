//! A local root's half of copies to and from other kinds of endpoint,
//! such as SFTP servers. Those carry data, mode and modification time
//! only; the same verification and staging rules apply.

use foldover_fs::{Chunks, ForeignMetadata, NativeMetadata, Placement, Resume, WriteError};
use foldover_types::{Digest, EntryContent, EntryVersion, SyncPath};

use super::{
    ApplyError, Expected, LocalEndpoint, LocalEntry, ObservedEntry, Payload, RunScope, ScanControl,
    ScanError, entry_version, metadata_version, observed,
};

/// What a streamed copy carries besides data: mode and time, and the
/// attributes another Mac sends; None from sources without any, such as
/// SFTP servers, which leaves the receiving entry's own.
#[derive(Clone, Copy)]
pub(crate) struct Carried<'a> {
    pub(crate) metadata: ForeignMetadata,
    pub(crate) native: Option<&'a NativeMetadata>,
}

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

    /// What a copy to another Mac carries of a verified entry besides data,
    /// mode and time.
    pub(crate) fn native_metadata(
        &self,
        entry: &LocalEntry,
        policy: foldover_types::MetadataPolicy,
    ) -> Result<NativeMetadata, ApplyError> {
        self.root
            .native_metadata(&entry.path, &entry.metadata, policy)
            .map_err(|source| match source {
                WriteError::Changed => ApplyError::SourceChanged(entry.path.clone()),
                source => write_error(&entry.path, source),
            })
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

    /// Push the entry's data from byte `from` on, for a copy that resumes.
    /// The skipped bytes are still read: the whole file must still be the
    /// observed one.
    pub(crate) fn read_entry_from(
        &self,
        entry: &LocalEntry,
        from: u64,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut impl FnMut(&[u8]) -> std::io::Result<()>,
    ) -> Result<(), ApplyError> {
        let mut skipped = 0u64;
        self.read_entry(entry, control, &mut |chunk| {
            let skip = usize::try_from(from - skipped)
                .unwrap_or(usize::MAX)
                .min(chunk.len());
            skipped += skip as u64;
            match &chunk[skip..] {
                [] => Ok(()),
                rest => sink(rest),
            }
        })
        .map(|_| ())
    }

    /// How much of the file with `content` a broken-off copy here kept.
    pub(crate) fn partial_length(&self, content: &Digest) -> Result<u64, ApplyError> {
        self.root
            .partial_length(content)
            .map_err(|source| ScanError::Io { path: None, source }.into())
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

    /// Receive a file from pushed data, after what `resume` takes from a
    /// kept partial. Its content must come out as `resume.content`;
    /// anything it replaces is archived for the run.
    pub(crate) fn receive_file(
        &self,
        path: &SyncPath,
        destination: Option<&LocalEntry>,
        carried: Carried<'_>,
        resume: Resume,
        run: &RunScope,
        produce: impl FnOnce(Chunks<'_>) -> std::io::Result<()>,
    ) -> Result<EntryVersion, ApplyError> {
        let options = run.options();
        let staged = self
            .root
            .stage_resumable(
                path,
                resume,
                carried.metadata,
                carried.native,
                options,
                produce,
            )
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
        // A resumed file is hashed whole, so a bad kept part ends here
        // with the staged file, and the next copy starts over.
        if version.content() != EntryContent::File(resume.content) {
            return Err(ApplyError::ContentNotStored(path.clone()));
        }
        // The data arrived whole and checked out. If the file it replaces
        // changed meanwhile, it is kept for the next attempt to place.
        let placement = match self.placement(path, destination) {
            Ok(placement) => placement,
            Err(error) => {
                let _ = self.root.keep_staged(
                    staged.into_staged(),
                    &resume.content,
                    staged_metadata.size(),
                );
                return Err(error);
            }
        };
        self.place(staged.into_staged(), path, placement, run)?;
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
        carried: Carried<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        self.root
            .create_foreign_directory(path, carried.metadata, carried.native, run.options())
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
        carried: Carried<'_>,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<EntryVersion, ApplyError> {
        let destination = self
            .receiving(path, Expected::Present(current), run, control)?
            .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
        if destination.metadata.kind() != super::NodeKind::Symlink {
            self.root
                .update_foreign_metadata(
                    path,
                    &destination.metadata,
                    carried.metadata,
                    carried.native,
                    run.options(),
                )
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
        let placement = self.placement(path, destination)?;
        self.place(staged, path, placement, run)
    }

    /// How to place at `path` while it still holds what was verified:
    /// beside nothing, or over `destination` unchanged.
    fn placement(
        &self,
        path: &SyncPath,
        destination: Option<&LocalEntry>,
    ) -> Result<Placement, ApplyError> {
        let Some(destination) = destination else {
            return Ok(Placement::Create);
        };
        if self.entry_at(path)?.map(|entry| entry.metadata) != Some(destination.metadata.clone()) {
            return Err(ApplyError::DestinationChanged(path.clone()));
        }
        Ok(Placement::Replace)
    }

    fn place(
        &self,
        staged: foldover_fs::Staged,
        path: &SyncPath,
        placement: Placement,
        run: &RunScope,
    ) -> Result<(), ApplyError> {
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
