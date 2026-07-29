//! Turning a package set into the entry file and modules the frontend analyzes.
//!
//! This is the in-memory counterpart of what `kira-program-graph` does on disk,
//! and it follows the same rules on purpose: a package's files are one flat
//! module scope, a dependency's modules are namespaced by their package so two
//! packages may declare the same name, and an `import` that names a bundled
//! package pulls in that package's whole surface.
//!
//! What differs is only where the bytes come from. A `path` here is not looked
//! up anywhere: it is what a diagnostic points at, and — with a leading `app/`
//! stripped, exactly as a package's source directory is on disk — the module
//! name an `import` inside the package resolves against.

use std::collections::{HashMap, HashSet};

use kira_diagnostic_messages::package_messages::{
    missing_source_file, unknown_root_package, unreadable_manifest,
};
use kira_diagnostics::Diagnostic;
use kira_manifest::{PackageKind, ProjectManifest};
use kira_program_graph::bundled::BundledRoot;
use kira_program_graph::{imports_of, load_bundled_modules};
use kira_runtime_abi::CheckRequest;
use kira_semantics::{BuildKind, ImportTable, ModuleSource};

/// The entry file of a resolved request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryFile {
    /// The path the entry file is known by.
    pub path: String,
    /// The entry file's source text.
    pub text: String,
}

/// A request turned into what the frontend needs, plus what reading it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRequest {
    /// The root package's first file, or `None` when the request named none.
    pub entry: Option<EntryFile>,
    /// Every other file taking part, dependencies before dependents.
    pub modules: Vec<ModuleSource>,
    /// What the root package's manifest said this build produces.
    pub build_kind: BuildKind,
    /// Problems found while reading the request itself.
    pub diagnostics: Vec<Diagnostic>,
}

/// Resolves a request against this toolchain's bundled packages.
///
/// Reads the bundled modules fresh; [`crate::CheckSession`] is what caches them
/// across calls.
#[must_use]
pub fn resolve(request: &CheckRequest, bundles: &[BundledRoot]) -> ResolvedRequest {
    let mut cache = HashMap::new();
    resolve_with(request, bundles, &mut cache)
}

/// Resolves a request, reading each bundled import set at most once.
pub(crate) fn resolve_with(
    request: &CheckRequest,
    bundles: &[BundledRoot],
    cache: &mut HashMap<Vec<String>, Vec<ModuleSource>>,
) -> ResolvedRequest {
    let mut diagnostics = Vec::new();
    let packages = read_manifests(request, &mut diagnostics);

    let Some(root) = packages
        .iter()
        .position(|package| package.manifest.name == request.root)
    else {
        diagnostics.push(unknown_root_package(&request.root));
        return ResolvedRequest {
            entry: None,
            modules: Vec::new(),
            build_kind: BuildKind::Application,
            diagnostics,
        };
    };

    let build_kind = match packages[root].manifest.kind {
        PackageKind::Library => BuildKind::Library,
        PackageKind::App => BuildKind::Application,
    };

    let Some((first, rest)) = packages[root].files.split_first() else {
        diagnostics.push(missing_source_file(&request.root));
        return ResolvedRequest {
            entry: None,
            modules: Vec::new(),
            build_kind,
            diagnostics,
        };
    };

    let mut walk = Walk::new(&packages, root);
    walk.seed(&packages[root]);
    let dependency_modules = walk.run();
    let bundled = bundled_modules(&walk.bundled_imports(), bundles, cache);

    // Dependencies first: bundled packages, then the request's own dependency
    // packages in the order the walk reached them, then the root package's own
    // files. A declaration may name a type from a module ahead of it, and this
    // is the order that makes that true for every tier.
    let mut modules = bundled;
    modules.extend(dependency_modules);
    modules.extend(rest.iter().map(|file| ModuleSource {
        module: module_name(&file.path),
        path: file.path.clone(),
        text: file.text.clone(),
    }));

    ResolvedRequest {
        entry: Some(EntryFile {
            path: first.path.clone(),
            text: first.text.clone(),
        }),
        modules,
        build_kind,
        diagnostics,
    }
}

/// One package of the request whose manifest could be read.
struct ReadPackage<'a> {
    /// What the manifest declared.
    manifest: ProjectManifest,
    /// The files the request listed for it.
    files: &'a [kira_runtime_abi::CheckFile],
}

/// Reads every manifest, reporting the ones that could not be read.
///
/// A package whose manifest is unreadable is dropped rather than guessed at: it
/// has no name, so nothing could import it and nothing could name it as root.
fn read_manifests<'a>(
    request: &'a CheckRequest,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<ReadPackage<'a>> {
    let mut packages = Vec::with_capacity(request.packages.len());
    for (position, package) in request.packages.iter().enumerate() {
        match kira_manifest::load_declaration(&package.manifest) {
            Ok(manifest) => packages.push(ReadPackage {
                manifest,
                files: &package.files,
            }),
            Err(error) => diagnostics.push(unreadable_manifest(position, &error.to_string())),
        }
    }
    packages
}

/// The transitive walk over the request's own packages.
///
/// Mirrors the disk walk: an import that names another package in the set pulls
/// in that package's whole surface, and every import it does not satisfy is a
/// candidate for a bundled package.
struct Walk<'a, 'p> {
    /// Every readable package of the request.
    packages: &'p [ReadPackage<'a>],
    /// The root package, which is already loaded and is never a dependency.
    root: usize,
    /// Import names still to resolve.
    pending: Vec<String>,
    /// Packages already pulled in.
    visited: HashSet<usize>,
    /// Import names no package in the request owns.
    unresolved: Vec<String>,
    /// Import names already resolved, so a cycle terminates.
    seen: HashSet<String>,
}

impl<'a, 'p> Walk<'a, 'p> {
    fn new(packages: &'p [ReadPackage<'a>], root: usize) -> Self {
        Self {
            packages,
            root,
            pending: Vec::new(),
            visited: HashSet::from([root]),
            unresolved: Vec::new(),
            seen: HashSet::new(),
        }
    }

    /// Queues every import written in one package's files.
    fn seed(&mut self, package: &ReadPackage<'a>) {
        for file in package.files {
            self.pending.extend(imports_of(&file.text));
        }
    }

    /// Pulls in every package the queued imports reach, in reach order.
    fn run(&mut self) -> Vec<ModuleSource> {
        let mut modules = Vec::new();
        while let Some(module) = self.pending.pop() {
            if !self.seen.insert(module.clone()) {
                continue;
            }
            let Some(index) = self.owner_of(&module) else {
                self.unresolved.push(module);
                continue;
            };
            if !self.visited.insert(index) {
                continue;
            }
            let package = &self.packages[index];
            modules.extend(package_modules(&package.manifest.name, package.files));
            for file in package.files {
                self.pending.extend(imports_of(&file.text));
            }
        }
        modules
    }

    /// Which package of the request owns this import's first segment.
    fn owner_of(&self, module: &str) -> Option<usize> {
        let first = module.split('.').next()?;
        self.packages
            .iter()
            .position(|package| package.manifest.name == first)
            .filter(|&index| index != self.root)
    }

    /// The import names to try against the bundled packages, sorted and unique.
    ///
    /// Sorted because the set is a cache key: two requests that import the same
    /// packages in a different order must reuse one reading of them.
    fn bundled_imports(&self) -> Vec<String> {
        let mut names = self.unresolved.clone();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// Every module of one dependency package, namespaced by its package.
///
/// The namespace is what keeps two packages that declare the same name apart,
/// and what makes an `import` of the package — rather than of the file — the
/// thing that puts its declarations in scope. A package with no file named
/// after it gets an empty module under its own name, so a bare `import <name>`
/// still binds something; that mirrors the disk loader exactly.
fn package_modules(name: &str, files: &[kira_runtime_abi::CheckFile]) -> Vec<ModuleSource> {
    let mut modules: Vec<ModuleSource> = files
        .iter()
        .map(|file| ModuleSource {
            module: ImportTable::package_module_identity(name, &module_name(&file.path)),
            path: file.path.clone(),
            text: file.text.clone(),
        })
        .collect();
    let root_identity = ImportTable::package_module_identity(name, name);
    if !modules.iter().any(|module| module.module == root_identity) {
        modules.push(ModuleSource {
            module: root_identity,
            path: name.to_owned(),
            text: String::new(),
        });
    }
    modules
}

/// Reads the modules a set of bundled import names pulls in, through the cache.
fn bundled_modules(
    imports: &[String],
    bundles: &[BundledRoot],
    cache: &mut HashMap<Vec<String>, Vec<ModuleSource>>,
) -> Vec<ModuleSource> {
    if imports.is_empty() {
        return Vec::new();
    }
    cache
        .entry(imports.to_vec())
        .or_insert_with(|| load_bundled_modules(imports, bundles))
        .clone()
}

/// The module name a package-relative path is imported by.
///
/// `app/` is a package's source directory on disk, so it is stripped here for
/// the same reason it is there; the rest is the path with its extension dropped
/// and its separators spelled as an import writes them.
fn module_name(path: &str) -> String {
    let trimmed = path.strip_prefix("app/").unwrap_or(path);
    let stem = trimmed.strip_suffix(".kira").unwrap_or(trimmed);
    stem.replace('/', ".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::CheckPackage;

    #[test]
    fn a_path_becomes_the_module_name_an_import_writes() {
        assert_eq!(module_name("app/main.kira"), "main");
        assert_eq!(module_name("app/Nested/Thing.kira"), "Nested.Thing");
        assert_eq!(module_name("Loose.kira"), "Loose");
        assert_eq!(module_name("app/main"), "main");
    }

    #[test]
    fn a_package_without_a_file_named_after_it_still_binds_its_bare_import() {
        let files = vec![kira_runtime_abi::CheckFile {
            path: "app/Values.kira".to_owned(),
            text: "function value() -> Int { return 1 }".to_owned(),
        }];
        let modules = package_modules("Core", &files);
        assert_eq!(modules.len(), 2);
        assert_eq!(modules[0].module, "Core::Values");
        assert_eq!(modules[1].module, "Core::Core");
        assert!(modules[1].text.is_empty());
    }

    #[test]
    fn a_package_with_a_root_file_gains_no_empty_namespace() {
        let files = vec![kira_runtime_abi::CheckFile {
            path: "app/Core.kira".to_owned(),
            text: "function value() -> Int { return 1 }".to_owned(),
        }];
        let modules = package_modules("Core", &files);
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].module, "Core::Core");
    }

    #[test]
    fn an_unreadable_manifest_is_reported_and_its_package_dropped() {
        let request = CheckRequest {
            root: "App".to_owned(),
            packages: vec![CheckPackage {
                manifest: "not a manifest".to_owned(),
                files: Vec::new(),
            }],
        };
        let resolved = resolve(&request, &[]);
        let codes: Vec<Option<&str>> = resolved
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect();
        assert_eq!(codes, vec![Some("KPK030"), Some("KPK031")]);
        assert_eq!(resolved.entry, None);
    }

    #[test]
    fn a_root_that_names_no_package_is_reported() {
        let request = CheckRequest {
            root: "Missing".to_owned(),
            packages: vec![CheckPackage {
                manifest: "Package App {\n    let kind = .App\n}\n".to_owned(),
                files: Vec::new(),
            }],
        };
        let resolved = resolve(&request, &[]);
        assert_eq!(
            resolved
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![Some("KPK031")]
        );
    }

    #[test]
    fn a_root_package_with_no_files_is_reported() {
        let request = CheckRequest {
            root: "App".to_owned(),
            packages: vec![CheckPackage {
                manifest: "Package App {\n    let kind = .App\n}\n".to_owned(),
                files: Vec::new(),
            }],
        };
        let resolved = resolve(&request, &[]);
        assert_eq!(
            resolved
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![Some("KPK007")]
        );
    }

    /// A package nothing imports contributes nothing, exactly as an unreferenced
    /// dependency on disk does.
    #[test]
    fn an_unimported_package_is_not_loaded() {
        let request = CheckRequest {
            root: "App".to_owned(),
            packages: vec![
                CheckPackage {
                    manifest: "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n"
                        .to_owned(),
                    files: vec![kira_runtime_abi::CheckFile {
                        path: "app/Core.kira".to_owned(),
                        text: "function core() -> Int { return 1 }".to_owned(),
                    }],
                },
                CheckPackage {
                    manifest: "Package App {\n    let kind = .App\n}\n".to_owned(),
                    files: vec![kira_runtime_abi::CheckFile {
                        path: "app/main.kira".to_owned(),
                        text: "@Main function main() { return }".to_owned(),
                    }],
                },
            ],
        };
        let resolved = resolve(&request, &[]);
        assert!(
            resolved.diagnostics.is_empty(),
            "{:?}",
            resolved.diagnostics
        );
        assert!(resolved.modules.is_empty(), "{:?}", resolved.modules);
    }

    #[test]
    fn an_imported_package_contributes_its_whole_surface() {
        let request = CheckRequest {
            root: "App".to_owned(),
            packages: vec![
                CheckPackage {
                    manifest: "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n"
                        .to_owned(),
                    files: vec![
                        kira_runtime_abi::CheckFile {
                            path: "app/Core.kira".to_owned(),
                            text: "function core() -> Int { return 1 }".to_owned(),
                        },
                        kira_runtime_abi::CheckFile {
                            path: "app/Extra.kira".to_owned(),
                            text: "function extra() -> Int { return 2 }".to_owned(),
                        },
                    ],
                },
                CheckPackage {
                    manifest: "Package App {\n    let kind = .App\n}\n".to_owned(),
                    files: vec![kira_runtime_abi::CheckFile {
                        path: "app/main.kira".to_owned(),
                        text: "import Core\n@Main function main() { return }".to_owned(),
                    }],
                },
            ],
        };
        let resolved = resolve(&request, &[]);
        let modules: Vec<&str> = resolved
            .modules
            .iter()
            .map(|module| module.module.as_str())
            .collect();
        assert_eq!(modules, vec!["Core::Core", "Core::Extra"]);
    }
}
