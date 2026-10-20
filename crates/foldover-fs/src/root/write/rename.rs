//! Renames that refuse to replace, and swaps. APFS has both
//! (`RENAME_EXCL`, `RENAME_SWAP`); HFS+, exFAT and FAT answer `ENOTSUP`,
//! as the device matrix found (ADR 0038), so they get fallbacks:
//!
//! ```text
//! exclusive rename: placeholder created exclusively at `to` ─▶ rename over it
//! swap (PLAN.md §4.7, two steps):
//!   `to` ─▶ aside, exclusively ─▶ staged ─▶ `to`, exclusively
//!   (on failure, aside ─▶ `to` again)
//! ```
//!
//! An exclusive rename keeps its refusal; an agent that dies between the
//! placeholder and the rename leaves the placeholder, empty, under the
//! destination's name. A swap has a moment with nothing at `to`: an agent
//! that dies then leaves both entries under temporary names, which the
//! next run archives, so nothing is lost but the path is empty until a run
//! puts something back. Volumes that answer `EINVAL` to the flags, as some
//! network file systems do, get the same fallbacks; where `EINVAL` means
//! something else, the plain rename fails the same way.

use std::path::{Path, PathBuf};

use cap_std::fs::Dir;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

/// Where a swap left the entry it displaced.
pub(super) enum Swapped {
    /// Under the staged entry's former name.
    InPlace,
    /// Beside the destination, under this temporary name.
    Aside(PathBuf),
}

#[cfg(test)]
thread_local! {
    /// Tests make this thread's volumes answer as HFS+ does.
    static FLAGS_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `body` as if every volume lacked exclusive renames and swaps; the
/// thread's volumes answer as before afterwards, even if `body` panics.
#[cfg(test)]
pub(super) fn without_rename_flags<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FLAGS_UNSUPPORTED.with(|unsupported| unsupported.set(self.0));
        }
    }
    let _restore = Restore(FLAGS_UNSUPPORTED.with(|unsupported| unsupported.replace(true)));
    body()
}

/// Whether a rename failed only because the volume lacks its flags.
fn flags_unsupported(error: Errno) -> bool {
    matches!(error, Errno::NOTSUP | Errno::INVAL)
}

fn flagged(
    from_directory: &Dir,
    from: &Path,
    to_directory: &Dir,
    to: &Path,
    flags: RenameFlags,
) -> Result<(), Errno> {
    #[cfg(test)]
    if FLAGS_UNSUPPORTED.with(std::cell::Cell::get) {
        return Err(Errno::NOTSUP);
    }
    rustix::fs::renameat_with(from_directory, from, to_directory, to, flags)
}

/// Rename `from` to `to` unless something is at `to`: `EEXIST` then.
pub(super) fn rename_exclusive(
    from_directory: &Dir,
    from: &Path,
    to_directory: &Dir,
    to: &Path,
) -> Result<(), Errno> {
    match flagged(
        from_directory,
        from,
        to_directory,
        to,
        RenameFlags::NOREPLACE,
    ) {
        Err(error) if flags_unsupported(error) => {}
        renamed => return renamed,
    }
    // A placeholder of the moved entry's kind, created exclusively, takes
    // the name; a plain rename may then replace only it.
    let kind = rustix::fs::statat(from_directory, from, AtFlags::SYMLINK_NOFOLLOW)?.st_mode;
    let directory = FileType::from_raw_mode(kind) == FileType::Directory;
    if directory {
        rustix::fs::mkdirat(to_directory, to, Mode::RWXU)?;
    } else {
        rustix::fs::openat(
            to_directory,
            to,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )?;
    }
    rustix::fs::renameat(from_directory, from, to_directory, to).inspect_err(|_| {
        let flags = if directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        };
        let _ = rustix::fs::unlinkat(to_directory, to, flags);
    })
}

/// Exchange the entries at `staged` and `to`; where the displaced one
/// went. `NOENT` when `to` holds nothing.
pub(super) fn swap(
    staged_directory: &Dir,
    staged: &Path,
    to_directory: &Dir,
    to: &Path,
) -> Result<Swapped, Errno> {
    match flagged(
        staged_directory,
        staged,
        to_directory,
        to,
        RenameFlags::EXCHANGE,
    ) {
        Ok(()) => return Ok(Swapped::InPlace),
        Err(error) if flags_unsupported(error) => {}
        Err(error) => return Err(error),
    }
    let aside = super::temporary_name();
    rename_exclusive(to_directory, to, to_directory, &aside)?;
    if let Err(error) = rename_exclusive(staged_directory, staged, to_directory, to) {
        // Put it back. Should something have taken its name meanwhile, it
        // stays aside, a leftover the next run archives.
        let _ = rename_exclusive(to_directory, &aside, to_directory, to);
        return Err(error);
    }
    Ok(Swapped::Aside(aside))
}
