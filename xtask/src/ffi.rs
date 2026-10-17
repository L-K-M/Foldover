//! `cargo xtask ffi` and `cargo xtask check-ffi`: the UniFFI Swift
//! bindings pipeline over `foldover-ffi` (PLAN.md §4.5).
//!
//! `ffi` builds `foldover-ffi` for the host (so the cdylib with
//! UniFFI's metadata exists), runs `foldover-bindgen` over it and
//! installs the generated Swift under
//! `apple/FoldoverCore/Sources/FoldoverCore/`, the C header and
//! modulemap under `apple/FoldoverCore/Headers/`. On macOS it then
//! builds the arm64 release staticlib and wraps it, with the committed
//! headers, in the XCFramework the Swift package links; elsewhere the
//! full command generates the bindings and fails clearly.
//!
//! `check-ffi` regenerates into a temp dir and diffs the committed
//! files byte for byte, checks the Swift imports the FFI module the
//! modulemap declares and, on macOS, type-checks the committed Swift
//! against the committed headers — so the check still does real work
//! on Linux.

// Adapted from L-K-M/Sweetbriar.

use std::collections::HashSet;
use std::env::consts::DLL_SUFFIX;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{ToolError, cargo, run, run_env, workspace_root};

/// The crate the bindings export (Cargo's target name for
/// `foldover-ffi`).
const FFI_CRATE: &str = "foldover_ffi";

/// The module name the generated modulemap declares (`--module-name`).
const BINDINGS_MODULE_NAME: &str = "FoldoverCoreFFI";

/// UniFFI's config for the Swift bindings (`--config`): it names the
/// Swift module and the FFI module the Swift imports.
const BINDINGS_CONFIG: &str = "crates/foldover-bindgen/swift-bindings.toml";

/// The app's minimum macOS (PLAN.md §1). The Swift type-check targets
/// it and the staticlibs are built for it.
const MACOS_DEPLOYMENT_TARGET: &str = "27.0";

/// The app supports only Apple silicon.
const MACOS_TARGET: &str = "aarch64-apple-darwin";

/// The modulemap's filename (`--modulemap-filename`).
const MODULEMAP_FILENAME: &str = "module.modulemap";

/// The condition UniFFI's Swift wraps its FFI module import in.
const CAN_IMPORT: &str = "canImport(";

/// The Swift package that carries the generated bindings.
fn core_package(root: &Path) -> PathBuf {
    root.join("apple/FoldoverCore")
}

/// Where the generated `.swift` files are committed.
fn swift_sources_dir(root: &Path) -> PathBuf {
    core_package(root).join("Sources/FoldoverCore")
}

/// Where the generated header and modulemap are committed.
fn headers_dir(root: &Path) -> PathBuf {
    core_package(root).join("Headers")
}

/// Where `xtask ffi` builds the XCFramework on macOS (gitignored).
fn xcframework_path(root: &Path) -> PathBuf {
    core_package(root).join("FoldoverCoreFFI.xcframework")
}

/// What `cargo xtask ffi` produces.
pub enum FfiOutput {
    /// The committed bindings only (`--bindings-only`).
    Bindings,
    /// The bindings, then the XCFramework (macOS only).
    BindingsAndXcframework,
}

/// `cargo xtask ffi [--bindings-only]`.
pub fn ffi(output: FfiOutput) -> bool {
    let root = workspace_root();
    let out = temp_dir("foldover-ffi-generate");
    let outcome = generate_bindings(&root, &out)
        .and_then(|_| install_bindings(&out, &swift_sources_dir(&root), &headers_dir(&root)))
        .and_then(|()| {
            eprintln!(
                "ffi: bindings installed under {}",
                core_package(&root).display()
            );
            finish(&root, output)
        });
    let _ = fs::remove_dir_all(&out);
    let Err(err) = outcome else {
        return true;
    };
    eprintln!("ffi: {err}");
    false
}

/// `cargo xtask check-ffi`: a fresh generation matches the committed
/// bindings, and its Swift resolves its FFI import; on macOS the
/// committed Swift also type-checks against the committed headers.
pub fn check_ffi() -> bool {
    let root = workspace_root();
    let out = temp_dir("foldover-ffi-check");
    let checked = generate_bindings(&root, &out).map(|()| {
        let checks = [
            (
                binding_drift(&out, &swift_sources_dir(&root), &headers_dir(&root)),
                "file(s) drifted; run `cargo xtask ffi --bindings-only` and commit",
            ),
            (
                unresolved_ffi_imports(&out, &out),
                "unresolved FFI import(s): the Swift can't see its C declarations",
            ),
            (
                typecheck_swift(&root),
                "Swift type-check failure(s) in the committed bindings",
            ),
        ];

        let mut passed = true;
        for (findings, summary) in checks {
            if findings.is_empty() {
                continue;
            }
            passed = false;
            for line in &findings {
                eprintln!("check-ffi: {line}");
            }
            eprintln!("check-ffi: {} {summary}", findings.len());
        }
        passed
    });
    let _ = fs::remove_dir_all(&out);
    match checked {
        Ok(true) => {
            eprintln!("check-ffi: bindings match and resolve");
            true
        }
        Ok(false) => false,
        Err(err) => {
            eprintln!("check-ffi: {err}");
            false
        }
    }
}

/// Type-check the committed Swift against the committed headers the
/// way the app compiles it: Swift 6, complete strict concurrency,
/// warnings as errors, the app's target. macOS only: the Swift
/// toolchain and SDK exist nowhere else; elsewhere it says it
/// skipped.
fn typecheck_swift(root: &Path) -> Vec<String> {
    if !cfg!(target_os = "macos") {
        eprintln!("check-ffi: the Swift type-check needs macOS (macOS CI runs it)");
        return Vec::new();
    }
    let sources = match read_by_extension(&swift_sources_dir(root), "swift") {
        Ok(sources) => sources,
        Err(err) => return vec![err.to_string()],
    };
    let mut paths = Vec::new();
    for (path, _) in &sources {
        match path_string(path) {
            Ok(path) => paths.push(path),
            Err(err) => return vec![err.to_string()],
        }
    }
    let headers = match path_string(&headers_dir(root)) {
        Ok(headers) => headers,
        Err(err) => return vec![err.to_string()],
    };

    let swift_target = format!("arm64-apple-macos{MACOS_DEPLOYMENT_TARGET}");
    let mut args = vec![
        "-typecheck",
        "-swift-version",
        "6",
        "-strict-concurrency=complete",
        "-warnings-as-errors",
        "-target",
        &swift_target,
        "-I",
        &headers,
    ];
    args.extend(paths.iter().map(String::as_str));
    match run("swiftc", &args, root, None) {
        Ok(_) => Vec::new(),
        Err(err) => vec![err.to_string()],
    }
}

/// Build `foldover-ffi` for the host and generate the Swift bindings
/// into `out`.
fn generate_bindings(root: &Path, out: &Path) -> Result<(), ToolError> {
    let library = build_library(
        root,
        &["build", "-p", "foldover-ffi"],
        None,
        FFI_CRATE,
        LibraryKind::Cdylib,
    )?;
    let out_dir = path_string(out)?;
    let lib = path_string(&library)?;
    let config = path_string(&root.join(BINDINGS_CONFIG))?;
    run(
        &cargo(),
        &[
            "run",
            "-p",
            "foldover-bindgen",
            "--",
            "--swift-sources",
            "--headers",
            "--modulemap",
            // A plain `module`, not `--xcframework`'s `framework
            // module`: the XCFramework wraps a static library and a
            // headers dir, where Swift needs a non-framework module
            // map.
            "--module-name",
            BINDINGS_MODULE_NAME,
            "--modulemap-filename",
            MODULEMAP_FILENAME,
            "--config",
            &config,
            &lib,
            &out_dir,
        ],
        root,
        None,
    )?;
    Ok(())
}

/// Which of a crate's library files to take from its build.
#[derive(Clone, Copy)]
enum LibraryKind {
    /// The host `cdylib`, which carries UniFFI's metadata for the
    /// bindgen.
    Cdylib,
    /// The `staticlib` the XCFramework wraps (Apple targets: `.a`).
    Staticlib,
}

impl LibraryKind {
    /// The crate type Cargo lists in the artifact's `target.kind`.
    fn crate_type(self) -> &'static str {
        match self {
            Self::Cdylib => "cdylib",
            Self::Staticlib => "staticlib",
        }
    }

    /// The file suffix that tells this library from the target's
    /// other outputs (the `.rlib`, the other library).
    fn suffix(self) -> &'static str {
        match self {
            Self::Cdylib => DLL_SUFFIX,
            Self::Staticlib => ".a",
        }
    }
}

/// A `compiler-artifact` line of Cargo's `--message-format=json`
/// output: the files one target produced.
#[derive(Deserialize)]
struct Artifact {
    reason: String,
    #[serde(default)]
    target: ArtifactTarget,
    #[serde(default)]
    filenames: Vec<PathBuf>,
}

/// The artifact's target: its crate name and crate types.
#[derive(Default, Deserialize)]
struct ArtifactTarget {
    #[serde(default)]
    name: String,
    #[serde(default)]
    kind: Vec<String>,
}

/// Run `cargo <args> --message-format=json` in `dir` and return the
/// `kind` library Cargo reports for `crate_name`. Cargo's own record
/// of where it wrote the file, so `CARGO_TARGET_DIR`, `--target` and
/// the profile all count: a guessed path could name a stale library
/// from another build and check the wrong exports. `target` overrides
/// the target dir.
fn build_library(
    dir: &Path,
    args: &[&str],
    target: Option<&Path>,
    crate_name: &str,
    kind: LibraryKind,
) -> Result<PathBuf, ToolError> {
    build_library_with_env(dir, args, target, &[], crate_name, kind)
}

/// `build_library` with environment overrides for the build
/// (`run_env`).
fn build_library_with_env(
    dir: &Path,
    args: &[&str],
    target: Option<&Path>,
    env: &[(&str, Option<&str>)],
    crate_name: &str,
    kind: LibraryKind,
) -> Result<PathBuf, ToolError> {
    let mut args = args.to_vec();
    args.push("--message-format=json");
    let stdout = run_env(&cargo(), &args, dir, target, env)?;
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Artifact>(line).ok())
        .filter(|artifact| {
            artifact.reason == "compiler-artifact"
                && artifact.target.name == crate_name
                && artifact.target.kind.iter().any(|k| k == kind.crate_type())
        })
        .flat_map(|artifact| artifact.filenames)
        .find(|file| {
            file.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(kind.suffix()))
        })
        .ok_or_else(|| {
            ToolError(format!(
                "cargo {} reported no {} for {crate_name}",
                args.join(" "),
                kind.crate_type()
            ))
        })
}

/// Copy the generated files into their committed homes — `.swift`
/// into `swift_dir`, the header and modulemap into `headers_dir` —
/// and remove committed files no longer generated, so a renamed
/// output leaves no stale copy behind (both dirs hold generated
/// files only; `binding_drift` enforces it). Copy rather than rename:
/// the generation dir may be on another filesystem.
fn install_bindings(
    generated: &Path,
    swift_dir: &Path,
    headers_dir: &Path,
) -> Result<(), ToolError> {
    fs::create_dir_all(swift_dir).map_err(io("create", swift_dir))?;
    fs::create_dir_all(headers_dir).map_err(io("create", headers_dir))?;
    let files = generated_files(generated)?;
    for file in &files {
        let to = home(file, swift_dir, headers_dir).join(file);
        fs::copy(generated.join(file), &to).map_err(io("copy", &to))?;
    }

    for dir in [swift_dir, headers_dir] {
        for entry in fs::read_dir(dir).map_err(io("read", dir))? {
            let path = entry.map_err(io("read", dir))?.path();
            let generated_here = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| belongs_here(name, dir, &files, swift_dir, headers_dir));
            if generated_here {
                continue;
            }
            fs::remove_file(&path).map_err(io("remove", &path))?;
            eprintln!("ffi: removed {}, not generated there", path.display());
        }
    }
    Ok(())
}

/// The directory a generated file is committed in: the Swift sources
/// apart from the C header and modulemap.
fn home<'a>(name: &str, swift_dir: &'a Path, headers_dir: &'a Path) -> &'a Path {
    if name.ends_with(".swift") {
        swift_dir
    } else {
        headers_dir
    }
}

/// Whether `name`, found in `dir`, is a generated file in its home.
/// A generated name in the other directory is as stale as one no
/// longer generated: a `.swift` copy under the headers, say.
fn belongs_here(
    name: &str,
    dir: &Path,
    files: &[String],
    swift_dir: &Path,
    headers_dir: &Path,
) -> bool {
    files.iter().any(|file| file == name) && home(name, swift_dir, headers_dir) == dir
}

/// After the bindings: the XCFramework on macOS; anywhere else the
/// full command fails clearly — `--bindings-only` is the portable
/// form.
fn finish(root: &Path, output: FfiOutput) -> Result<(), ToolError> {
    if let FfiOutput::Bindings = output {
        return Ok(());
    }
    if !cfg!(target_os = "macos") {
        return Err(ToolError(
            "the XCFramework needs macOS (run `cargo xtask ffi` there or in \
             macOS CI; elsewhere use `cargo xtask ffi --bindings-only`)"
                .to_string(),
        ));
    }
    build_xcframework(root)
}

/// Wrap the arm64 release staticlib and committed headers in the
/// XCFramework (macOS only).
fn build_xcframework(root: &Path) -> Result<(), ToolError> {
    let library = build_library_with_env(
        root,
        &[
            "build",
            "-p",
            "foldover-ffi",
            "--release",
            "--target",
            MACOS_TARGET,
        ],
        None,
        &[("MACOSX_DEPLOYMENT_TARGET", Some(MACOS_DEPLOYMENT_TARGET))],
        FFI_CRATE,
        LibraryKind::Staticlib,
    )?;

    let output = xcframework_path(root);
    if output.exists() {
        fs::remove_dir_all(&output).map_err(io("remove", &output))?;
    }
    let library = path_string(&library)?;
    let headers = path_string(&headers_dir(root))?;
    let output = path_string(&output)?;
    run(
        "xcodebuild",
        &[
            "-create-xcframework",
            "-library",
            &library,
            "-headers",
            &headers,
            "-output",
            &output,
        ],
        root,
        None,
    )?;
    eprintln!("ffi: XCFramework built at {output}");
    Ok(())
}

/// The generated files that must be committed, sorted: everything the
/// bindgen wrote except cargo leftovers. An empty list is an error —
/// a generation that produced nothing would silently pass the drift
/// check.
fn generated_files(generated: &Path) -> Result<Vec<String>, ToolError> {
    let mut files: Vec<String> = fs::read_dir(generated)
        .map_err(io("read", generated))?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            (name != "Cargo.lock" && !name.starts_with('.')).then_some(name)
        })
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(ToolError(format!(
            "the bindgen wrote nothing into {}",
            generated.display()
        )));
    }
    Ok(files)
}

/// Drift between a fresh generation and the committed bindings: every
/// generated file must be committed byte for byte in the directory
/// its kind belongs to, and every committed file must still be
/// generated for the directory it sits in. Each entry names the
/// committed path.
fn binding_drift(
    generated: &Path,
    committed_swift: &Path,
    committed_headers: &Path,
) -> Vec<String> {
    let mut drift = Vec::new();
    let Ok(files) = generated_files(generated) else {
        return vec![format!(
            "cannot read the generated bindings under {}",
            generated.display()
        )];
    };
    for name in &files {
        let dir = home(name, committed_swift, committed_headers);
        let label = if dir == committed_swift {
            "Swift sources"
        } else {
            "headers"
        };
        let committed = dir.join(name);
        let Ok(generated_bytes) = fs::read(generated.join(name)) else {
            drift.push(format!("could not read the generated {name}"));
            continue;
        };
        match fs::read(&committed) {
            Ok(committed_bytes) if committed_bytes == generated_bytes => {}
            Ok(_) => drift.push(format!(
                "{} {name} drifted from the generated bindings: {}",
                label,
                committed.display()
            )),
            Err(_) => drift.push(format!(
                "{name} is generated but not committed where it belongs: {}",
                committed.display()
            )),
        }
    }
    for dir in [committed_swift, committed_headers] {
        let Ok(entries) = fs::read_dir(dir) else {
            drift.push(format!(
                "cannot read the committed bindings in {}",
                dir.display()
            ));
            continue;
        };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let Some(name) = entry.file_name().into_string().ok() else {
                continue;
            };
            if belongs_here(&name, dir, &files, committed_swift, committed_headers) {
                continue;
            }
            if files.contains(&name) {
                drift.push(format!(
                    "{name} is generated for {}, not here: {}",
                    home(&name, committed_swift, committed_headers).display(),
                    dir.join(&name).display()
                ));
                continue;
            }
            drift.push(format!(
                "{name} is committed but no longer generated: {}",
                dir.join(&name).display()
            ));
        }
    }
    drift
}

/// Whether the Swift can see its C declarations: every module the
/// Swift sources import behind `canImport(…)` — how UniFFI's Swift
/// imports its FFI module — must be declared by a modulemap in
/// `headers`, and every header a modulemap names must sit beside it.
/// Swift built against a mismatch finds no `RustBuffer` and no FFI
/// symbol, yet the XCFramework still builds; this catches it on any
/// host. Swift with no FFI import at all is reported too, so the
/// check never passes for want of anything to check. Each entry names
/// the file at fault.
fn unresolved_ffi_imports(swift: &Path, headers: &Path) -> Vec<String> {
    let (sources, modulemaps) = match (
        read_by_extension(swift, "swift"),
        read_by_extension(headers, "modulemap"),
    ) {
        (Ok(sources), Ok(modulemaps)) => (sources, modulemaps),
        (Err(err), _) | (_, Err(err)) => return vec![err.to_string()],
    };

    let mut problems = Vec::new();
    let mut declared = HashSet::new();
    for (path, text) in &modulemaps {
        let (modules, named_headers) = modulemap_entries(text);
        declared.extend(modules);
        for header in named_headers {
            if !headers.join(header).is_file() {
                problems.push(format!(
                    "{} names the header {header}, which isn't beside it",
                    path.display()
                ));
            }
        }
    }

    let mut imports = 0;
    for (path, text) in &sources {
        for module in conditional_imports(text) {
            imports += 1;
            if !declared.contains(module) {
                problems.push(format!(
                    "{} imports {module}, which no modulemap in {} declares",
                    path.display(),
                    headers.display()
                ));
            }
        }
    }
    if imports == 0 {
        problems.push(format!(
            "no Swift source in {} imports an FFI module",
            swift.display()
        ));
    }
    problems
}

/// The modules named by `canImport(Name)` in Swift source: UniFFI's
/// generated Swift wraps its FFI import in it (`#if canImport(XFFI)
/// import XFFI #endif`).
fn conditional_imports(swift: &str) -> Vec<&str> {
    swift
        .split(CAN_IMPORT)
        .skip(1)
        .filter_map(|rest| rest.split_once(')').map(|(module, _)| module.trim()))
        .collect()
}

/// A modulemap's declared modules (`module Name {`, also after
/// `framework`) and the headers it names (`header "x.h"`).
fn modulemap_entries(modulemap: &str) -> (Vec<&str>, Vec<&str>) {
    let mut modules = Vec::new();
    let mut headers = Vec::new();
    let mut tokens = modulemap.split_whitespace();
    while let Some(token) = tokens.next() {
        match token {
            "module" => modules.extend(tokens.next().map(|name| name.trim_end_matches('{'))),
            "header" => headers.extend(tokens.next().map(|name| name.trim_matches('"'))),
            _ => {}
        }
    }
    (modules, headers)
}

/// Every file in `dir` with extension `ext`, with its contents,
/// sorted by path.
fn read_by_extension(dir: &Path, ext: &str) -> Result<Vec<(PathBuf, String)>, ToolError> {
    let mut paths: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(io("read", dir))?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| path.extension().is_some_and(|found| found == ext))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).map_err(io("read", &path))?;
            Ok((path, text))
        })
        .collect()
}

/// A fresh, empty scratch directory path under the system temp dir,
/// named for the run so parallel invocations can't collide. Callers
/// remove it.
fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{label}-{}", std::process::id()));
    // A killed earlier run with a reused pid may have left files.
    let _ = fs::remove_dir_all(&dir);
    dir
}

/// A path as a String for `run`, which takes `&[&str]`.
fn path_string(path: &Path) -> Result<String, ToolError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ToolError(format!("{} is not valid UTF-8", path.display())))
}

/// An I/O failure message naming the operation and the path.
fn io<'a, 'b>(what: &'a str, path: &'b Path) -> impl Fn(std::io::Error) -> ToolError + use<'a, 'b> {
    move |err| ToolError(format!("cannot {what} {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installing over outdated bindings leaves exactly the fresh
    /// generation behind: changed files replaced, files no longer
    /// generated (a renamed output) or in the wrong directory removed.
    #[test]
    fn install_replaces_outdated_and_removes_stale_bindings() {
        let root = temp_dir("xtask-ffi-install");
        let generated = root.join("generated");
        let swift = root.join("Sources");
        let headers = root.join("Headers");
        for dir in [&generated, &swift, &headers] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(generated.join("Core.swift"), "let a = 2\n").unwrap();
        fs::write(generated.join("CoreFFI.h"), "// new\n").unwrap();
        fs::write(generated.join(MODULEMAP_FILENAME), "module CoreFFI {}\n").unwrap();
        fs::write(swift.join("Core.swift"), "let a = 1\n").unwrap();
        fs::write(swift.join("old.swift"), "let old = 0\n").unwrap();
        fs::write(headers.join("oldFFI.h"), "// old\n").unwrap();
        // Generated names in the other kind's directory are stale too.
        fs::write(headers.join("Core.swift"), "let a = 1\n").unwrap();
        fs::write(swift.join("CoreFFI.h"), "// old\n").unwrap();

        install_bindings(&generated, &swift, &headers).expect("installs");

        for misplaced in [headers.join("Core.swift"), swift.join("CoreFFI.h")] {
            assert!(!misplaced.exists(), "{} survived", misplaced.display());
        }

        assert_eq!(
            binding_drift(&generated, &swift, &headers),
            Vec::<String>::new()
        );
        let _ = fs::remove_dir_all(&root);
    }
}
