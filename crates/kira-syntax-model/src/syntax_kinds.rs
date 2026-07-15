//! Coarse syntax-node kinds used by tooling surfaces.
//!
//! Mirrors kira-zig `packages/kira_syntax_model/src/syntax_kinds.zig`.

/// A coarse classification of syntax nodes (full port of the Zig `SyntaxKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyntaxKind {
    /// A whole program / source file.
    Program,
    /// A function declaration.
    FunctionDecl,
    /// A `{ ... }` block.
    Block,
    /// A `let` statement.
    LetStatement,
    /// An expression statement.
    ExpressionStatement,
    /// A `return` statement.
    ReturnStatement,
    /// An integer literal.
    IntegerLiteral,
    /// A string literal.
    StringLiteral,
    /// An identifier.
    Identifier,
    /// A binary expression.
    BinaryExpression,
    /// A call expression.
    CallExpression,
}
