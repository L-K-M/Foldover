//! `foldover-ffi` — client layer. The UniFFI export of foldover-client
//! for the Swift app (the `FoldoverCore` module; P0 exports
//! `core_version` only). Depends on foldover-client. See PLAN.md §4.4.
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

uniffi::setup_scaffolding!();

#[cfg(test)]
mod tests {
    #[test]
    fn core_version_is_the_package_version() {
        assert_eq!(super::core_version(), env!("CARGO_PKG_VERSION"));
    }
}
