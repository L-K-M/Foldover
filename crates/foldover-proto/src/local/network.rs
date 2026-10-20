//! Servers on the local network (PLAN.md §3.8): what Bonjour shows.

use serde::{Deserialize, Serialize};

/// SMB file servers seen nearby; the answer waits about two seconds.
pub const NETWORK_SERVERS: &str = "network.servers";

/// An SMB file server seen on the local network.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkServer {
    /// The server's display name, e.g. "NAS".
    pub name: String,
    /// Where SMB connects, e.g. "nas.local".
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkServers {
    pub servers: Vec<NetworkServer>,
}
