//! `foldover-client` — client layer. The typed async client for the
//! agent's local API: requests, subscriptions and reconnect, shared by
//! the CLI, the FFI crate and later the Linux app. Depends on
//! foldover-ipc. See PLAN.md §4.4.

pub use foldover_ipc::{Error, SocketLocation};
pub use foldover_proto::local::{AgentCapability, DaemonStatus};
use foldover_proto::local::{DAEMON_STATUS, Hello, PROTOCOL_VERSION, Request, SESSION_HELLO};
use serde_json::json;

/// The workspace version expected from the agent handshake and exposed to clients.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub async fn agent_status(location: &SocketLocation) -> Result<DaemonStatus, Error> {
    let mut connection = foldover_ipc::connect(location).await?;
    let hello: Hello = serde_json::from_value(
        connection
            .call(Request::call(
                1,
                SESSION_HELLO,
                Some(json!({"protocol_version": PROTOCOL_VERSION})),
            ))
            .await?,
    )?;
    if hello.protocol_version != PROTOCOL_VERSION || hello.core_version != version() {
        return Err(Error::Protocol(
            "Agent version mismatch; restart the background agent",
        ));
    }
    let status = connection
        .call(Request::call(2, DAEMON_STATUS, None))
        .await?;
    Ok(serde_json::from_value(status)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_the_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::version().is_empty());
    }
}
