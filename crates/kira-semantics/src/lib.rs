//! Semantic analyzer: the salsa frontend that turns source text into a typed
//! [`HirProgram`] plus diagnostics.
//!
//! Layer 2 of the Kira package graph.
//!
//! The frontend is built on salsa from the start so the language server and the
//! compiler share one query graph. The single input is the file's text; the
//! tracked queries are [`parsed`] (lex + parse) and [`analyzed`] (name
//! resolution + type checking). Diagnostics are never thrown — they are pushed
//! into the [`DiagnosticAccumulator`], which salsa propagates up the call
//! graph, so a caller collects every diagnostic from one `accumulated` call.

mod analyze;
mod decl;
mod stmt;
mod typeck;

pub use analyze::{Analysis, analyze};

use kira_core::Interner;
use kira_diagnostics::Diagnostic;
use kira_semantics_model::HirProgram;
use kira_source::SourceId;
use kira_syntax_model::SyntaxTree;
use salsa::Accumulator;

/// The fixed source id the v0 single-file frontend uses; the CLI mirrors it in
/// its [`kira_source::SourceMap`] so diagnostic spans render against the file.
pub const FILE_SOURCE_ID: SourceId = SourceId::new(0);

/// The one salsa input: a source file's text and path.
#[salsa::input]
pub struct SourceProgram {
    /// The full source text.
    #[returns(clone)]
    pub text: String,
    /// The path the source was loaded from (for diagnostics).
    #[returns(clone)]
    pub path: String,
}

/// A diagnostic emitted by any frontend query.
///
/// Wrapping [`Diagnostic`] as a salsa accumulator lets every stage report
/// without threading a sink; `query::accumulated::<DiagnosticAccumulator>`
/// gathers them, including those from called queries.
#[salsa::accumulator]
#[derive(Debug, Clone)]
pub struct DiagnosticAccumulator(pub Diagnostic);

/// A parsed program: the syntax tree and the interner backing its symbols.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProgram {
    /// The parsed syntax tree.
    pub tree: SyntaxTree,
    /// The interner holding every identifier symbol in the tree.
    pub interner: Interner,
}

/// Lexes and parses the source, accumulating lexer/parser diagnostics.
#[salsa::tracked(returns(clone))]
pub fn parsed(db: &dyn salsa::Database, source: SourceProgram) -> ParsedProgram {
    let text = source.text(db);
    let result = kira_parser::parse(FILE_SOURCE_ID, &text);
    for diagnostic in result.diagnostics {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    ParsedProgram {
        tree: result.tree,
        interner: result.interner,
    }
}

/// Resolves names and type-checks the program, accumulating diagnostics.
#[salsa::tracked(returns(clone))]
pub fn analyzed(db: &dyn salsa::Database, source: SourceProgram) -> HirProgram {
    let parsed = parsed(db, source);
    let analysis = analyze(FILE_SOURCE_ID, &parsed.tree, &parsed.interner);
    for diagnostic in analysis.diagnostics {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    analysis.program
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_semantics_model::Type;

    fn diagnostics(text: &str) -> Vec<Diagnostic> {
        let db = salsa::DatabaseImpl::new();
        let source = SourceProgram::new(&db, text.to_owned(), "test.kira".to_owned());
        analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
            .into_iter()
            .map(|accumulator| accumulator.0.clone())
            .collect()
    }

    fn codes(text: &str) -> Vec<&'static str> {
        diagnostics(text)
            .into_iter()
            .filter_map(|diagnostic| diagnostic.code)
            .collect()
    }

    #[test]
    fn a_for_loop_binds_its_variable_and_type_checks_its_bounds() {
        assert!(
            diagnostics("@Main function main() { for i in 0..5 { print(i) } return }").is_empty()
        );
        // The loop variable is an `Int`, and visible only inside the body.
        assert_eq!(
            codes("@Main function main() { for i in 0..5 { } print(i) return }"),
            vec!["KSEM060"],
            "the loop variable does not outlive its loop"
        );
    }

    /// A range bound is an `Int`. A `String` bound is reported once, against
    /// the bound itself rather than the loop.
    #[test]
    fn a_non_integer_for_bound_is_reported() {
        assert_eq!(
            codes(r#"@Main function main() { for i in 0.."five" { } return }"#),
            vec!["KSEM043"]
        );
    }

    /// The loop variable is a fresh immutable binding each iteration, so
    /// writing to it is the same error writing to any `let` is.
    #[test]
    fn a_for_loop_variable_cannot_be_assigned() {
        assert_eq!(
            codes("@Main function main() { for i in 0..5 { i = 9 } return }"),
            vec!["KSEM021"]
        );
    }

    /// The cursor and limit the desugar introduces are bound to no name, so a
    /// body is free to declare its own variables without colliding with them.
    #[test]
    fn a_for_body_may_declare_any_name_it_likes() {
        assert!(
            diagnostics(
                "@Main function main() { for i in 0..3 { let cursor = 1 let limit = 2 print(cursor + limit) } return }"
            )
            .is_empty()
        );
    }

    #[test]
    fn a_switch_type_checks_each_label_against_its_subject() {
        assert!(
            diagnostics(
                r#"@Main function main() { var s = "" switch 1 { case 0 { s = "z" } default { s = "d" } } print(s) return }"#
            )
            .is_empty()
        );
        // A label the subject cannot be compared to is reported per arm.
        assert_eq!(
            codes(r#"@Main function main() { switch 1 { case "x" { print(1) } } return }"#),
            vec!["KSEM044"]
        );
    }

    /// Strings and bools are legal subjects: what a `case` may match is
    /// whatever `==` accepts against the subject's type.
    #[test]
    fn a_switch_accepts_every_type_equality_does() {
        for source in [
            r#"@Main function main() { switch "a" { case "a" { print(1) } } return }"#,
            r#"@Main function main() { switch true { case false { print(1) } } return }"#,
            r#"@Main function main() { switch 1.5 { case 1.5 { print(1) } } return }"#,
        ] {
            assert!(diagnostics(source).is_empty(), "{source}");
        }
    }

    /// A `break` in a switch arm acts on the enclosing loop; outside one it has
    /// nothing to break, because a switch is not a loop.
    #[test]
    fn break_in_a_switch_arm_belongs_to_the_enclosing_loop() {
        assert!(
            diagnostics(
                "@Main function main() { for i in 0..3 { switch i { case 1 { break } } } return }"
            )
            .is_empty()
        );
        assert_eq!(
            codes("@Main function main() { switch 1 { case 1 { break } } return }"),
            vec!["KSEM041"],
            "a switch is not a loop, so `break` in one outside a loop is an error"
        );
    }

    /// A switch is not exhaustive-checked and duplicate labels are legal: the
    /// language has no such rule, and inventing one would reject a program the
    /// corpus accepts.
    #[test]
    fn a_switch_needs_no_default_and_may_repeat_a_label() {
        assert!(
            diagnostics("@Main function main() { switch 9 { case 1 { print(1) } } return }")
                .is_empty()
        );
        assert!(
            diagnostics("@Main function main() { switch 1 { case 1 { print(1) } case 1 { print(2) } } return }")
                .is_empty()
        );
    }

    /// A switch satisfies the definite-return check exactly when it has a
    /// `default` *and* every arm returns — with no `default` the chain can fall
    /// out of the bottom, so it proves nothing.
    ///
    /// The desugar gets this rule rather than implementing it: a `default`
    /// becomes the final `else`, and an `if` counts only when both arms do.
    #[test]
    fn a_switch_returns_definitely_only_when_a_default_covers_it() {
        assert!(
            diagnostics(
                "@Main function main() { return } \
                 function f() -> Int { switch 1 { case 1 { return 1 } default { return 0 } } }"
            )
            .is_empty(),
            "a default plus returning arms covers every path"
        );
        assert_eq!(
            codes(
                "@Main function main() { return } \
                 function f() -> Int { switch 1 { case 1 { return 1 } } }"
            ),
            vec!["KSEM033"],
            "without a default the switch can fall through"
        );
        assert_eq!(
            codes(
                "@Main function main() { return } \
                 function f() -> Int { switch 1 { case 1 { print(1) } default { return 0 } } }"
            ),
            vec!["KSEM033"],
            "an arm that does not return leaves a path open"
        );
    }

    #[test]
    fn break_and_continue_outside_a_loop_are_reported() {
        assert_eq!(
            codes("@Main function main() { break return }"),
            vec!["KSEM041"]
        );
        assert_eq!(
            codes("@Main function main() { continue return }"),
            vec!["KSEM042"]
        );
        // Inside an `if` that is itself outside a loop: still no loop.
        assert_eq!(
            codes("@Main function main() { if true { break } return }"),
            vec!["KSEM041"]
        );
    }

    #[test]
    fn break_and_continue_inside_a_loop_are_accepted() {
        assert!(
            diagnostics(
                "@Main function main() { for i in 0..3 { if i > 1 { break } continue } return }"
            )
            .is_empty()
        );
        assert!(diagnostics("@Main function main() { while true { break } return }").is_empty());
    }

    /// A loop does not make a function definitely return: its body may run
    /// zero times, so a `return` inside one cannot be the only one.
    #[test]
    fn a_return_only_inside_a_for_loop_does_not_satisfy_the_return_check() {
        assert_eq!(
            codes(
                "@Main function main() { return } function f() -> Int { for i in 0..3 { return i } }"
            ),
            vec!["KSEM033"]
        );
    }

    #[test]
    fn a_clean_program_has_no_diagnostics() {
        assert!(diagnostics("@Main function main() { print(1) return }").is_empty());
    }

    #[test]
    fn missing_main_is_reported() {
        assert!(codes("function f() { return }").contains(&"KSEM011"));
    }

    #[test]
    fn duplicate_main_is_reported() {
        let text = "@Main function a() { return }\n@Main function b() { return }";
        assert!(codes(text).contains(&"KSEM010"));
    }

    #[test]
    fn undefined_name_is_reported() {
        assert!(codes("@Main function main() { print(x) return }").contains(&"KSEM060"));
    }

    #[test]
    fn wrong_argument_type_is_reported() {
        let text = "function f(n: Int) -> Int { return n }\n@Main function main() { print(f(true)) return }";
        assert!(codes(text).contains(&"KSEM063"));
    }

    #[test]
    fn arity_mismatch_is_reported() {
        let text = "function f(n: Int) -> Int { return n }\n@Main function main() { print(f(1, 2)) return }";
        assert!(codes(text).contains(&"KSEM062"));
    }

    #[test]
    fn assigning_to_let_is_reported() {
        let text = "@Main function main() { let x = 1 x = 2 return }";
        assert!(codes(text).contains(&"KSEM021"));
    }

    #[test]
    fn missing_return_on_some_paths_is_reported() {
        // The review's reproduced hole: only the `n > 100` path returns.
        let text = "function f(n: Int) -> Int { if n > 100 { return 1 } }\n\
                    @Main function main() { print(f(5)) return }";
        assert!(codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn if_else_where_both_arms_return_is_accepted() {
        let text = "function f(n: Int) -> Int { if n > 0 { return 1 } else { return 2 } }\n\
                    @Main function main() { print(f(5)) return }";
        assert!(!codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn else_if_chain_with_full_coverage_is_accepted() {
        let text = "function f(n: Int) -> Int {\n\
                        if n > 0 { return 1 } else if n < 0 { return 2 } else { return 3 }\n\
                    }\n\
                    @Main function main() { print(f(5)) return }";
        assert!(!codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn else_if_chain_missing_final_else_is_reported() {
        let text = "function f(n: Int) -> Int {\n\
                        if n > 0 { return 1 } else if n < 0 { return 2 }\n\
                    }\n\
                    @Main function main() { print(f(5)) return }";
        assert!(codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn while_containing_return_does_not_count_as_definite() {
        // A while body may run zero times, so it can never satisfy the check.
        let text = "function f(n: Int) -> Int { while n > 0 { return n } }\n\
                    @Main function main() { print(f(5)) return }";
        assert!(codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn trailing_return_after_if_is_accepted() {
        let text = "function f(n: Int) -> Int { if n > 100 { return 1 } return 0 }\n\
                    @Main function main() { print(f(5)) return }";
        assert!(!codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn void_functions_are_exempt_from_definite_return() {
        let text = "function f() { print(1) }\n@Main function main() { f() return }";
        assert!(!codes(text).contains(&"KSEM033"));
    }

    #[test]
    fn analyzed_program_records_types_and_main() {
        let db = salsa::DatabaseImpl::new();
        let source = SourceProgram::new(
            &db,
            "@Main function main() { let x = 3 print(x) return }".to_owned(),
            "test.kira".to_owned(),
        );
        let program = analyzed(&db, source);
        assert!(program.main.is_some());
        assert_eq!(program.functions.len(), 1);
        assert_eq!(program.functions[0].locals[0].ty, Type::Int);
    }
}
