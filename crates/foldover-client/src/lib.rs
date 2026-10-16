//! `foldover-client` — client layer. The typed async client for the
//! agent's local API: requests, subscriptions and reconnect, shared by
//! the CLI, the FFI crate and later the Linux app. Depends on
//! foldover-ipc. See PLAN.md §4.4.

/// The workspace version, sent in the `session.hello` handshake once P3
/// lands; P0 exposes it for `core_version()` and `foldover --version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_the_package_version() {
        assert_eq!(super::version(), env!("CARGO_PKG_VERSION"));
        assert!(!super::version().is_empty());
    }
}
