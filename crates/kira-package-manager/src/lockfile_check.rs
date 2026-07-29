//! Read-only verification of an optional `kira.lock`.

use crate::graph::{LockfileStatus, ResolvedPackage};
use kira_diagnostic_messages::package_messages::lockfile_drift;
use kira_diagnostics::Diagnostic;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LockSnapshot {
    root: Option<LockedRoot>,
    #[serde(rename = "root_dependency")]
    root_dependencies: Option<Vec<LockedDependency>>,
    #[serde(alias = "package")]
    packages: Vec<LockedPackage>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LockedRoot {
    name: String,
    dependencies: Option<Vec<LockedDependency>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LockedPackage {
    name: String,
    dependencies: Vec<LockedDependency>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LockedDependency {
    Name(String),
    Entry { name: String },
}

impl LockedDependency {
    fn name(&self) -> &str {
        match self {
            Self::Name(name) | Self::Entry { name } => name,
        }
    }
}

/// Checks an existing lockfile against the manifest-resolved package graph.
///
/// Reports what it found rather than repairing it: the caller decides whether
/// a stale lockfile is worth writing to disk, and resolution runs in places
/// (an editor, a language server) where writing would be a surprise.
pub(crate) fn check(
    root_dir: &Path,
    packages: &[ResolvedPackage],
    diagnostics: &mut Vec<Diagnostic>,
) -> LockfileStatus {
    let lock_path = root_dir.join("kira.lock");
    let text = match fs::read_to_string(&lock_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return LockfileStatus::Absent;
        }
        Err(error) => {
            diagnostics.push(lockfile_drift(&format!(
                "the file could not be read ({error})"
            )));
            return LockfileStatus::Drifted;
        }
    };
    let snapshot = match toml::from_str::<LockSnapshot>(&text) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            diagnostics.push(lockfile_drift(&format!(
                "the file could not be parsed ({error})"
            )));
            return LockfileStatus::Drifted;
        }
    };

    match drift_description(&snapshot, packages) {
        Some(description) => {
            diagnostics.push(lockfile_drift(&description));
            LockfileStatus::Drifted
        }
        None => LockfileStatus::Current,
    }
}

fn drift_description(snapshot: &LockSnapshot, packages: &[ResolvedPackage]) -> Option<String> {
    let root = packages.first()?;
    let Some(locked_root) = snapshot.root.as_ref() else {
        return Some("the root package record is missing".to_owned());
    };
    if locked_root.name != root.name {
        return Some(format!(
            "the root is `{}` instead of `{}`",
            locked_root.name, root.name
        ));
    }

    let resolved_root_edges = names(root);
    let locked_root_edges = match locked_root_edges(snapshot, locked_root) {
        Ok(edges) => edges,
        Err(description) => return Some(description),
    };
    if locked_root_edges != resolved_root_edges {
        return Some(format!(
            "the lockfile's root dependency set is {}, but the manifests resolve to {}",
            display_names(&locked_root_edges),
            display_names(&resolved_root_edges)
        ));
    }

    let resolved = package_edges(
        packages
            .iter()
            .skip(1)
            .map(|package| (package.name.as_str(), names(package))),
    );
    let locked = package_edges(
        snapshot
            .packages
            .iter()
            .map(|package| (package.name.as_str(), locked_names(&package.dependencies))),
    );

    let resolved_names = resolved.keys().cloned().collect::<BTreeSet<_>>();
    let locked_names = locked.keys().cloned().collect::<BTreeSet<_>>();
    if locked_names != resolved_names {
        return Some(format!(
            "the lockfile's package set is {}, but the manifests resolve to {}",
            display_names(&locked_names),
            display_names(&resolved_names)
        ));
    }

    for (name, resolved_edges) in resolved {
        let Some(locked_edges) = locked.get(&name) else {
            continue;
        };
        if locked_edges != &resolved_edges {
            return Some(format!(
                "the lockfile has `{name}` depending on {}, but its manifest declares {}",
                display_names(locked_edges),
                display_names(&resolved_edges)
            ));
        }
    }
    None
}

fn locked_root_edges(
    snapshot: &LockSnapshot,
    root: &LockedRoot,
) -> Result<BTreeSet<String>, String> {
    let top_level = snapshot.root_dependencies.as_deref().map(locked_names);
    let nested = root.dependencies.as_deref().map(locked_names);

    match (top_level, nested) {
        (Some(top_level), Some(nested)) if top_level != nested => Err(format!(
            "the top-level root dependency set is {}, but `root.dependencies` is {}",
            display_names(&top_level),
            display_names(&nested)
        )),
        (Some(top_level), _) => Ok(top_level),
        (None, Some(nested)) => Ok(nested),
        (None, None) => Ok(BTreeSet::new()),
    }
}

fn package_edges<'a>(
    packages: impl Iterator<Item = (&'a str, BTreeSet<String>)>,
) -> BTreeMap<String, BTreeSet<String>> {
    packages
        .map(|(name, dependencies)| (name.to_owned(), dependencies))
        .collect()
}

fn names(package: &ResolvedPackage) -> BTreeSet<String> {
    package
        .dependency_names()
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
}

fn locked_names(dependencies: &[LockedDependency]) -> BTreeSet<String> {
    dependencies
        .iter()
        .map(|dependency| dependency.name().to_owned())
        .collect()
}

fn display_names(names: &BTreeSet<String>) -> String {
    if names.is_empty() {
        "[]".to_owned()
    } else {
        format!("[{}]", names.iter().cloned().collect::<Vec<_>>().join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_diagnostics::Severity;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    const PROJECT_MATTER_LOCK: &str = r#"
version = 1

[root]
name = "editor"
version = "0.1.0"
kind = "app"
kira = "0.1.0"

[[root_dependency]]
name = "Core"
source = "path"
path = "../../modules/core"

[[root_dependency]]
name = "Editor"
source = "path"
path = "../../modules/editor"

[[root_dependency]]
name = "Graphics"
source = "path"
path = "../../modules/graphics"

[[root_dependency]]
name = "KiraGraphics"
source = "path"
path = "../../../kira-graphics"

[[package]]
name = "Core"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "Core"
source = "path"
path = "/project-matter/modules/core"

[[package]]
name = "Editor"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "Editor"
source = "path"
path = "/project-matter/modules/editor"
dependencies = ["KiraGraphics", "KiraUI", "Telemetry", "World"]

[[package]]
name = "Graphics"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "Graphics"
source = "path"
path = "/project-matter/modules/graphics"
dependencies = ["Core"]

[[package]]
name = "KiraGraphics"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "KiraGraphics"
source = "path"
path = "/kira-graphics"

[[package]]
name = "KiraLayout"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "KiraLayout"
source = "path"
path = "/kira-layout"

[[package]]
name = "KiraUI"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "KiraUI"
source = "path"
path = "/kira-ui"
dependencies = ["KiraUIFoundation"]

[[package]]
name = "KiraUIFoundation"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "KiraUIFoundation"
source = "path"
path = "/ui-foundation"
dependencies = ["KiraGraphics", "KiraLayout"]

[[package]]
name = "MathX"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "MathX"
source = "path"
path = "/project-matter/modules/math"

[[package]]
name = "Telemetry"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "Telemetry"
source = "path"
path = "/project-matter/modules/telemetry"
dependencies = ["Core"]

[[package]]
name = "World"
version = "0.1.0"
kind = "library"
kira = "0.1.0"
module_root = "World"
source = "path"
path = "/project-matter/modules/world"
dependencies = ["Core", "MathX"]
"#;

    fn resolved_package(name: &str, dependencies: &[&str]) -> ResolvedPackage {
        ResolvedPackage {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            kind: "library".to_owned(),
            kira_version: "0.1.0".to_owned(),
            module_root: name.to_owned(),
            root_dir: PathBuf::new(),
            source_dir: PathBuf::new(),
            dependencies: dependencies
                .iter()
                .map(|dependency| crate::graph::ResolvedDependency {
                    name: (*dependency).to_owned(),
                    source: kira_manifest::DependencySource::Path(kira_manifest::PathSource {
                        path: format!("../{dependency}"),
                    }),
                })
                .collect(),
        }
    }

    fn project_matter_packages() -> Vec<ResolvedPackage> {
        vec![
            resolved_package("editor", &["Editor", "KiraGraphics", "Core", "Graphics"]),
            resolved_package("Editor", &["KiraGraphics", "KiraUI", "Telemetry", "World"]),
            resolved_package("KiraGraphics", &[]),
            resolved_package("World", &["Core", "MathX"]),
            resolved_package("Core", &[]),
            resolved_package("KiraUI", &["KiraUIFoundation"]),
            resolved_package("Graphics", &["Core"]),
            resolved_package("Telemetry", &["Core"]),
            resolved_package("MathX", &[]),
            resolved_package("KiraUIFoundation", &["KiraGraphics", "KiraLayout"]),
            resolved_package("KiraLayout", &[]),
        ]
    }

    fn check_text(text: Option<&str>, packages: &[ResolvedPackage]) -> Vec<Diagnostic> {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let root_dir = std::env::temp_dir().join(format!(
            "kira-lockfile-check-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root_dir).expect("create lockfile check test directory");
        if let Some(text) = text {
            fs::write(root_dir.join("kira.lock"), text).expect("write test lockfile");
        }

        let mut diagnostics = Vec::new();
        check(&root_dir, packages, &mut diagnostics);

        fs::remove_dir_all(root_dir).expect("remove lockfile check test directory");
        diagnostics
    }

    #[test]
    fn top_level_root_dependencies_match_manifest_order_independently() {
        let diagnostics = check_text(Some(PROJECT_MATTER_LOCK), &project_matter_packages());

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn top_level_root_dependency_drift_is_reported() {
        let drifted = PROJECT_MATTER_LOCK.replacen(
            "name = \"KiraGraphics\"\nsource = \"path\"",
            "name = \"KiraLayout\"\nsource = \"path\"",
            1,
        );

        let diagnostics = check_text(Some(&drifted), &project_matter_packages());

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Some("KPK024"));
        assert_eq!(diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn nested_root_dependencies_remain_compatible() {
        let lock = r#"
[root]
name = "App"
dependencies = [{ name = "Core", source = { path = "../Core" } }]

[[package]]
name = "Core"
dependencies = []
"#;
        let packages = vec![
            resolved_package("App", &["Core"]),
            resolved_package("Core", &[]),
        ];

        let diagnostics = check_text(Some(lock), &packages);

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
    }

    #[test]
    fn malformed_lockfile_remains_a_warning() {
        let diagnostics = check_text(Some("[root"), &[]);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Some("KPK024"));
        assert_eq!(diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn absent_lockfile_remains_silent() {
        let diagnostics = check_text(None, &[]);

        assert!(diagnostics.is_empty());
    }
}
