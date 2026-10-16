//! hdiutil/diskutil plumbing for the disk-image experiments: create small
//! APFS/ExFAT images, attach them at chosen mount points, detach and
//! reattach, all unprivileged.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy)]
pub enum ImageFs {
    Apfs,
    ExFat,
}

impl ImageFs {
    /// Filesystem name accepted by `hdiutil -fs` and `diskutil eraseDisk`.
    pub fn arg(self) -> &'static str {
        match self {
            ImageFs::Apfs => "APFS",
            ImageFs::ExFat => "ExFAT",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            ImageFs::Apfs => "apfs",
            ImageFs::ExFat => "exfat",
        }
    }
}

pub struct AttachedImage {
    pub dmg: PathBuf,
    pub mount_point: PathBuf,
    /// BSD names the attach output reported, e.g. disk9s1.
    pub bsd_names: Vec<String>,
    /// st_dev of the mount point: the device FSEvents/DA care about.
    pub dev: i32,
}

fn run(cmd: &str, args: &[OsString]) -> Result<String, String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| format!("{cmd}: spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{cmd} {} exited {}: {}",
            args.iter()
                .map(|a| a.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn args(items: &[&str], paths: &[&Path]) -> Vec<OsString> {
    items
        .iter()
        .map(OsString::from)
        .chain(paths.iter().map(|p| p.as_os_str().to_os_string()))
        .collect()
}

/// Create a small disk image formatted as `fs`. The primary path is
/// `hdiutil create -fs`; when hdiutil declines to lay the filesystem down
/// itself, fall back to a blank image plus `diskutil eraseDisk` on its
/// device node.
pub fn create_image(dir: &Path, fs_kind: ImageFs, size_mb: u32) -> Result<PathBuf, String> {
    let name = format!("folds3{}", fs_kind.label());
    let dmg = dir.join(format!("{name}.dmg"));
    let _ = fs::remove_file(&dmg);
    let size = format!("{size_mb}m");
    let first = run(
        "hdiutil",
        &args(
            &[
                "create",
                "-size",
                &size,
                "-fs",
                fs_kind.arg(),
                "-volname",
                &name,
                "-ov",
            ],
            &[&dmg],
        ),
    );
    if first.is_ok() {
        return Ok(dmg);
    }
    run(
        "hdiutil",
        &args(&["create", "-size", &size, "-ov"], &[&dmg]),
    )?;
    let out = run(
        "hdiutil",
        &args(&["attach", "-nomount", "-nobrowse"], &[&dmg]),
    )?;
    let disk = whole_disk(&out)
        .ok_or_else(|| format!("no whole-disk node in hdiutil attach output: {out}"))?;
    run(
        "diskutil",
        &args(&["eraseDisk", fs_kind.arg(), &name, "GPT", &disk], &[]),
    )?;
    // eraseDisk mounts at /Volumes/<name>; detach so the caller attaches at
    // its chosen mount point like on the primary path.
    let _ = detach(&disk);
    Ok(dmg)
}

/// The bare /dev/diskN (no sN slice suffix) of the first line of attach
/// output.
fn whole_disk(attach_output: &str) -> Option<String> {
    for line in attach_output.lines() {
        if let Some(dev) = line.split_whitespace().next()
            && dev.starts_with("/dev/disk")
            && !dev.contains('s')
        {
            return Some(dev.to_string());
        }
    }
    None
}

/// All /dev nodes mentioned in hdiutil attach output, as BSD names.
fn bsd_names(attach_output: &str) -> Vec<String> {
    attach_output
        .split_whitespace()
        .filter_map(|f| f.strip_prefix("/dev/"))
        .filter(|n| n.starts_with("disk"))
        .map(|n| {
            n.trim_end_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .collect()
}

/// Attach `dmg` at `mount_point` (created if needed) and stat the mount to
/// learn its dev_t.
pub fn attach(dmg: &Path, mount_point: &Path) -> Result<AttachedImage, String> {
    fs::create_dir_all(mount_point).map_err(|e| format!("mkdir {}: {e}", mount_point.display()))?;
    let out = run(
        "hdiutil",
        &args(
            // Retry browsable mounts: the -nobrowse run reported no
            // FSEvents UUID and replayed no image changes.
            &["attach", "-noverify", "-mountpoint"],
            &[mount_point, dmg],
        ),
    )?;
    let meta =
        fs::metadata(mount_point).map_err(|e| format!("stat {}: {e}", mount_point.display()))?;
    Ok(AttachedImage {
        dmg: dmg.to_path_buf(),
        mount_point: mount_point.to_path_buf(),
        bsd_names: bsd_names(&out),
        dev: meta.dev() as i32,
    })
}

/// Detach by BSD device or mount point; retries with -force once.
pub fn detach(target: &str) -> Result<(), String> {
    if run("hdiutil", &args(&["detach", target], &[])).is_ok() {
        return Ok(());
    }
    run("hdiutil", &args(&["detach", "-force", target], &[]))
        .map(|_| ())
        .map_err(|e| format!("detach {target}: {e}"))
}
