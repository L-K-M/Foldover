//! Spike S3: what a plain command-line process (the shape foldoverd will
//! have, a LaunchAgent with no app context) gets from macOS FSEvents and
//! DiskArbitration. Writes a JSON report; prints a human summary.
//!
//! macOS-only: elsewhere the binary is a stub that says so. flags and
//! report are portable and unit-tested on every platform.

// On non-macOS the harness is not compiled, so flags/report only feed their
// own unit tests there and would warn as dead code in plain builds.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod flags;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod report;

#[cfg(target_os = "macos")]
mod diskarb;
#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
mod harness;
#[cfg(target_os = "macos")]
mod hdiutil;
#[cfg(target_os = "macos")]
mod ops;
#[cfg(target_os = "macos")]
mod sys;

fn main() {
    #[cfg(target_os = "macos")]
    std::process::exit(harness::run());
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!(
            "spike-s3-change-detection measures macOS FSEvents and DiskArbitration;\n\
             build and run it on macOS (the spike-s3 workflow runs it on macos-26)."
        );
        std::process::exit(1);
    }
}
