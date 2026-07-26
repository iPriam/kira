//! The KSL lexer: source bytes to a token vector.
//!
//! Total, like the parser above it. An unrecognized character becomes
//! [`TokenKind::Unknown`] and lexing continues, so one stray byte never costs
//! the file the rest of its diagnostics.

use kira_ksl_syntax_model::token::{Token, TokenKind, keyword};
use kira_source::Span;

/// Lexes `text`, always ending in one [`TokenKind::Eof`].
#[must_use]
pub fn lex(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0usize;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte.is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if byte == b'/' && bytes.get(at + 1) == Some(&b'/') {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        let start = at;
        let kind = if is_name_start(byte) {
            while at < bytes.len() && is_name_continue(bytes[at]) {
                at += 1;
            }
            let word = text.get(start..at).unwrap_or("");
            keyword(word).unwrap_or(TokenKind::Identifier)
        } else if byte.is_ascii_digit() {
            number(bytes, &mut at)
        } else {
            punctuation(bytes, &mut at)
        };
        tokens.push(Token::new(kind, span(start, at)));
    }
    tokens.push(Token::new(TokenKind::Eof, span(bytes.len(), bytes.len())));
    tokens
}

/// Whether `byte` may open a name.
fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Whether `byte` may continue a name.
fn is_name_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Lexes a number starting at `at`, advancing past it.
///
/// A `.` is part of the number only when a digit follows it, so `position.x`
/// stays a field access and `1.0` stays one literal.
fn number(bytes: &[u8], at: &mut usize) -> TokenKind {
    let mut kind = TokenKind::IntLiteral;
    while *at < bytes.len() && bytes[*at].is_ascii_digit() {
        *at += 1;
    }
    if bytes.get(*at) == Some(&b'.') && bytes.get(*at + 1).is_some_and(u8::is_ascii_digit) {
        kind = TokenKind::FloatLiteral;
        *at += 1;
        while *at < bytes.len() && bytes[*at].is_ascii_digit() {
            *at += 1;
        }
    }
    if matches!(bytes.get(*at), Some(b'e' | b'E')) {
        let mut ahead = *at + 1;
        if matches!(bytes.get(ahead), Some(b'+' | b'-')) {
            ahead += 1;
        }
        if bytes.get(ahead).is_some_and(u8::is_ascii_digit) {
            kind = TokenKind::FloatLiteral;
            *at = ahead;
            while *at < bytes.len() && bytes[*at].is_ascii_digit() {
                *at += 1;
            }
        }
    }
    kind
}

/// Lexes an operator or delimiter starting at `at`, advancing past it.
fn punctuation(bytes: &[u8], at: &mut usize) -> TokenKind {
    let two = |bytes: &[u8], at: usize| (bytes.get(at).copied(), bytes.get(at + 1).copied());
    let (first, second) = two(bytes, *at);
    if let (Some(first), Some(second)) = (first, second) {
        let paired = match (first, second) {
            (b'-', b'>') => Some(TokenKind::Arrow),
            (b'=', b'=') => Some(TokenKind::EqualsEquals),
            (b'!', b'=') => Some(TokenKind::BangEquals),
            (b'<', b'=') => Some(TokenKind::LessEquals),
            (b'>', b'=') => Some(TokenKind::GreaterEquals),
            (b'&', b'&') => Some(TokenKind::AmpAmp),
            (b'|', b'|') => Some(TokenKind::PipePipe),
            (b'<', b'<') => Some(TokenKind::LessLess),
            (b'>', b'>') => Some(TokenKind::GreaterGreater),
            _ => None,
        };
        if let Some(kind) = paired {
            *at += 2;
            return kind;
        }
    }
    let single = match first {
        Some(b'(') => TokenKind::LParen,
        Some(b')') => TokenKind::RParen,
        Some(b'{') => TokenKind::LBrace,
        Some(b'}') => TokenKind::RBrace,
        Some(b'[') => TokenKind::LBracket,
        Some(b']') => TokenKind::RBracket,
        Some(b',') => TokenKind::Comma,
        Some(b'.') => TokenKind::Dot,
        Some(b':') => TokenKind::Colon,
        Some(b'@') => TokenKind::At,
        Some(b'=') => TokenKind::Equals,
        Some(b'+') => TokenKind::Plus,
        Some(b'-') => TokenKind::Minus,
        Some(b'*') => TokenKind::Star,
        Some(b'/') => TokenKind::Slash,
        Some(b'%') => TokenKind::Percent,
        Some(b'<') => TokenKind::Less,
        Some(b'>') => TokenKind::Greater,
        Some(b'&') => TokenKind::Amp,
        Some(b'|') => TokenKind::Pipe,
        Some(b'^') => TokenKind::Caret,
        Some(b'!') => TokenKind::Bang,
        _ => TokenKind::Unknown,
    };
    // A multi-byte character would otherwise be split into one `Unknown` per
    // byte; stepping to the next boundary reports it once.
    *at += if single == TokenKind::Unknown {
        first.map_or(1, utf8_width)
    } else {
        1
    };
    single
}

/// How many bytes the UTF-8 character opening with `byte` occupies.
fn utf8_width(byte: u8) -> usize {
    match byte {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// The span from `start` to `end`, clamped into `u32`.
fn span(start: usize, end: usize) -> Span {
    let start_u32 = u32::try_from(start).unwrap_or(u32::MAX);
    let end_u32 = u32::try_from(end).unwrap_or(u32::MAX);
    Span::new(start_u32, end_u32.saturating_sub(start_u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<TokenKind> {
        lex(text).into_iter().map(|token| token.kind).collect()
    }

    #[test]
    fn a_dot_between_digits_is_a_float_and_after_a_name_is_a_field() {
        assert_eq!(kinds("1.0"), vec![TokenKind::FloatLiteral, TokenKind::Eof]);
        assert_eq!(
            kinds("position.x"),
            vec![
                TokenKind::Identifier,
                TokenKind::Dot,
                TokenKind::Identifier,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn two_character_operators_win_over_their_first_byte() {
        assert_eq!(
            kinds("a -> b >= c << d"),
            vec![
                TokenKind::Identifier,
                TokenKind::Arrow,
                TokenKind::Identifier,
                TokenKind::GreaterEquals,
                TokenKind::Identifier,
                TokenKind::LessLess,
                TokenKind::Identifier,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn a_line_comment_runs_to_the_newline_and_no_further() {
        assert_eq!(
            kinds("// gone\nkept"),
            vec![TokenKind::Identifier, TokenKind::Eof]
        );
    }

    #[test]
    fn an_unknown_byte_is_one_token_and_lexing_continues() {
        assert_eq!(
            kinds("a $ b"),
            vec![
                TokenKind::Identifier,
                TokenKind::Unknown,
                TokenKind::Identifier,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn a_multibyte_character_is_reported_once_rather_than_per_byte() {
        assert_eq!(kinds("é"), vec![TokenKind::Unknown, TokenKind::Eof]);
    }

    #[test]
    fn spans_cover_exactly_what_was_written() {
        let tokens = lex("let x");
        assert_eq!(tokens[0].span, Span::new(0, 3));
        assert_eq!(tokens[1].span, Span::new(4, 1));
    }
}
