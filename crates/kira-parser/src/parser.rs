//! Parser entrypoint and shared cursor state.
//!
//! Mirrors kira-zig `packages/kira_parser/src/parser.zig` (`parse`,
//! `parseSource`, the `Parser` struct, and the shared expect/match/advance
//! helpers the per-topic modules build on).
//! TODO(port): the parsing logic lands during migration.

use kira_diagnostics::DiagnosticSink;
use kira_syntax_model::Token;
use kira_syntax_model::ast::Program;

/// Bounds expression-parsing recursion: past this depth the parser emits a
/// clean diagnostic instead of overflowing the native stack on pathological
/// nesting (~1100 nested parens/calls/arrays/closures SIGSEGVs otherwise).
pub const MAX_EXPR_DEPTH: u32 = 256;

/// Recursive-descent parser state over one file's token stream.
#[derive(Debug, Default)]
pub struct Parser {
    /// The token stream being consumed.
    pub tokens: Vec<Token>,
    /// Index of the next unconsumed token.
    pub index: usize,
    /// Whether a trailing `{ ... }` block binds to the current call.
    pub allow_trailing_block_call: bool,
    /// Current expression-parsing recursion depth (see [`MAX_EXPR_DEPTH`]).
    pub expr_depth: u32,
}

/// What one parse run produced: the program (possibly partial after errors).
///
/// Diagnostics travel separately through the [`DiagnosticSink`] handed to
/// [`parse`], mirroring the Zig out-parameter shape.
#[derive(Debug, Default)]
pub struct ParseResult {
    /// The parsed program; arenas and top-level declarations.
    pub program: Program,
}

/// Parses one file's tokens into a [`Program`], reporting into `sink`.
///
/// TODO(port): currently returns an empty program; the recursive-descent
/// grammar is ported module by module during migration (see this crate's
/// module tree, one Rust module per Zig parser file).
pub fn parse(_tokens: &[Token], _sink: &mut DiagnosticSink) -> ParseResult {
    ParseResult::default()
}
