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
/// Every binding form the language has, because hygiene is only worth as much
/// as its least-covered one: a form left out is a name the caller can collide
/// with, and the collision is silent. `let`, `var` and a `for` variable are the
/// statement binders; a `match` arm and a `handle` arm each bind a payload; a
/// closure binds its parameters. Each name found here is renamed to a fresh
/// symbol.
fn introduced_names(template: &str, fragments: &[crate::registry::Fragment]) -> Vec<String> {
    let file = Lexed::new(kira_source::SourceId::new(0), template);
    let mut names = Vec::new();
    let bound = |name: String, names: &mut Vec<String>| {
        if fragments.iter().any(|fragment| fragment.name == name) {
            return;
        }
        if !names.contains(&name) {
            names.push(name);
        }
    };
    for index in 0..file.len() {
        match file.kind(index) {
            TokenKind::Let | TokenKind::Var if file.is_ident(index + 1) => {
                bound(file.text_at(index + 1).to_owned(), &mut names);
            }
            TokenKind::For if file.is_ident(index + 1) && file.kind(index + 2) == TokenKind::In => {
                bound(file.text_at(index + 1).to_owned(), &mut names);
            }
            TokenKind::LParen if is_arm_payload(&file, index) => {
                bound(file.text_at(index + 1).to_owned(), &mut names);
            }
            TokenKind::LBrace => {
                for parameter in closure_parameters(&file, index) {
                    bound(parameter, &mut names);
                }
            }
            _ => {}
        }
    }
    names
}

/// Whether the `(` at `open` is an arm's payload binding rather than a call.
///
/// A `match` arm is `Variant(name) ->` and a `handle` arm is `Variant(name) {`;
/// both bind `name` for the arm's body alone, and both are spelled exactly like
/// a one-argument call.
///
/// The arrow tells them apart on its own: nothing else in the language puts an
/// arrow after a parenthesized single identifier, and a function type's `(A) ->
/// B` is preceded by a colon rather than by a name.
///
/// The block does not, because `if ready(flag) { … }` is that shape too. What
/// separates the arm is that it *starts a statement* — an arm is written on its
/// own line, while the condition of an `if` or a `while` follows the keyword on
/// the same one. Renaming a condition's argument would rewrite a name the
/// caller owns, so the block form asks for the line break and the arrow form
/// does not need to.
fn is_arm_payload(file: &Lexed<'_>, open: usize) -> bool {
    if open == 0 || !file.is_ident(open - 1) {
        return false;
    }
    if !file.is_ident(open + 1) || file.kind(open + 2) != TokenKind::RParen {
        return false;
    }
    match file.kind(open + 3) {
        TokenKind::Arrow => true,
        TokenKind::LBrace => file.newline_before(open - 1),
        _ => false,
    }
}

/// The parameters of the closure opening at `open`, or nothing when the `{` is
/// an ordinary block.
///
/// A closure is `{ a, b in … }`, so the parameters are the identifiers between
/// the brace and an `in` that no other token interrupts. Stopping at the first
/// token that is neither an identifier nor a comma is what keeps a block whose
/// first statement happens to mention `in` — a `for` loop — from being read as
/// a parameter list.
fn closure_parameters(file: &Lexed<'_>, open: usize) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut index = open + 1;
    loop {
        if !file.is_ident(index) {
            return Vec::new();
        }
        parameters.push(file.text_at(index).to_owned());
        index += 1;
        match file.kind(index) {
            TokenKind::In => return parameters,
            TokenKind::Comma => index += 1,
            _ => return Vec::new(),
        }
    }
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

    /// A `match` arm binds its payload, so a template's arm may not capture a
    /// fragment that mentions the same name. Without this the caller's `text`
    /// would read the arm's binding instead of the caller's own.
    #[test]
    fn a_match_arm_payload_is_hygienic() {
        let (text, diagnostics) = expand_all(
            "macro describe(value: expr) {\n    expand {\n        match note {\n\
             Tag(text) -> print(value)\n            Blank -> print(0)\n        }\n    }\n}\n\
             function f() {\n    let text = 5\n    describe!(text)\n    return\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(
            text.contains("Tag(__kmac_text_0) -> print((text))"),
            "{text}"
        );
        assert!(text.contains("let text = 5"), "{text}");
    }

    /// A `handle` arm binds its payload the same way, and is written with a
    /// block rather than an arrow.
    #[test]
    fn a_handle_arm_payload_is_hygienic() {
        let (text, diagnostics) = expand_all(
            "macro orElse(fallback: expr) {\n    expand {\n        attempt {\n\
             let v = try read()\n            print(v)\n        } handle {\n\
             Failed(reason) { print(fallback) }\n        }\n    }\n}\n\
             function f() {\n    let reason = 7\n    orElse!(reason)\n    return\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(
            text.contains("Failed(__kmac_reason_1) { print((reason)) }"),
            "{text}"
        );
    }

    /// A closure's parameters are bindings too, so a template that writes one
    /// may not capture a fragment naming the same thing.
    #[test]
    fn a_closure_parameter_is_hygienic() {
        let (text, diagnostics) = expand_all(
            "macro applyTwice(seed: expr) {\n    expand {\n\
             let step: (Int) -> Int = { n in n + seed }\n        print(step(1))\n    }\n}\n\
             function f() {\n    let n = 3\n    applyTwice!(n)\n    return\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(
            text.contains("{ __kmac_n_1 in __kmac_n_1 + (n) }"),
            "{text}"
        );
    }

    /// The condition of an `if` is a call, not an arm, even though it is
    /// spelled the same. Renaming its argument would rewrite a name the caller
    /// owns and the expansion would stop compiling.
    #[test]
    fn an_if_condition_is_not_mistaken_for_an_arm() {
        let (text, diagnostics) = expand_all(
            "macro guard(flag: expr) {\n    expand {\n        if ready(flag) {\n\
             print(1)\n        }\n    }\n}\n\
             function f() {\n    let flag = true\n    guard!(flag)\n    return\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(text.contains("if ready(flag)"), "{text}");
        assert!(!text.contains("__kmac_flag"), "{text}");
    }

    /// A block whose first statement is a `for` loop is a block, not a closure
    /// whose parameters happen to be followed by `in`.
    #[test]
    fn a_for_loop_block_is_not_a_closure_parameter_list() {
        let (text, diagnostics) = expand_all(
            "macro total(items: expr) {\n    expand {\n        var sum = 0\n\
             for entry in items {\n            sum = sum + entry\n        }\n\
             print(sum)\n    }\n}\n\
             function f() {\n    let entry = 1\n    total!(entry)\n    return\n}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert!(text.contains("for __kmac_entry_1 in (entry)"), "{text}");
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
            diagnostics.iter().any(|d| d.has_code("KMAC005")),
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
            diagnostics.iter().any(|d| d.has_code("KMAC004")),
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
            diagnostics.iter().any(|d| d.has_code("KMAC002")),
            "{diagnostics:?}"
        );
    }
}
