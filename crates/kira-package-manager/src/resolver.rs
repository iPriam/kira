//! Canonical-path resolution of transitive package dependencies.

use crate::graph::{ResolvedPackage, ResolvedPackageGraph};
use crate::lockfile_check;
use kira_diagnostic_messages::package_messages::{
    conflicting_package_identity, cyclic_package_dependency, duplicate_dependency_declaration,
    missing_dependency_package,
};
use kira_diagnostics::Diagnostic;
use kira_manifest::{DeclarationError, DependencySpec, ProjectManifest, load_declaration};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

/// A hard failure that prevents the root package from being resolved.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// The requested root directory does not exist or cannot be canonicalized.
    #[error("package root `{path}` is not readable")]
    RootDirectory {
        /// The requested package root.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The root package manifest cannot be read.
    #[error("root package manifest `{path}` is not readable")]
    RootManifestRead {
        /// The expected root manifest path.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The root package manifest is not a valid package declaration.
    #[error("root package manifest `{path}` is invalid")]
    RootManifestInvalid {
        /// The root manifest path.
        path: PathBuf,
        /// The declaration decoding failure.
        #[source]
        source: DeclarationError,
    },
}

#[derive(Debug)]
struct PendingDependency {
    parent: usize,
    name: String,
    candidate_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Unvisited,
    Visiting,
    Visited,
}

/// Resolves the root package and every reachable local path dependency.
///
/// An unreadable root is returned as [`ResolveError`]. Problems below the root
/// are accumulated as diagnostics so callers can continue with the readable
/// portion of the graph.
pub fn resolve(root_dir: &Path) -> Result<ResolvedPackageGraph, ResolveError> {
    let canonical_root =
        fs::canonicalize(root_dir).map_err(|source| ResolveError::RootDirectory {
            path: root_dir.to_path_buf(),
            source,
        })?;
    let manifest_path = canonical_root.join("package.kira");
    let root_text =
        fs::read_to_string(&manifest_path).map_err(|source| ResolveError::RootManifestRead {
            path: manifest_path.clone(),
            source,
        })?;
    let root_manifest =
        load_declaration(&root_text).map_err(|source| ResolveError::RootManifestInvalid {
            path: manifest_path,
            source,
        })?;

    let mut diagnostics = Vec::new();
    let (root_dependencies, root_pending) = prepare_dependencies(
        0,
        &canonical_root,
        &root_manifest.dependencies,
        &mut diagnostics,
    );
    let root_package = resolved_package(root_manifest, canonical_root.clone(), root_dependencies);
    let root_name = root_package.name.clone();

    let mut packages = vec![root_package];
    let mut adjacency = vec![Vec::new()];
    let mut directories = HashMap::from([(canonical_root.clone(), 0usize)]);
    let mut names = HashMap::from([(root_name, canonical_root.clone())]);
    let mut pending = VecDeque::from(root_pending);

    while let Some(dependency) = pending.pop_front() {
        let display_path = dependency.candidate_dir.display().to_string();
        let canonical_dir = match fs::canonicalize(&dependency.candidate_dir) {
            Ok(path) if path.is_dir() => path,
            Ok(_) => {
                diagnostics.push(missing_dependency_package(&dependency.name, &display_path));
                continue;
            }
            Err(_) => {
                diagnostics.push(missing_dependency_package(&dependency.name, &display_path));
                continue;
            }
        };

        if let Some(&package_index) = directories.get(&canonical_dir) {
            check_expected_identity(
                &dependency.name,
                &packages[package_index],
                &canonical_dir,
                &mut diagnostics,
            );
            record_name_root(
                &dependency.name,
                &canonical_dir,
                &mut names,
                &mut diagnostics,
            );
            push_edge(&mut adjacency, dependency.parent, package_index);
            continue;
        }

        let dependency_manifest_path = canonical_dir.join("package.kira");
        let dependency_manifest = match fs::read_to_string(&dependency_manifest_path)
            .ok()
            .and_then(|text| load_declaration(&text).ok())
        {
            Some(manifest) => manifest,
            None => {
                diagnostics.push(missing_dependency_package(
                    &dependency.name,
                    &canonical_dir.display().to_string(),
                ));
                continue;
            }
        };

        let (dependencies, child_pending) = prepare_dependencies(
            packages.len(),
            &canonical_dir,
            &dependency_manifest.dependencies,
            &mut diagnostics,
        );
        let package = resolved_package(dependency_manifest, canonical_dir.clone(), dependencies);
        check_expected_identity(&dependency.name, &package, &canonical_dir, &mut diagnostics);
        record_name_root(
            &dependency.name,
            &canonical_dir,
            &mut names,
            &mut diagnostics,
        );
        if package.name != dependency.name {
            record_name_root(&package.name, &canonical_dir, &mut names, &mut diagnostics);
        }

        let package_index = packages.len();
        directories.insert(canonical_dir, package_index);
        packages.push(package);
        adjacency.push(Vec::new());
        push_edge(&mut adjacency, dependency.parent, package_index);
        pending.extend(child_pending);
    }

    diagnose_cycles(&packages, &adjacency, &mut diagnostics);
    lockfile_check::check(&canonical_root, &packages, &mut diagnostics);

    Ok(ResolvedPackageGraph {
        packages,
        diagnostics,
    })
}

fn resolved_package(
    manifest: ProjectManifest,
    root_dir: PathBuf,
    dependencies: Vec<String>,
) -> ResolvedPackage {
    let module_root = manifest
        .module_root
        .unwrap_or_else(|| manifest.name.clone());
    ResolvedPackage {
        name: manifest.name,
        module_root,
        source_dir: root_dir.join("app"),
        root_dir,
        dependencies,
    }
}

fn prepare_dependencies(
    parent: usize,
    package_dir: &Path,
    dependencies: &[DependencySpec],
    diagnostics: &mut Vec<Diagnostic>,
) -> (Vec<String>, Vec<PendingDependency>) {
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    let mut pending = Vec::new();
    for dependency in dependencies {
        if !seen.insert(dependency.name.clone()) {
            diagnostics.push(duplicate_dependency_declaration(&dependency.name));
            continue;
        }
        names.push(dependency.name.clone());
        if let Some(path_source) = dependency.source.as_path() {
            pending.push(PendingDependency {
                parent,
                name: dependency.name.clone(),
                candidate_dir: package_dir.join(&path_source.path),
            });
        }
    }
    (names, pending)
}

fn check_expected_identity(
    expected_name: &str,
    package: &ResolvedPackage,
    canonical_dir: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if package.name == expected_name && package.module_root == expected_name {
        return;
    }
    diagnostics.push(conflicting_package_identity(
        expected_name,
        &format!(
            "dependency `{expected_name}` at {}",
            canonical_dir.display()
        ),
        &format!(
            "manifest package `{}` with module root `{}`",
            package.name, package.module_root
        ),
    ));
}

fn record_name_root(
    name: &str,
    canonical_dir: &Path,
    names: &mut HashMap<String, PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match names.get(name) {
        Some(first_dir) if first_dir != canonical_dir => {
            diagnostics.push(conflicting_package_identity(
                name,
                &first_dir.display().to_string(),
                &canonical_dir.display().to_string(),
            ));
        }
        Some(_) => {}
        None => {
            names.insert(name.to_owned(), canonical_dir.to_path_buf());
        }
    }
}

fn push_edge(adjacency: &mut [Vec<usize>], parent: usize, child: usize) {
    if let Some(edges) = adjacency.get_mut(parent) {
        edges.push(child);
    }
}

fn diagnose_cycles(
    packages: &[ResolvedPackage],
    adjacency: &[Vec<usize>],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut states = vec![VisitState::Unvisited; packages.len()];
    let mut stack = Vec::new();
    for package in 0..packages.len() {
        if states.get(package) == Some(&VisitState::Unvisited) {
            visit(
                package,
                packages,
                adjacency,
                &mut states,
                &mut stack,
                diagnostics,
            );
        }
    }
}

fn visit(
    package: usize,
    packages: &[ResolvedPackage],
    adjacency: &[Vec<usize>],
    states: &mut [VisitState],
    stack: &mut Vec<usize>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(state) = states.get_mut(package) else {
        return;
    };
    *state = VisitState::Visiting;
    stack.push(package);

    if let Some(edges) = adjacency.get(package) {
        for &dependency in edges {
            match states.get(dependency).copied() {
                Some(VisitState::Unvisited) => {
                    visit(dependency, packages, adjacency, states, stack, diagnostics);
                }
                Some(VisitState::Visiting) => {
                    if let Some(cycle_start) = stack.iter().position(|entry| *entry == dependency) {
                        let mut cycle = stack[cycle_start..]
                            .iter()
                            .filter_map(|index| packages.get(*index))
                            .map(|package| package.name.clone())
                            .collect::<Vec<_>>();
                        if let Some(package) = packages.get(dependency) {
                            cycle.push(package.name.clone());
                        }
                        diagnostics.push(cyclic_package_dependency(&cycle));
                    }
                }
                Some(VisitState::Visited) | None => {}
            }
        }
    }

    stack.pop();
    if let Some(state) = states.get_mut(package) {
        *state = VisitState::Visited;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_diagnostics::Severity;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "kira-package-resolver-{tag}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create resolver test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_package(dir: &Path, name: &str, dependencies: &[(&str, &str)]) {
        fs::create_dir_all(dir.join("app")).expect("create package source directory");
        let dependencies = dependencies
            .iter()
            .map(|(dependency, path)| {
                format!("Dependency {{ name: \"{dependency}\", path: \"{path}\" }}")
            })
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = format!(
            "Package {name} {{\n \
             let kind = .Library\n \
             let moduleRoot = \"{name}\"\n \
             let dependencies = [{dependencies}]\n\
             }}\n"
        );
        fs::write(dir.join("package.kira"), manifest).expect("write package manifest");
    }

    fn diagnostic_codes(graph: &ResolvedPackageGraph) -> Vec<&'static str> {
        graph
            .diagnostics
            .iter()
            .filter_map(|diagnostic| diagnostic.code)
            .collect()
    }

    #[test]
    fn resolves_and_deduplicates_a_diamond() {
        let temp = TempDir::new("diamond");
        let a = temp.path().join("A");
        let b = temp.path().join("B");
        let c = temp.path().join("C");
        let d = temp.path().join("D");
        write_package(&a, "A", &[("B", "../B"), ("C", "../C")]);
        write_package(&b, "B", &[("D", "../D")]);
        write_package(&c, "C", &[("D", "../D")]);
        write_package(&d, "D", &[]);

        let graph = resolve(&a).unwrap();

        assert!(graph.diagnostics.is_empty(), "{:?}", graph.diagnostics);
        assert_eq!(graph.packages.len(), 4);
        let names = graph
            .packages
            .iter()
            .map(|package| package.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["A", "B", "C", "D"]);
        let package_b = graph
            .packages
            .iter()
            .find(|package| package.name == "B")
            .unwrap();
        let package_c = graph
            .packages
            .iter()
            .find(|package| package.name == "C")
            .unwrap();
        let package_d = graph
            .packages
            .iter()
            .find(|package| package.name == "D")
            .unwrap();
        assert_eq!(package_b.dependencies, ["D"]);
        assert_eq!(package_c.dependencies, ["D"]);
        assert_eq!(package_d.module_root, "D");
        assert_eq!(package_d.source_dir, package_d.root_dir.join("app"));
    }

    #[test]
    fn missing_dependency_does_not_stop_other_edges() {
        let temp = TempDir::new("missing");
        let a = temp.path().join("A");
        let good = temp.path().join("Good");
        write_package(&a, "A", &[("Missing", "../Missing"), ("Good", "../Good")]);
        write_package(&good, "Good", &[]);

        let graph = resolve(&a).unwrap();

        assert_eq!(diagnostic_codes(&graph), ["KPK020"]);
        assert_eq!(graph.packages.len(), 2);
        assert!(graph.packages.iter().any(|package| package.name == "Good"));
        assert_eq!(graph.packages[0].dependencies, ["Missing", "Good"]);
    }

    #[test]
    fn conflicting_name_at_two_canonical_roots_is_diagnosed() {
        let temp = TempDir::new("identity");
        let a = temp.path().join("A");
        let b = temp.path().join("B");
        let c = temp.path().join("C");
        let core_one = temp.path().join("CoreOne");
        let core_two = temp.path().join("CoreTwo");
        write_package(&a, "A", &[("B", "../B"), ("C", "../C")]);
        write_package(&b, "B", &[("Core", "../CoreOne")]);
        write_package(&c, "C", &[("Core", "../CoreTwo")]);
        write_package(&core_one, "Core", &[]);
        write_package(&core_two, "Core", &[]);

        let graph = resolve(&a).unwrap();

        assert!(diagnostic_codes(&graph).contains(&"KPK023"));
        assert_eq!(
            graph
                .packages
                .iter()
                .filter(|package| package.name == "Core")
                .count(),
            2
        );
    }

    #[test]
    fn package_cycle_is_diagnosed_and_terminates() {
        let temp = TempDir::new("cycle");
        let a = temp.path().join("A");
        let b = temp.path().join("B");
        write_package(&a, "A", &[("B", "../B")]);
        write_package(&b, "B", &[("A", "../A")]);

        let graph = resolve(&a).unwrap();

        assert_eq!(graph.packages.len(), 2);
        assert_eq!(diagnostic_codes(&graph), ["KPK021"]);
    }

    #[test]
    fn drifted_lockfile_is_only_a_warning() {
        let temp = TempDir::new("lock-drift");
        let a = temp.path().join("A");
        let b = temp.path().join("B");
        write_package(&a, "A", &[("B", "../B")]);
        write_package(&b, "B", &[]);
        fs::write(
            a.join("kira.lock"),
            "schemaVersion = 1\npackages = []\n[root]\nname = \"A\"\ndependencies = []\n",
        )
        .unwrap();

        let graph = resolve(&a).unwrap();

        assert_eq!(diagnostic_codes(&graph), ["KPK024"]);
        assert_eq!(graph.diagnostics[0].severity, Severity::Warning);
    }

    #[test]
    fn matching_lockfile_adds_no_diagnostic() {
        let temp = TempDir::new("lock-match");
        let a = temp.path().join("A");
        let b = temp.path().join("B");
        write_package(&a, "A", &[("B", "../B")]);
        write_package(&b, "B", &[]);
        fs::write(
            a.join("kira.lock"),
            "schema_version = 1\n\
             [[packages]]\nname = \"B\"\nmodule_root = \"B\"\ndependencies = []\n\
             [root]\nname = \"A\"\ndependencies = [{ name = \"B\", source = { path = \"../B\" } }]\n",
        )
        .unwrap();

        let graph = resolve(&a).unwrap();

        assert!(graph.diagnostics.is_empty(), "{:?}", graph.diagnostics);
    }

    #[test]
    fn duplicate_dependency_names_are_warned_and_deduplicated() {
        let temp = TempDir::new("duplicate");
        let a = temp.path().join("A");
        let b = temp.path().join("B");
        write_package(&a, "A", &[("B", "../B"), ("B", "../B")]);
        write_package(&b, "B", &[]);

        let graph = resolve(&a).unwrap();

        assert_eq!(diagnostic_codes(&graph), ["KPK022"]);
        assert_eq!(graph.packages[0].dependencies, ["B"]);
        assert_eq!(graph.packages.len(), 2);
    }

    #[test]
    fn an_unreadable_root_is_a_hard_error() {
        let temp = TempDir::new("root-error");
        let missing = temp.path().join("missing");

        assert!(matches!(
            resolve(&missing),
            Err(ResolveError::RootDirectory { .. })
        ));
    }
}
