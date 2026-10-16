//! The filesystem mutations the harness performs while watching FSEvents.

use crate::sys;
use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// One operation in the experiment-1 matrix. The variants chain onto a
/// single probe file: create it, modify it, rename it, chmod and xattr it,
/// delete it.
#[derive(Clone, Copy)]
pub enum FsOp {
    Create,
    Modify,
    Rename,
    Chmod,
    XattrSet,
    Delete,
}

impl FsOp {
    pub const ALL: [FsOp; 6] = [
        FsOp::Create,
        FsOp::Modify,
        FsOp::Rename,
        FsOp::Chmod,
        FsOp::XattrSet,
        FsOp::Delete,
    ];
    pub fn name(self) -> &'static str {
        match self {
            FsOp::Create => "create",
            FsOp::Modify => "modify",
            FsOp::Rename => "rename",
            FsOp::Chmod => "chmod",
            FsOp::XattrSet => "xattr_set",
            FsOp::Delete => "delete",
        }
    }
}

pub struct OpOutcome {
    /// Paths an event could legitimately name for this operation (a rename
    /// reports the old and/or the new path).
    pub watched_paths: Vec<PathBuf>,
}

const PROBE_A: &str = "probe-file-a";
const PROBE_B: &str = "probe-file-b";

/// Perform `op` inside `dir`; returns the paths that count as its events.
pub fn apply(op: FsOp, dir: &Path) -> Result<OpOutcome, String> {
    let a = dir.join(PROBE_A);
    let b = dir.join(PROBE_B);
    let res = match op {
        FsOp::Create => fs::write(&a, b"foldover s3 probe\n"),
        FsOp::Modify => fs::OpenOptions::new()
            .append(true)
            .open(&a)
            .and_then(|mut f| f.write_all(b"more\n")),
        FsOp::Rename => fs::rename(&a, &b),
        FsOp::Chmod => fs::set_permissions(&b, fs::Permissions::from_mode(0o640)),
        FsOp::XattrSet => {
            let path = CString::new(b.to_string_lossy().into_owned()).map_err(|e| e.to_string())?;
            let value = b"s3";
            let rc = unsafe {
                sys::setxattr(
                    path.as_ptr(),
                    c"user.foldover.spike".as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            if rc == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }
        FsOp::Delete => fs::remove_file(&b),
    };
    res.map_err(|e| format!("{}: {e}", op.name()))?;
    let watched_paths = match op {
        FsOp::Rename => vec![a, b],
        FsOp::Create | FsOp::Modify => vec![a],
        FsOp::Chmod | FsOp::XattrSet | FsOp::Delete => vec![b],
    };
    Ok(OpOutcome { watched_paths })
}

/// Create `n` files under `dir` as fast as `fs::write` allows; returns the
/// elapsed time. Used by the burst experiment.
pub fn create_files(dir: &Path, prefix: &str, n: usize) -> io::Result<Duration> {
    fs::create_dir_all(dir)?;
    let t0 = Instant::now();
    for i in 0..n {
        fs::write(dir.join(format!("{prefix}-{i:06}.dat")), b"x")?;
    }
    Ok(t0.elapsed())
}
