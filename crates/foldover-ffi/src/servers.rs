//! File servers on the local network, for Swift (PLAN.md §3.8).

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
