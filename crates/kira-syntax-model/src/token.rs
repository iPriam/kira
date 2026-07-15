//! Token kinds and the token record produced by the lexer.
//!
//! Mirrors kira-zig `packages/kira_syntax_model/src/token.zig`. The Zig token
//! carries a `lexeme` slice into the source; the Rust port recovers the lexeme
//! by slicing the source text with `span` instead, so tokens stay lifetime-free.

use kira_source::Span;

/// Every terminal the Kira lexer can produce (full port of the Zig `TokenKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// End of input.
    Eof,
    /// An identifier.
    Identifier,
    /// An integer literal.
    Integer,
    /// A floating-point literal.
    Float,
    /// A string literal.
    String,
    /// A `///` doc comment line.
    DocComment,
    /// `annotation`
    KwAnnotation,
    /// `capability`
    KwCapability,
    /// `class`
    KwClass,
    /// `comptime`
    KwComptime,
    /// `macro`
    KwMacro,
    /// `quote`
    KwQuote,
    /// `construct`
    KwConstruct,
    /// `enum`
    KwEnum,
    /// `struct`
    KwStruct,
    /// `type`
    KwType,
    /// `extends`
    KwExtends,
    /// `extend`
    KwExtend,
    /// `attempt`
    KwAttempt,
    /// `try`
    KwTry,
    /// `Self`
    KwSelfType,
    /// `async`
    KwAsync,
    /// `function`
    KwFunction,
    /// `generated`
    KwGenerated,
    /// `override`
    KwOverride,
    /// `overridable`
    KwOverridable,
    /// `targets`
    KwTargets,
    /// `uses`
    KwUses,
    /// `let`
    KwLet,
    /// `var`
    KwVar,
    /// `return`
    KwReturn,
    /// `import`
    KwImport,
    /// `as`
    KwAs,
    /// `if`
    KwIf,
    /// `else`
    KwElse,
    /// `for`
    KwFor,
    /// `in`
    KwIn,
    /// `while`
    KwWhile,
    /// `break`
    KwBreak,
    /// `continue`
    KwContinue,
    /// `match`
    KwMatch,
    /// `switch`
    KwSwitch,
    /// `case`
    KwCase,
    /// `default`
    KwDefault,
    /// `true`
    KwTrue,
    /// `false`
    KwFalse,
    /// `@`
    AtSign,
    /// `$`
    Dollar,
    /// `#{` (quote splice opener)
    HashBrace,
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
    /// `;`
    Semicolon,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `?`
    Question,
    /// `=`
    Equal,
    /// `==`
    EqualEqual,
    /// `!`
    Bang,
    /// `!=`
    BangEqual,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `->`
    Arrow,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `~`
    Tilde,
    /// `<<`
    LessLess,
    /// `>>`
    GreaterGreater,
}

/// One lexed token: its kind plus the source range it covers.
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
