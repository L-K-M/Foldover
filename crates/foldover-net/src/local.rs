use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

const RUNTIME_DIRECTORY: &str = "foldover";
const SOCKET_NAME: &str = "s";
const DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const PERMISSION_BITS: u32 = 0o777;

#[derive(Clone, Debug)]
pub enum SocketLocation {
    Default,
    Path(PathBuf),
}

impl SocketLocation {
    pub fn resolve(&self) -> io::Result<PathBuf> {
        let path = match self {
            Self::Path(path) => path.clone(),
            Self::Default => default_directory().join(SOCKET_NAME),
        };
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Socket path must be absolute",
            ));
        }
        // std uses the host sockaddr_un capacity, including its NUL terminator.
        std::os::unix::net::SocketAddr::from_pathname(&path)?;
        Ok(path)
    }
}

fn default_directory() -> PathBuf {
    #[cfg(target_os = "macos")]
    return std::env::temp_dir().join(RUNTIME_DIRECTORY);
    #[cfg(not(target_os = "macos"))]
    {
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(base) => PathBuf::from(base).join(RUNTIME_DIRECTORY),
            None => std::env::temp_dir().join(format!("foldover-{}", current_user())),
        }
    }
}

fn current_user() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn validate_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.uid() != current_user()
        || metadata.mode() & PERMISSION_BITS != DIRECTORY_MODE
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Agent socket directory must be a real directory owned by this user with mode 0700",
        ));
    }
    Ok(())
}

pub struct Connection {
    stream: UnixStream,
}

impl Connection {
    fn checked(stream: UnixStream) -> io::Result<Self> {
        if stream.peer_cred()?.uid() != current_user() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Agent peer has a different user ID",
            ));
        }
        Ok(Self { stream })
    }

    pub async fn connect(location: &SocketLocation) -> io::Result<Self> {
        let path = location.resolve()?;
        validate_directory(
            path.parent()
                .ok_or_else(|| io::Error::other("Socket has no parent"))?,
        )?;
        Self::checked(UnixStream::connect(path).await?)
    }

    pub async fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buffer).await
    }

    pub async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }
}

/// The runtime lock owns this listener. Stale sockets are removed only after
/// acquiring that lock; a regular file or symlink is never treated as stale.
pub struct Listener {
    listener: UnixListener,
    _lock: File,
}

impl Listener {
    pub fn bind(location: &SocketLocation) -> io::Result<Self> {
        let path = location.resolve()?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("Socket has no parent"))?;
        match DirBuilder::new().mode(DIRECTORY_MODE).create(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        validate_directory(parent)?;

        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(parent.join("agent.lock"))?;
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != current_user()
            || metadata.mode() & PERMISSION_BITS != PRIVATE_FILE_MODE
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Agent lock has unsafe ownership or permissions",
            ));
        }
        lock.try_lock().map_err(|error| {
            io::Error::other(format!("Another agent owns the runtime lock: {error}"))
        })?;

        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket() && metadata.uid() == current_user() =>
            {
                fs::remove_file(&path)?
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Socket path contains a non-socket entry",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(
            &path,
            std::os::unix::fs::PermissionsExt::from_mode(PRIVATE_FILE_MODE),
        )?;
        Ok(Self {
            listener,
            _lock: lock,
        })
    }

    pub async fn accept(&self) -> io::Result<Connection> {
        let (stream, _) = self.listener.accept().await?;
        Connection::checked(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn private_directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(DIRECTORY_MODE))
            .tempdir()
            .unwrap()
    }

    #[tokio::test]
    async fn private_socket_has_one_owner_and_can_restart_after_drop() {
        let directory = private_directory();
        let location = SocketLocation::Path(directory.path().join("s"));
        let listener = Listener::bind(&location).unwrap();
        assert!(Listener::bind(&location).is_err());
        assert_eq!(
            fs::metadata(location.resolve().unwrap()).unwrap().mode() & PERMISSION_BITS,
            PRIVATE_FILE_MODE
        );
        let mut client = Connection::connect(&location).await.unwrap();
        let mut server = listener.accept().await.unwrap();
        client.write_all(b"data").await.unwrap();
        let mut buffer = [0; 4];
        assert_eq!(server.read(&mut buffer).await.unwrap(), buffer.len());
        assert_eq!(&buffer, b"data");
        drop(listener);
        assert!(Listener::bind(&location).is_ok());
    }

    #[tokio::test]
    async fn refuses_unsafe_parents_and_never_removes_regular_files() {
        let directory = private_directory();
        let path = directory.path().join("s");
        fs::write(&path, b"keep").unwrap();
        assert!(Listener::bind(&SocketLocation::Path(path.clone())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Listener::bind(&SocketLocation::Path(path)).is_err());
    }

    #[tokio::test]
    async fn refuses_symlink_runtime_directories() {
        let directory = private_directory();
        let alias = directory.path().join("alias");
        symlink(directory.path(), &alias).unwrap();
        assert!(Listener::bind(&SocketLocation::Path(alias.join("s"))).is_err());
    }
}
