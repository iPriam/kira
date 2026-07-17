//! Semantic analyzer: the salsa frontend that turns source text into a typed
//! [`HirProgram`] plus diagnostics.
//!
//! Layer 2 of the Kira package graph.
//!
//! The frontend is built on salsa from the start so the language server and the
//! compiler share one query graph. The single input is the file's text; the
//! tracked queries are [`parsed`] (lex + parse) and [`analyzed`] (name
//! resolution + type checking). Diagnostics are never thrown — they are pushed
//! into the [`DiagnosticAccumulator`], which salsa propagates up the call
//! graph, so a caller collects every diagnostic from one `accumulated` call.

mod analyze;
mod arrays;
mod decl;
mod enums;
mod operators;
mod ownership;
mod place;
mod stmt;
mod typeck;
mod types;

pub use analyze::{Analysis, analyze};

use kira_core::Interner;
use kira_diagnostics::Diagnostic;
use kira_semantics_model::HirProgram;
use kira_source::SourceId;
use kira_syntax_model::SyntaxTree;
use salsa::Accumulator;

/// The fixed source id the v0 single-file frontend uses; the CLI mirrors it in
/// its [`kira_source::SourceMap`] so diagnostic spans render against the file.
pub const FILE_SOURCE_ID: SourceId = SourceId::new(0);

/// The one salsa input: a source file's text and path.
#[salsa::input]
pub struct SourceProgram {
    /// The full source text.
    #[returns(clone)]
    pub text: String,
    /// The path the source was loaded from (for diagnostics).
    #[returns(clone)]
    pub path: String,
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
    /// The parsed syntax tree.
    pub tree: SyntaxTree,
    /// The interner holding every identifier symbol in the tree.
    pub interner: Interner,
}

/// Lexes and parses the source, accumulating lexer/parser diagnostics.
#[salsa::tracked(returns(clone))]
pub fn parsed(db: &dyn salsa::Database, source: SourceProgram) -> ParsedProgram {
    let text = source.text(db);
    let result = kira_parser::parse(FILE_SOURCE_ID, &text);
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
    let analysis = analyze(FILE_SOURCE_ID, &parsed.tree, &parsed.interner);
    for diagnostic in analysis.diagnostics {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    analysis.program
}

#[cfg(test)]
mod tests;
