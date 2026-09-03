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

/// What `lift` could not lift.
///
/// A quote or splice the brace matcher never finds the end of. Reported rather
/// than skipped: the old behavior stopped lifting at the first one, so a
/// single unbalanced `quote { … }` left every later quote in the body as raw
/// text and the parser reported each surviving `#{` as an unexpected character
/// far from the brace that was never closed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiftError {
    /// Byte offset of the unclosed opener in the lifted text.
    pub offset: usize,
    /// Whether the opener was a `quote { … }` or a `#{ … }` splice.
    pub unclosed_quote: bool,
}

impl LiftError {
    /// What went wrong, in the words the diagnostic carries.
    pub(crate) fn message(&self) -> &'static str {
        if self.unclosed_quote {
            "a `quote { … }` opened here never closes"
        } else {
            "a `#{ … }` splice opened here never closes"
        }
    }
}

/// Rewrites `body` so every `quote { … }` becomes `__kmac_quote_N(args…)`, and
/// returns the templates those calls render along with what could not lift.
///
/// A body with lift errors still returns its templates: the caller reports the
/// errors and drops the body, but nothing after the first failure is left
/// unexamined to surprise a later stage.
pub(crate) fn lift(body: &str) -> (String, Vec<Template>, Vec<LiftError>) {
    let mut templates = Vec::new();
    let mut errors = Vec::new();
    let rewritten = lift_into(body, 0, &mut templates, &mut errors);
    (rewritten, templates, errors)
}

/// Lifts every quote in `text`, appending templates to `templates`.
///
/// `base` is `text`'s byte offset in the body `lift` was handed, so an error
/// deep inside a nested splice still points at the opener the author wrote.
fn lift_into(
    text: &str,
    base: usize,
    templates: &mut Vec<Template>,
    errors: &mut Vec<LiftError>,
) -> String {
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
            errors.push(LiftError {
                offset: base + file.span(index + 1).start as usize,
                unclosed_quote: true,
            });
            // Past the opener rather than out of the loop: one unbalanced
            // quote must not silence every quote after it.
            index += 2;
            continue;
        };
        let inner = file.slice(Span::from_bounds(
            file.span(index + 1).end(),
            file.span(close).start,
        ));
        let inner_base = base + file.span(index + 1).end() as usize;
        let (chunks, splices) = split_splices(inner, inner_base, errors);
        let arguments: Vec<String> = splices
            .iter()
            .map(|(splice, splice_base)| lift_into(splice, *splice_base, templates, errors))
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
/// Returns the chunk list and the splice expressions — each with its byte
/// offset in the body `lift` was handed — in the order the chunks refer to
/// them. An unclosed splice is reported and skipped rather than ending the
/// scan, for the same reason an unclosed quote is.
fn split_splices(
    inner: &str,
    base: usize,
    errors: &mut Vec<LiftError>,
) -> (Vec<Chunk>, Vec<(String, usize)>) {
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
            errors.push(LiftError {
                offset: base + file.span(index).start as usize,
                unclosed_quote: false,
            });
            index += 2;
            continue;
        };
        let start = (file.span(index).start as usize).min(inner.len());
        let end = (file.span(close).end() as usize).min(inner.len());
        if start > cursor {
            chunks.push(Chunk::Text(
                inner.get(cursor..start).unwrap_or("").to_owned(),
            ));
        }
        chunks.push(Chunk::Splice(splices.len()));
        splices.push((
            file.slice(Span::from_bounds(
                file.span(index + 1).end(),
                file.span(close).start,
            ))
            .trim()
            .to_owned(),
            base + file.span(index + 1).end() as usize,
        ));
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
        let (body, templates, errors) = lift("return quote { let x = #{value} }");
        assert!(errors.is_empty());
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
        let (body, templates, errors) = lift("return quote { }");
        assert!(errors.is_empty());
        assert_eq!(body, "return __kmac_quote_0()");
        assert_eq!(templates[0].chunks, vec![Chunk::Text(" ".to_owned())]);
    }

    #[test]
    fn adjacent_text_and_splice_keep_their_exact_bytes() {
        let (_, templates, errors) = lift("return quote { mxp_#{name}() }");
        assert!(errors.is_empty());
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
        let (body, _, errors) = lift("return quote { #{Syntax.join(parts, separator: \", \")} }");
        assert!(errors.is_empty());
        assert_eq!(
            body,
            "return __kmac_quote_0(Syntax.join(parts, separator: \", \"))"
        );
    }

    #[test]
    fn a_quote_inside_a_splice_is_lifted_too() {
        let (body, templates, errors) = lift("return quote { #{ inner(quote { a }) } }");
        assert!(errors.is_empty());
        assert_eq!(templates.len(), 2);
        assert!(body.starts_with("return __kmac_quote_1("), "{body}");
        assert!(body.contains("__kmac_quote_0()"), "{body}");
    }

    #[test]
    fn two_quotes_get_separate_templates() {
        let (body, templates, errors) = lift("a(quote { one })\nb(quote { two })");
        assert!(errors.is_empty());
        assert_eq!(templates.len(), 2);
        assert_eq!(body, "a(__kmac_quote_0())\nb(__kmac_quote_1())");
    }

    #[test]
    fn a_callee_names_its_template() {
        assert_eq!(template_id("__kmac_quote_3"), Some(3));
        assert_eq!(template_id("print"), None);
    }

    /// An unclosed quote is reported with its offset rather than silently
    /// ending the lift: the old behavior left every later quote as raw text,
    /// and the parser reported each surviving `#{` far from the brace that
    /// never closed.
    #[test]
    fn an_unclosed_quote_is_reported_and_lifting_continues_past_it() {
        let (body, templates, errors) =
            lift("return quote { a }\nreturn quote { b\nreturn quote { c }");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].unclosed_quote);
        assert_eq!(templates.len(), 2);
        assert!(body.contains("__kmac_quote_0()"), "{body}");
        assert!(body.contains("__kmac_quote_1()"), "{body}");
    }

    /// The two failure kinds name what never closed. (An unclosed splice
    /// inside a balanced quote cannot reach the lifter — the quote's own match
    /// guarantees every inner opener a close — so this pins the words, not a
    /// path through `lift`.)
    #[test]
    fn lift_errors_name_what_never_closed() {
        assert_eq!(
            LiftError { offset: 0, unclosed_quote: true }.message(),
            "a `quote { … }` opened here never closes"
        );
        assert_eq!(
            LiftError { offset: 0, unclosed_quote: false }.message(),
            "a `#{ … }` splice opened here never closes"
        );
    }
}
