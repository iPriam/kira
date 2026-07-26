//! File-scoped import resolution: which module each file may name, and under
//! what root.
//!
//! An import binds a **namespace root** in exactly one file. `import support as
//! Support` in `main.kira` lets *that file* write `Support.hello()`; a sibling
//! that wants the same spelling writes its own import. That is the whole rule,
//! and it is why the table is keyed by [`SourceId`] rather than being a single
//! program-wide map.
//!
//! # What an import gates
//!
//! Visibility is keyed by **owner package**, so an import decides whether
//! another package's declarations are nameable here at all — bare or qualified.
//! Three rules, and nothing else ([`ImportTable::sees`]):
//!
//! * **One program is one flat scope.** Every top-level declaration in an app
//!   or library is visible bare in every one of its own files, so `import
//!   support` never changes whether a sibling's `hello()` resolves.
//! * **A dependency arrives through an import.** Importing any module of a
//!   package makes what that package declares nameable in *that file*.
//! * **Visibility does not compose.** What a dependency itself imports stays
//!   its own business: importing `Outer` never lends you `Inner`, and an import
//!   written in one file gates that file only.
//!
//! Without the third rule a program's vocabulary is whatever its dependency
//! graph happens to reach, and two packages that each declare a `Text` become
//! one name that the loader order picks between.
//!
//! # Cycles are not an error
//!
//! Two modules that import each other resolve fine: loading is
//! visited-set-guarded, so a cycle terminates instead of diverging, and both
//! files end up in the program exactly once. The oracle accepts this, so no
//! cycle diagnostic is invented here — rejecting a program the reference
//! accepts would be a worse bug than the one it would catch.

use std::collections::HashMap;

use kira_core::Interner;
use kira_source::{SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::Item;

use crate::analyze::Analyzer;

/// Separates a dependency package namespace from its package-relative module.
const PACKAGE_MODULE_SEPARATOR: &str = "::";

/// One resolved module binding in a file.
#[derive(Debug, Clone)]
struct ModuleBinding {
    /// The import path as the source wrote it.
    module: String,
}

/// What one file's imports bind.
#[derive(Debug, Clone, Default)]
pub struct FileImports {
    /// Namespace root spelling (the alias, or the path's last segment) to the
    /// resolved module binding.
    roots: HashMap<String, ModuleBinding>,
}

impl FileImports {
    /// The module a namespace root names in this file, if the file imports one.
    #[must_use]
    pub fn module_for_root(&self, root: &str) -> Option<&str> {
        self.roots.get(root).map(|binding| binding.module.as_str())
    }
}

/// Module sources indexed without collapsing equal package-relative names.
#[derive(Debug, Clone, Default)]
struct ModuleIndex {
    /// Project and bundled modules, keyed by their written dotted path.
    plain: HashMap<String, SourceId>,
    /// Dependency modules, first by package namespace and then relative path.
    packages: HashMap<String, HashMap<String, SourceId>>,
    /// The dependency package each source belongs to.
    source_packages: HashMap<SourceId, String>,
}

impl ModuleIndex {
    /// Builds the package-aware indexes from canonical module identities.
    fn new(modules: &[(String, SourceId)]) -> Self {
        let mut index = Self::default();
        for (identity, source) in modules {
            if let Some((package, module)) = package_identity(identity) {
                index
                    .packages
                    .entry(package.to_owned())
                    .or_default()
                    .insert(module.to_owned(), *source);
                index.source_packages.insert(*source, package.to_owned());
            } else {
                index.plain.insert(identity.clone(), *source);
            }
        }
        index
    }

    /// Selects the source an import names from the importing file's context.
    fn source_for(&self, source: SourceId, module: &str) -> Option<SourceId> {
        if let Some(package) = self.source_packages.get(&source)
            && let Some(module_source) = self
                .packages
                .get(package)
                .and_then(|modules| modules.get(module))
        {
            return Some(*module_source);
        }
        if let Some(module_source) = self.plain.get(module) {
            return Some(*module_source);
        }
        let (package, relative) = module.split_once('.').unwrap_or((module, module));
        self.packages
            .get(package)
            .and_then(|modules| modules.get(relative))
            .copied()
    }

    /// Whether any loaded module has this written or package-relative name.
    fn contains(&self, module: &str) -> bool {
        self.plain.contains_key(module)
            || self.source_for(crate::FILE_SOURCE_ID, module).is_some()
            || self
                .packages
                .values()
                .any(|modules| modules.contains_key(module))
    }
}

/// Every file's imports, keyed by the file they were written in.
#[derive(Debug, Clone, Default)]
pub struct ImportTable {
    files: HashMap<SourceId, FileImports>,
    /// Every loaded module, preserving dependency-package namespace identity.
    modules: ModuleIndex,
}

/// One import as the analyzer needs it: where it was written and what it names.
#[derive(Debug, Clone)]
pub(crate) struct ImportEntry {
    /// The file the import was written in.
    pub(crate) source: SourceId,
    /// The dotted module path.
    pub(crate) module: String,
    /// The namespace root the file binds: the alias, or the last path segment.
    pub(crate) root: String,
    /// Span of the module path, for the unresolved-import diagnostic.
    pub(crate) span: Span,
}

impl ImportTable {
    /// Encodes a dependency module with the package namespace that owns it.
    ///
    /// Loaders must use this for dependency sources so equal relative names in
    /// separate packages remain distinct during semantic import resolution.
    #[must_use]
    pub fn package_module_identity(package: &str, module: &str) -> String {
        format!("{package}{PACKAGE_MODULE_SEPARATOR}{module}")
    }

    /// The dependency package a file belongs to, or `None` for a file of the
    /// project itself.
    ///
    /// Two declarations of the same name in different packages are two
    /// declarations, not one; this is what lets a resolver tell them apart.
    #[must_use]
    pub fn package_of(&self, source: SourceId) -> Option<&str> {
        self.modules
            .source_packages
            .get(&source)
            .map(String::as_str)
    }

    /// Whether a declaration written in `declaration` is nameable, bare, from
    /// `file`.
    ///
    /// Two things are, and nothing else is:
    ///
    /// * **This program's own work.** Every file of one app or library shares
    ///   one flat scope, so a sibling's declaration needs no import.
    /// * **What a package this file imports provides.** The import is what
    ///   makes the package's own declarations nameable here.
    ///
    /// What an imported package *itself* imports is not included: visibility
    /// does not compose, so a library's dependencies stay its own business and
    /// do not become a consumer's vocabulary. Nor does an import in one file
    /// reach another file — imports are written per file and gate per file.
    #[must_use]
    pub fn sees(&self, file: SourceId, declaration: SourceId) -> bool {
        if file == declaration {
            return true;
        }
        let owner = self.package_of(declaration);
        if self.package_of(file) == owner {
            return true;
        }
        self.imports_owner_of(file, declaration, owner)
    }

    /// The packages `file` imports, in no particular order.
    ///
    /// A name a file writes may come from any of them; which one it resolves
    /// against is the resolver's question, and this is the candidate set.
    #[must_use]
    pub fn imported_packages(&self, file: SourceId) -> Vec<String> {
        let Some(imports) = self.files.get(&file) else {
            return Vec::new();
        };
        let mut packages: Vec<String> = imports
            .roots
            .values()
            .filter_map(|binding| {
                let source = self.module_source_for(file, &binding.module)?;
                self.package_of(source).map(str::to_owned)
            })
            .collect();
        packages.sort();
        packages.dedup();
        packages
    }

    /// Whether one of `file`'s imports reaches the declaration's home.
    ///
    /// Two shapes of home, because a program has two kinds of module. A
    /// dependency package is imported *as a package*: importing any of its
    /// modules makes what the package provides nameable, which is what a
    /// package is for. A module with no package — a bundled library like
    /// `Foundation`, or another file of this program — is imported as itself,
    /// so it is the module that has to be named.
    fn imports_owner_of(&self, file: SourceId, declaration: SourceId, owner: Option<&str>) -> bool {
        let Some(imports) = self.files.get(&file) else {
            return false;
        };
        imports.roots.values().any(|binding| {
            let Some(source) = self.module_source_for(file, &binding.module) else {
                return false;
            };
            match owner {
                Some(package) => self.package_of(source) == Some(package),
                None => source == declaration,
            }
        })
    }

    /// Builds the table from the modules the program was loaded with and the
    /// imports each file wrote.
    ///
    /// An import naming a module that is not in `modules` binds nothing: it is
    /// left out of the table entirely, which is what makes a later
    /// `Missing.name()` report an unresolved *namespace root* rather than
    /// silently resolving through an import that never landed.
    pub(crate) fn build(modules: &[(String, SourceId)], imports: &[ImportEntry]) -> Self {
        let modules = ModuleIndex::new(modules);
        let mut files: HashMap<SourceId, FileImports> = HashMap::new();
        for import in imports {
            if modules.source_for(import.source, &import.module).is_none() {
                continue;
            }
            files.entry(import.source).or_default().roots.insert(
                import.root.clone(),
                ModuleBinding {
                    module: import.module.clone(),
                },
            );
        }
        Self { files, modules }
    }

    /// What `source` imports. An empty set for a file that imported nothing.
    #[must_use]
    pub fn for_file(&self, source: SourceId) -> Option<&FileImports> {
        self.files.get(&source)
    }

    /// Whether the program contains a module by this name at all.
    ///
    /// The difference between "no such module" and "this file did not import
    /// it" is the difference between two diagnostics, so it is asked
    /// separately.
    #[must_use]
    pub fn has_module(&self, module: &str) -> bool {
        self.modules.contains(module)
    }

    /// The file a project import of this module name selects, when available.
    #[must_use]
    pub fn module_source(&self, module: &str) -> Option<SourceId> {
        self.modules.source_for(crate::FILE_SOURCE_ID, module)
    }

    /// The file an import selects from the importing file's package context.
    fn module_source_for(&self, source: SourceId, module: &str) -> Option<SourceId> {
        self.modules.source_for(source, module)
    }
}

/// Splits a canonical dependency module identity into package and relative path.
fn package_identity(identity: &str) -> Option<(&str, &str)> {
    let (package, module) = identity.split_once(PACKAGE_MODULE_SEPARATOR)?;
    (!package.is_empty() && !module.is_empty()).then_some((package, module))
}

/// Reads every `import` item out of the tree, paired with the file that wrote
/// it.
pub(crate) fn collect_imports(tree: &SyntaxTree, interner: &Interner) -> Vec<ImportEntry> {
    let mut entries = Vec::new();
    for (source, item) in tree.items_with_source() {
        let Item::Import(declaration) = item else {
            continue;
        };
        let segments: Vec<&str> = declaration
            .path
            .iter()
            .map(|&segment| interner.resolve(segment))
            .collect();
        // The parser produces no import with an empty path, but the type
        // permits one; an import that names nothing is skipped rather than
        // reported, because the parser already said what was wrong.
        let Some(&last) = segments.last() else {
            continue;
        };
        let root = match declaration.alias {
            Some(alias) => interner.resolve(alias).to_owned(),
            None => last.to_owned(),
        };
        entries.push(ImportEntry {
            source,
            module: segments.join("."),
            root,
            span: declaration.path_span,
        });
    }
    entries
}

impl Analyzer<'_> {
    /// Reports every import that names no module the program was built from.
    ///
    /// The loader is what looks on disk; by the time analysis runs, an import
    /// whose module is absent is one that could not be found, so this is where
    /// the user hears about it — with the span of the module path they wrote.
    pub(crate) fn report_unresolved_imports(&mut self, entries: &[ImportEntry]) {
        for entry in entries {
            if self
                .imports
                .module_source_for(entry.source, &entry.module)
                .is_some()
            {
                continue;
            }
            self.source = entry.source;
            let module = &entry.module;
            let path = module.replace('.', "/");
            self.emit(
                entry.span,
                "KSEM032",
                format!(
                    "Kira could not find a module for import `{module}`; \
                     expected to find it at `{path}.kira` beside the program"
                ),
            );
        }
        self.source = crate::FILE_SOURCE_ID;
    }

    /// Links every resolved import's path to the top of the file it names, so
    /// go-to-definition on `import support` opens `support.kira`.
    pub(crate) fn link_resolved_imports(&mut self, entries: &[ImportEntry]) {
        for entry in entries {
            let Some(module_source) = self.imports.module_source_for(entry.source, &entry.module)
            else {
                continue;
            };
            self.source = entry.source;
            self.link(
                entry.span,
                kira_source::FileSpan::new(module_source, Span::new(0, 0)),
            );
        }
        self.source = crate::FILE_SOURCE_ID;
    }

    /// The module a namespace root names in the file being analyzed.
    ///
    /// `None` when this file imports no such root — which is the file-scoped
    /// rule doing its job, and is *not* the same as the module not existing.
    pub(crate) fn module_for_root(&self, root: &str) -> Option<&str> {
        self.imports
            .for_file(self.source)
            .and_then(|file| file.module_for_root(root))
    }

    /// The file a namespace root names in the file being analyzed.
    ///
    /// A qualifier is not only a scope check: `KiraUIFoundation.Color` says
    /// *whose* `Color` is meant, and the package that owns this file is what
    /// answers that. Resolving the root to its module is the step that makes the
    /// owner knowable.
    pub(crate) fn module_source_for_root(&self, root: &str) -> Option<SourceId> {
        let module = self.module_for_root(root)?;
        self.imports.module_source_for(self.source, module)
    }

    /// Reports a namespace root this file cannot name, distinguishing a module
    /// it merely failed to import from a name that is nothing at all.
    ///
    /// Returns whether it reported: a caller that gets `false` still owes the
    /// user its own diagnostic, because the root was not a module in any file.
    pub(crate) fn report_unimported_root(&mut self, root: &str, span: Span) -> bool {
        if self.imports.module_source_for(self.source, root).is_none() {
            return false;
        }
        self.emit(
            span,
            "KSEM027",
            format!(
                "`{root}` is not in scope in this file; a sibling file's `import {root}` \
                 does not carry over — add `import {root}` here"
            ),
        );
        true
    }
}
