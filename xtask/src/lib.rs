//! `xtask` — tool (outside the layer graph). The library half of
//! Foldover's task runner, split from the binary so unit tests can
//! drive the checks directly. See PLAN.md §4.4.

// Adapted from L-K-M/Sweetbriar.

pub mod agent;
pub mod ffi;
pub mod layers;

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// An external command or tool the task runner invokes failed: a bad
/// spawn, a nonzero exit, or output it couldn't read.
#[derive(Debug)]
pub struct ToolError(pub String);

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

/// The cargo binary to run: `$CARGO` inside cargo's own child
/// processes, `cargo` elsewhere.
pub(crate) fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// Runs `prog args` in `dir` with `CARGO_TARGET_DIR` set when given,
/// returning stdout. A failed spawn or a nonzero exit is an error
/// whose message carries the first compiler error or stderr's
/// `error:` lines — the first line is usually just progress
/// (`Documenting …`), the last the cause.
pub(crate) fn run(
    prog: &str,
    args: &[&str],
    dir: &Path,
    target: Option<&Path>,
) -> Result<String, ToolError> {
    run_env(prog, args, dir, target, &[])
}

/// `run` plus environment overrides: a `None` value removes the
/// variable, a `Some` sets it — cargo's own environment wins over
/// config files, so setting a var empty also neutralizes a
/// `[build]`-table flag.
pub(crate) fn run_env(
    prog: &str,
    args: &[&str],
    dir: &Path,
    target: Option<&Path>,
    env: &[(&str, Option<&str>)],
) -> Result<String, ToolError> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).current_dir(dir);
    if let Some(t) = target {
        cmd.env("CARGO_TARGET_DIR", t);
    }
    for (key, value) in env {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    let out = cmd
        .output()
        .map_err(|e| ToolError(format!("{prog} {}: {e}", args.join(" "))))?;
    if !out.status.success() {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ToolError(format!(
            "{prog} {}: {}",
            args.join(" "),
            error_detail(&stdout, &stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What a failed tool actually complained about. With
/// `--message-format=json` the compiler's diagnostics are on stdout —
/// the first error-level message's rendered text says what broke —
/// else every stderr line starting with `error`, else stderr's last
/// lines. A silent failure still yields `failed`, never an empty
/// message.
fn error_detail(stdout: &str, stderr: &str) -> String {
    for line in stdout.lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let level = &msg["message"]["level"];
        if msg["reason"] == "compiler-message"
            && level.as_str().is_some_and(|l| l.starts_with("error"))
            && let Some(rendered) = msg["message"]["rendered"].as_str()
        {
            return rendered.trim_end().to_string();
        }
    }
    let errors: Vec<&str> = stderr.lines().filter(|l| l.starts_with("error")).collect();
    if !errors.is_empty() {
        return errors.join("; ");
    }
    let tail = stderr
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("; ");
    if tail.is_empty() {
        "failed".to_string()
    } else {
        tail
    }
}

/// `pkg`'s crate name — hyphens belong to packages, underscores to
/// crates.
pub(crate) fn underscored(pkg: &str) -> String {
    pkg.replace('-', "_")
}

/// The workspace root: this crate lives at `<workspace>/xtask`.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives at <workspace>/xtask")
        .to_path_buf()
}
