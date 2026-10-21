//! Folders on servers, for choosing a pair's side (PLAN.md §4.3).

use serde::{Deserialize, Serialize};

/// The folders inside a folder on an SFTP server; connecting can take a
/// few seconds.
pub const ENDPOINT_BROWSE: &str = "endpoint.browse";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointBrowse {
    /// An `sftp://` root, as a pair side takes it.
    pub root: Vec<u8>,
}

/// One folder on a server, as a pair side can name it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFolderRecord {
    pub name: String,
    pub root: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerFolderList {
    /// The folder listed, `~` resolved.
    pub root: Vec<u8>,
    /// The folder above it; `None` at the server's `/`.
    pub parent: Option<Vec<u8>>,
    pub folders: Vec<ServerFolderRecord>,
}
