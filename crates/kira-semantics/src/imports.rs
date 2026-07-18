//! File-scoped import resolution: which module each file may name, and under
//! what root.
//!
//! An import binds a **namespace root** in exactly one file. `import support as
//! Support` in `main.kira` lets *that file* write `Support.hello()`; a sibling
//! that wants the same spelling writes its own import. That is the whole rule,
//! and it is why the table is keyed by [`SourceId`] rather than being a single
//! program-wide map.
//!
//! # What an import does not do
//!
//! It does not gate bare names. Every top-level declaration in a package is
//! visible bare in every file of that package — the oracle keys its
//! file-scope gate by *owner package*, and same-package symbols carry no owner
//! — so `import support` never changes whether `hello()` resolves. What it
//! changes is whether `Support.hello()` does, and whether `support.kira` is
//! part of the program at all.
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

/// What one file's imports bind.
#[derive(Debug, Clone, Default)]
pub struct FileImports {
    /// Namespace root spelling (the alias, or the path's last segment) to the
    /// dotted module path it names.
    roots: HashMap<String, String>,
}

impl FileImports {
    /// The module a namespace root names in this file, if the file imports one.
    #[must_use]
    pub fn module_for_root(&self, root: &str) -> Option<&str> {
        self.roots.get(root).map(String::as_str)
    }
}

/// Every file's imports, keyed by the file they were written in.
#[derive(Debug, Clone, Default)]
pub struct ImportTable {
    files: HashMap<SourceId, FileImports>,
    /// Every module name the program was built from, and the file each is.
    modules: HashMap<String, SourceId>,
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
    /// Builds the table from the modules the program was loaded with and the
    /// imports each file wrote.
    ///
    /// An import naming a module that is not in `modules` binds nothing: it is
    /// left out of the table entirely, which is what makes a later
    /// `Missing.name()` report an unresolved *namespace root* rather than
    /// silently resolving through an import that never landed.
    pub(crate) fn build(modules: &[(String, SourceId)], imports: &[ImportEntry]) -> Self {
        let modules: HashMap<String, SourceId> = modules.iter().cloned().collect();
        let mut files: HashMap<SourceId, FileImports> = HashMap::new();
        for import in imports {
            if !modules.contains_key(&import.module) {
                continue;
            }
            files
                .entry(import.source)
                .or_default()
                .roots
                .insert(import.root.clone(), import.module.clone());
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
        self.modules.contains_key(module)
    }
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
            if self.imports.has_module(&entry.module) {
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

    /// The module a namespace root names in the file being analyzed.
    ///
    /// `None` when this file imports no such root — which is the file-scoped
    /// rule doing its job, and is *not* the same as the module not existing.
    pub(crate) fn module_for_root(&self, root: &str) -> Option<&str> {
        self.imports
            .for_file(self.source)
            .and_then(|file| file.module_for_root(root))
    }

    /// Reports a namespace root this file cannot name, distinguishing a module
    /// it merely failed to import from a name that is nothing at all.
    ///
    /// Returns whether it reported: a caller that gets `false` still owes the
    /// user its own diagnostic, because the root was not a module in any file.
    pub(crate) fn report_unimported_root(&mut self, root: &str, span: Span) -> bool {
        if !self.imports.has_module(root) {
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
