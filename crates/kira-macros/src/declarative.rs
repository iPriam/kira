//! Expanding a declarative `macro`: fragment binding, single evaluation, and
//! hygiene.
//!
//! The two rules that make these compose with Kira's affine ownership instead
//! of fighting it live here:
//!
//! * an `expr` fragment is evaluated **exactly once** — an argument that could
//!   have an effect is bound to a hygienic temporary hoisted ahead of the
//!   statement, and that temporary is what the template sees at every
//!   occurrence;
//! * every name the template introduces that is not a fragment parameter is
//!   renamed to a fresh one, so a macro can neither shadow a caller's binding
//!   nor be shadowed by it.
//!
//! Ownership itself is untouched. A template that consumes an owned fragment
//! twice is an ordinary move error, exactly as the same code written by hand
//! would be.

use std::collections::HashMap;

use kira_syntax_model::TokenKind;

use crate::diagnostics::{self, Reporter};
use crate::invoke::{Invocation, Position};
use crate::probe;
use crate::registry::{Declarative, FragmentKind};
use crate::rename::{self, Gensym};
use crate::tokens::Lexed;

/// What one expanded call site contributes to the file.
#[derive(Debug)]
pub(crate) struct Expanded {
    /// Statements hoisted ahead of the enclosing statement, if any.
    pub(crate) hoist: Option<String>,
    /// The text replacing the `name!(…)` call itself.
    pub(crate) replacement: String,
}

/// Expands one call of `declared`, or reports why it cannot be expanded.
pub(crate) fn expand(
    declared: &Declarative,
    call: &Invocation,
    file: &Lexed<'_>,
    gensym: &mut Gensym,
    reporter: &mut Reporter,
) -> Option<Expanded> {
    if call.arguments.len() != declared.fragments.len() {
        reporter.error(
            file.source,
            call.span,
            diagnostics::ARGUMENT_COUNT,
            format!(
                "macro `{}` takes {} fragment(s), but {} were passed",
                declared.name,
                declared.fragments.len(),
                call.arguments.len()
            ),
        );
        return None;
    }

    let mut bindings: HashMap<String, String> = HashMap::new();
    let mut hoists: Vec<String> = Vec::new();
    for (fragment, span) in declared.fragments.iter().zip(&call.arguments) {
        let argument = file.slice(*span).trim();
        match fragment.kind {
            FragmentKind::Expr => {
                if !probe::is_expression(argument) {
                    reporter.error(
                        file.source,
                        *span,
                        diagnostics::FRAGMENT_KIND,
                        format!(
                            "macro `{}` declares `{}: expr`, so this argument must be a single \
                             expression",
                            declared.name, fragment.name
                        ),
                    );
                    return None;
                }
                if evaluates_once_when_repeated(argument) {
                    bindings.insert(fragment.name.clone(), format!("({argument})"));
                } else {
                    let temporary = gensym.fresh(&fragment.name);
                    hoists.push(format!("let {temporary} = {argument}"));
                    bindings.insert(fragment.name.clone(), temporary);
                }
            }
            FragmentKind::Place => {
                if !probe::is_place(argument) {
                    reporter.error(
                        file.source,
                        *span,
                        diagnostics::PLACE_NOT_ASSIGNABLE,
                        format!(
                            "macro `{}` declares `{}: place`, so this argument must be assignable \
                             — a variable, a field, or an index target",
                            declared.name, fragment.name
                        ),
                    );
                    return None;
                }
                bindings.insert(fragment.name.clone(), argument.to_owned());
            }
        }
    }

    for introduced in introduced_names(&declared.template, &declared.fragments) {
        let fresh = gensym.fresh(&introduced);
        bindings.insert(introduced, fresh);
    }

    let body = rename::free(&declared.template, &bindings);
    let trimmed = body.trim();

    match call.position {
        Position::Expression => {
            if !probe::is_expression(&without_calls(trimmed)) {
                reporter.error(
                    file.source,
                    call.span,
                    diagnostics::STATEMENT_ONLY,
                    format!(
                        "macro `{}` expands to statements, so it cannot be used where a value is \
                         expected",
                        declared.name
                    ),
                );
                return None;
            }
            Some(Expanded {
                hoist: hoist_text(&hoists),
                replacement: format!("({trimmed})"),
            })
        }
        Position::Statement => {
            let mut replacement = String::new();
            for statement in &hoists {
                replacement.push_str(statement);
                replacement.push('\n');
            }
            replacement.push_str(trimmed);
            Some(Expanded {
                hoist: None,
                replacement,
            })
        }
        Position::Declaration => {
            reporter.error(
                file.source,
                call.span,
                diagnostics::NOT_STATEMENTS,
                format!(
                    "macro `{}` expands to statements, so it cannot be written at file scope",
                    declared.name
                ),
            );
            None
        }
    }
}

/// The hoisted bindings as one block of statements, or `None` when there are
/// none.
fn hoist_text(hoists: &[String]) -> Option<String> {
    if hoists.is_empty() {
        return None;
    }
    let mut text = String::new();
    for statement in hoists {
        text.push_str(statement);
        text.push('\n');
    }
    Some(text)
}

/// `text` with every `name!(…)` call replaced by a placeholder name.
///
/// A template may itself invoke a macro, and the round that expands the outer
/// one runs before the round that expands the inner. Asking the parser whether
/// unexpanded macro syntax is an expression would answer no, so the shape
/// question is asked of the *shape* — a call site stands in as one name.
fn without_calls(text: &str) -> String {
    let file = Lexed::new(kira_source::SourceId::new(0), text);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for call in crate::invoke::find(&file) {
        let start = (call.span.start as usize).min(text.len());
        let end = (call.span.end() as usize).min(text.len());
        if start < cursor {
            continue;
        }
        out.push_str(text.get(cursor..start).unwrap_or(""));
        out.push_str("__kmac_hole");
        cursor = end;
    }
    out.push_str(text.get(cursor..).unwrap_or(""));
    out
}

/// Whether substituting `argument` at several occurrences still evaluates it
/// once.
///
/// A call is the only expression form that can *do* something, and it is the
/// only one written with `(`. Everything else — a literal, a name, a field
/// path, an index — reads the same value however many times the template names
/// it, so binding it to a temporary would only add noise. Anything containing a
/// call is hoisted.
fn evaluates_once_when_repeated(argument: &str) -> bool {
    !argument.contains('(')
}

/// The names a template binds that are not fragment parameters.
///
/// `let`, `var`, and a `for` loop variable are the ways a template introduces a
/// name; each one found here is renamed to a fresh symbol, which is the whole
/// of hygiene.
fn introduced_names(template: &str, fragments: &[crate::registry::Fragment]) -> Vec<String> {
    let file = Lexed::new(kira_source::SourceId::new(0), template);
    let mut names = Vec::new();
    for index in 0..file.len() {
        let binds = match file.kind(index) {
            TokenKind::Let | TokenKind::Var => file.is_ident(index + 1),
            TokenKind::For => file.is_ident(index + 1) && file.kind(index + 2) == TokenKind::In,
            _ => false,
        };
        if !binds {
            continue;
        }
        let name = file.text_at(index + 1).to_owned();
        if fragments.iter().any(|fragment| fragment.name == name) {
            continue;
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invoke;
    use crate::registry;
    use kira_source::SourceId;

    /// Expands every call in `program` against the macros `program` declares.
    fn expand_all(program: &str) -> (String, Vec<kira_diagnostics::Diagnostic>) {
        let files = [Lexed::new(SourceId::new(0), program)];
        let mut reporter = Reporter::new();
        let mut registry = registry::Registry::default();
        registry.absorb(&registry::collect_file(&files[0], &mut reporter));
        let file = &files[0];
        let mut gensym = Gensym::new();
        let mut buffer = crate::edits::EditBuffer::new();
        for call in invoke::find(file) {
            let Some(declared) = registry.declarative(&call.name) else {
                continue;
            };
            if let Some(expanded) = expand(declared, &call, file, &mut gensym, &mut reporter) {
                if let Some(hoist) = expanded.hoist {
                    buffer.insert(call.statement_start, hoist);
                }
                buffer.replace(call.span, expanded.replacement);
            }
        }
        (buffer.apply(program).text, reporter.into_diagnostics())
    }

    #[test]
    fn an_expression_macro_substitutes_its_fragment() {
        let (text, diagnostics) = expand_all(
            "macro square(value: expr) { expand { value * value } }\n\
             function f() -> Int {\n    return square!(6)\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(text.contains("return ((6) * (6))"), "{text}");
    }

    #[test]
    fn an_effectful_argument_is_evaluated_once() {
        let (text, diagnostics) = expand_all(
            "macro square(value: expr) { expand { value * value } }\n\
             function f() -> Int {\n    return square!(build())\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(text.contains("let __kmac_value_0 = build()"), "{text}");
        assert!(
            text.contains("return (__kmac_value_0 * __kmac_value_0)"),
            "{text}"
        );
    }

    #[test]
    fn a_statement_macro_is_hygienic() {
        let (text, diagnostics) = expand_all(
            "macro swap(a: place, b: place) {\n    expand {\n        let temporary = a\n\
             a = b\n        b = temporary\n    }\n}\n\
             function f() {\n    var x = 1\n    var y = 2\n    swap!(x, y)\n    let temporary = 3\n    return\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(text.contains("let __kmac_temporary_0 = x"), "{text}");
        assert!(text.contains("x = y"), "{text}");
        assert!(text.contains("y = __kmac_temporary_0"), "{text}");
        assert!(text.contains("let temporary = 3"), "{text}");
    }

    #[test]
    fn two_calls_never_share_a_temporary() {
        let (text, _) = expand_all(
            "macro swap(a: place, b: place) { expand { let t = a\na = b\nb = t } }\n\
             function f() {\n    var x = 1\n    var y = 2\n    swap!(x, y)\n    swap!(x, y)\n    return\n}\n",
        );
        assert!(text.contains("__kmac_t_0"), "{text}");
        assert!(text.contains("__kmac_t_1"), "{text}");
    }

    #[test]
    fn a_statement_only_macro_in_expression_position_is_refused() {
        let (_, diagnostics) = expand_all(
            "macro swap(a: place, b: place) { expand { let t = a\na = b\nb = t } }\n\
             function f() -> Int {\n    var x = 1\n    var y = 2\n    return swap!(x, y)\n}\n",
        );
        assert!(
            diagnostics.iter().any(|d| d.code == Some("KMAC005")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_non_assignable_place_argument_is_refused() {
        let (_, diagnostics) = expand_all(
            "macro swap(a: place, b: place) { expand { let t = a\na = b\nb = t } }\n\
             function f() {\n    var y = 2\n    swap!(1, y)\n    return\n}\n",
        );
        assert!(
            diagnostics.iter().any(|d| d.code == Some("KMAC004")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn a_wrong_fragment_count_is_refused() {
        let (_, diagnostics) = expand_all(
            "macro square(value: expr) { expand { value * value } }\n\
             function f() -> Int {\n    return square!(1, 2)\n}\n",
        );
        assert!(
            diagnostics.iter().any(|d| d.code == Some("KMAC002")),
            "{diagnostics:?}"
        );
    }
}
