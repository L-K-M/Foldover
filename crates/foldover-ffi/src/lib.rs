//! `foldover-ffi` — client layer. The UniFFI export of foldover-client
//! for the Swift app (the `FoldoverCore` module). Exports local version
//! and agent health. Depends on foldover-client. See PLAN.md §4.4.
//!
//! This crate cannot opt into the workspace's `unsafe_code = "forbid"`
//! lint: UniFFI's scaffolding expands to `unsafe extern "C"` shims. It
//! contains no hand-written `unsafe`.

// Adapted from L-K-M/Sweetbriar.

/// `coreVersion()` in Swift: the workspace version, via the client.
#[uniffi::export]
pub fn core_version() -> String {
    foldover_client::version().to_owned()
}

#[derive(Clone, Debug, uniffi::Enum)]
pub enum AgentCapability {
    HealthOnly,
    PairConfiguration,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct AgentHealth {
    pub core_version: String,
    pub capability: AgentCapability,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum AgentError {
    #[error("{message}")]
    NotRunning { message: String },
    #[error("{message}")]
    Connection { message: String },
}

#[uniffi::export(async_runtime = "tokio")]
pub async fn agent_status() -> Result<AgentHealth, AgentError> {
    let status = foldover_client::agent_status(&foldover_client::SocketLocation::Default)
        .await
        .map_err(|error| {
            let message = error.to_string();
            match error {
                foldover_client::Error::Io(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    AgentError::NotRunning { message }
                }
                _ => AgentError::Connection { message },
            }
        })?;
    Ok(AgentHealth {
        core_version: status.core_version,
        capability: match status.capability {
            foldover_client::AgentCapability::HealthOnly => AgentCapability::HealthOnly,
            foldover_client::AgentCapability::PairConfiguration => {
                AgentCapability::PairConfiguration
            }
        },
    })
}

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    #[test]
    fn core_version_is_the_package_version() {
        assert_eq!(super::core_version(), env!("CARGO_PKG_VERSION"));
    }
}
