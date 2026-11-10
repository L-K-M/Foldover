use std::fs::File;
use std::io::{self, Read};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use foldover_types::{Digest, ScanControl, SyncPath};

use crate::prevent_materialization;

use super::extended::capture_observed_file;
use super::{
    DataResidency, EntryMetadata, ExtendedMetadata, MetadataCapture, MetadataReadError, NodeKind,
    Root,
};

const HASH_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum HashError {
    #[error("Content read failed: {0}")]
    Io(#[from] io::Error),
    #[error("Content hashing requires a regular file")]
    NotRegular,
    #[error("Dataless file requires an explicit materialization policy")]
    Dataless,
    #[error("File changed since its metadata was observed")]
    Changed,
    #[error("Content hashing cancelled; no digest was returned")]
    Cancelled,
}

#[derive(Debug, thiserror::Error)]
pub enum ObserveFileError {
    #[error("Content observation failed: {0}")]
    Hash(#[from] HashError),
    #[error("Extended metadata observation failed: {0}")]
    Metadata(#[from] MetadataReadError),
}

impl Root {
    /// Hash a regular file's data fork with bounded memory and work limited to
    /// its observed size. macOS reads enforce non-materialization on the calling
    /// thread; provider, metadata and volume gates still precede API exposure.
    pub fn hash_file(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<Digest, HashError> {
        let mut file = self.open_observed_file(path, expected, control)?;
        self.stream_observed_file(path, &mut file, expected, control, &mut |_| Ok(()))
    }

    /// Open the observed regular file without following links. The descriptor
    /// must still match `expected`.
    pub(super) fn open_observed_file(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<File, HashError> {
        check_control(control, 0)?;
        if expected.kind() != NodeKind::File {
            return Err(HashError::NotRegular);
        }
        if expected.data_residency() == DataResidency::Dataless {
            return Err(HashError::Dataless);
        }
        let (parent, name) = self.parent_of(path)?;
        let before = EntryMetadata::at(&parent, &name)?;
        if before.kind() != NodeKind::File {
            return Err(HashError::NotRegular);
        }
        if before.data_residency() == DataResidency::Dataless {
            return Err(HashError::Dataless);
        }
        if &before != expected {
            return Err(HashError::Changed);
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        // A file swapped to a FIFO between stat/open must not block the worker.
        options.custom_flags(libc::O_NONBLOCK);
        let file = parent.open_with(&name, &options)?.into_std();
        if EntryMetadata::from_file(&file)? != before {
            return Err(HashError::Changed);
        }
        Ok(file)
    }

    /// Stream exactly the observed size through `sink` and hash it. The file
    /// and its root-relative binding must be unchanged afterwards.
    /// Push an observed regular file's data to `sink` and return its digest.
    /// The file must still match `expected` throughout.
    pub fn read_observed(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
        sink: &mut impl FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Digest, HashError> {
        let mut file = self.open_observed_file(path, expected, control)?;
        self.stream_observed_file(path, &mut file, expected, control, sink)
    }

    /// Observe a regular file's selected extended metadata and data digest
    /// through one already-opened descriptor. The metadata capture runs before
    /// the content stream, and the descriptor and root-relative binding are
    /// checked around both. This is the fused form of `read_extended_metadata`
    /// followed by `stream_observed_file` for a regular file.
    pub fn observe_file(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        capture: MetadataCapture,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<(Digest, ExtendedMetadata), ObserveFileError> {
        let mut file = self.open_observed_file(path, expected, control)?;
        let extended = capture_observed_file(&file, expected, capture, control)?;
        let digest =
            self.stream_observed_file(path, &mut file, expected, control, &mut |_| Ok(()))?;
        Ok((digest, extended))
    }

    pub(super) fn stream_observed_file(
        &self,
        path: &SyncPath,
        file: &mut File,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
        sink: &mut impl FnMut(&[u8]) -> io::Result<()>,
    ) -> Result<Digest, HashError> {
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0; HASH_BUFFER_BYTES];
        let mut total = 0;
        while total < expected.size() {
            check_control(control, total)?;
            prevent_materialization()?;
            let remaining = usize::try_from(expected.size() - total).unwrap_or(usize::MAX);
            let requested = remaining.min(buffer.len());
            let count = match file.read(&mut buffer[..requested]) {
                Ok(0) => return Err(HashError::Changed),
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            hasher.update(&buffer[..count]);
            sink(&buffer[..count])?;
            total += count as u64;
        }
        check_control(control, total)?;
        prevent_materialization()?;
        let mut extra = [0];
        if file.read(&mut extra)? != 0
            || EntryMetadata::from_file(file)? != *expected
            || self.metadata(path)? != *expected
        {
            return Err(HashError::Changed);
        }
        Ok(Digest::from_bytes(*hasher.finalize().as_bytes()))
    }
}

fn check_control(
    control: &mut impl FnMut(u64) -> ScanControl,
    bytes: u64,
) -> Result<(), HashError> {
    match control(bytes) {
        ScanControl::Continue => Ok(()),
        ScanControl::Cancel => Err(HashError::Cancelled),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn path() -> SyncPath {
        SyncPath::try_from(b"file".as_slice()).unwrap()
    }

    fn hex(digest: Digest) -> String {
        digest
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[test]
    fn hashes_official_vectors_and_streams_larger_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = Root::open(directory.path()).unwrap();
        for (content, expected) in [
            (
                b"".as_slice(),
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
            ),
            (
                b"abc",
                "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
            ),
        ] {
            fs::write(directory.path().join("file"), content).unwrap();
            let metadata = root.metadata(&path()).unwrap();
            assert_eq!(
                hex(root
                    .hash_file(&path(), &metadata, &mut |_| ScanControl::Continue)
                    .unwrap()),
                expected
            );
        }
        let content = vec![b'x'; HASH_BUFFER_BYTES * 2 + 9];
        fs::write(directory.path().join("file"), &content).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let mut progress = Vec::new();
        let digest = root
            .hash_file(&path(), &metadata, &mut |bytes| {
                progress.push(bytes);
                ScanControl::Continue
            })
            .unwrap();
        assert_eq!(digest.as_bytes(), blake3::hash(&content).as_bytes());
        assert_eq!(progress.last(), Some(&(content.len() as u64)));
        assert!(
            progress
                .windows(2)
                .all(|pair| pair[1] - pair[0] <= HASH_BUFFER_BYTES as u64)
        );
    }

    #[test]
    fn cancellation_never_yields_a_partial_digest() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("file"),
            vec![b'x'; HASH_BUFFER_BYTES + 1],
        )
        .unwrap();
        let root = Root::open(directory.path()).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        assert!(matches!(
            root.hash_file(&path(), &metadata, &mut |_| ScanControl::Cancel),
            Err(HashError::Cancelled)
        ));
        assert!(matches!(
            root.hash_file(&path(), &metadata, &mut |bytes| if bytes == 0 {
                ScanControl::Continue
            } else {
                ScanControl::Cancel
            }),
            Err(HashError::Cancelled)
        ));
    }

    #[test]
    fn known_dataless_metadata_is_refused_before_opening_content() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, b"abc").unwrap();
        let root = Root::open(directory.path()).unwrap();
        let mut metadata = root.metadata(&path()).unwrap();
        metadata.residency = DataResidency::Dataless;
        fs::remove_file(file).unwrap();
        let result = root.hash_file(&path(), &metadata, &mut |bytes| {
            assert_eq!(bytes, 0, "Dataless files must never reach a content read");
            ScanControl::Continue
        });
        assert!(matches!(result, Err(HashError::Dataless)));
    }

    #[test]
    fn stale_metadata_and_changes_during_read_are_errors() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, vec![b'x'; HASH_BUFFER_BYTES + 1]).unwrap();
        let root = Root::open(directory.path()).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let result = root.hash_file(&path(), &metadata, &mut |bytes| {
            if bytes > 0 {
                fs::write(&file, b"shortened").unwrap();
            }
            ScanControl::Continue
        });
        assert!(matches!(result, Err(HashError::Changed)));
        assert!(matches!(
            root.hash_file(&path(), &metadata, &mut |_| ScanControl::Continue),
            Err(HashError::Changed)
        ));
    }

    #[test]
    fn a_growing_file_cannot_extend_the_observed_read_budget() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        let size = HASH_BUFFER_BYTES + 1;
        fs::write(&file, vec![b'x'; size]).unwrap();
        let root = Root::open(directory.path()).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let mut maximum = 0;
        let result = root.hash_file(&path(), &metadata, &mut |bytes| {
            maximum = maximum.max(bytes);
            if bytes > 0 {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&file)
                    .unwrap()
                    .write_all(&[b'y'; HASH_BUFFER_BYTES])
                    .unwrap();
            }
            ScanControl::Continue
        });
        assert!(matches!(result, Err(HashError::Changed)));
        assert_eq!(maximum, size as u64);
    }

    #[test]
    fn refuses_links_fifos_and_rebound_paths_without_following_them() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, vec![b'x'; HASH_BUFFER_BYTES + 1]).unwrap();
        fs::write(outside.path().join("secret"), b"outside").unwrap();
        let root = Root::open(directory.path()).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let mut rebound = false;
        let result = root.hash_file(&path(), &metadata, &mut |bytes| {
            if bytes > 0 && !rebound {
                fs::remove_file(&file).unwrap();
                symlink(outside.path().join("secret"), &file).unwrap();
                rebound = true;
            }
            ScanControl::Continue
        });
        assert!(matches!(result, Err(HashError::Changed)));
        assert!(matches!(
            root.hash_file(&path(), &metadata, &mut |_| ScanControl::Continue),
            Err(HashError::NotRegular)
        ));
        fs::remove_file(&file).unwrap();
        nix::unistd::mkfifo(
            &file,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(matches!(
            root.hash_file(&path(), &metadata, &mut |_| ScanControl::Continue),
            Err(HashError::NotRegular)
        ));
    }

    #[test]
    fn held_root_reads_its_original_file_after_an_ambient_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().join("selected");
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("file"), b"abc").unwrap();
        let root = Root::open(&selected).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        fs::rename(&selected, directory.path().join("moved")).unwrap();
        fs::create_dir(&selected).unwrap();
        fs::write(selected.join("file"), b"new").unwrap();
        assert_eq!(
            hex(root
                .hash_file(&path(), &metadata, &mut |_| ScanControl::Continue)
                .unwrap()),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
        );
    }

    #[test]
    fn fused_observation_matches_separate_content_and_metadata_reads() {
        use super::super::{AccessControl, AttributeRole, CaptureRoles};

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, b"fused payload").unwrap();
        {
            let handle = fs::File::open(&file).unwrap();
            rustix::fs::fsetxattr(
                &handle,
                c"user.foldover.tag",
                b"tag",
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
            #[cfg(target_os = "macos")]
            rustix::fs::fsetxattr(
                &handle,
                c"com.apple.ResourceFork",
                b"fork bytes",
                rustix::fs::XattrFlags::empty(),
            )
            .unwrap();
        }
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("/bin/chmod")
                .args([
                    "+a",
                    "everyone allow read,readattr,readextattr,readsecurity",
                ])
                .arg(&file)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let root = Root::open(directory.path()).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let separate_digest = root
            .hash_file(&path(), &metadata, &mut |_| ScanControl::Continue)
            .unwrap();

        for capture in [
            MetadataCapture::Full,
            MetadataCapture::Values(CaptureRoles::ALL),
            MetadataCapture::ValuesAndAccessControl(CaptureRoles::ALL),
        ] {
            let (digest, extended) = root
                .observe_file(&path(), &metadata, capture, &mut |_| ScanControl::Continue)
                .unwrap();
            let separate = root
                .read_extended_metadata(&path(), &metadata, capture, &mut |_| ScanControl::Continue)
                .unwrap();
            assert_eq!(digest, separate_digest, "capture {capture:?}");
            assert_eq!(extended, separate, "capture {capture:?}");
        }

        let full = root
            .read_extended_metadata(&path(), &metadata, MetadataCapture::Full, &mut |_| {
                ScanControl::Continue
            })
            .unwrap();
        assert!(
            full.attributes()
                .iter()
                .any(|attribute| attribute.name() == b"user.foldover.tag")
        );
        #[cfg(target_os = "macos")]
        {
            assert!(
                full.attributes()
                    .iter()
                    .any(|attribute| attribute.name() == b"com.apple.ResourceFork")
            );
            assert!(matches!(
                full.access_control(),
                AccessControl::MacOs(Some(bytes)) if !bytes.is_empty()
            ));
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(full.access_control(), &AccessControl::NotReported);

        // A filtered capture must skip roles its policy cannot use, and the
        // fused result must equal a separate filtered read.
        let filtered = MetadataCapture::Values(CaptureRoles::new(&[AttributeRole::Synced]));
        let (digest, extended) = root
            .observe_file(&path(), &metadata, filtered, &mut |_| ScanControl::Continue)
            .unwrap();
        assert_eq!(digest, separate_digest);
        assert_eq!(
            extended,
            root.read_extended_metadata(&path(), &metadata, filtered, &mut |_| {
                ScanControl::Continue
            })
            .unwrap()
        );
        assert_eq!(extended.access_control(), &AccessControl::NotReported);
        assert!(
            extended
                .attributes()
                .iter()
                .all(|attribute| attribute.name() == b"user.foldover.tag")
        );
        assert!(
            !extended
                .attributes()
                .iter()
                .any(|attribute| attribute.name() == b"com.apple.ResourceFork")
        );
    }

    #[test]
    fn fused_observation_handles_zero_length_and_rejects_replacement_and_edits() {
        use super::super::CaptureRoles;

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, b"").unwrap();
        let root = Root::open(directory.path()).unwrap();
        let capture = MetadataCapture::Values(CaptureRoles::NONE);
        let metadata = root.metadata(&path()).unwrap();
        let (digest, extended) = root
            .observe_file(&path(), &metadata, capture, &mut |_| ScanControl::Continue)
            .unwrap();
        assert_eq!(digest.as_bytes(), blake3::hash(b"").as_bytes());
        assert_eq!(
            extended,
            root.read_extended_metadata(&path(), &metadata, capture, &mut |_| {
                ScanControl::Continue
            })
            .unwrap()
        );

        let size = HASH_BUFFER_BYTES + 1;

        // Replace the path while the content stream is underway. The held
        // descriptor keeps the original bytes, but the observation must fail.
        fs::write(&file, vec![b'x'; size]).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let mut swapped = false;
        let result = root.observe_file(&path(), &metadata, capture, &mut |bytes| {
            if bytes > 0 && !swapped {
                swapped = true;
                fs::remove_file(&file).unwrap();
                fs::write(&file, b"a different size").unwrap();
            }
            ScanControl::Continue
        });
        assert!(matches!(
            result,
            Err(ObserveFileError::Hash(HashError::Changed))
        ));

        // Unlinking the observed path changes the held inode's link count, so
        // the descriptor consistency check fails before content is read.
        fs::write(&file, vec![b'x'; size]).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let mut calls = 0;
        let result = root.observe_file(&path(), &metadata, capture, &mut |_| {
            calls += 1;
            if calls == 2 {
                fs::remove_file(&file).unwrap();
            }
            ScanControl::Continue
        });
        assert!(matches!(
            result,
            Err(ObserveFileError::Metadata(MetadataReadError::Changed))
        ));

        // Growing the file during the content stream cannot extend its budget.
        fs::write(&file, vec![b'x'; size]).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let mut maximum = 0;
        let result = root.observe_file(&path(), &metadata, capture, &mut |bytes| {
            maximum = maximum.max(bytes);
            if bytes > 0 {
                use std::io::Write;
                let mut handle = fs::OpenOptions::new().append(true).open(&file).unwrap();
                handle.write_all(&[b'y'; HASH_BUFFER_BYTES]).unwrap();
            }
            ScanControl::Continue
        });
        assert!(matches!(
            result,
            Err(ObserveFileError::Hash(HashError::Changed))
        ));
        assert_eq!(maximum, size as u64);

        // Shrinking it during the content stream leaves the budget unreachable.
        fs::write(&file, vec![b'x'; size]).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let result = root.observe_file(&path(), &metadata, capture, &mut |bytes| {
            if bytes > 0 {
                fs::write(&file, b"short").unwrap();
            }
            ScanControl::Continue
        });
        assert!(matches!(
            result,
            Err(ObserveFileError::Hash(HashError::Changed))
        ));
    }

    #[test]
    fn fused_observation_cancellation_never_yields_a_partial_result() {
        use super::super::{AttributeRole, CaptureRoles};

        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, vec![b'x'; HASH_BUFFER_BYTES + 1]).unwrap();
        let root = Root::open(directory.path()).unwrap();
        let metadata = root.metadata(&path()).unwrap();
        let capture = MetadataCapture::Values(CaptureRoles::NONE);

        // Cancelled before the descriptor is opened.
        assert!(matches!(
            root.observe_file(&path(), &metadata, capture, &mut |_| ScanControl::Cancel),
            Err(ObserveFileError::Hash(HashError::Cancelled))
        ));
        // Cancelled once the content stream is underway.
        assert!(matches!(
            root.observe_file(&path(), &metadata, capture, &mut |bytes| {
                if bytes > 0 {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            }),
            Err(ObserveFileError::Hash(HashError::Cancelled))
        ));

        // Cancelled while capturing a wanted value.
        let handle = fs::File::open(&file).unwrap();
        rustix::fs::fsetxattr(
            &handle,
            c"user.foldover.cancel",
            b"value",
            rustix::fs::XattrFlags::empty(),
        )
        .unwrap();
        drop(handle);
        let metadata = root.metadata(&path()).unwrap();
        let capture = MetadataCapture::Values(CaptureRoles::new(&[AttributeRole::Synced]));
        assert!(matches!(
            root.observe_file(&path(), &metadata, capture, &mut |bytes| {
                if bytes > 0 {
                    ScanControl::Cancel
                } else {
                    ScanControl::Continue
                }
            }),
            Err(ObserveFileError::Metadata(MetadataReadError::Cancelled))
        ));
    }
}
