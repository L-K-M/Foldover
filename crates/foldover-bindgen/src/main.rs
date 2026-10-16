//! `foldover-bindgen` — tool (outside the layer graph). The Swift
//! bindings generator pinned to the workspace's `uniffi` version, so
//! bindings and runtime can't drift. Delegates to UniFFI's own Swift
//! CLI entry point; `cargo xtask ffi` and `check-ffi` drive it.
//! See PLAN.md §4.4.

// Adapted from L-K-M/Sweetbriar.

fn main() {
    uniffi::uniffi_bindgen_swift()
}
