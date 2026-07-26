//! Whole-identifier substitution over Kira source text.
//!
//! Both of a macro's name-rewriting jobs are here: binding fragment parameters
//! and hygienic locals inside a template ([`free`]), and the wholesale
//! `Syntax.replaceIdentifier` a wrapper macro uses to monomorphize its template
//! ([`every`]). Both work on token boundaries, so a name inside a string
//! literal or a comment is never touched and `counter` is never found inside
//! `counters`.

use std::collections::HashMap;

use kira_source::SourceId;
use kira_syntax_model::TokenKind;

use crate::tokens::Lexed;

/// Replaces every occurrence of each key, wherever it appears as an identifier.
///
/// This is `Syntax.replaceIdentifier`: a wrapper template's own name has to be
/// rewritten in member positions too (`nativeRecover<State>` is one), so no
/// position is exempt.
pub(crate) fn every(text: &str, replacements: &HashMap<String, String>) -> String {
    rewrite(text, |file, index| {
        replacements.get(file.text_at(index)).cloned()
    })
}

/// Replaces every *free* occurrence of each key: not a member after `.`, and
/// not the label of an argument or a struct-literal field.
///
/// A macro binds fragment parameters and its own hygienic locals, and neither
/// is a member name: rewriting `p.value` because the macro has a parameter
/// called `value` would rename a field the caller owns.
pub(crate) fn free(text: &str, replacements: &HashMap<String, String>) -> String {
    rewrite(text, |file, index| {
        if index > 0 && file.kind(index - 1) == TokenKind::Dot {
            return None;
        }
        if is_label(file, index) {
            return None;
        }
        replacements.get(file.text_at(index)).cloned()
    })
}

/// Whether the identifier at `index` names a parameter or field rather than a
/// value: `f(count: 1)` and `Point { x: 1 }` both write one.
fn is_label(file: &Lexed<'_>, index: usize) -> bool {
    if file.kind(index + 1) != TokenKind::Colon {
        return false;
    }
    index > 0
        && matches!(
            file.kind(index - 1),
            TokenKind::LBrace | TokenKind::LParen | TokenKind::Comma
        )
}

/// Rebuilds `text` with each identifier `chosen` gives a replacement for.
fn rewrite(text: &str, chosen: impl Fn(&Lexed<'_>, usize) -> Option<String>) -> String {
    let file = Lexed::new(SourceId::new(0), text);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for index in 0..file.len() {
        if !file.is_ident(index) {
            continue;
        }
        let Some(replacement) = chosen(&file, index) else {
            continue;
        };
        let span = file.span(index);
        let start = (span.start as usize).min(text.len());
        let end = (span.end() as usize).min(text.len());
        if start < cursor {
            continue;
        }
        out.push_str(text.get(cursor..start).unwrap_or(""));
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(text.get(cursor..).unwrap_or(""));
    out
}

/// Mints the fresh names hygiene needs.
///
/// One counter for a whole expansion run, so two `swap!` calls in one function
/// never share a temporary and nothing a macro introduces can collide with a
/// name the caller wrote — `__kmac_` is not spellable by accident.
#[derive(Debug, Default)]
pub(crate) struct Gensym {
    next: u32,
}

impl Gensym {
    /// Creates a counter starting at zero.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A fresh name derived from `base`.
    pub(crate) fn fresh(&mut self, base: &str) -> String {
        let index = self.next;
        self.next += 1;
        format!("__kmac_{base}_{index}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(from, to)| ((*from).to_owned(), (*to).to_owned()))
            .collect()
    }

    #[test]
    fn a_free_name_is_replaced_and_a_member_is_not() {
        let text = "value + p.value";
        assert_eq!(free(text, &map(&[("value", "t")])), "t + p.value");
    }

    #[test]
    fn a_label_keeps_its_name() {
        let text = "f(value: value)";
        assert_eq!(free(text, &map(&[("value", "t")])), "f(value: t)");
    }

    #[test]
    fn a_prefix_of_a_longer_name_is_not_replaced() {
        assert_eq!(free("values", &map(&[("value", "t")])), "values");
    }

    #[test]
    fn a_string_literal_is_untouched() {
        assert_eq!(
            free("\"value\" + value", &map(&[("value", "t")])),
            "\"value\" + t"
        );
    }

    #[test]
    fn every_occurrence_includes_members() {
        assert_eq!(
            every("State + s.State", &map(&[("State", "Mono")])),
            "Mono + s.Mono"
        );
    }

    #[test]
    fn gensyms_are_unique() {
        let mut gensym = Gensym::new();
        assert_eq!(gensym.fresh("t"), "__kmac_t_0");
        assert_eq!(gensym.fresh("t"), "__kmac_t_1");
    }
}
