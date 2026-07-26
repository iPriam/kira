//! Lexer and parser for KSL (Kira Shading Language) source.
//!
//! Layer 1 of the Kira package graph.
//!
//! Hand-written and error-resilient, like Kira's own parser: [`parse`] always
//! returns a tree, so a file with a malformed field still reaches semantics and
//! still reports everything else wrong with it. Nothing here resolves a name or
//! checks a type — the tree carries what was written and no more.

pub mod lexer;

mod diagnostics;
mod parser;
#[cfg(test)]
mod tests;

use kira_core::Interner;
use kira_diagnostics::Diagnostic;
use kira_ksl_syntax_model::tree::KslTree;
use kira_source::SourceId;

pub use lexer::lex;

/// What parsing one KSL file produced.
#[derive(Debug)]
pub struct Parsed {
    /// The tree, which is always present even when the file did not parse.
    pub tree: KslTree,
    /// The names the tree's symbols resolve through.
    pub interner: Interner,
    /// Everything the parse reported, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

impl Parsed {
    /// Whether the parse reported nothing.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Parses `text` as the KSL file `source`.
///
/// Total: a rejected construct is reported, skipped, and the parse continues
/// at the next declaration.
#[must_use]
pub fn parse(source: SourceId, text: &str) -> Parsed {
    let tokens = lex(text);
    let mut parser = parser::Parser::new(text, tokens, diagnostics::Reporter::new(source));
    parser.items();
    let parser::Parser {
        tree,
        interner,
        reporter,
        ..
    } = parser;
    Parsed {
        tree,
        interner,
        diagnostics: reporter.into_diagnostics(),
    }
}
