//! Tokenizer for Kira source text.
//!
//! Layer 1 of the Kira package graph.
//!
//! The lexer is a total function: any input produces a token stream (always
//! terminated by [`TokenKind::Eof`]) plus a list of diagnostics. It never
//! panics and never bails early — an unrecognized byte becomes a
//! [`TokenKind::Unknown`] token with a diagnostic and lexing continues.
//!
//! Whitespace and `//` line comments are trivia and are skipped rather than
//! tokenized. Kira separates statements with `;` or a newline, but the parser
//! recovers statement boundaries structurally, so the lexer does not emit
//! newline tokens.

use kira_diagnostics::{Code, Diagnostic, Label, Severity};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::{Token, TokenKind};

/// The result of lexing one source file.
#[derive(Debug, Clone, Default)]
pub struct LexResult {
    /// The token stream, always ending in [`TokenKind::Eof`].
    pub tokens: Vec<Token>,
    /// Diagnostics produced while lexing (unknown bytes, unterminated strings).
    pub diagnostics: Vec<Diagnostic>,
}

/// Tokenizes `text`, attributing every span to `source`.
pub fn lex(source: SourceId, text: &str) -> LexResult {
    Lexer::new(source, text).run()
}

struct Lexer<'a> {
    source: SourceId,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    fn new(source: SourceId, text: &'a str) -> Self {
        Self {
            source,
            bytes: text.as_bytes(),
            pos: 0,
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> LexResult {
        loop {
            self.skip_trivia();
            if self.pos >= self.bytes.len() {
                break;
            }
            self.lex_one();
        }
        let eof = Span::new(self.bytes.len() as u32, 0);
        self.tokens.push(Token::new(TokenKind::Eof, eof));
        LexResult {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn skip_trivia(&mut self) {
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() {
                self.pos += 1;
            } else if byte == b'/' && self.peek_at(1) == Some(b'/') {
                while let Some(b) = self.peek() {
                    if b == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn lex_one(&mut self) {
        let byte = match self.peek() {
            Some(byte) => byte,
            None => return,
        };
        if byte == b'"' {
            self.lex_string();
        } else if byte.is_ascii_digit() {
            self.lex_number();
        } else if byte == b'_' || byte.is_ascii_alphabetic() {
            self.lex_identifier();
        } else {
            self.lex_symbol();
        }
    }

    fn emit(&mut self, kind: TokenKind, start: usize) {
        let span = Span::from_bounds(start as u32, self.pos as u32);
        self.tokens.push(Token::new(kind, span));
    }

    fn lex_identifier(&mut self) {
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte == b'_' || byte.is_ascii_alphanumeric() {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
        let kind = TokenKind::keyword_from_text(text).unwrap_or(TokenKind::Identifier);
        self.emit(kind, start);
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        // `0x…` is one integer token, digits and all. Only when a hex digit
        // actually follows: `0xyz` is not a number with a typo in it, it is the
        // literal `0` and the name `xyz`, which is what it lexed as before hex
        // existed and what it still has to lex as.
        if self.peek() == Some(b'0')
            && matches!(self.peek_at(1), Some(b'x') | Some(b'X'))
            && self.peek_at(2).is_some_and(|b| b.is_ascii_hexdigit())
        {
            self.pos += 2;
            while self.peek().is_some_and(|b| b.is_ascii_hexdigit()) {
                self.pos += 1;
            }
            self.emit(TokenKind::IntLiteral, start);
            return;
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut is_float = false;
        // A `.` is a decimal point only when followed by a digit; otherwise it
        // is member access on an integer and belongs to the next token.
        if self.peek() == Some(b'.') && self.peek_at(1).is_some_and(|b| b.is_ascii_digit()) {
            is_float = true;
            self.pos += 1;
            while self.peek().is_some_and(|b| b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let kind = if is_float {
            TokenKind::FloatLiteral
        } else {
            TokenKind::IntLiteral
        };
        self.emit(kind, start);
    }

    fn lex_string(&mut self) {
        let start = self.pos;
        self.pos += 1; // opening quote
        let mut terminated = false;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.pos += 1;
                    terminated = true;
                    break;
                }
                b'\\' => {
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.pos += 1;
                    }
                }
                b'\n' => break,
                _ => self.pos += 1,
            }
        }
        if !terminated {
            let span = Span::from_bounds(start as u32, self.pos as u32);
            self.diagnostics
                .push(unterminated_string(self.source, span));
        }
        self.emit(TokenKind::StringLiteral, start);
    }

    fn lex_symbol(&mut self) {
        let start = self.pos;
        let byte = self.peek().unwrap_or(0);
        let next = self.peek_at(1);
        // Two-character operators first.
        let two = match (byte, next) {
            (b'-', Some(b'>')) => Some(TokenKind::Arrow),
            (b'=', Some(b'=')) => Some(TokenKind::EqEq),
            (b'!', Some(b'=')) => Some(TokenKind::BangEq),
            (b'<', Some(b'=')) => Some(TokenKind::LtEq),
            (b'>', Some(b'=')) => Some(TokenKind::GtEq),
            (b'&', Some(b'&')) => Some(TokenKind::AmpAmp),
            (b'|', Some(b'|')) => Some(TokenKind::PipePipe),
            (b'<', Some(b'<')) => Some(TokenKind::LtLt),
            (b'>', Some(b'>')) => Some(TokenKind::GtGt),
            (b'.', Some(b'.')) => Some(TokenKind::DotDot),
            _ => None,
        };
        if let Some(kind) = two {
            self.pos += 2;
            self.emit(kind, start);
            return;
        }
        let single = match byte {
            b'(' => Some(TokenKind::LParen),
            b')' => Some(TokenKind::RParen),
            b'{' => Some(TokenKind::LBrace),
            b'}' => Some(TokenKind::RBrace),
            b'[' => Some(TokenKind::LBracket),
            b']' => Some(TokenKind::RBracket),
            b',' => Some(TokenKind::Comma),
            b';' => Some(TokenKind::Semicolon),
            b':' => Some(TokenKind::Colon),
            b'.' => Some(TokenKind::Dot),
            b'@' => Some(TokenKind::At),
            b'?' => Some(TokenKind::Question),
            b'=' => Some(TokenKind::Equals),
            b'+' => Some(TokenKind::Plus),
            b'-' => Some(TokenKind::Minus),
            b'*' => Some(TokenKind::Star),
            b'/' => Some(TokenKind::Slash),
            b'%' => Some(TokenKind::Percent),
            b'<' => Some(TokenKind::Lt),
            b'>' => Some(TokenKind::Gt),
            b'&' => Some(TokenKind::Amp),
            b'|' => Some(TokenKind::Pipe),
            b'^' => Some(TokenKind::Caret),
            b'~' => Some(TokenKind::Tilde),
            b'!' => Some(TokenKind::Bang),
            _ => None,
        };
        match single {
            Some(kind) => {
                self.pos += 1;
                self.emit(kind, start);
            }
            None => {
                // An unclassifiable byte: consume it as Unknown so lexing is
                // total, and advance by a whole UTF-8 char to stay aligned.
                let char_len = utf8_char_len(byte);
                self.pos = (self.pos + char_len).min(self.bytes.len());
                let span = Span::from_bounds(start as u32, self.pos as u32);
                self.diagnostics.push(unknown_byte(self.source, span));
                self.tokens.push(Token::new(TokenKind::Unknown, span));
            }
        }
    }
}

/// Decodes the contents of a string-literal token span into an owned string,
/// resolving the escapes the lexer accepts (`\n`, `\t`, `\r`, `\"`, `\\`, `\0`).
///
/// `raw` is the full literal text including the surrounding quotes.
pub fn decode_string_literal(raw: &str) -> String {
    let inner = raw
        .strip_prefix('"')
        .map(|s| s.strip_suffix('"').unwrap_or(s))
        .unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn utf8_char_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

fn unknown_byte(source: SourceId, span: Span) -> Diagnostic {
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        "unexpected character in source",
        Label::primary(file_span, "not a valid Kira token"),
    );
    diagnostic.code = Some(Code::known("KLEX001"));
    diagnostic.phase = Some("lexer");
    diagnostic
}

fn unterminated_string(source: SourceId, span: Span) -> Diagnostic {
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        "unterminated string literal",
        Label::primary(file_span, "string is not closed before end of line"),
    );
    diagnostic.code = Some(Code::known("KLEX002"));
    diagnostic.phase = Some("lexer");
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        lex(SourceId::new(0), text)
            .tokens
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn lexes_a_small_function() {
        let kinds = kinds("function f(x: Int) -> Int { return x + 1 }");
        assert_eq!(kinds.first(), Some(&TokenKind::Function));
        assert_eq!(kinds.last(), Some(&TokenKind::Eof));
        assert!(kinds.contains(&TokenKind::Arrow));
        assert!(kinds.contains(&TokenKind::Plus));
    }

    #[test]
    fn distinguishes_ints_floats_and_member_dots() {
        assert_eq!(
            kinds("3 3.5"),
            vec![
                TokenKind::IntLiteral,
                TokenKind::FloatLiteral,
                TokenKind::Eof
            ]
        );
        assert_eq!(
            kinds("x.y"),
            vec![
                TokenKind::Identifier,
                TokenKind::Dot,
                TokenKind::Identifier,
                TokenKind::Eof
            ]
        );
    }

    /// The text each token covers, so a hex literal's extent is checked rather
    /// than just its kind.
    fn texts(text: &str) -> Vec<String> {
        lex(SourceId::new(0), text)
            .tokens
            .into_iter()
            .filter(|token| token.kind != TokenKind::Eof)
            .map(|token| text[token.span.start as usize..token.span.end() as usize].to_owned())
            .collect()
    }

    #[test]
    fn a_hex_literal_is_one_integer_token() {
        assert_eq!(
            kinds("0xff"),
            vec![TokenKind::IntLiteral, TokenKind::Eof],
            "the digits belong to the literal, not to a name after it"
        );
        assert_eq!(texts("0x1bc6ea02"), vec!["0x1bc6ea02"]);
        // Upper-case prefix and digits, and a hex literal ending at a delimiter.
        assert_eq!(texts("0XdeadBEEF, 1"), vec!["0XdeadBEEF", ",", "1"]);
    }

    #[test]
    fn a_zero_followed_by_a_name_is_still_two_tokens() {
        // `0x` only opens a literal when a hex digit follows, so nothing that
        // lexed as a number and a name before hex existed changed meaning.
        assert_eq!(
            kinds("0xyz"),
            vec![TokenKind::IntLiteral, TokenKind::Identifier, TokenKind::Eof]
        );
        assert_eq!(texts("0xyz"), vec!["0", "xyz"]);
        assert_eq!(texts("0x"), vec!["0", "x"]);
    }

    #[test]
    fn keywords_and_annotations() {
        assert_eq!(
            kinds("@Main let var"),
            vec![
                TokenKind::At,
                TokenKind::Identifier,
                TokenKind::Let,
                TokenKind::Var,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn skips_line_comments_and_whitespace() {
        assert_eq!(
            kinds("  // a comment\n  42 // trailing\n"),
            vec![TokenKind::IntLiteral, TokenKind::Eof]
        );
    }

    #[test]
    fn garbage_is_total_and_diagnosed() {
        let result = lex(SourceId::new(0), "let x = §");
        assert_eq!(result.tokens.last().map(|t| t.kind), Some(TokenKind::Eof));
        assert!(result.tokens.iter().any(|t| t.kind == TokenKind::Unknown));
        assert!(!result.diagnostics.is_empty());
    }

    #[test]
    fn unterminated_string_is_diagnosed_but_total() {
        let result = lex(SourceId::new(0), "\"open");
        assert!(
            result
                .tokens
                .iter()
                .any(|t| t.kind == TokenKind::StringLiteral)
        );
        assert_eq!(result.diagnostics.len(), 1);
    }

    #[test]
    fn decodes_escapes() {
        assert_eq!(decode_string_literal("\"a\\nb\""), "a\nb");
        assert_eq!(decode_string_literal("\"q\\\"q\""), "q\"q");
        assert_eq!(decode_string_literal("\"plain\""), "plain");
    }
}
