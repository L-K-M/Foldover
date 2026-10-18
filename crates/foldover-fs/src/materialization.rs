use std::ffi::c_int;
use std::io;

// Darwin sys/resource.h. Native tests compare these against the installed SDK.
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
const IOPOL_SCOPE_THREAD: c_int = 1;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

// SAFETY: signatures match Darwin sys/resource.h: C int parameters/results,
// no pointers, callbacks or ownership transfers.
unsafe extern "C" {
    fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
}

fn policy() -> io::Result<c_int> {
    // SAFETY: both arguments are valid Darwin constants. No pointers or borrowed
    // storage cross this synchronous libc call; it queries the calling thread.
    let value = unsafe {
        getiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_THREAD,
        )
    };
    if value == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

pub(super) fn prevent_materialization() -> io::Result<()> {
    if policy()? == IOPOL_MATERIALIZE_DATALESS_FILES_OFF {
        return Ok(());
    }
    set_policy(IOPOL_MATERIALIZE_DATALESS_FILES_OFF)?;
    if policy()? != IOPOL_MATERIALIZE_DATALESS_FILES_OFF {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The calling thread did not disable dataless materialization",
        ));
    }
    Ok(())
}

fn set_policy(value: c_int) -> io::Result<()> {
    // SAFETY: the type/scope are valid constants and this private caller supplies
    // only SDK policy values. No pointers cross the call. Errors remain errors.
    let result = unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_THREAD,
            value,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DirectoryScope, Root};
    use foldover_types::{ScanControl, SyncPath};
    use std::fs;

    const IOPOL_MATERIALIZE_DATALESS_FILES_ON: c_int = 2;

    #[test]
    fn binding_constants_match_the_native_sdk() {
        let output = std::process::Command::new("/usr/bin/xcrun")
            .args(["swift", "-e", "import Darwin; print(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_THREAD, IOPOL_MATERIALIZE_DATALESS_FILES_OFF, IOPOL_MATERIALIZE_DATALESS_FILES_ON)"])
            .stdin(std::process::Stdio::null()).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let values = String::from_utf8(output.stdout)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse::<c_int>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
                IOPOL_MATERIALIZE_DATALESS_FILES_ON
            ]
        );
    }

    #[test]
    fn root_operations_disable_policy_on_the_thread_that_performs_io() {
        std::thread::spawn(|| {
            let temporary = tempfile::tempdir().unwrap();
            fs::write(temporary.path().join("file"), b"abc").unwrap();
            let path = SyncPath::try_from(b"file".as_slice()).unwrap();
            set_policy(IOPOL_MATERIALIZE_DATALESS_FILES_ON).unwrap();
            let root = Root::open(temporary.path()).unwrap();
            assert_eq!(policy().unwrap(), IOPOL_MATERIALIZE_DATALESS_FILES_OFF);
            set_policy(IOPOL_MATERIALIZE_DATALESS_FILES_ON).unwrap();
            let metadata = root.metadata(&path).unwrap();
            assert_eq!(policy().unwrap(), IOPOL_MATERIALIZE_DATALESS_FILES_OFF);

            let entries = root.entries(DirectoryScope::Root).unwrap();
            std::thread::spawn(move || {
                set_policy(IOPOL_MATERIALIZE_DATALESS_FILES_ON).unwrap();
                assert_eq!(policy().unwrap(), IOPOL_MATERIALIZE_DATALESS_FILES_ON);
                assert_eq!(entries.collect::<io::Result<Vec<_>>>().unwrap().len(), 1);
                assert_eq!(policy().unwrap(), IOPOL_MATERIALIZE_DATALESS_FILES_OFF);
            })
            .join()
            .unwrap();

            // Even a callback that changes thread policy cannot enable a read.
            root.hash_file(&path, &metadata, &mut |_| {
                set_policy(IOPOL_MATERIALIZE_DATALESS_FILES_ON).unwrap();
                ScanControl::Continue
            })
            .unwrap();
            assert_eq!(policy().unwrap(), IOPOL_MATERIALIZE_DATALESS_FILES_OFF);
        })
        .join()
        .unwrap();
    }
}
