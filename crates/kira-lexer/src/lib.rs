//! Tokenizer for Kira source text.
//!
//! Layer 1 of the Kira package graph.
//!
//! The lexer is a total function: any input produces a token stream (always
//! terminated by [`TokenKind::Eof`]) plus a list of diagnostics. It never
//! panics and never bails early — an unrecognized byte becomes a
//! [`TokenKind::Unknown`] token with a diagnostic and lexing continues.
//!
//! Whitespace, `//` line comments, and nesting `/* … */` block comments are
//! trivia and are skipped rather than tokenized. Newlines are whitespace: Kira
//! has no statement terminator, the grammar delimits statements structurally,
//! so the lexer emits no newline token and `;` is an unknown character.
//!
//! Source is UTF-8. A leading byte-order mark is accepted and skipped; one
//! anywhere else is an unknown character like any other stray byte.

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
        if self.bytes.starts_with(UTF8_BOM) {
            self.pos = UTF8_BOM.len();
        }
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
            } else if byte == b'/' && self.peek_at(1) == Some(b'*') {
                self.skip_block_comment();
            } else {
                break;
            }
        }
    }

    /// Skips a `/* … */` comment, with the cursor on its `/*`.
    ///
    /// Block comments nest, so a commented-out region that itself holds one
    /// closes where it should. A comment the file ends inside is a fatal
    /// lexer error, reported at the opener.
    fn skip_block_comment(&mut self) {
        let start = self.pos;
        let mut depth = 0usize;
        while self.pos < self.bytes.len() {
            if self.peek() == Some(b'/') && self.peek_at(1) == Some(b'*') {
                depth += 1;
                self.pos += 2;
            } else if self.peek() == Some(b'*') && self.peek_at(1) == Some(b'/') {
                depth -= 1;
                self.pos += 2;
                if depth == 0 {
                    return;
                }
            } else {
                self.pos += 1;
            }
        }
        let span = Span::from_bounds(start as u32, self.pos as u32);
        self.diagnostics
            .push(unterminated_comment(self.source, span));
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
                    let escape = self.pos;
                    self.pos += 1;
                    match self.peek() {
                        // A backslash at the end of a line CONTINUES the string
                        // on the next one: the literal keeps scanning past the
                        // newline instead of running into the unterminated
                        // diagnostic below.
                        //
                        // This is how a message longer than a line is written
                        // without either overrunning the margin or being
                        // concatenated out of pieces, and it is the one escape
                        // whose meaning is "produce nothing" — the newline and
                        // the indentation that follows it are layout, so
                        // `decode_string_literal` drops them.
                        Some(b'\n') => self.pos += 1,
                        // The same line ending written the other way. Splitting
                        // the pair would leave the `\r` in the text and the
                        // `\n` to close the literal.
                        Some(b'\r') if self.peek_at(1) == Some(b'\n') => self.pos += 2,
                        // A backslash with nothing after it at all: the file
                        // ended mid-literal, which the unterminated diagnostic
                        // below is exactly the report for.
                        None => {}
                        Some(escaped) => {
                            if !matches!(escaped, b'n' | b't' | b'r' | b'e' | b'0' | b'"' | b'\\') {
                                // The diagnostic names the whole escaped
                                // character, so its span must cover every
                                // byte of it rather than cut one mid-char.
                                let end =
                                    (escape + 1 + utf8_char_len(escaped)).min(self.bytes.len());
                                let span = Span::from_bounds(escape as u32, end as u32);
                                self.diagnostics.push(unknown_escape(self.source, span));
                            }
                            self.pos += 1;
                        }
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
        // Three-character operators first: the shifting compound assignments,
        // the only tokens whose prefix (`<<`, `>>`) is itself a token.
        let three = match (byte, next, self.peek_at(2)) {
            (b'<', Some(b'<'), Some(b'=')) => Some(TokenKind::LtLtEq),
            (b'>', Some(b'>'), Some(b'=')) => Some(TokenKind::GtGtEq),
            _ => None,
        };
        if let Some(kind) = three {
            self.pos += 3;
            self.emit(kind, start);
            return;
        }
        // Two-character operators next.
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
            (b'+', Some(b'=')) => Some(TokenKind::PlusEq),
            (b'-', Some(b'=')) => Some(TokenKind::MinusEq),
            (b'*', Some(b'=')) => Some(TokenKind::StarEq),
            (b'/', Some(b'=')) => Some(TokenKind::SlashEq),
            (b'%', Some(b'=')) => Some(TokenKind::PercentEq),
            (b'&', Some(b'=')) => Some(TokenKind::AmpEq),
            (b'|', Some(b'=')) => Some(TokenKind::PipeEq),
            (b'^', Some(b'=')) => Some(TokenKind::CaretEq),
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
                let diagnostic = if byte == b';' {
                    semicolon(self.source, span)
                } else if self.bytes[start..self.pos] == *UTF8_BOM {
                    misplaced_bom(self.source, span)
                } else {
                    unknown_byte(self.source, span)
                };
                self.diagnostics.push(diagnostic);
                self.tokens.push(Token::new(TokenKind::Unknown, span));
            }
        }
    }
}

/// An escape the lexer does not define, found while decoding a literal.
///
/// The lexer already reported it as `KLEX003`; the decoder's caller uses this
/// to keep the literal out of the tree rather than to report it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownEscape;

/// Decodes the contents of a string-literal token span into an owned string,
/// resolving the escapes the lexer accepts (`\n`, `\t`, `\r`, `\e`, `\"`, `\\`, `\0`)
/// and the line continuation a backslash before a newline writes.
///
/// `raw` is the full literal text including the surrounding quotes. A literal
/// holding an escape outside that set decodes to no value at all: the lexer
/// diagnosed it, and no recovered text may stand in for what was written.
pub fn decode_string_literal(raw: &str) -> Result<String, UnknownEscape> {
    let inner = raw
        .strip_prefix('"')
        .map(|s| s.strip_suffix('"').unwrap_or(s))
        .unwrap_or(raw);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('e') => out.push('\x1b'),
            Some('0') => out.push('\0'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            // A line continuation contributes nothing. The newline is not in
            // the message, and neither is the indentation that lines the next
            // line up under this one — that whitespace is there for the reader
            // of the source, and a literal that kept it would put a run of
            // spaces in the middle of a sentence.
            Some('\n') => skip_continuation_indent(&mut chars),
            Some('\r') => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                skip_continuation_indent(&mut chars);
            }
            Some(_) | None => return Err(UnknownEscape),
        }
    }
    Ok(out)
}

/// Drops the whitespace a continued line is indented by.
///
/// Every whitespace character, not just the spaces on the next line: a
/// continuation followed by a blank line is still one continuation, and the
/// text resumes at the first thing that is not layout.
fn skip_continuation_indent(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
        chars.next();
    }
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

/// The UTF-8 encoding of U+FEFF, the byte-order mark.
const UTF8_BOM: &[u8] = &[0xef, 0xbb, 0xbf];

fn semicolon(source: SourceId, span: Span) -> Diagnostic {
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        "`;` is not Kira syntax",
        Label::primary(
            file_span,
            "a statement ends where its grammar ends; remove the `;`",
        ),
    );
    diagnostic.code = Some(Code::known("KLEX005"));
    diagnostic.phase = Some("lexer");
    diagnostic
}

fn misplaced_bom(source: SourceId, span: Span) -> Diagnostic {
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        "byte-order mark inside the file",
        Label::primary(
            file_span,
            "a UTF-8 byte-order mark is accepted only as the first bytes of a file",
        ),
    );
    diagnostic.code = Some(Code::known("KLEX006"));
    diagnostic.phase = Some("lexer");
    diagnostic
}

fn unterminated_comment(source: SourceId, span: Span) -> Diagnostic {
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        "unterminated block comment",
        Label::primary(
            file_span,
            "this `/*` is never closed; block comments nest, so every `/*` needs its own `*/`",
        ),
    );
    diagnostic.code = Some(Code::known("KLEX004"));
    diagnostic.phase = Some("lexer");
    diagnostic
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

fn unknown_escape(source: SourceId, span: Span) -> Diagnostic {
    let file_span = FileSpan::new(source, span);
    let mut diagnostic = Diagnostic::single(
        Severity::Error,
        "unknown string escape",
        Label::primary(
            file_span,
            "the escapes are \\n \\t \\r \\e \\0 \\\" \\\\, and a newline, which continues the string",
        ),
    );
    diagnostic.code = Some(Code::known("KLEX003"));
    diagnostic.phase = Some("lexer");
    diagnostic
}

#[cfg(test)]
mod tests;
