use std::io::{self, Read};

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use foldover_types::{Digest, ScanControl, SyncPath};

use super::{DataResidency, EntryMetadata, NodeKind, Root};

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

impl Root {
    /// Hash a regular file's data fork with bounded memory and work limited to
    /// its observed size. Residency/volume policy must precede caller exposure;
    /// this primitive alone does not prevent File Provider materialization.
    pub fn hash_file(
        &self,
        path: &SyncPath,
        expected: &EntryMetadata,
        control: &mut impl FnMut(u64) -> ScanControl,
    ) -> Result<Digest, HashError> {
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
        let mut file = parent.open_with(&name, &options)?.into_std();
        if EntryMetadata::from_file(&file)? != before {
            return Err(HashError::Changed);
        }
        let mut hasher = blake3::Hasher::new();
        let mut buffer = [0; HASH_BUFFER_BYTES];
        let mut total = 0;
        while total < before.size() {
            check_control(control, total)?;
            let remaining = usize::try_from(before.size() - total).unwrap_or(usize::MAX);
            let requested = remaining.min(buffer.len());
            let count = match file.read(&mut buffer[..requested]) {
                Ok(0) => return Err(HashError::Changed),
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.into()),
            };
            hasher.update(&buffer[..count]);
            total += count as u64;
        }
        check_control(control, total)?;
        let mut extra = [0];
        if file.read(&mut extra)? != 0
            || EntryMetadata::from_file(&file)? != before
            || self.metadata(path)? != before
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
}
