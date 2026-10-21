//! File servers on the local network (PLAN.md §3.8) and the folders on
//! SFTP servers (§4.3), for Swift.

use foldover_client::SocketLocation;

use crate::AgentError;

/// An SMB file server seen on the local network.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct NearbyServer {
    /// The server's display name, e.g. "NAS".
    pub name: String,
    /// Where SMB connects, e.g. "nas.local".
    pub host: String,
    pub port: u16,
}

/// SMB file servers seen nearby; waits about two seconds while the
/// agent browses.
#[uniffi::export(async_runtime = "tokio")]
pub async fn nearby_file_servers() -> Result<Vec<NearbyServer>, AgentError> {
    let servers = foldover_client::network_servers(&SocketLocation::Default)
        .await
        .map_err(AgentError::from)?;
    Ok(servers
        .into_iter()
        .map(|server| NearbyServer {
            name: server.name,
            host: server.host,
            port: server.port,
        })
        .collect())
}

/// One folder on an SFTP server, as a pair side can name it.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ServerFolder {
    pub name: String,
    pub root: Vec<u8>,
}

/// A folder on an SFTP server and the folders in it.
#[derive(Clone, Debug, Eq, PartialEq, uniffi::Record)]
pub struct ServerFolderListing {
    /// The folder listed, with `~` resolved.
    pub root: Vec<u8>,
    /// The folder above it; none at the server's `/`.
    pub parent: Option<Vec<u8>>,
    pub folders: Vec<ServerFolder>,
}

/// The folders in the folder an `sftp://` root names; connecting to the
/// server can take a few seconds.
#[uniffi::export(async_runtime = "tokio")]
pub async fn browse_server(root: Vec<u8>) -> Result<ServerFolderListing, AgentError> {
    let listed = foldover_client::browse_server(&SocketLocation::Default, &root)
        .await
        .map_err(AgentError::from)?;
    Ok(ServerFolderListing {
        root: listed.root,
        parent: listed.parent,
        folders: listed
            .folders
            .into_iter()
            .map(|folder| ServerFolder {
                name: folder.name,
                root: folder.root,
            })
            .collect(),
    })
}
