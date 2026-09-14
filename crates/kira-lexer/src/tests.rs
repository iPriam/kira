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
fn lexes_the_compound_assignment_operators() {
    assert_eq!(
        kinds("+= -= *= /= %= &= |= ^= <<= >>="),
        vec![
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::StarEq,
            TokenKind::SlashEq,
            TokenKind::PercentEq,
            TokenKind::AmpEq,
            TokenKind::PipeEq,
            TokenKind::CaretEq,
            TokenKind::LtLtEq,
            TokenKind::GtGtEq,
            TokenKind::Eof
        ]
    );
    // The prefixes are still their own tokens, so `<<` and `<` are unaffected.
    assert_eq!(
        kinds("a << b < c"),
        vec![
            TokenKind::Identifier,
            TokenKind::LtLt,
            TokenKind::Identifier,
            TokenKind::Lt,
            TokenKind::Identifier,
            TokenKind::Eof
        ]
    );
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
        kinds("@Main let var infinity"),
        vec![
            TokenKind::At,
            TokenKind::Identifier,
            TokenKind::Let,
            TokenKind::Var,
            TokenKind::Infinity,
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

/// A backslash before a newline continues the literal, so what would
/// otherwise be an unterminated string is one token and no diagnostic.
#[test]
fn a_backslash_before_a_newline_continues_the_string() {
    let result = lex(SourceId::new(0), "\"first \\\n   second\"");
    assert_eq!(
        result
            .tokens
            .iter()
            .filter(|t| t.kind == TokenKind::StringLiteral)
            .count(),
        1
    );
    assert!(
        result.diagnostics.is_empty(),
        "a continued string is not unterminated: {:?}",
        result.diagnostics
    );
}

/// The continuation produces nothing: neither the newline nor the
/// indentation lining the next line up reaches the message.
#[test]
fn a_continuation_contributes_neither_newline_nor_indent() {
    assert_eq!(
        decode_string_literal("\"first \\\n            second\"").as_deref(),
        Ok("first second")
    );
    // The space belongs to the text before the backslash, so a
    // continuation written without one joins the halves directly.
    assert_eq!(
        decode_string_literal("\"un\\\n            split\"").as_deref(),
        Ok("unsplit")
    );
}

/// A CRLF line ending is the same continuation. Consuming only the `\r`
/// would leave the `\n` to close the literal.
#[test]
fn a_continuation_spans_a_crlf_line_ending() {
    let result = lex(SourceId::new(0), "\"first \\\r\n   second\"");
    assert!(result.diagnostics.is_empty());
    assert_eq!(
        decode_string_literal("\"first \\\r\n   second\"").as_deref(),
        Ok("first second")
    );
}

/// A blank line inside a continuation is still layout.
#[test]
fn a_continuation_swallows_a_blank_line() {
    assert_eq!(
        decode_string_literal("\"first \\\n\n      second\"").as_deref(),
        Ok("first second")
    );
}

/// The continuation does not swallow a string that never closes: a
/// backslash at end of file still leaves the literal unterminated.
#[test]
fn a_backslash_at_end_of_file_is_still_unterminated() {
    let result = lex(SourceId::new(0), "\"open \\");
    assert_eq!(result.diagnostics.len(), 1);
}

#[test]
fn decodes_escapes() {
    assert_eq!(decode_string_literal("\"a\\nb\"").as_deref(), Ok("a\nb"));
    assert_eq!(
        decode_string_literal("\"\\e[2K\"").as_deref(),
        Ok("\x1b[2K")
    );
    assert_eq!(decode_string_literal("\"q\\\"q\"").as_deref(), Ok("q\"q"));
    assert_eq!(decode_string_literal("\"plain\"").as_deref(), Ok("plain"));
}

fn codes(text: &str) -> Vec<String> {
    lex(SourceId::new(0), text)
        .diagnostics
        .iter()
        .filter_map(|d| d.code_text().map(str::to_owned))
        .collect()
}

#[test]
fn a_leading_bom_is_skipped() {
    let text = "\u{feff}function f() {}";
    let result = lex(SourceId::new(0), text);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
    assert_eq!(result.tokens[0].kind, TokenKind::Function);
    assert_eq!(result.tokens[0].span.start, 3);
}

#[test]
fn a_bom_anywhere_else_is_klex006() {
    assert_eq!(codes("let \u{feff}a = 1"), vec!["KLEX006"]);
}

#[test]
fn a_semicolon_is_klex005_and_an_unknown_token() {
    assert_eq!(codes("let a = 1;"), vec!["KLEX005"]);
    assert!(kinds("let a = 1;").contains(&TokenKind::Unknown));
}

#[test]
fn block_comments_nest() {
    let text = "let /* outer /* inner */ still outer */ a = 1";
    assert!(codes(text).is_empty());
    assert_eq!(
        kinds(text),
        vec![
            TokenKind::Let,
            TokenKind::Identifier,
            TokenKind::Equals,
            TokenKind::IntLiteral,
            TokenKind::Eof
        ]
    );
}

#[test]
fn an_unterminated_block_comment_is_klex004() {
    assert_eq!(codes("let a = 1 /* open /* nested */"), vec!["KLEX004"]);
}

#[test]
fn an_unknown_escape_is_fatal_to_the_literal() {
    assert_eq!(codes("\"es\\qcape\""), vec!["KLEX003"]);
    assert!(decode_string_literal("\"es\\qcape\"").is_err());
    assert!(decode_string_literal("\"trailing\\").is_err());
}
