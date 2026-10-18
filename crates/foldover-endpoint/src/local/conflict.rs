//! Keeping both versions of a conflict: before the other side's version
//! replaces an entry, a copy of it is saved beside it under a new name.

use foldover_fs::{Placement, WriteError};
use foldover_types::{EntryKind, EntryVersion, PairOptions, SyncPath};

use super::archive::{date_name, sibling_name};
use super::{
    ApplyError, LocalEndpoint, Payload, ScanControl, entry_version, metadata_error, observed,
};

/// Names tried for one copy before giving up.
const COPY_ATTEMPTS: u32 = 100;

impl LocalEndpoint {
    /// Copy the file or link at `path`, which must still be `expected`, to
    /// a free name beside it: `report.txt` becomes
    /// `report (from Folder B, 2026-10-18).txt`, dated by its modification
    /// time in UTC. Nothing is replaced. Returns the copy's path.
    pub fn keep_copy(
        &self,
        path: &SyncPath,
        expected: EntryVersion,
        from: &str,
        options: &PairOptions,
        control: &mut impl FnMut() -> ScanControl,
    ) -> Result<SyncPath, ApplyError> {
        let changed = || ApplyError::SourceChanged(path.clone());
        let entry = self.entry_at(path)?.ok_or_else(changed)?;
        if self.entry_version(&entry, options.metadata(), control)? != expected {
            return Err(changed());
        }
        let label = format!("from {from}, {}", date_name(entry.metadata().modified()));
        let mut free = None;
        for attempt in 1..=COPY_ATTEMPTS {
            let candidate = sibling_name(path, &label, attempt)?;
            if self.entry_at(&candidate)?.is_none() {
                free = Some(candidate);
                break;
            }
        }
        let copy = free.ok_or_else(|| ApplyError::Occupied(path.clone()))?;

        let write_error = |source| ApplyError::Write {
            path: copy.clone(),
            source,
        };
        let staged = match expected.kind() {
            EntryKind::File => {
                let mut checkpoint = |_| control();
                let staged = self
                    .root
                    .stage_file(path, &self.root, entry.metadata(), options, &mut checkpoint)
                    .map_err(|error| match error {
                        WriteError::Source(_) => changed(),
                        error => write_error(error),
                    })?;
                let metadata = staged
                    .metadata()
                    .map_err(|error| write_error(error.into()))?;
                let extended = staged
                    .extended_metadata(&mut checkpoint)
                    .map_err(|source| metadata_error(Some(path.clone()), source))?;
                let version = entry_version(
                    Payload::File(staged.data_digest()),
                    &observed(&metadata, Some(&extended)),
                    options.metadata(),
                );
                if version.content() != expected.content() {
                    return Err(ApplyError::ContentNotStored(copy));
                }
                staged.into_staged()
            }
            EntryKind::Symlink => {
                let target = entry
                    .link_target()
                    .ok_or_else(|| ApplyError::Unsupported(path.clone()))?;
                self.root.stage_symlink(path, target).map_err(write_error)?
            }
            EntryKind::Directory => return Err(ApplyError::Unsupported(path.clone())),
        };
        self.root
            .place(staged, &copy, Placement::Create, options.durability())
            .map_err(|error| match error {
                WriteError::Exists => ApplyError::DestinationChanged(copy.clone()),
                error => write_error(error),
            })?;
        Ok(copy)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn proceed() -> ScanControl {
        ScanControl::Continue
    }

    #[test]
    fn copies_go_beside_the_original_under_a_free_name() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("docs")).unwrap();
        fs::write(directory.path().join("docs/report.txt"), b"mine").unwrap();
        let endpoint = LocalEndpoint::open(directory.path()).unwrap();
        let options = PairOptions::default();
        let path = SyncPath::try_from(b"docs/report.txt".as_slice()).unwrap();
        let observed = endpoint.observe(options.metadata(), &mut proceed).unwrap();
        let version = observed.entries()[&path].version;
        let copy = |endpoint: &LocalEndpoint| {
            endpoint
                .keep_copy(&path, version, "Folder B", &options, &mut proceed)
                .unwrap()
        };

        let first = copy(&endpoint);
        let date = date_name(
            fs::metadata(directory.path().join("docs/report.txt"))
                .unwrap()
                .modified()
                .unwrap(),
        );
        assert_eq!(
            first.as_bytes(),
            format!("docs/report (from Folder B, {date}).txt").as_bytes()
        );
        let first_name = std::str::from_utf8(first.as_bytes()).unwrap();
        assert_eq!(
            fs::read(directory.path().join(first_name)).unwrap(),
            b"mine"
        );
        let second = copy(&endpoint);
        assert!(
            second
                .as_bytes()
                .ends_with(format!(", {date} 2).txt").as_bytes())
        );

        fs::write(directory.path().join("docs/report.txt"), b"changed").unwrap();
        assert!(matches!(
            endpoint.keep_copy(&path, version, "Folder B", &options, &mut proceed),
            Err(ApplyError::SourceChanged(_))
        ));
    }
}
