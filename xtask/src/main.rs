//! `xtask` — tool (outside the layer graph). Foldover's task runner:
//! `cargo xtask ci` mirrors the CI gate, `check-layers` enforces the
//! PLAN.md §4.4 layer graph, `ffi`/`check-ffi` manage the UniFFI Swift
//! bindings, `bench-check` enforces the perf/budgets.toml medians.

// Adapted from L-K-M/Sweetbriar.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use xtask::ffi::FfiOutput;

type Step = fn(&Path) -> bool;

fn main() -> ExitCode {
    let Some(task) = env::args().nth(1) else {
        usage();
        return ExitCode::FAILURE;
    };

    match task.as_str() {
        "ci" => ci(),
        "check-layers" => check_layers(),
        "ffi" => ffi(),
        "check-ffi" => check_ffi(),
        "agent-smoke" => agent_smoke(),
        "bench-check" => bench_check(),
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

/// `cargo xtask ffi [--bindings-only]`: generate the Swift bindings
/// (and, on macOS, build the XCFramework).
fn ffi() -> ExitCode {
    let flags: Vec<String> = env::args().skip(2).collect();
    let output = match flags.as_slice() {
        [] => FfiOutput::BindingsAndXcframework,
        [flag] if flag == "--bindings-only" => FfiOutput::Bindings,
        _ => {
            usage();
            return ExitCode::FAILURE;
        }
    };
    if xtask::ffi::ffi(output) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `cargo xtask check-ffi`: the bindings drift and import check.
fn check_ffi() -> ExitCode {
    if env::args().nth(2).is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    if xtask::ffi::check_ffi() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The full local CI gate; first failure stops the run and names the
/// step.
fn ci() -> ExitCode {
    let root = workspace_root();
    let steps: [(&str, Step); 7] = [
        ("fmt", step_fmt),
        ("clippy", step_clippy),
        ("test", step_test),
        ("agent-smoke", step_agent_smoke),
        ("doc", step_doc),
        ("check-layers", step_check_layers),
        ("check-ffi", step_check_ffi),
    ];

    for (name, step) in steps {
        eprintln!("==> cargo xtask ci: {name}");
        if step(&root) {
            continue;
        }
        eprintln!("==> cargo xtask ci: step `{name}` failed; stopping");
        return ExitCode::FAILURE;
    }

    eprintln!("==> cargo xtask ci: all steps passed");
    ExitCode::SUCCESS
}

fn step_fmt(root: &Path) -> bool {
    run(&mut cargo(root, &["fmt", "--all", "--check"]))
}

fn step_clippy(root: &Path) -> bool {
    run(&mut cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ))
}

fn step_test(root: &Path) -> bool {
    run(&mut cargo(root, &["test", "--workspace"]))
}

fn agent_smoke() -> ExitCode {
    if env::args().nth(2).is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    if xtask::agent::smoke() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn step_agent_smoke(_root: &Path) -> bool {
    xtask::agent::smoke()
}

/// `cargo xtask bench-check`: run the criterion benches and enforce
/// perf/budgets.toml.
fn bench_check() -> ExitCode {
    if env::args().nth(2).is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    if xtask::bench::bench_check() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn step_doc(root: &Path) -> bool {
    let mut cmd = cargo(root, &["doc", "--workspace", "--no-deps"]);
    cmd.env("RUSTDOCFLAGS", "-D warnings");
    run(&mut cmd)
}

/// `cargo xtask check-layers`: the PLAN.md §4.4 layering gate, run on
/// its own.
fn check_layers() -> ExitCode {
    if env::args().nth(2).is_some() {
        usage();
        return ExitCode::FAILURE;
    }
    if run_check_layers(&workspace_root()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The `ci` step: same checker, folded into the step loop.
fn step_check_layers(root: &Path) -> bool {
    run_check_layers(root)
}

fn step_check_ffi(_root: &Path) -> bool {
    xtask::ffi::check_ffi()
}

/// Checks the workspace's manifests against `layers.toml` and prints
/// each violation, one actionable line each.
fn run_check_layers(root: &Path) -> bool {
    match xtask::layers::check_workspace(root) {
        Ok(violations) if violations.is_empty() => {
            eprintln!("check-layers: all layer boundaries hold");
            true
        }
        Ok(violations) => {
            for violation in &violations {
                eprintln!("check-layers: {violation}");
            }
            eprintln!("check-layers: {} violation(s)", violations.len());
            false
        }
        Err(err) => {
            eprintln!("check-layers: {err}");
            false
        }
    }
}

/// A `cargo` invocation pinned to the workspace root so `xtask` works
/// from any directory.
fn cargo(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    cmd.current_dir(root).args(args);
    cmd
}

/// Runs a command to completion, echoing it first; false on nonzero
/// exit or a spawn error.
fn run(cmd: &mut Command) -> bool {
    eprintln!("+ {}", describe(cmd));
    match cmd.status() {
        Ok(status) => status.success(),
        Err(err) => {
            eprintln!("failed to spawn: {err}");
            false
        }
    }
}

fn describe(cmd: &Command) -> String {
    let mut out = cmd.get_program().to_string_lossy().into_owned();
    for arg in cmd.get_args() {
        out.push(' ');
        out.push_str(&arg.to_string_lossy());
    }
    out
}

fn workspace_root() -> PathBuf {
    xtask::workspace_root()
}

fn usage() {
    eprintln!(
        "usage: cargo xtask <ci|check-layers|ffi [--bindings-only]|check-ffi|agent-smoke|bench-check>"
    );
}
