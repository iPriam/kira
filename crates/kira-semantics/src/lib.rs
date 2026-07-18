//! Semantic analyzer: the salsa frontend that turns source text into a typed
//! [`HirProgram`] plus diagnostics.
//!
//! Layer 2 of the Kira package graph.
//!
//! The frontend is built on salsa from the start so the language server and the
//! compiler share one query graph. The inputs are the entry file's text plus
//! the sibling modules it imports, already read from disk by the caller; the
//! tracked queries are [`parsed`] (lex + parse) and [`analyzed`] (name
//! resolution + type checking). Diagnostics are never thrown — they are pushed
//! into the [`DiagnosticAccumulator`], which salsa propagates up the call
//! graph, so a caller collects every diagnostic from one `accumulated` call.
//!
//! # Why the caller reads the files
//!
//! Resolving `import support` to `support.kira` is a filesystem question, and
//! this crate has no filesystem: it sits below [`kira_vm_runtime`] in the
//! layering and must keep compiling for `wasm32-unknown-unknown`. So module
//! *loading* is injected — `kira-program-graph` walks the imports and hands the
//! texts in — while module *resolution* (which import binds which name, and
//! which import binds nothing) stays here, where the diagnostics belong.

mod aliases;
mod analyze;
mod arrays;
mod classes;
mod closures;
mod decl;
mod enums;
mod imports;
mod operators;
mod ownership;
mod place;
mod stmt;
mod typeck;
mod types;

pub use analyze::{Analysis, analyze};
pub use imports::{FileImports, ImportTable};

use kira_core::Interner;
use kira_diagnostics::Diagnostic;
use kira_semantics_model::HirProgram;
use kira_source::SourceId;
use kira_syntax_model::SyntaxTree;
use salsa::Accumulator;

/// The source id the program's **entry** file is pinned at; the CLI mirrors it
/// in its [`kira_source::SourceMap`] so diagnostic spans render against the
/// file.
///
/// Imported modules take the ids after it, in the order they were loaded — see
/// [`module_source_id`].
pub const FILE_SOURCE_ID: SourceId = SourceId::new(0);

/// The source id of the module at `index` in a [`SourceProgram`]'s module list.
///
/// A total function of the index rather than a lookup: the entry file owns id
/// 0 and module *i* owns id *i+1*, which is the same rule the caller's
/// [`kira_source::SourceMap`] follows when it inserts the entry first and the
/// modules after it in order. Both sides computing the same function is what
/// keeps a diagnostic pointing at the file it was written in.
#[must_use]
pub fn module_source_id(index: usize) -> SourceId {
    SourceId::new(index as u32 + 1)
}

/// One module the program is built from, already read from disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ModuleSource {
    /// The dotted module path the import wrote (`support`, `Foundation.Web`).
    ///
    /// This is the key an `import` resolves against, not a file path: two files
    /// importing the same module name get the same module.
    pub module: String,
    /// The path the module was loaded from (for diagnostics).
    pub path: String,
    /// The module's full source text.
    pub text: String,
}

/// The one salsa input: the entry file plus the modules it imports.
#[salsa::input]
pub struct SourceProgram {
    /// The entry file's full source text.
    #[returns(clone)]
    pub text: String,
    /// The path the entry file was loaded from (for diagnostics).
    #[returns(clone)]
    pub path: String,
    /// The imported modules, dependencies before dependents.
    ///
    /// The order is load order and it is the order the modules' items appear in
    /// the tree, so a module may name a struct declared in a module it imports.
    #[returns(clone)]
    pub modules: Vec<ModuleSource>,
}

impl SourceProgram {
    /// Creates a single-file program: an entry file that imports nothing.
    pub fn single(db: &dyn salsa::Database, text: String, path: String) -> Self {
        Self::new(db, text, path, Vec::new())
    }
}

/// A diagnostic emitted by any frontend query.
///
/// Wrapping [`Diagnostic`] as a salsa accumulator lets every stage report
/// without threading a sink; `query::accumulated::<DiagnosticAccumulator>`
/// gathers them, including those from called queries.
#[salsa::accumulator]
#[derive(Debug, Clone)]
pub struct DiagnosticAccumulator(pub Diagnostic);

/// A parsed program: the syntax tree and the interner backing its symbols.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProgram {
    /// The parsed syntax tree, spanning every file the program is built from.
    pub tree: SyntaxTree,
    /// The interner holding every identifier symbol in the tree.
    pub interner: Interner,
}

/// Lexes and parses every file of the program, accumulating lexer/parser
/// diagnostics.
///
/// Modules are parsed **before** the entry file so their declarations come
/// first in the tree: a struct field may only name a struct declared earlier,
/// and the entry file is the one that depends on the modules, never the other
/// way round.
#[salsa::tracked(returns(clone))]
pub fn parsed(db: &dyn salsa::Database, source: SourceProgram) -> ParsedProgram {
    let modules = source.modules(db);
    let entry_text = source.text(db);
    let mut files: Vec<(SourceId, &str)> = modules
        .iter()
        .enumerate()
        .map(|(index, module)| (module_source_id(index), module.text.as_str()))
        .collect();
    files.push((FILE_SOURCE_ID, entry_text.as_str()));

    let result = kira_parser::parse_files(&files);
    for diagnostic in result.diagnostics {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    ParsedProgram {
        tree: result.tree,
        interner: result.interner,
    }
}

/// Resolves names and type-checks the program, accumulating diagnostics.
#[salsa::tracked(returns(clone))]
pub fn analyzed(db: &dyn salsa::Database, source: SourceProgram) -> HirProgram {
    let parsed = parsed(db, source);
    let modules = source.modules(db);
    let known: Vec<(String, SourceId)> = modules
        .iter()
        .enumerate()
        .map(|(index, module)| (module.module.clone(), module_source_id(index)))
        .collect();
    let analysis = analyze(&parsed.tree, &parsed.interner, &known);
    for diagnostic in analysis.diagnostics {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    analysis.program
}

#[cfg(test)]
mod tests;
