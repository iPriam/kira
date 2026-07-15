//! The KSL parser.
//!
//! Mirrors kira-zig `packages/kira_ksl_parser/src/parser.zig` (`parse` and the
//! recursive-descent `Parser` over modules, types, functions, shaders, stages).
//! TODO(port): the parsing logic lands during migration.

use kira_diagnostics::DiagnosticSink;
use kira_ksl_syntax_model::Token;
use kira_ksl_syntax_model::ast::Module;

/// Recursive-descent KSL parser state over one file's token stream.
#[derive(Debug, Default)]
pub struct Parser {
    /// The token stream being consumed.
    pub tokens: Vec<Token>,
    /// Index of the next unconsumed token.
    pub index: usize,
}

/// Parses one KSL file's tokens into a [`Module`], reporting into `sink`.
///
/// TODO(port): currently returns an empty module; the grammar is ported from
/// `parser.zig` during migration.
pub fn parse(_tokens: &[Token], _sink: &mut DiagnosticSink) -> Module {
    Module::default()
}
