//! KSL token kinds and the token record produced by the KSL lexer.
//!
//! Mirrors kira-zig `packages/kira_ksl_syntax_model/src/token.zig`. Like the
//! Kira token, the lexeme is recovered by slicing the source with `span`.

use kira_source::Span;

/// Every terminal the KSL lexer can produce (full port of the Zig `TokenKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// End of input.
    Eof,
    /// An identifier.
    Identifier,
    /// An integer literal.
    IntegerLiteral,
    /// A floating-point literal.
    FloatLiteral,
    /// A string literal.
    StringLiteral,
    /// `@`
    AtSign,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `;`
    Semicolon,
    /// `.`
    Dot,
    /// `->`
    Arrow,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `!`
    Bang,
    /// `=`
    Equal,
    /// `==`
    EqualEqual,
    /// `!=`
    BangEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `import`
    KwImport,
    /// `as`
    KwAs,
    /// `type`
    KwType,
    /// `function`
    KwFunction,
    /// `shader`
    KwShader,
    /// `option`
    KwOption,
    /// `group`
    KwGroup,
    /// `uniform`
    KwUniform,
    /// `storage`
    KwStorage,
    /// `read`
    KwRead,
    /// `readWrite`
    KwReadWrite,
    /// `texture`
    KwTexture,
    /// `sampler`
    KwSampler,
    /// `vertex`
    KwVertex,
    /// `fragment`
    KwFragment,
    /// `compute`
    KwCompute,
    /// `input`
    KwInput,
    /// `output`
    KwOutput,
    /// `threads`
    KwThreads,
    /// `let`
    KwLet,
    /// `if`
    KwIf,
    /// `else`
    KwElse,
    /// `while`
    KwWhile,
    /// `return`
    KwReturn,
    /// `true`
    KwTrue,
    /// `false`
    KwFalse,
}

/// One lexed KSL token: its kind plus the source range it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// What kind of terminal this is.
    pub kind: TokenKind,
    /// The byte range the token covers; slice the source text to get the lexeme.
    pub span: Span,
}

impl Token {
    /// Builds a token from its kind and span.
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}
