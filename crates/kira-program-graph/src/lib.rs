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

pub mod assembly;
pub mod bundled;
pub mod package_roots;
mod sources;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use bundled::BundledRoot;
use kira_semantics::ModuleSource;
use kira_source::SourceId;
use kira_syntax_model::ast::Item;

pub use assembly::{AssemblyError, ProgramSources, load_program, load_program_with};
pub use package_roots::PackageRoot;

pub(crate) use sources::Sources;

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
///
/// Selected, not read: resolution decides *which* file answers an import from
/// the shape of the tree, and the bytes are wanted only for a module the walk
/// has not already seen. Reading here instead is what made `import <package>`
/// read the whole package once per file that imported it.
struct Selected {
    module: String,
    path: PathBuf,
    key: ModuleKey,
    package: Option<PackageRoot>,
    /// The text, for the one selection that has no file: the empty namespace
    /// marker a package with no root file contributes.
    given: Option<String>,
}

/// One unit of the iterative depth-first post-order walk.
enum Step {
    /// Resolve an import through the available roots.
    Resolve {
        module: String,
        package: Option<PackageRoot>,
    },
    /// Visit one already-resolved source file.
    Visit(Box<Selected>),
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
    ModuleWalk::new(bundles, packages).modules_for(entry_path, entry_text)
}

/// A module walk that keeps what it learned about the tree between entries.
///
/// One assembly walks from many entry files — the program's, and then every
/// other source its package owns — and each walk asks the same questions of the
/// same directories. Driving them through one of these answers each question
/// once. Every walk still gets its own visited set and its own module list, so
/// what [`ModuleWalk::modules_for`] returns for a file is exactly what a walk
/// begun from nothing would return for it.
pub struct ModuleWalk<'a> {
    bundles: &'a [BundledRoot],
    packages: &'a [PackageRoot],
    sources: Sources,
}

impl<'a> ModuleWalk<'a> {
    /// A walk that resolves against `bundles` and `packages`.
    #[must_use]
    pub fn new(bundles: &'a [BundledRoot], packages: &'a [PackageRoot]) -> Self {
        Self {
            bundles,
            packages,
            sources: Sources::default(),
        }
    }

    /// Every module the file at `entry_path` imports, transitively.
    ///
    /// `entry_text` is supplied rather than read for the same reason
    /// [`assembly::load_program`] takes it: an editor's unsaved buffer is the
    /// truth for the document it holds.
    #[must_use]
    pub fn modules_for(&mut self, entry_path: &Path, entry_text: &str) -> Vec<ModuleSource> {
        // The module root is the entry file's directory. A module path is a
        // sequence of identifiers, so it can name nothing above the root.
        let root = entry_path.parent().unwrap_or_else(|| Path::new("."));
        walk(
            Some(root),
            imports_of(entry_text),
            self.bundles,
            self.packages,
            &mut self.sources,
        )
    }

    /// The text of a source file, read once however often it is asked for.
    ///
    /// What lets package aggregation hand a file's bytes to the frontend
    /// without reading a file the walk already read.
    pub fn read(&mut self, path: &Path) -> std::io::Result<Rc<str>> {
        self.sources.read(path)
    }
}

/// Reads every module `imports` names, and their closure, from bundles alone.
///
/// The loader for a program that has no directory: a package set held in
/// memory still writes `import Foundation`, and Foundation is on disk. There is
/// no project root to consult, so no file beside whatever directory the process
/// happens to be in can be reached — which is what makes an in-memory check
/// hermetic rather than dependent on the caller's working directory.
#[must_use]
pub fn load_bundled_modules(imports: &[String], bundles: &[BundledRoot]) -> Vec<ModuleSource> {
    walk(
        None,
        imports.to_vec(),
        bundles,
        &[],
        &mut Sources::default(),
    )
}

/// The transitive module walk, from a seed of import names.
///
/// `root` is the project directory whose files an import prefers, and `None`
/// says there is no such directory — the two loaders above differ in that and
/// nothing else, so they cannot disagree about resolution order.
fn walk(
    root: Option<&Path>,
    seed: Vec<String>,
    bundles: &[BundledRoot],
    packages: &[PackageRoot],
    sources: &mut Sources,
) -> Vec<ModuleSource> {
    let mut loaded: Vec<ModuleSource> = Vec::new();
    let mut seen: HashSet<ModuleKey> = HashSet::new();
    let mut stack: Vec<Step> = Vec::new();
    push_resolves(&mut stack, seed, None);

    while let Some(step) = stack.pop() {
        match step {
            Step::Emit(source) => loaded.push(*source),
            Step::Resolve { module, package } => {
                let selected =
                    select_module(root, &module, package.as_ref(), bundles, packages, sources);
                push_modules(&mut stack, selected);
            }
            Step::Visit(source) => {
                let Selected {
                    module,
                    path,
                    key,
                    package,
                    given,
                } = *source;
                if !seen.insert(key) {
                    continue;
                }
                if seen.len() > MAX_MODULES {
                    break;
                }
                // The bytes are wanted here and nowhere earlier: a module
                // already seen costs a set lookup rather than a file read.
                let (text, nested) = match given {
                    Some(text) => {
                        let nested = imports_of(&text);
                        (text, nested)
                    }
                    None => {
                        // A file that resolution selected and that cannot be
                        // read now answers nothing, and the frontend reports
                        // the import against its own span.
                        let Some(text) = sources.text(&path) else {
                            continue;
                        };
                        let nested = sources.imports(&path, &text).to_vec();
                        (text.to_string(), nested)
                    }
                };
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
fn push_modules(stack: &mut Vec<Step>, modules: Vec<Selected>) {
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
///
/// Public because a caller holding sources this crate never read — a package
/// set built in memory — still has to ask which modules they name, and asking
/// it a second way is how two answers to one question appear.
#[must_use]
pub fn imports_of(text: &str) -> Vec<String> {
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

/// Selects every source file `module` names, in resolution-tier order.
///
/// `root` is the project's own directory, or `None` when the program has none.
/// A tier answers when it *holds* the file; the bytes are read later, by the
/// walk, and only for a module it has not already visited.
fn select_module(
    root: Option<&Path>,
    module: &str,
    package: Option<&PackageRoot>,
    bundles: &[BundledRoot],
    packages: &[PackageRoot],
    sources: &mut Sources,
) -> Vec<Selected> {
    // A dependency source resolves inside its own package before it may see a
    // same-named file in the consumer project.
    if let Some(package) = package {
        let relative = package.relative_module(module).unwrap_or(module);
        let sibling = module_path(&package.source_dir, relative);
        if let Some(source) = select_package_module(relative.to_owned(), sibling, package, sources)
        {
            return vec![source];
        }
    }

    if let Some(root) = root {
        let own = module_path(root, module);
        if own.is_file() {
            return vec![named_module(module, own)];
        }
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
            return select_aggregate_modules(package, sources);
        }
        let path = module_path(&package.source_dir, relative);
        if let Some(source) = select_package_module(relative.to_owned(), path, package, sources) {
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
            let aggregate = select_aggregate_modules(&package, sources);
            if !aggregate.is_empty() {
                return aggregate;
            }
        }
        let path = module_path(bundle.source_dir(), module);
        if path.is_file() {
            return vec![named_module(module, path)];
        }
    }
    Vec::new()
}

/// A project-owned or bundled module, which retains name-based cycle identity.
fn named_module(module: &str, path: PathBuf) -> Selected {
    Selected {
        module: module.to_owned(),
        path,
        key: ModuleKey::Name(module.to_owned()),
        package: None,
        given: None,
    }
}

/// Selects a dependency-package module, with absolute-path cycle identity.
fn select_package_module(
    module: String,
    path: PathBuf,
    package: &PackageRoot,
    sources: &mut Sources,
) -> Option<Selected> {
    if !path.is_file() {
        return None;
    }
    let absolute = sources.identity(&path)?;
    Some(Selected {
        module: kira_semantics::ImportTable::package_module_identity(&package.name, &module),
        path,
        key: ModuleKey::PackagePath(absolute),
        package: Some(package.clone()),
        given: None,
    })
}

/// Selects every source file of a package and, when it has no `<name>.kira`
/// root file, adds its import namespace.
///
/// Every `.kira` file below the package's source directory is one flat module
/// scope, so all of them are selected. The bare `import <name>` needs one module
/// to bind to: a `<name>.kira` root file already provides it — its identity is
/// the package-name module — so the empty namespace alias is emitted only when
/// the package has no such file, and never duplicates the root's identity.
fn select_aggregate_modules(package: &PackageRoot, sources: &mut Sources) -> Vec<Selected> {
    let mut modules: Vec<Selected> = Vec::new();
    let mut has_root = false;
    for path in sources.listing(&package.source_dir).iter() {
        let Some(module) = package_module_name(&package.source_dir, path) else {
            continue;
        };
        let is_root = module == package.name;
        let Some(selected) = select_package_module(module, path.clone(), package, sources) else {
            continue;
        };
        if is_root {
            has_root = true;
        }
        modules.push(selected);
    }
    if modules.is_empty() || has_root {
        return modules;
    }
    let Some(absolute) = sources.identity(&package.source_dir) else {
        return modules;
    };
    // The namespace has no root file. Emit its empty semantic alias after the
    // real files so package declarations retain dependency-first order.
    modules.push(Selected {
        module: kira_semantics::ImportTable::package_module_identity(&package.name, &package.name),
        path: package.source_dir.clone(),
        key: ModuleKey::PackageNamespace(package.name.clone(), absolute),
        package: Some(package.clone()),
        given: Some(String::new()),
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
