//! The KSL token set.
//!
//! KSL is a small language and its keywords are all contextual in the sense
//! that matters here: `input`, `output`, `texture`, `sampler`, `read`, and
//! `write` are ordinary identifiers wherever a declaration is not being
//! parsed, and the corpus uses several of them as variable names. So the lexer
//! classifies only the words that can never be an identifier, and the parser
//! recognizes the rest by position. That keeps `let sample = …` legal beside
//! `sample(albedo, linear, uv)`.

use kira_source::Span;

/// One lexical class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenKind {
    /// A name, or one of the position-recognized words.
    Identifier,
    /// An integer literal.
    IntLiteral,
    /// A floating-point literal.
    FloatLiteral,

    /// `import`
    Import,
    /// `as`
    As,
    /// `type`
    Type,
    /// `const`
    Const,
    /// `enum`
    Enum,
    /// `function`
    Function,
    /// `shader`
    Shader,
    /// `group`
    Group,
    /// `option`
    Option,
    /// `let`
    Let,
    /// `if`
    If,
    /// `else`
    Else,
    /// `while`
    While,
    /// `return`
    Return,
    /// `true`
    True,
    /// `false`
    False,

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
    /// `.`
    Dot,
    /// `:`
    Colon,
    /// `@`
    At,
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
    EqualsEquals,
    /// `!=`
    BangEquals,
    /// `<`
    Less,
    /// `<=`
    LessEquals,
    /// `>`
    Greater,
    /// `>=`
    GreaterEquals,
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
    /// `<<`
    LessLess,
    /// `>>`
    GreaterGreater,
    /// `!`
    Bang,

    /// A character the lexer could not classify.
    Unknown,
    /// The end of the file.
    Eof,
}

impl TokenKind {
    /// How the token is written, for a diagnostic that names what it expected.
    #[must_use]
    pub fn spelling(self) -> &'static str {
        match self {
            TokenKind::Identifier => "an identifier",
            TokenKind::IntLiteral => "an integer",
            TokenKind::FloatLiteral => "a number",
            TokenKind::Import => "`import`",
            TokenKind::As => "`as`",
            TokenKind::Type => "`type`",
            TokenKind::Const => "`const`",
            TokenKind::Enum => "`enum`",
            TokenKind::Function => "`function`",
            TokenKind::Shader => "`shader`",
            TokenKind::Group => "`group`",
            TokenKind::Option => "`option`",
            TokenKind::Let => "`let`",
            TokenKind::If => "`if`",
            TokenKind::Else => "`else`",
            TokenKind::While => "`while`",
            TokenKind::Return => "`return`",
            TokenKind::True => "`true`",
            TokenKind::False => "`false`",
            TokenKind::LParen => "`(`",
            TokenKind::RParen => "`)`",
            TokenKind::LBrace => "`{`",
            TokenKind::RBrace => "`}`",
            TokenKind::LBracket => "`[`",
            TokenKind::RBracket => "`]`",
            TokenKind::Comma => "`,`",
            TokenKind::Dot => "`.`",
            TokenKind::Colon => "`:`",
            TokenKind::At => "`@`",
            TokenKind::Arrow => "`->`",
            TokenKind::Equals => "`=`",
            TokenKind::Plus => "`+`",
            TokenKind::Minus => "`-`",
            TokenKind::Star => "`*`",
            TokenKind::Slash => "`/`",
            TokenKind::Percent => "`%`",
            TokenKind::EqualsEquals => "`==`",
            TokenKind::BangEquals => "`!=`",
            TokenKind::Less => "`<`",
            TokenKind::LessEquals => "`<=`",
            TokenKind::Greater => "`>`",
            TokenKind::GreaterEquals => "`>=`",
            TokenKind::AmpAmp => "`&&`",
            TokenKind::PipePipe => "`||`",
            TokenKind::Amp => "`&`",
            TokenKind::Pipe => "`|`",
            TokenKind::Caret => "`^`",
            TokenKind::LessLess => "`<<`",
            TokenKind::GreaterGreater => "`>>`",
            TokenKind::Bang => "`!`",
            TokenKind::Unknown => "an unrecognized character",
            TokenKind::Eof => "the end of the file",
        }
    }
}

/// The keyword `word` spells, when it spells one.
///
/// Only words that can never name a value are here — see the module note.
#[must_use]
pub fn keyword(word: &str) -> Option<TokenKind> {
    Some(match word {
        "import" => TokenKind::Import,
        "as" => TokenKind::As,
        "type" => TokenKind::Type,
        "const" => TokenKind::Const,
        "enum" => TokenKind::Enum,
        "function" => TokenKind::Function,
        "shader" => TokenKind::Shader,
        "group" => TokenKind::Group,
        "option" => TokenKind::Option,
        "let" => TokenKind::Let,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "while" => TokenKind::While,
        "return" => TokenKind::Return,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        _ => return None,
    })
}

/// One lexed token: what it is and where it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// The lexical class.
    pub kind: TokenKind,
    /// The bytes it occupies.
    pub span: Span,
}

impl Token {
    /// Builds a token of `kind` covering `span`.
    #[must_use]
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Self { kind, span }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_words_the_corpus_uses_as_names_are_not_keywords() {
        // Every one of these appears as a variable or field name in the shader
        // corpus, so lexing them as keywords would break real shaders.
        for word in [
            "input",
            "output",
            "texture",
            "sampler",
            "storage",
            "uniform",
            "read",
            "read_write",
            "write",
            "vertex",
            "fragment",
            "compute",
            "threads",
            "entry",
            "sample",
            "result",
            "out",
        ] {
            assert_eq!(keyword(word), None, "`{word}` must stay an identifier");
        }
    }

    #[test]
    fn the_words_that_can_never_name_a_value_are_keywords() {
        assert_eq!(keyword("shader"), Some(TokenKind::Shader));
        assert_eq!(keyword("let"), Some(TokenKind::Let));
        assert_eq!(keyword("import"), Some(TokenKind::Import));
    }
}
