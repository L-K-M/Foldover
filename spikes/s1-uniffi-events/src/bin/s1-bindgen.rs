//! `s1-bindgen` — the Swift bindings generator, pinned to the same
//! `uniffi` version the library links so bindings and runtime can't
//! drift. `run.sh` drives it over the staticlib, the same artifact
//! the XCFramework ships.
//!
//! Adapted from L-K-M/Sweetbriar.

fn main() {
    uniffi::uniffi_bindgen_swift()
}
