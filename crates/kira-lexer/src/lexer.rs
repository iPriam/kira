//! The Kira tokenizer.
//!
//! Mirrors kira-zig `packages/kira_lexer/src/lexer.zig` (`tokenize` plus its
//! keyword table, comment/doc-comment handling, string decoding, and numeric
//! literal scanning). TODO(port): the scanning logic lands during migration.

use kira_diagnostics::DiagnosticSink;
use kira_source::SourceFile;
use kira_syntax_model::Token;

/// Cursor state of one tokenizer run over a source file.
#[derive(Debug, Default)]
pub struct Lexer {
    /// Byte offset of the next unconsumed byte.
    pub index: usize,
    /// Tokens produced so far.
    pub tokens: Vec<Token>,
}

/// Tokenizes one source file, reporting problems into `sink`.
///
/// TODO(port): currently returns no tokens; the scanning loop is ported from
/// `lexer.zig` during migration (whitespace/comment skipping, doc-comment
/// capture, keyword table, operators, string decoding, numeric literals).
pub fn tokenize(_source: &SourceFile, _sink: &mut DiagnosticSink) -> Vec<Token> {
    Vec::new()
}
