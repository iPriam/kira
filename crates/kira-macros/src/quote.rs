//! `quote { … }` and `#{ … }`: turning the literal Kira inside a quote into a
//! template, and rendering that template back to source.
//!
//! `quote` is a compiler intrinsic rather than a function, so it is lifted out
//! of an `expand` body *before* the body is parsed: each quote becomes a call
//! to a synthetic `__kmac_quote_N`, and each `#{ … }` inside it becomes one of
//! that call's arguments. The splice expressions are then ordinary expressions
//! the evaluator runs, and the surrounding literal source is text this module
//! keeps byte-exact — which is what makes `mxp_#{name}` render as the single
//! identifier `mxp_Foo` while `a + b` keeps its spaces.

use kira_source::{SourceId, Span};
use kira_syntax_model::TokenKind;

use crate::tokens::Lexed;

/// The prefix of the synthetic call a lifted `quote` becomes.
pub(crate) const QUOTE_CALL: &str = "__kmac_quote_";

/// One piece of a quote template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Chunk {
    /// Literal source, exactly as written.
    Text(String),
    /// The value of the splice at this position in the argument list.
    Splice(usize),
}

/// One lifted `quote { … }`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Template {
    /// The literal source and splice holes, in order.
    pub(crate) chunks: Vec<Chunk>,
}

/// Rewrites `body` so every `quote { … }` becomes `__kmac_quote_N(args…)`, and
/// returns the templates those calls render.
pub(crate) fn lift(body: &str) -> (String, Vec<Template>) {
    let mut templates = Vec::new();
    let rewritten = lift_into(body, &mut templates);
    (rewritten, templates)
}

/// Lifts every quote in `text`, appending templates to `templates`.
fn lift_into(text: &str, templates: &mut Vec<Template>) -> String {
    let file = Lexed::new(SourceId::new(0), text);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let mut index = 0usize;
    while index < file.len() {
        if !file.is_word(index, "quote") || file.kind(index + 1) != TokenKind::LBrace {
            index += 1;
            continue;
        }
        let Some(close) = file.match_close(index + 1) else {
            break;
        };
        let inner = file.slice(Span::from_bounds(
            file.span(index + 1).end(),
            file.span(close).start,
        ));
        let (chunks, splices) = split_splices(inner);
        let arguments: Vec<String> = splices
            .iter()
            .map(|splice| lift_into(splice, templates))
            .collect();
        let id = templates.len();
        templates.push(Template { chunks });

        let start = (file.span(index).start as usize).min(text.len());
        let end = (file.span(close).end() as usize).min(text.len());
        out.push_str(text.get(cursor..start).unwrap_or(""));
        out.push_str(QUOTE_CALL);
        out.push_str(&id.to_string());
        out.push('(');
        out.push_str(&arguments.join(", "));
        out.push(')');
        cursor = end;
        index = close + 1;
    }
    out.push_str(text.get(cursor..).unwrap_or(""));
    out
}

/// Splits a quote's literal source at its `#{ … }` splices.
///
/// Returns the chunk list and the splice expressions in the order the chunks
/// refer to them.
fn split_splices(inner: &str) -> (Vec<Chunk>, Vec<String>) {
    let file = Lexed::new(SourceId::new(0), inner);
    let mut chunks = Vec::new();
    let mut splices = Vec::new();
    let mut cursor = 0usize;
    let mut index = 0usize;
    while index < file.len() {
        let is_splice = file.kind(index) == TokenKind::Unknown
            && file.text_at(index) == "#"
            && file.kind(index + 1) == TokenKind::LBrace
            && file.span(index).end() == file.span(index + 1).start;
        if !is_splice {
            index += 1;
            continue;
        }
        let Some(close) = file.match_close(index + 1) else {
            break;
        };
        let start = (file.span(index).start as usize).min(inner.len());
        let end = (file.span(close).end() as usize).min(inner.len());
        if start > cursor {
            chunks.push(Chunk::Text(
                inner.get(cursor..start).unwrap_or("").to_owned(),
            ));
        }
        chunks.push(Chunk::Splice(splices.len()));
        splices.push(
            file.slice(Span::from_bounds(
                file.span(index + 1).end(),
                file.span(close).start,
            ))
            .trim()
            .to_owned(),
        );
        cursor = end;
        index = close + 1;
    }
    if cursor < inner.len() {
        chunks.push(Chunk::Text(inner.get(cursor..).unwrap_or("").to_owned()));
    }
    (chunks, splices)
}

/// The template id a `__kmac_quote_N` callee names, when it is one.
pub(crate) fn template_id(callee: &str) -> Option<usize> {
    callee.strip_prefix(QUOTE_CALL)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quote_becomes_a_call_and_a_template() {
        let (body, templates) = lift("return quote { let x = #{value} }");
        assert_eq!(body, "return __kmac_quote_0(value)");
        assert_eq!(
            templates[0].chunks,
            vec![
                Chunk::Text(" let x = ".to_owned()),
                Chunk::Splice(0),
                Chunk::Text(" ".to_owned()),
            ]
        );
    }

    #[test]
    fn an_empty_quote_lifts_to_an_argumentless_call() {
        let (body, templates) = lift("return quote { }");
        assert_eq!(body, "return __kmac_quote_0()");
        assert_eq!(templates[0].chunks, vec![Chunk::Text(" ".to_owned())]);
    }

    #[test]
    fn adjacent_text_and_splice_keep_their_exact_bytes() {
        let (_, templates) = lift("return quote { mxp_#{name}() }");
        assert_eq!(
            templates[0].chunks,
            vec![
                Chunk::Text(" mxp_".to_owned()),
                Chunk::Splice(0),
                Chunk::Text("() ".to_owned()),
            ]
        );
    }

    #[test]
    fn a_splice_expression_may_hold_a_call_with_commas() {
        let (body, _) = lift("return quote { #{Syntax.join(parts, separator: \", \")} }");
        assert_eq!(
            body,
            "return __kmac_quote_0(Syntax.join(parts, separator: \", \"))"
        );
    }

    #[test]
    fn a_quote_inside_a_splice_is_lifted_too() {
        let (body, templates) = lift("return quote { #{ inner(quote { a }) } }");
        assert_eq!(templates.len(), 2);
        assert!(body.starts_with("return __kmac_quote_1("), "{body}");
        assert!(body.contains("__kmac_quote_0()"), "{body}");
    }

    #[test]
    fn two_quotes_get_separate_templates() {
        let (body, templates) = lift("a(quote { one })\nb(quote { two })");
        assert_eq!(templates.len(), 2);
        assert_eq!(body, "a(__kmac_quote_0())\nb(__kmac_quote_1())");
    }

    #[test]
    fn a_callee_names_its_template() {
        assert_eq!(template_id("__kmac_quote_3"), Some(3));
        assert_eq!(template_id("print"), None);
    }
}
