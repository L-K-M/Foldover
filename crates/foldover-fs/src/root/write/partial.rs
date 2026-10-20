//! Data a broken-off transfer already wrote, kept so the next attempt
//! receives only the rest (PLAN.md §4.9). One file per content digest:
//!
//! ```text
//! .foldover/partial/<digest hex>    the first bytes of that content
//! ```
//!
//! A resumed file is hashed whole before it is placed, so a partial that
//! holds anything else fails the copy instead of corrupting it; the copy
//! then starts over.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use cap_std::fs::Dir;
use foldover_types::{Digest, PairOptions, SyncPath};
use rustix::fs::{AtFlags, Mode, OFlags};
use rustix::io::Errno;

use super::{
    Chunks, ForeignMetadata, NativeMetadata, Parents, StagedFile, WriteError,
    create_temporary_file, flush, metadata, temporary_name,
};
use crate::root::{EntryMetadata, NodeKind, Root};

const PARTIAL_DIRECTORY: &[u8] = b".foldover/partial";
/// A transfer that broke off with less than this written starts over.
pub const PARTIAL_MIN_BYTES: u64 = 16 * 1024 * 1024;

/// Which content a staged file will hold, and how much of it a kept
/// partial already has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Resume {
    pub content: Digest,
    /// Bytes to take from the partial; 0 starts afresh.
    pub from: u64,
}

impl Root {
    /// How much of `content` a broken-off transfer kept; 0 if nothing.
    pub fn partial_length(&self, content: &Digest) -> io::Result<u64> {
        let (directory, name) = match self.partial_directory() {
            Ok(directory) => (directory, partial_name(content)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        match EntryMetadata::at(&directory, &name) {
            Ok(metadata) if metadata.kind() == NodeKind::File => Ok(metadata.size()),
            Ok(_) => Ok(0),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        }
    }

    /// Stage a file for `path` as `stage_stream` does, continuing from the
    /// bytes `resume` takes from a kept partial. If `produce` fails after
    /// enough data, what was written is kept for the next attempt.
    pub fn stage_resumable(
        &self,
        path: &SyncPath,
        resume: Resume,
        metadata: ForeignMetadata,
        native: Option<&NativeMetadata>,
        options: &PairOptions,
        produce: impl FnOnce(Chunks<'_>) -> io::Result<()>,
    ) -> Result<StagedFile, WriteError> {
        let Resume { content, from } = resume;
        let content = &content;
        let (parent, _) = self.parent_of(path)?;
        let mode = super::file_mode(options);
        let (mut staged, mut file) = match from {
            0 => create_temporary_file(parent, mode)?,
            from => self.adopt_partial(content, parent, mode, from)?,
        };

        // The kept bytes count towards the digest the plan checks.
        let mut hasher = blake3::Hasher::new();
        if from > 0 {
            file.seek(SeekFrom::Start(0))?;
            let kept = io::copy(&mut io::Read::take(&file, from), &mut hasher)?;
            if kept != from {
                return Err(WriteError::Changed);
            }
            file.seek(SeekFrom::Start(from))?;
        }
        let mut written = from;
        let produced = produce(&mut |chunk| {
            hasher.update(chunk);
            file.write_all(chunk)?;
            written += chunk.len() as u64;
            Ok(())
        });
        if let Err(error) = produced {
            if written >= PARTIAL_MIN_BYTES
                && self
                    .keep_partial(&staged.parent, &staged.name, content)
                    .is_ok()
            {
                staged.placed = true;
            }
            return Err(error.into());
        }

        let finished = (|| -> Result<(), WriteError> {
            if let Some(native) = native {
                metadata::apply_native(
                    &file,
                    native,
                    metadata::Destination::New,
                    options.metadata(),
                )?;
            }
            metadata::apply_foreign(&file, metadata, options.metadata())?;
            flush(&file, options.durability())?;
            Ok(())
        })();
        // All the data arrived: a failure giving it its metadata keeps
        // it, so the next attempt sends nothing again.
        if let Err(error) = finished {
            if written >= PARTIAL_MIN_BYTES
                && self
                    .keep_partial(&staged.parent, &staged.name, content)
                    .is_ok()
            {
                staged.placed = true;
            }
            return Err(error);
        }
        Ok(StagedFile {
            staged,
            file,
            data: Digest::from_bytes(*hasher.finalize().as_bytes()),
        })
    }

    /// Keep a whole staged copy of `content` that could not be placed as
    /// its partial, so the next attempt sends nothing again. A copy too
    /// small to resume is removed as usual.
    pub fn keep_staged(
        &self,
        staged: super::Staged,
        content: &Digest,
        size: u64,
    ) -> io::Result<()> {
        let mut staged = staged;
        if size >= PARTIAL_MIN_BYTES {
            self.keep_partial(&staged.parent, &staged.name, content)?;
            staged.placed = true;
        }
        Ok(())
    }

    /// Remove partials nothing wrote to for `retention`; how many.
    pub fn prune_partials(&self, retention: Duration, now: SystemTime) -> io::Result<u64> {
        let directory = match self.partial_directory() {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut pruned = 0;
        for entry in directory.entries()? {
            let name = PathBuf::from(entry?.file_name());
            let Ok(metadata) = EntryMetadata::at(&directory, &name) else {
                continue;
            };
            let stale = now
                .duration_since(metadata.modified())
                .is_ok_and(|age| age > retention);
            if metadata.kind() != NodeKind::File || !stale {
                continue;
            }
            match rustix::fs::unlinkat(&directory, &name, AtFlags::empty()) {
                Ok(()) => pruned += 1,
                Err(Errno::NOENT) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(pruned)
    }

    fn partial_directory(&self) -> io::Result<Dir> {
        let path = SyncPath::try_from(PARTIAL_DIRECTORY).expect("the partial path is valid");
        self.directory_at(&path)
    }

    /// Move the staged `name` in `parent` to the partial for `content`.
    fn keep_partial(&self, parent: &Dir, name: &PathBuf, content: &Digest) -> io::Result<()> {
        let path = partial_path(content);
        let (directory, kept) = self.create_parents(&path, Parents::Private)?;
        rustix::fs::renameat(parent, name, &directory, &kept)?;
        Ok(())
    }

    /// Move the partial for `content` beside its destination in `parent`
    /// under a temporary name, cut to its first `from` bytes.
    fn adopt_partial(
        &self,
        content: &Digest,
        parent: Dir,
        mode: Mode,
        from: u64,
    ) -> Result<(super::Staged, File), WriteError> {
        let directory = self.partial_directory()?;
        let name = temporary_name();
        match rustix::fs::renameat(&directory, partial_name(content), &parent, &name) {
            Ok(()) => {}
            Err(Errno::NOENT) => return Err(WriteError::Changed),
            Err(error) => return Err(error.into()),
        }
        let staged = super::Staged {
            parent,
            name,
            placed: false,
        };
        let file = File::from(rustix::fs::openat(
            &staged.parent,
            &staged.name,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let kept = EntryMetadata::from_file(&file)?;
        if kept.kind() != NodeKind::File || kept.size() < from {
            return Err(WriteError::Changed);
        }
        file.set_len(from)?;
        rustix::fs::fchmod(&file, mode)?;
        Ok((staged, file))
    }
}

fn partial_name(content: &Digest) -> PathBuf {
    let hex: String = content
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    PathBuf::from(hex)
}

fn partial_path(content: &Digest) -> SyncPath {
    let mut path = PARTIAL_DIRECTORY.to_vec();
    path.push(b'/');
    path.extend_from_slice(partial_name(content).as_os_str().as_bytes());
    SyncPath::try_from(path).expect("a partial path is valid")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::UNIX_EPOCH;

    use foldover_types::{Durability, MetadataPolicy, Propagation};

    use super::*;
    use crate::Placement;

    const OPTIONS: PairOptions = PairOptions::new(
        MetadataPolicy::new(Propagation::Sync, Propagation::Ignore, Propagation::Ignore),
        Durability::Standard,
    );
    const FOREIGN: ForeignMetadata = ForeignMetadata {
        permissions: 0o644,
        modified: UNIX_EPOCH,
    };
    const MIB: usize = 1024 * 1024;

    fn content(length: usize) -> (Vec<u8>, Digest) {
        let data: Vec<u8> = (0..length).map(|index| (index % 251) as u8).collect();
        let digest = Digest::from_bytes(*blake3::hash(&data).as_bytes());
        (data, digest)
    }

    /// Stage `data` for `big.bin`, failing once `fail_after` bytes are in.
    fn stage(
        root: &Root,
        data: &[u8],
        resume: Resume,
        fail_after: Option<usize>,
    ) -> Result<StagedFile, WriteError> {
        let path = SyncPath::try_from(&b"big.bin"[..]).unwrap();
        root.stage_resumable(&path, resume, FOREIGN, None, &OPTIONS, |sink| {
            for chunk in data.chunks(MIB) {
                sink(chunk)?;
            }
            match fail_after {
                Some(_) => Err(io::Error::other("the connection dropped")),
                None => Ok(()),
            }
        })
    }

    fn visible(directory: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != ".foldover")
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_broken_off_copy_keeps_its_data_and_the_next_one_sends_only_the_rest() {
        let directory = tempfile::tempdir().unwrap();
        let root = Root::open(directory.path()).unwrap();
        let (data, digest) = content(20 * MIB);
        let fresh = Resume {
            content: digest,
            from: 0,
        };

        let broken = stage(&root, &data[..18 * MIB], fresh, Some(18 * MIB));
        assert!(broken.is_err());
        assert_eq!(root.partial_length(&digest).unwrap(), 18 * MIB as u64);
        assert!(visible(directory.path()).is_empty());

        let from = root.partial_length(&digest).unwrap();
        let resumed = Resume {
            content: digest,
            from,
        };
        let staged = stage(&root, &data[from as usize..], resumed, None).unwrap();
        assert_eq!(staged.data_digest(), digest);
        let path = SyncPath::try_from(&b"big.bin"[..]).unwrap();
        root.place(
            staged.into_staged(),
            &path,
            Placement::Create,
            Durability::Standard,
        )
        .unwrap();
        assert_eq!(fs::read(directory.path().join("big.bin")).unwrap(), data);
        assert_eq!(root.partial_length(&digest).unwrap(), 0);
    }

    /// A whole copy that could not be placed is kept, ready to resume
    /// from its end; a small one is removed as usual.
    #[test]
    fn a_whole_staged_copy_can_be_kept_for_the_next_attempt() {
        let directory = tempfile::tempdir().unwrap();
        let root = Root::open(directory.path()).unwrap();
        let (data, digest) = content(20 * MIB);
        let fresh = Resume {
            content: digest,
            from: 0,
        };
        let staged = stage(&root, &data, fresh, None).unwrap();
        root.keep_staged(staged.into_staged(), &digest, data.len() as u64)
            .unwrap();
        assert_eq!(root.partial_length(&digest).unwrap(), data.len() as u64);
        assert!(visible(directory.path()).is_empty());

        let (small, small_digest) = content(1024);
        let fresh = Resume {
            content: small_digest,
            from: 0,
        };
        let staged = stage(&root, &small, fresh, None).unwrap();
        root.keep_staged(staged.into_staged(), &small_digest, small.len() as u64)
            .unwrap();
        assert_eq!(root.partial_length(&small_digest).unwrap(), 0);
        assert!(visible(directory.path()).is_empty());
    }

    #[test]
    fn a_short_break_keeps_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let root = Root::open(directory.path()).unwrap();
        let (data, digest) = content(2 * MIB);
        let fresh = Resume {
            content: digest,
            from: 0,
        };
        assert!(stage(&root, &data, fresh, Some(2 * MIB)).is_err());
        assert_eq!(root.partial_length(&digest).unwrap(), 0);
        assert!(visible(directory.path()).is_empty());
    }

    /// The digest covers the kept bytes, so a partial holding anything
    /// else cannot pass for the planned content.
    #[test]
    fn kept_bytes_that_differ_show_in_the_digest() {
        let directory = tempfile::tempdir().unwrap();
        let root = Root::open(directory.path()).unwrap();
        let (data, digest) = content(20 * MIB);
        let fresh = Resume {
            content: digest,
            from: 0,
        };
        assert!(stage(&root, &data[..17 * MIB], fresh, Some(17 * MIB)).is_err());
        let kept = directory
            .path()
            .join(".foldover/partial")
            .join(partial_name(&digest));
        let mut garbled = fs::read(&kept).unwrap();
        garbled[MIB] ^= 0xff;
        fs::write(&kept, garbled).unwrap();

        let resumed = Resume {
            content: digest,
            from: 17 * MIB as u64,
        };
        let staged = stage(&root, &data[17 * MIB..], resumed, None).unwrap();
        assert_ne!(staged.data_digest(), digest);
    }

    #[test]
    fn partials_nothing_wrote_to_for_the_retention_are_pruned() {
        let directory = tempfile::tempdir().unwrap();
        let root = Root::open(directory.path()).unwrap();
        let (data, digest) = content(17 * MIB);
        let fresh = Resume {
            content: digest,
            from: 0,
        };
        assert!(stage(&root, &data, fresh, Some(17 * MIB)).is_err());
        let retention = Duration::from_secs(7 * 24 * 3600);
        assert_eq!(
            root.prune_partials(retention, SystemTime::now()).unwrap(),
            0
        );
        let later = SystemTime::now() + retention + Duration::from_secs(60);
        assert_eq!(root.prune_partials(retention, later).unwrap(), 1);
        assert_eq!(root.partial_length(&digest).unwrap(), 0);
    }
}
