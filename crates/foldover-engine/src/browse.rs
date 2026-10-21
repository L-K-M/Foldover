//! Folders on an SFTP server, for choosing a pair's side (PLAN.md §4.3,
//! `endpoint.browse`). The pair editor lists one folder at a time and
//! walks in and out:
//!
//! ```text
//! sftp://nas/~  ─▶  /home/alice   parent /home   folders: photos, music
//!               ─▶  /home/alice/photos ...
//! ```

use foldover_endpoint::{Endpoint, Filter, ListedKind, Reach, ScanError};
use foldover_types::{PairRoot, SftpRoot};

use crate::Error;

/// One folder on a server and the folders in it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerFolder {
    /// This folder, with `~` resolved: `sftp://alice@nas/home/alice`.
    pub root: SftpRoot,
    /// The folder above it; `None` at the server's `/`.
    pub parent: Option<SftpRoot>,
    /// The folders in it, by name, sorted. Names that are not UTF-8
    /// cannot be shown or typed, and a locator cannot name a folder `~`
    /// at the server's top, so those are left out.
    pub folders: Vec<(String, SftpRoot)>,
}

/// Connect to `root`'s server and list the folders in it.
pub fn browse_server(root: &SftpRoot) -> Result<ServerFolder, Error> {
    let endpoint = Endpoint::open(&PairRoot::Sftp(root.clone()), &Reach::default())?;
    browse(&endpoint, root)
}

/// List the folders in `endpoint`, the folder `root` names.
fn browse(endpoint: &Endpoint, root: &SftpRoot) -> Result<ServerFolder, Error> {
    let Endpoint::Sftp(server) = endpoint else {
        unreachable!("an SFTP root opens an SFTP endpoint")
    };
    let path = server.path().to_vec();
    // Locators are at most 4096 bytes; a deeper path cannot be a root.
    let at = |path: &[u8]| root.at(path).map_err(|_| Error::ServerPathTooLong);
    let listed = endpoint
        .list(None, &Filter::new(std::iter::empty::<&str>())?)?
        .ok_or(ScanError::RootMissing)?;

    let mut folders = Vec::new();
    for entry in listed {
        if entry.kind() != ListedKind::Directory {
            continue;
        }
        let Ok(name) = std::str::from_utf8(entry.name()) else {
            continue;
        };
        // Too deep for a locator, or `~` at the top, which names home.
        let Ok(folder) = at(&child(&path, entry.name())) else {
            continue;
        };
        folders.push((name.to_owned(), folder));
    }
    folders.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(ServerFolder {
        root: at(&path)?,
        parent: parent(&path).map(|parent| at(&parent)).transpose()?,
        folders,
    })
}

/// `/home` + `alice` is `/home/alice`; `/` + `srv` is `/srv`.
fn child(path: &[u8], name: &[u8]) -> Vec<u8> {
    match path {
        b"/" => [b"/", name].concat(),
        _ => [path, b"/", name].concat(),
    }
}

/// `/home/alice` is in `/home`, `/srv` in `/`; `/` is in nothing.
fn parent(path: &[u8]) -> Option<Vec<u8>> {
    if path == b"/" {
        return None;
    }
    let slash = path.iter().rposition(|byte| *byte == b'/')?;
    Some(match slash {
        0 => b"/".to_vec(),
        slash => path[..slash].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::ffi::OsStrExt;

    use foldover_endpoint::SftpEndpoint;

    use super::*;

    fn sftp_server() -> Option<std::path::PathBuf> {
        std::env::var_os("FOLDOVER_SFTP_SERVER")
            .map(std::path::PathBuf::from)
            .into_iter()
            .chain(["/usr/lib/openssh/sftp-server", "/usr/libexec/sftp-server"].map(Into::into))
            .find(|path| path.exists())
    }

    #[test]
    fn a_server_folder_lists_its_folders_and_its_parent() {
        let Some(server) = sftp_server() else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("photos");
        for name in ["2026", "2025", ".foldover"] {
            fs::create_dir_all(folder.join(name)).unwrap();
        }
        fs::write(folder.join("notes.txt"), b"not a folder").unwrap();
        let path = folder.as_os_str().as_bytes();
        let endpoint = Endpoint::Sftp(
            SftpEndpoint::spawn(&server, &[std::ffi::OsStr::new("-d"), "/".as_ref()], path)
                .unwrap(),
        );
        let locator = [b"sftp://alice@nas.local".as_slice(), path].concat();
        let PairRoot::Sftp(root) = PairRoot::from_bytes(locator.clone()).unwrap() else {
            unreachable!("an sftp:// locator")
        };

        let browsed = browse(&endpoint, &root).unwrap();
        assert_eq!(browsed.root.as_bytes(), locator.as_slice());
        let parent = [
            b"sftp://alice@nas.local".as_slice(),
            temporary.path().as_os_str().as_bytes(),
        ]
        .concat();
        assert_eq!(browsed.parent.unwrap().as_bytes(), parent.as_slice());
        let names: Vec<&str> = browsed
            .folders
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        // Foldover's own folder never shows.
        assert_eq!(names, ["2025", "2026"]);
        assert_eq!(
            browsed.folders[1].1.as_bytes(),
            [locator.as_slice(), b"/2026"].concat().as_slice()
        );
    }

    #[test]
    fn paths_walk_in_and_out_of_folders() {
        assert_eq!(child(b"/", b"srv"), b"/srv");
        assert_eq!(child(b"/home", b"alice"), b"/home/alice");
        assert_eq!(parent(b"/home/alice").as_deref(), Some(b"/home".as_slice()));
        assert_eq!(parent(b"/srv").as_deref(), Some(b"/".as_slice()));
        assert_eq!(parent(b"/"), None);
    }
}
