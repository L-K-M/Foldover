//! `foldover-service` — service layer. The agent's public API: sessions,
//! `handle` request dispatch, validation and the event bus. Depends on
//! foldover-engine. See PLAN.md §4.4.

use foldover_proto::local::{
    AgentCapability, DAEMON_STATUS, DaemonStatus, Hello, INTERNAL_ERROR, INVALID_PARAMS,
    METHOD_NOT_FOUND, PROTOCOL_VERSION, Request, RpcError, SESSION_HELLO, VERSION_MISMATCH,
};
use serde_json::Value;

/// The workspace version reported by health methods and `foldoverd --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Acquire persistent state before serving requests. The returned owner must
/// outlive the server, even while the API exposes only health methods.
pub fn open_state() -> Result<impl Send, impl std::error::Error + Send + Sync> {
    foldover_engine::open_state()
}

/// Only the user-gated health methods exist. Privileged routes remain closed
/// until installed-client authentication and method scope are implemented.
pub async fn handle(request: Request) -> Result<Value, RpcError> {
    match request.method() {
        SESSION_HELLO => {
            let protocol = request
                .params()
                .and_then(Value::as_object)
                .and_then(|params| params.get("protocol_version"))
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    RpcError::new(INVALID_PARAMS, "session.hello needs protocol_version")
                })?;
            if protocol != u64::from(PROTOCOL_VERSION) {
                return Err(RpcError::new(
                    VERSION_MISMATCH,
                    "Unsupported agent protocol version",
                ));
            }
            serde_json::to_value(Hello {
                protocol_version: PROTOCOL_VERSION,
                core_version: version().to_owned(),
            })
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "Cannot encode hello"))
        }
        DAEMON_STATUS => {
            if request.params().is_some_and(|params| {
                !matches!(params, Value::Object(object) if object.is_empty())
                    && !matches!(params, Value::Array(array) if array.is_empty())
            }) {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "daemon.status takes no parameters",
                ));
            }
            serde_json::to_value(DaemonStatus {
                core_version: version().to_owned(),
                capability: AgentCapability::HealthOnly,
            })
            .map_err(|_| RpcError::new(INTERNAL_ERROR, "Cannot encode status"))
        }
        _ => Err(RpcError::new(METHOD_NOT_FOUND, "Method not found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn health_dispatch_validates_parameters_and_keeps_privileged_routes_closed() {
        let status = handle(Request::call(1, DAEMON_STATUS, None)).await.unwrap();
        let status: DaemonStatus = serde_json::from_value(status).unwrap();
        assert_eq!(status.capability, AgentCapability::HealthOnly);
        assert_eq!(status.core_version, version());
        assert_eq!(
            handle(Request::call(1, "pair.list", None))
                .await
                .unwrap_err()
                .code(),
            METHOD_NOT_FOUND
        );
        assert_eq!(
            handle(Request::call(
                1,
                DAEMON_STATUS,
                Some(json!({"root":"private"}))
            ))
            .await
            .unwrap_err()
            .code(),
            INVALID_PARAMS
        );
        assert_eq!(
            handle(Request::call(1, SESSION_HELLO, None))
                .await
                .unwrap_err()
                .code(),
            INVALID_PARAMS
        );
        assert_eq!(
            handle(Request::call(
                1,
                SESSION_HELLO,
                Some(json!({"protocol_version": PROTOCOL_VERSION + 1}))
            ))
            .await
            .unwrap_err()
            .code(),
            VERSION_MISMATCH
        );
        let hello = handle(Request::call(
            1,
            SESSION_HELLO,
            Some(json!({"protocol_version": PROTOCOL_VERSION})),
        ))
        .await
        .unwrap();
        assert_eq!(hello["core_version"], version());
    }

    #[test]
    fn version_is_the_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::version().is_empty());
    }
}
