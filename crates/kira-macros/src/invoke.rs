//! Locating `name!(args)` call sites and deciding what position each one is in.
//!
//! `!` immediately followed by `(` after an identifier is the one shape a macro
//! call wears and nothing else in Kira wears it: `!=` is a single token and a
//! prefix `!` never follows a name. So a call site is found without parsing,
//! and *which* position it is in — a declaration, a statement, or an expression
//! — is the only judgement this module makes.

use kira_source::Span;
use kira_syntax_model::TokenKind;

use crate::tokens::Lexed;

/// Where a `name!(…)` call site sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    /// At file scope, where its expansion contributes declarations.
    Declaration,
    /// As a whole statement inside a body.
    Statement,
    /// Inside a larger expression, whose value it becomes.
    Expression,
}

/// One located `name!(args)` call site.
#[derive(Debug, Clone)]
pub(crate) struct Invocation {
    /// The macro's name as written.
    pub(crate) name: String,
    /// Span of the whole `name!(args)`, name through closing paren.
    pub(crate) span: Span,
    /// Span of just the name, for diagnostics that point at the macro.
    pub(crate) name_span: Span,
    /// Where the call sits.
    pub(crate) position: Position,
    /// The argument spans, in written order.
    pub(crate) arguments: Vec<Span>,
    /// The byte offset the enclosing statement starts at.
    ///
    /// Where an expression-position expansion hoists the single-evaluation
    /// temporaries it needs. Equal to the call's own start in statement and
    /// declaration position, where nothing has to be hoisted past anything.
    pub(crate) statement_start: u32,
}

/// Every `name!(…)` call site in `file`, in source order.
pub(crate) fn find(file: &Lexed<'_>) -> Vec<Invocation> {
    let mut found = Vec::new();
    let mut depth = 0usize;
    let mut index = 0usize;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof => break,
            TokenKind::LBrace => depth += 1,
            TokenKind::RBrace => depth = depth.saturating_sub(1),
            _ => {}
        }
        if file.is_ident(index)
            && file.kind(index + 1) == TokenKind::Bang
            && file.kind(index + 2) == TokenKind::LParen
            && adjacent(file, index)
            && let Some(close) = file.match_close(index + 2)
        {
            {
                let position = if depth == 0 {
                    Position::Declaration
                } else if starts_a_statement(file, index) {
                    Position::Statement
                } else {
                    Position::Expression
                };
                let statement_start = match position {
                    Position::Expression => file.span(statement_start(file, index)).start,
                    _ => file.span(index).start,
                };
                found.push(Invocation {
                    name: file.text_at(index).to_owned(),
                    span: file.span_of(index, close),
                    name_span: file.span(index),
                    position,
                    arguments: file
                        .split_group(index + 2, close)
                        .into_iter()
                        .map(|(first, last)| file.span_of(first, last))
                        .collect(),
                    statement_start,
                });
            }
        }
        index += 1;
    }
    found
}

/// The calls in `found` that contain no other call.
///
/// A macro argument may itself be a macro call, and the inner one has to expand
/// first — `is_expression` would otherwise be asked whether unexpanded macro
/// syntax is an expression, and it is not. Each round expands only the
/// innermost calls; the round after sees the outer one with ordinary Kira in
/// its arguments.
pub(crate) fn innermost(found: &[Invocation]) -> Vec<Invocation> {
    found
        .iter()
        .filter(|call| {
            !found.iter().any(|other| {
                other.span != call.span
                    && call.span.start <= other.span.start
                    && other.span.end() <= call.span.end()
            })
        })
        .cloned()
        .collect()
}

/// Whether `name`, `!`, and `(` are written with nothing between them.
///
/// `a ! (b)` is not a macro call, and neither is `a !(b)` written across a line
/// break. Requiring adjacency keeps the one shape unambiguous.
fn adjacent(file: &Lexed<'_>, name: usize) -> bool {
    file.span(name).end() == file.span(name + 1).start
        && file.span(name + 1).end() == file.span(name + 2).start
}

/// Whether a statement begins at `index`.
///
/// Kira separates statements with `;` or a newline, so a call that opens a
/// line, follows a `;`, or opens a block is the whole statement; anything else
/// is part of a larger expression.
fn starts_a_statement(file: &Lexed<'_>, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    if file.newline_before(index) {
        return true;
    }
    matches!(
        file.kind(index - 1),
        TokenKind::LBrace | TokenKind::RBrace | TokenKind::Semicolon
    )
}

/// The index of the first token of the statement containing the call at
/// `index`.
///
/// Walks outwards, stepping over balanced groups so a call nested in an
/// argument list or an index still hoists past the whole statement rather than
/// into the middle of it.
fn statement_start(file: &Lexed<'_>, index: usize) -> usize {
    let mut at = index;
    while at > 0 {
        if file.newline_before(at) {
            return at;
        }
        let previous = at - 1;
        match file.kind(previous) {
            TokenKind::LBrace | TokenKind::RBrace | TokenKind::Semicolon => return at,
            TokenKind::RParen | TokenKind::RBracket => match file.match_open(previous) {
                Some(open) => at = open,
                None => return at,
            },
            _ => at = previous,
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_source::SourceId;

    fn find_in(text: &str) -> (Lexed<'_>, Vec<Invocation>) {
        let file = Lexed::new(SourceId::new(0), text);
        let found = find(&file);
        (file, found)
    }

    #[test]
    fn a_top_level_call_is_a_declaration() {
        let (_, found) = find_in("bits!(Read, Write)\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].position, Position::Declaration);
        assert_eq!(found[0].arguments.len(), 2);
    }

    #[test]
    fn a_call_after_a_newline_is_a_statement() {
        let (_, found) = find_in("function f() {\n    var r = 0\n    assign!()\n    return r\n}\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].position, Position::Statement);
    }

    #[test]
    fn a_call_after_return_is_an_expression() {
        let (_, found) = find_in("function f() {\n    return square!(6)\n}\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].position, Position::Expression);
    }

    #[test]
    fn an_expression_call_hoists_past_the_whole_statement() {
        let text = "function f() {\n    let r = square!(3) + square!(4)\n    return r\n}\n";
        let (file, found) = find_in(text);
        assert_eq!(found.len(), 2);
        let let_offset = text.find("let r").expect("the let statement") as u32;
        for call in &found {
            assert_eq!(call.statement_start, let_offset, "{:?}", file.text);
        }
    }

    #[test]
    fn a_call_nested_in_an_argument_list_hoists_past_the_statement() {
        let text = "function f() {\n    print(square!(3))\n    return\n}\n";
        let (_, found) = find_in(text);
        assert_eq!(found[0].position, Position::Expression);
        assert_eq!(
            found[0].statement_start,
            text.find("print").expect("the print call") as u32
        );
    }

    #[test]
    fn a_spaced_bang_is_not_a_call() {
        let (_, found) = find_in("function f() { return a ! (b) }");
        assert!(found.is_empty());
    }

    #[test]
    fn only_the_inner_call_of_a_nested_pair_is_innermost() {
        let (_, found) = find_in("function f() -> Int {\n    return outer!(inner!(1))\n}\n");
        assert_eq!(found.len(), 2);
        let inner = innermost(&found);
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name, "inner");
    }

    #[test]
    fn an_empty_argument_list_has_no_arguments() {
        let (_, found) = find_in("function f() {\n    thing!()\n    return\n}\n");
        assert!(found[0].arguments.is_empty());
    }
}
