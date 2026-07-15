//! The KSL tokenizer.
//!
//! Mirrors kira-zig `packages/kira_ksl_parser/src/lexer.zig`.
//! TODO(port): the scanning logic lands during migration.

use kira_diagnostics::DiagnosticSink;
use kira_ksl_syntax_model::Token;
use kira_source::SourceFile;

/// Cursor state of one KSL tokenizer run over a source file.
#[derive(Debug, Default)]
pub struct Lexer {
    /// Byte offset of the next unconsumed byte.
    pub index: usize,
    /// Tokens produced so far.
    pub tokens: Vec<Token>,
}

/// Tokenizes one KSL source file, reporting problems into `sink`.
///
/// TODO(port): currently returns no tokens; the scanning loop is ported from
/// `lexer.zig` during migration.
pub fn tokenize(_source: &SourceFile, _sink: &mut DiagnosticSink) -> Vec<Token> {
    Vec::new()
}
