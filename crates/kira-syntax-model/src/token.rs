//! The token set the lexer produces and the parser consumes.
//!
//! Tokens are deliberately coarse: identifiers, literals, keywords, and
//! punctuation. Trivia (whitespace and comments) is not tokenized — the lexer
//! skips it — so the parser sees only meaningful tokens plus a terminating
//! [`TokenKind::Eof`].

use kira_source::Span;

/// One lexical token: what it is plus where it sits in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// The lexical category of this token.
    pub kind: TokenKind,
    /// The byte range this token covers in its source file.
    pub span: Span,
}

impl Token {
    /// Builds a token from its kind and span.
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

/// Every lexical category the Kira lexer emits.
///
/// Keyword variants are distinguished up front so the parser never re-compares
/// identifier text. `Unknown` carries an unexpected byte forward instead of
/// aborting, keeping the lexer total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    // Literals and names.
    /// An identifier or a keyword-shaped name the parser resolves by context.
    Identifier,
    /// A decimal integer literal (`42`).
    IntLiteral,
    /// A decimal floating-point literal (`3.5`).
    FloatLiteral,
    /// A double-quoted string literal (`"text"`).
    StringLiteral,

    // Keywords.
    /// `function`
    Function,
    /// `let`
    Let,
    /// `var`
    Var,
    /// `return`
    Return,
    /// `if`
    If,
    /// `else`
    Else,
    /// `while`
    While,
    /// `true`
    True,
    /// `false`
    False,
    /// `import`
    Import,
    /// `as`
    As,
    /// `is`
    Is,
    /// `struct`
    Struct,
    /// `enum`
    Enum,
    /// `class`
    Class,
    /// `construct`
    Construct,
    /// `trait`
    Trait,
    /// `extends`
    Extends,
    /// `override`
    Override,
    /// `match`
    Match,
    /// `attempt`
    Attempt,
    /// `try`
    Try,
    /// `for`
    For,
    /// `in`
    In,
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `type`
    Type,
    /// `distinct`
    Distinct,

    // Punctuation and operators.
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
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `@`
    At,
    /// `?`
    Question,
    /// `->`
    Arrow,
    /// `=`
    Equals,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `~`
    Tilde,
    /// `<<`
    LtLt,
    /// `>>`
    GtGt,
    /// `!`
    Bang,

    /// A byte the lexer could not classify; carried forward with a diagnostic.
    Unknown,
    /// The synthetic end-of-input marker that terminates every token stream.
    Eof,
}

impl TokenKind {
    /// Maps an identifier's text to its keyword kind, or `None` when it is a
    /// plain identifier.
    pub fn keyword_from_text(text: &str) -> Option<TokenKind> {
        Some(match text {
            "function" => TokenKind::Function,
            "let" => TokenKind::Let,
            "var" => TokenKind::Var,
            "return" => TokenKind::Return,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "import" => TokenKind::Import,
            "as" => TokenKind::As,
            "is" => TokenKind::Is,
            "struct" => TokenKind::Struct,
            "enum" => TokenKind::Enum,
            "class" => TokenKind::Class,
            "construct" => TokenKind::Construct,
            "trait" => TokenKind::Trait,
            "extends" => TokenKind::Extends,
            "override" => TokenKind::Override,
            "match" => TokenKind::Match,
            // `handle` is deliberately absent: the reference lexes it as an
            // identifier, so it is recognized contextually after an `attempt`
            // block and stays usable as a name everywhere else.
            "attempt" => TokenKind::Attempt,
            "try" => TokenKind::Try,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "type" => TokenKind::Type,
            "distinct" => TokenKind::Distinct,
            _ => return None,
        })
    }

    /// A short human-readable name for this kind, used in diagnostics.
    pub fn describe(self) -> &'static str {
        match self {
            TokenKind::Identifier => "identifier",
            TokenKind::IntLiteral => "integer literal",
            TokenKind::FloatLiteral => "float literal",
            TokenKind::StringLiteral => "string literal",
            TokenKind::Function => "`function`",
            TokenKind::Let => "`let`",
            TokenKind::Var => "`var`",
            TokenKind::Return => "`return`",
            TokenKind::If => "`if`",
            TokenKind::Else => "`else`",
            TokenKind::While => "`while`",
            TokenKind::True => "`true`",
            TokenKind::False => "`false`",
            TokenKind::Import => "`import`",
            TokenKind::As => "`as`",
            TokenKind::Is => "`is`",
            TokenKind::Struct => "`struct`",
            TokenKind::Enum => "`enum`",
            TokenKind::Class => "`class`",
            TokenKind::Construct => "`construct`",
            TokenKind::Trait => "`trait`",
            TokenKind::Extends => "`extends`",
            TokenKind::Override => "`override`",
            TokenKind::Match => "`match`",
            TokenKind::Attempt => "`attempt`",
            TokenKind::Try => "`try`",
            TokenKind::For => "`for`",
            TokenKind::In => "`in`",
            TokenKind::Break => "`break`",
            TokenKind::Continue => "`continue`",
            TokenKind::Type => "`type`",
            TokenKind::Distinct => "`distinct`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::Comma => "`,`",
            TokenKind::Colon => "`:`",
            TokenKind::Dot => "`.`",
            TokenKind::DotDot => "`..`",
            TokenKind::At => "`@`",
            TokenKind::Question => "`?`",
            TokenKind::Arrow => "`->`",
            TokenKind::Equals => "`=`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::EqEq => "`==`",
            TokenKind::BangEq => "`!=`",
            TokenKind::Lt => "`<`",
            TokenKind::LtEq => "`<=`",
            TokenKind::Gt => "`>`",
            TokenKind::GtEq => "`>=`",
            TokenKind::AmpAmp => "`&&`",
            TokenKind::PipePipe => "`||`",
            TokenKind::Amp => "`&`",
            TokenKind::Pipe => "`|`",
            TokenKind::Caret => "`^`",
            TokenKind::Tilde => "`~`",
            TokenKind::LtLt => "`<<`",
            TokenKind::GtGt => "`>>`",
            TokenKind::Bang => "`!`",
            TokenKind::Unknown => "unknown token",
            TokenKind::Eof => "end of input",
        }
    }
}
