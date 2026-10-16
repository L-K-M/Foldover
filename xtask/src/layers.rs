//! `cargo xtask check-layers` — the PLAN.md §4.4 layering check.
//!
//! Reads `cargo metadata --format-version 1 --no-deps` (toolchain
//! output, never Rust source) and checks every workspace member's
//! first-party edges against `layers.toml`:
//!
//! - every member must be classified;
//! - a layered crate's normal and build dependencies must appear in
//!   its `deps` list or be vocabulary crates;
//! - vocabulary crates may depend only on each other;
//! - dev-role crates may only be dev-dependencies;
//! - tool-role crates are unrestricted.
//!
//! `check` is a pure function over the parsed inputs so the unit
//! tests need no fixture tree.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::Path;

use serde::Deserialize;

use crate::{ToolError, cargo, run, underscored};

/// The config file name at the workspace root.
const LAYERS_FILE: &str = "layers.toml";

/// The `check-layers` entry point: config + metadata in, violation
/// lines out.
pub fn check_workspace(root: &Path) -> Result<Vec<String>, ToolError> {
    let layers = Layers::load(&root.join(LAYERS_FILE))?;
    let crates = collect(root)?;
    Ok(check(&crates, &layers))
}

/// The check over already-parsed inputs: `crates` is every workspace
/// member's dependency edges, `layers` the parsed layers.toml.
/// Returns one actionable line per violation.
pub fn check(crates: &[CrateDeps], layers: &Layers) -> Vec<String> {
    let mut violations = Vec::new();
    for krate in crates {
        let Some(class) = layers.classify(&krate.name) else {
            violations.push(format!("{}: not classified in layers.toml", krate.name));
            continue;
        };

        // A deps entry naming an unclassified crate is a typo the edge
        // checks below would otherwise silently ignore.
        if let Class::Layer(spec) = class {
            for dep in &spec.deps {
                if layers.classify(dep).is_none() {
                    violations.push(format!(
                        "{}: deps names `{dep}`, which is not classified in layers.toml",
                        krate.name
                    ));
                }
            }
        }

        for edge in &krate.deps {
            let Some(target) = layers.classify(&edge.name) else {
                // Not a workspace crate: third-party, out of scope.
                continue;
            };
            if let Some(violation) = edge_violation(&krate.name, class, edge, target) {
                violations.push(violation);
            }
        }
    }
    violations
}

/// The one edge rule application per (source class, edge kind, target
/// class) — `None` when the edge is legal.
fn edge_violation(from: &str, class: &Class, edge: &DepEdge, target: &Class) -> Option<String> {
    match class {
        // Vocabulary crates hold data only: every first-party edge of
        // theirs must land on another vocabulary crate.
        Class::Vocab if !matches!(target, Class::Vocab) => Some(format!(
            "{from} (vocab) -> {} ({}): vocabulary crates may only depend on each other",
            edge.name,
            label(target)
        )),
        // dev-role crates may only ever be dev-dependencies.
        Class::Layer(_)
            if matches!(edge.kind, DepKind::Normal | DepKind::Build)
                && matches!(target, Class::Dev) =>
        {
            Some(format!(
                "{from} ({}) -> {} (dev): `{}` may only be a dev-dependency",
                label(class),
                edge.name,
                edge.name
            ))
        }
        Class::Layer(spec) if matches!(edge.kind, DepKind::Normal | DepKind::Build) => {
            if spec.deps.contains(&edge.name) || matches!(target, Class::Vocab) {
                return None;
            }
            Some(format!(
                "{from} ({}) -> {} ({}): first-party dependency on `{}` is not allowed by layers.toml",
                label(class),
                edge.name,
                label(target),
                edge.name
            ))
        }
        // Layered crates' dev-dependencies, and dev/tool crates' own
        // dependencies, are unrestricted.
        _ => None,
    }
}

/// How a classification prints in an edge violation.
fn label(class: &Class) -> &str {
    match class {
        Class::Layer(spec) => &spec.layer,
        Class::Vocab => "vocab",
        Class::Dev => "dev",
        Class::Tool => "tool",
    }
}

/// What a `layers.toml` entry makes of a crate.
enum Class {
    /// First-party code at a named layer, with the first-party crates
    /// it may take as normal or build dependencies.
    Layer(LayerSpec),
    /// Vocabulary: any crate may depend on it; it may depend only on
    /// other vocabulary crates.
    Vocab,
    /// Test support: a dev-dependency only; own deps unrestricted.
    Dev,
    /// Outside the graph (xtask, foldover-bindgen): unrestricted.
    Tool,
}

/// A `{ layer = ..., deps = [...] }` entry.
struct LayerSpec {
    /// The layer name (`driver`, `domain`, …) for diagnostics.
    layer: String,
    /// The allowed first-party dependencies, underscored.
    deps: BTreeSet<String>,
}

/// The parsed policy: crate name → classification.
pub struct Layers {
    classes: BTreeMap<String, Class>,
}

impl Layers {
    /// What `krate` is classified as; `None` is a violation for a
    /// workspace member and just "third-party" for a dependency.
    fn classify(&self, krate: &str) -> Option<&Class> {
        self.classes.get(krate)
    }

    /// Loads and validates `layers.toml`.
    fn load(path: &Path) -> Result<Self, ToolError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ToolError(format!("{}: {e}", path.display())))?;
        Self::parse(&text).map_err(|e| ToolError(format!("{}: {}", path.display(), e.0)))
    }

    /// Parses and validates `layers.toml` text; kept separate from
    /// `load` so tests exercise the same validation without files.
    fn parse(text: &str) -> Result<Self, ToolError> {
        let parsed: LayersFile =
            toml::from_str(text).map_err(|e| ToolError(format!("bad layers.toml: {e}")))?;
        let mut classes = BTreeMap::new();
        for (name, entry) in &parsed.crates {
            if !is_crate_name(name) {
                return Err(ToolError(format!(
                    "crate key `{name}` must be an underscored crate name"
                )));
            }
            let class = match (&entry.layer, &entry.role) {
                (Some(layer), None) => Class::Layer(LayerSpec {
                    layer: layer.clone(),
                    deps: entry.deps.clone(),
                }),
                (None, Some(role)) => match role.as_str() {
                    "vocab" => Class::Vocab,
                    "dev" => Class::Dev,
                    "tool" => Class::Tool,
                    other => {
                        return Err(ToolError(format!(
                            "crate `{name}` has unknown role `{other}` (vocab, dev or tool)"
                        )));
                    }
                },
                _ => {
                    return Err(ToolError(format!(
                        "crate `{name}` needs exactly one of `layer` or `role`"
                    )));
                }
            };
            if !entry.deps.is_empty() && !matches!(class, Class::Layer(_)) {
                return Err(ToolError(format!(
                    "crate `{name}` is a role crate; only layer crates declare deps"
                )));
            }
            classes.insert(name.clone(), class);
        }
        Ok(Self { classes })
    }
}

/// The checked-in shape of `layers.toml`.
#[derive(Deserialize)]
struct LayersFile {
    /// `foldover_state = { layer = "domain", deps = ["foldover_db"] }`,
    /// `xtask = { role = "tool" }`.
    #[serde(default)]
    crates: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
struct Entry {
    layer: Option<String>,
    role: Option<String>,
    #[serde(default)]
    deps: BTreeSet<String>,
}

/// Crate names are Rust identifiers (underscored); hyphens belong to
/// package names.
fn is_crate_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}

/// A workspace member's dependency edges, from `cargo metadata`.
pub struct CrateDeps {
    /// Crate name, underscored.
    pub name: String,
    /// Direct dependencies, all kinds and all cfg targets.
    pub deps: Vec<DepEdge>,
}

/// One dependency edge.
pub struct DepEdge {
    /// The depended-on package's name, underscored.
    pub name: String,
    /// normal / dev / build.
    pub kind: DepKind,
}

/// Cargo's dependency kinds.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    Normal,
    Dev,
    Build,
}

impl DepKind {
    /// metadata spells normal deps `"normal"` (or `null` in older
    /// JSON); anything unrecognized counts as normal — the checked
    /// kind — so a new kind can't slip past the rules.
    fn parse(kind: Option<&str>) -> Self {
        match kind {
            Some("dev") => Self::Dev,
            Some("build") => Self::Build,
            _ => Self::Normal,
        }
    }
}

/// `cargo metadata` JSON, the parts the check reads.
#[derive(Deserialize)]
struct Metadata {
    #[serde(default)]
    packages: Vec<Package>,
    #[serde(default)]
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    #[serde(default)]
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    kind: Option<String>,
}

/// Runs `cargo metadata --no-deps` in `root` and returns every
/// workspace member's edges. With `--no-deps`, `packages` lists the
/// members themselves; the `workspace_members` filter is belt and
/// braces.
fn collect(root: &Path) -> Result<Vec<CrateDeps>, ToolError> {
    let out = run(
        &cargo(),
        &["metadata", "--format-version", "1", "--no-deps"],
        root,
        None,
    )?;
    let meta: Metadata = serde_json::from_str(&out)
        .map_err(|e| ToolError(format!("cannot parse cargo metadata: {e}")))?;
    let members: HashSet<&str> = meta.workspace_members.iter().map(String::as_str).collect();
    Ok(meta
        .packages
        .iter()
        .filter(|pkg| members.contains(pkg.id.as_str()))
        .map(|pkg| CrateDeps {
            name: underscored(&pkg.name),
            deps: pkg
                .dependencies
                .iter()
                .map(|dep| DepEdge {
                    name: underscored(&dep.name),
                    kind: DepKind::parse(dep.kind.as_deref()),
                })
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A layered graph mirroring the real one, small enough to read:
    /// client (client) -> ipc (transport) -> net (driver), sim is dev,
    /// proto -> types is the one vocabulary edge.
    const LAYERS: &str = r#"
        [crates]
        foldover_types = { role = "vocab" }
        foldover_proto = { role = "vocab" }
        foldover_net = { layer = "driver", deps = [] }
        foldover_ipc = { layer = "transport", deps = ["foldover_net"] }
        foldover_client = { layer = "client", deps = ["foldover_ipc"] }
        foldover_sim = { role = "dev" }
        xtask = { role = "tool" }
    "#;

    fn layers() -> Layers {
        Layers::parse(LAYERS).expect("the test config parses")
    }

    /// A member with underscored `name` and `(dep, kind)` edges.
    fn krate(name: &str, deps: &[(&str, DepKind)]) -> CrateDeps {
        CrateDeps {
            name: name.to_string(),
            deps: deps
                .iter()
                .map(|(name, kind)| DepEdge {
                    name: (*name).to_string(),
                    kind: *kind,
                })
                .collect(),
        }
    }

    #[test]
    fn a_legal_workspace_passes() {
        let crates = vec![
            krate("foldover_types", &[]),
            krate("foldover_proto", &[("foldover_types", DepKind::Normal)]),
            krate("foldover_net", &[]),
            krate("foldover_ipc", &[("foldover_net", DepKind::Normal)]),
            krate(
                "foldover_client",
                &[
                    ("foldover_ipc", DepKind::Normal),
                    ("foldover_sim", DepKind::Dev), // dev support as a dev-dep
                    ("serde", DepKind::Normal),     // third-party is out of scope
                ],
            ),
            krate("foldover_sim", &[("foldover_net", DepKind::Normal)]),
            krate("xtask", &[("foldover_client", DepKind::Normal)]),
        ];
        assert_eq!(check(&crates, &layers()), Vec::<String>::new());
    }

    #[test]
    fn unclassified_member_is_a_violation() {
        let crates = vec![krate("foldover_weird", &[])];
        let violations = check(&crates, &layers());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("foldover_weird: not classified"));
    }

    #[test]
    fn deps_naming_an_unknown_crate_is_a_violation() {
        let text = r#"
            [crates]
            foldover_net = { layer = "driver", deps = ["foldover_typo"] }
        "#;
        let crates = vec![krate("foldover_net", &[])];
        let violations = check(&crates, &Layers::parse(text).unwrap());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("foldover_typo"));
    }

    #[test]
    fn undeclared_normal_and_build_deps_are_violations() {
        let crates = vec![krate(
            "foldover_client",
            &[
                ("foldover_ipc", DepKind::Normal), // declared: fine
                ("foldover_net", DepKind::Normal), // skips a layer
                ("foldover_net", DepKind::Build),  // same edge as a build dep
            ],
        )];
        let violations = check(&crates, &layers());
        assert_eq!(violations.len(), 2, "{violations:?}");
        assert!(violations.iter().all(|v| v.contains("not allowed")));
    }

    #[test]
    fn vocab_cannot_leave_the_vocabulary() {
        let crates = vec![
            krate("foldover_proto", &[("foldover_ipc", DepKind::Normal)]),
            krate("foldover_ipc", &[]),
        ];
        let violations = check(&crates, &layers());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("vocabulary crates"));
    }

    #[test]
    fn dev_support_as_a_normal_dep_is_a_violation() {
        let crates = vec![krate(
            "foldover_client",
            &[("foldover_sim", DepKind::Normal)],
        )];
        let violations = check(&crates, &layers());
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("dev-dependency"));
    }

    #[test]
    fn role_crates_cannot_declare_deps() {
        let text = r#"
            [crates]
            foldover_sim = { role = "dev", deps = ["foldover_net"] }
        "#;
        let err = Layers::parse(text).err().unwrap().to_string();
        assert!(err.contains("only layer crates declare deps"), "{err}");
    }

    /// The real workspace must satisfy its own `layers.toml`.
    #[test]
    fn real_workspace_passes() {
        let violations = check_workspace(&crate::workspace_root()).expect("real workspace checks");
        assert!(
            violations.is_empty(),
            "real workspace should be clean:\n{}",
            violations.join("\n")
        );
    }
}
