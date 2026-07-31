//! Program assembly: the whole source set an entry file compiles as.
//!
//! Loading the modules an entry file imports is only part of the answer. When
//! that file belongs to a package, the program is also every dependency the
//! package's `package.kira` resolves to and every sibling `.kira` file below
//! the package's source root — an `app/main.kira` that never imports
//! `app/area/Thing.kira` is still compiled with it.
//!
//! # Why this is not the build's private business
//!
//! Two callers assemble the same program: `kira build` and the language
//! server. When only the build knew about manifests, an editor opening
//! `app/main.kira` analyzed it as a lone file — every dependency import
//! reported as an unresolved library, every sibling declaration reported as
//! undefined — while `kira check` on the same tree was clean. A squiggle that
//! disagrees with the compiler is worse than no squiggle, so the discovery,
//! resolution, and aggregation all live here, above the module walk and below
//! both callers.
//!
//! # What it does not do
//!
//! It never writes. Resolution reports a drifted `kira.lock` as a diagnostic
//! and hands the graph back; whether to rewrite the file is the caller's call,
//! and a language server answering a keystroke must not touch the tree.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kira_diagnostics::Diagnostic;
use kira_package_manager::{ResolveError, ResolvedPackageGraph};
use kira_project::{DiscoveryError, Manifest};
use kira_semantics::{BuildKind, ModuleSource};

use crate::PackageRoot;
use crate::bundled::BundledRoot;

/// Why a program's source set could not be assembled at all.
///
/// Distinct from a program that assembled and has errors: an import naming no
/// readable file is not here, because the frontend reports it against the
/// import's own span. These are failures to reach the frontend.
#[derive(Debug, thiserror::Error)]
pub enum AssemblyError {
    /// A package-owned source file could not be read.
    #[error("cannot read `{path}`: {source}")]
    Read {
        /// The path that could not be read.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A `package.kira` was found above the entry and could not be used.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// The governing package's dependency graph could not be resolved.
    #[error(transparent)]
    Resolution(#[from] ResolveError),
}

/// Every source file the program entered at one path is built from.
#[derive(Debug)]
pub struct ProgramSources {
    /// Every module of the program, dependencies first, entry file excluded.
    ///
    /// The order [`kira_semantics::module_source_id`] assigns ids in, so a
    /// caller mirroring these into a [`kira_source::SourceMap`] inserts the
    /// entry file first and then these, in order.
    pub modules: Vec<ModuleSource>,
    /// The manifest governing the entry file, if a `package.kira` sits above it.
    pub package: Option<Manifest>,
    /// What the governing package says this program produces.
    ///
    /// [`BuildKind::Application`] for a bare `.kira` file: a file handed to the
    /// compiler with no manifest above it is a program in its own right.
    pub build_kind: BuildKind,
    /// Non-fatal problems found while resolving the package graph.
    pub diagnostics: Vec<Diagnostic>,
    /// The resolved dependency graph, when a package governed the entry.
    ///
    /// Carried so a caller that wants a drifted `kira.lock` rewritten has the
    /// graph to write; assembly itself never does.
    pub graph: Option<ResolvedPackageGraph>,
}

/// Assembles the whole program the file at `entry_path` compiles as.
///
/// `entry_text` is supplied rather than read, because the two callers disagree
/// about where the entry's bytes come from: the build reads the file, and a
/// language server holds an unsaved buffer that is the truth for that document.
/// Every *other* file of the program is read from disk by both.
pub fn load_program(entry_path: &Path, entry_text: &str) -> Result<ProgramSources, AssemblyError> {
    load_program_with(entry_path, entry_text, &crate::bundled::bundled_roots())
}

/// [`load_program`], against an explicit set of bundled packages.
///
/// Split out for the same reason [`crate::load_modules_with`] is: a test hands
/// the walk a bundle it built itself rather than whichever toolchain the
/// machine happens to have installed.
pub fn load_program_with(
    entry_path: &Path,
    entry_text: &str,
    bundles: &[BundledRoot],
) -> Result<ProgramSources, AssemblyError> {
    let package = kira_project::manifest_for(entry_path)?;
    let resolved = resolve_packages(package.as_ref())?;
    let package_roots = resolved.roots;

    // Everything the entry file imports, transitively, dependencies first. An
    // import that names no readable file comes back as nothing here and is
    // reported by the frontend, which has the span to point at. Resolved
    // package roots sit between project-local modules and toolchain bundles.
    let mut modules =
        crate::load_modules_with_packages(entry_path, entry_text, bundles, &package_roots);

    // Every `.kira` file under a package's source root is a member of that
    // package — app or library. What a package *produces* does not decide which
    // of its own files belong to it, and an app used to compile its entry file
    // plus that file's imports and nothing else: a program split across
    // `app/main.kira` and `app/area/Thing.kira` reported every function in the
    // sibling as undefined, while the identical library layout compiled.
    //
    // Aggregating adds files to the package, not to each other's scope: imports
    // stay file-scoped, so a sibling's `import Foundation` still does not put
    // `Foundation` in this file's namespace.
    if let Some(found) = package.as_ref()
        && let Some(library_sources) = kira_project::library_sources_for_entry(found, entry_path)?
    {
        aggregate_package_modules(
            entry_path,
            &library_sources,
            bundles,
            &package_roots,
            &mut modules,
        )?;
    }

    let build_kind = match package.as_ref().map(Manifest::kind) {
        Some(kira_manifest::PackageKind::Library) => BuildKind::Library,
        Some(kira_manifest::PackageKind::App) | None => BuildKind::Application,
    };

    Ok(ProgramSources {
        modules,
        package,
        build_kind,
        diagnostics: resolved.diagnostics,
        graph: resolved.graph,
    })
}

/// What resolving the governing package's dependencies produced.
#[derive(Debug, Default)]
struct ResolvedDependencies {
    /// The graph itself, when a package governed the entry.
    graph: Option<ResolvedPackageGraph>,
    /// The source root each resolved package contributes to import resolution.
    roots: Vec<PackageRoot>,
    /// Non-fatal problems resolution reported.
    diagnostics: Vec<Diagnostic>,
}

/// Resolves the governing package's dependency graph into module roots.
///
/// A file with no manifest above it has no dependencies to resolve, and gets
/// the empty answer rather than an error: a bare `.kira` file is a program.
fn resolve_packages(package: Option<&Manifest>) -> Result<ResolvedDependencies, AssemblyError> {
    let Some(package) = package else {
        return Ok(ResolvedDependencies::default());
    };
    let graph = kira_package_manager::resolve(&package_root_dir(package))?;
    let roots = graph
        .packages
        .iter()
        .map(|resolved| PackageRoot::new(resolved.name.clone(), resolved.source_dir.clone()))
        .collect();
    let diagnostics = graph.diagnostics.clone();
    Ok(ResolvedDependencies {
        graph: Some(graph),
        roots,
        diagnostics,
    })
}

/// The directory a manifest governs, which is the directory it sits in.
fn package_root_dir(package: &Manifest) -> PathBuf {
    match Path::new(&package.path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Adds every unreferenced package source and each source's import closure.
fn aggregate_package_modules(
    entry_path: &Path,
    library_sources: &kira_project::LibrarySources,
    bundles: &[BundledRoot],
    package_roots: &[PackageRoot],
    modules: &mut Vec<ModuleSource>,
) -> Result<(), AssemblyError> {
    let entry_identity = source_identity(entry_path);
    let mut seen = modules
        .iter()
        .map(|module| source_identity(Path::new(&module.path)))
        .collect::<HashSet<_>>();
    seen.insert(entry_identity.clone());

    for source in library_sources.iter() {
        let identity = source_identity(source.path());
        if identity == entry_identity || seen.contains(&identity) {
            continue;
        }

        let display = source.path().display().to_string();
        let text =
            std::fs::read_to_string(source.path()).map_err(|read_error| AssemblyError::Read {
                path: display.clone(),
                source: read_error,
            })?;
        let imported =
            crate::load_modules_with_packages(source.path(), &text, bundles, package_roots);
        for module in imported {
            if seen.insert(source_identity(Path::new(&module.path))) {
                modules.push(module);
            }
        }
        if seen.insert(identity) {
            modules.push(ModuleSource {
                module: source.module().to_owned(),
                path: display,
                text,
            });
        }
    }
    Ok(())
}

/// Produces a stable filesystem identity without changing diagnostic path spelling.
fn source_identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}
