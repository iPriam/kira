//! Whole-program module graph construction: reading the files an entry program
//! imports.
//!
//! Layer 6 of the Kira package graph.
//!
//! Imports are file-scoped, but *loading* them is not a per-file question: a
//! module imported by a module is still part of the program, so this walks the
//! import graph transitively from the entry file and returns every module it
//! reached.
//!
//! # Why loading lives here and resolution does not
//!
//! `kira-semantics` decides which import binds which name, and reports the ones
//! that bind nothing. It cannot decide which *file* an import names, because it
//! has no filesystem — it compiles for `wasm32-unknown-unknown`. So this crate
//! does the one thing that needs a disk: turn `import support` into the text of
//! `support.kira`, and hand the texts to the frontend as an input.
//!
//! # Cycles
//!
//! A module already loaded is never loaded again, so two modules that import
//! each other terminate and appear in the program once each. That is not a
//! leniency to be tightened later: the reference implementation accepts a
//! cyclic import graph for exactly this reason, and rejecting one here would
//! turn a working program into a compile error. A cycle is the one shape with
//! no dependencies-first order to return; the walk still terminates and still
//! returns every module once, which is all a cyclic program can be given.
//!
//! # Bundled packages
//!
//! A module the program's own directory does not hold may still be found in a
//! package that ships with the toolchain — that is how `import Foundation`
//! resolves with no path and no dependency entry. See [`bundled`] for which
//! names a bundle owns and why the project's own files always win.

pub mod bundled;
pub mod package_roots;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bundled::BundledRoot;
use kira_semantics::ModuleSource;
use kira_source::SourceId;
use kira_syntax_model::ast::Item;
pub use package_roots::PackageRoot;

/// The maximum number of modules one program may be built from.
///
/// A bound rather than a promise of unboundedness: the module ids are handed to
/// [`kira_source::SourceMap`] and to salsa as small integers, and a program
/// that reached this many files has a generator loop, not a design. Hitting it
/// stops the walk instead of growing without limit.
const MAX_MODULES: usize = 1024;

/// The identity used to stop a module-loading cycle.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ModuleKey {
    /// Project and bundled modules retain the established name-based identity.
    Name(String),
    /// Dependency-package modules are distinct files, even when names repeat.
    PackagePath(PathBuf),
    /// An entryless package's import namespace, anchored to its source directory.
    PackageNamespace(String, PathBuf),
}

/// One source unit selected for the walk, including a package namespace marker.
struct ReadModule {
    module: String,
    path: PathBuf,
    text: String,
    key: ModuleKey,
    package: Option<PackageRoot>,
}

/// One unit of the iterative depth-first post-order walk.
enum Step {
    /// Resolve an import through the available roots.
    Resolve {
        module: String,
        package: Option<PackageRoot>,
    },
    /// Visit one already-resolved source file.
    Visit(Box<ReadModule>),
    /// Record this module after everything it imports.
    Emit(Box<ModuleSource>),
}

/// Reads every module the program at `entry_path` imports, transitively.
///
/// Modules come back **dependencies first**: the walk is depth-first
/// *post-order*, so a module is recorded only after every module it imports has
/// been, and a declaration may name a type from a module it imports regardless
/// of the order the entry file happens to list its own imports in. Where the
/// graph has a topological order, this is one.
///
/// The order is the one the frontend assigns source ids in — see
/// [`kira_semantics::module_source_id`] — so a caller mirroring it into a
/// [`kira_source::SourceMap`] must insert the entry file first and then these,
/// in order.
///
/// An import naming a file that cannot be read is skipped; the frontend reports
/// it as an unresolved import, where the import's span is.
#[must_use]
pub fn load_modules(entry_path: &Path, entry_text: &str) -> Vec<ModuleSource> {
    load_modules_with_packages(entry_path, entry_text, &bundled::bundled_roots(), &[])
}

/// [`load_modules`], against an explicit set of bundled packages.
///
/// Split out so a test can hand the walk a bundle it built itself rather than
/// whichever toolchain the machine happens to have installed. Dependency
/// packages are omitted; callers that resolved them use
/// [`load_modules_with_packages`].
#[must_use]
pub fn load_modules_with(
    entry_path: &Path,
    entry_text: &str,
    bundles: &[BundledRoot],
) -> Vec<ModuleSource> {
    load_modules_with_packages(entry_path, entry_text, bundles, &[])
}

/// Reads every transitively imported module with explicit bundled and resolved
/// dependency-package roots.
///
/// Imports written by the project prefer project files, while imports written
/// inside a dependency prefer that dependency's own files. Resolved packages
/// are consulted next and bundled roots remain the final fallback. A bare
/// package-name import always aggregates every `.kira` file below that package's
/// source directory — the package's files are one flat module scope, and a
/// `<name>.kira` root file, if present, is just one of them.
#[must_use]
pub fn load_modules_with_packages(
    entry_path: &Path,
    entry_text: &str,
    bundles: &[BundledRoot],
    packages: &[PackageRoot],
) -> Vec<ModuleSource> {
    // The module root is the entry file's directory. A module path is a
    // sequence of identifiers, so it can name nothing above the root.
    let root = entry_path.parent().unwrap_or_else(|| Path::new("."));
    let mut loaded: Vec<ModuleSource> = Vec::new();
    let mut seen: HashSet<ModuleKey> = HashSet::new();
    let mut stack: Vec<Step> = Vec::new();
    push_resolves(&mut stack, imports_of(entry_text), None);

    while let Some(step) = stack.pop() {
        match step {
            Step::Emit(source) => loaded.push(*source),
            Step::Resolve { module, package } => {
                let sources = read_module(root, &module, package.as_ref(), bundles, packages);
                push_modules(&mut stack, sources);
            }
            Step::Visit(source) => {
                let ReadModule {
                    module,
                    path,
                    text,
                    key,
                    package,
                } = *source;
                if !seen.insert(key) {
                    continue;
                }
                if seen.len() > MAX_MODULES {
                    break;
                }
                let nested = imports_of(&text);
                stack.push(Step::Emit(Box::new(ModuleSource {
                    module,
                    path: path.to_string_lossy().into_owned(),
                    text,
                })));
                push_resolves(&mut stack, nested, package);
            }
        }
    }

    loaded
}

/// Schedules imports to resolve in source order, first one first.
fn push_resolves(stack: &mut Vec<Step>, modules: Vec<String>, package: Option<PackageRoot>) {
    stack.extend(modules.into_iter().rev().map(|module| Step::Resolve {
        module,
        package: package.clone(),
    }));
}

/// Schedules resolved source files in deterministic order, first one first.
fn push_modules(stack: &mut Vec<Step>, modules: Vec<ReadModule>) {
    stack.extend(
        modules
            .into_iter()
            .rev()
            .map(|module| Step::Visit(Box::new(module))),
    );
}

/// The dotted module paths a source file imports, in source order.
///
/// Parsing the file to read its imports is deliberate: the import grammar is
/// the parser's, and a second scanner that "just looked for `import`" would
/// disagree with it on the first file that wrote the word inside a string.
/// Diagnostics are discarded — this pass answers a filesystem question, and the
/// frontend parses the same text again and reports everything it finds.
fn imports_of(text: &str) -> Vec<String> {
    let parsed = kira_parser::parse(SourceId::new(0), text);
    parsed
        .tree
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Import(declaration) => {
                let segments: Vec<&str> = declaration
                    .path
                    .iter()
                    .map(|&segment| parsed.interner.resolve(segment))
                    .collect();
                match segments.is_empty() {
                    true => None,
                    false => Some(segments.join(".")),
                }
            }
            _ => None,
        })
        .collect()
}

/// Reads every source file selected by `module` in resolution-tier order.
fn read_module(
    root: &Path,
    module: &str,
    package: Option<&PackageRoot>,
    bundles: &[BundledRoot],
    packages: &[PackageRoot],
) -> Vec<ReadModule> {
    // A dependency source resolves inside its own package before it may see a
    // same-named file in the consumer project.
    if let Some(package) = package {
        let relative = package.relative_module(module).unwrap_or(module);
        let sibling = module_path(&package.source_dir, relative);
        if let Some(source) = read_package_module(relative.to_owned(), sibling, package) {
            return vec![source];
        }
    }

    let own = module_path(root, module);
    if let Some(source) = read_named_module(module, own) {
        return vec![source];
    }

    for package in packages {
        let Some(relative) = package.relative_module(module) else {
            continue;
        };
        if module == package.name {
            // A bare package-name import pulls in the package's entire top-level
            // surface: every `.kira` file below its source directory forms one
            // flat module scope, so the walk aggregates all of them. A
            // `<name>.kira` root file, if present, is just one of the aggregated
            // files — it is never loaded alone, which is what let a sibling
            // holding `ComponentStore` go unread.
            return read_aggregate_modules(package);
        }
        let path = module_path(&package.source_dir, relative);
        if let Some(source) = read_package_module(relative.to_owned(), path, package) {
            return vec![source];
        }
    }

    for bundle in bundles {
        if !bundle.owns(module) {
            continue;
        }
        if module == bundle.module_root() {
            // A bundled package is a package: importing it by its bare name
            // pulls in its whole top-level surface, exactly as importing a
            // dependency by name does. Foundation grew a second file the moment
            // it grew a filesystem, and reading only `Foundation.kira` would
            // have made that file unreachable by any spelling.
            let package = PackageRoot::new(bundle.module_root(), bundle.source_dir());
            let aggregate = read_aggregate_modules(&package);
            if !aggregate.is_empty() {
                return aggregate;
            }
        }
        let path = module_path(bundle.source_dir(), module);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return vec![ReadModule {
                module: module.to_owned(),
                path,
                text,
                key: ModuleKey::Name(module.to_owned()),
                package: None,
            }];
        }
    }
    Vec::new()
}

/// Reads a project-owned module, which retains name-based cycle identity.
fn read_named_module(module: &str, path: PathBuf) -> Option<ReadModule> {
    let text = std::fs::read_to_string(&path).ok()?;
    Some(ReadModule {
        module: module.to_owned(),
        path,
        text,
        key: ModuleKey::Name(module.to_owned()),
        package: None,
    })
}

/// Reads a dependency-package module with absolute-path cycle identity.
fn read_package_module(module: String, path: PathBuf, package: &PackageRoot) -> Option<ReadModule> {
    let text = std::fs::read_to_string(&path).ok()?;
    let absolute = std::fs::canonicalize(&path)
        .or_else(|_| std::path::absolute(&path))
        .ok()?;
    Some(ReadModule {
        module: kira_semantics::ImportTable::package_module_identity(&package.name, &module),
        path,
        text,
        key: ModuleKey::PackagePath(absolute),
        package: Some(package.clone()),
    })
}

/// Reads every source file of a package and, when it has no `<name>.kira` root
/// file, adds its import namespace.
///
/// Every `.kira` file below the package's source directory is one flat module
/// scope, so all of them are read. The bare `import <name>` needs one module to
/// bind to: a `<name>.kira` root file already provides it — its identity is the
/// package-name module — so the empty namespace alias is emitted only when the
/// package has no such file, and never duplicates the root's identity.
fn read_aggregate_modules(package: &PackageRoot) -> Vec<ReadModule> {
    let mut modules: Vec<ReadModule> = Vec::new();
    let mut has_root = false;
    for path in package.source_files() {
        let Some(module) = package_module_name(&package.source_dir, &path) else {
            continue;
        };
        let is_root = module == package.name;
        let Some(read) = read_package_module(module, path, package) else {
            continue;
        };
        if is_root {
            has_root = true;
        }
        modules.push(read);
    }
    if modules.is_empty() || has_root {
        return modules;
    }
    let Some(absolute) = std::fs::canonicalize(&package.source_dir)
        .or_else(|_| std::path::absolute(&package.source_dir))
        .ok()
    else {
        return modules;
    };
    // The namespace has no root file. Emit its empty semantic alias after the
    // real files so package declarations retain dependency-first order.
    modules.push(ReadModule {
        module: kira_semantics::ImportTable::package_module_identity(&package.name, &package.name),
        path: package.source_dir.clone(),
        text: String::new(),
        key: ModuleKey::PackageNamespace(package.name.clone(), absolute),
        package: Some(package.clone()),
    });
    modules
}

/// Converts a package-relative source path back to its dotted module name.
fn package_module_name(source_dir: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(source_dir).ok()?;
    let mut module_path = relative.to_path_buf();
    module_path.set_extension("");
    let segments: Option<Vec<&str>> = module_path.iter().map(|segment| segment.to_str()).collect();
    let module = segments?.join(".");
    (!module.is_empty()).then_some(module)
}

/// Where a dotted module path lives on disk, relative to a search root.
///
/// `support` is `support.kira`; `Foundation.Web` is `Foundation/Web.kira`. A
/// dot is a directory separator, which is what makes the module path a *name*
/// rather than a path — the source never spells a slash, an extension, or a
/// parent directory. The same mapping is used inside a bundled package, with
/// that package's `app/` as the root; nothing about a bundle's layout is
/// special-cased.
fn module_path(root: &Path, module: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in module.split('.') {
        path.push(segment);
    }
    path.set_extension("kira");
    path
}

#[cfg(test)]
mod tests;
