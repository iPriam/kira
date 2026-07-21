//! Semantic-analysis tests: the diagnostics `kirac check` reports, driven
//! through the same salsa `analyzed` query the CLI and the LSP use.

mod aliases;
mod arrays;
mod attempts;
mod calls;
mod classes;
mod closures;
mod constructs;
mod definitions;
mod enums;
mod exports;
mod ffi_types;
mod foreign;
mod generics;
mod imports;
mod libraries;
mod matches;
mod operators;
mod widths;

use super::*;
use kira_semantics_model::Type;

fn diagnostics(text: &str) -> Vec<Diagnostic> {
    module_diagnostics(text, &[])
}

/// The diagnostics of a program built from an entry file plus named modules.
///
/// The modules are handed in directly, which is what module loading looks like
/// from this crate's side: resolving `import support` to a file is the CLI's
/// job and needs a disk, and this crate has none.
fn module_diagnostics(text: &str, modules: &[(&str, &str)]) -> Vec<Diagnostic> {
    let db = salsa::DatabaseImpl::new();
    let modules: Vec<ModuleSource> = modules
        .iter()
        .map(|&(module, text)| ModuleSource {
            module: module.to_owned(),
            path: format!("{module}.kira"),
            text: text.to_owned(),
        })
        .collect();
    let source = SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), modules);
    analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0.clone())
        .collect()
}

/// The diagnostics of a program analyzed as a library rather than an
/// application.
fn library_diagnostics(text: &str) -> Vec<Diagnostic> {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(
        &db,
        text.to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        BuildKind::Library,
    );
    analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0.clone())
        .collect()
}

/// The diagnostic codes of a library, in order.
fn library_codes(text: &str) -> Vec<&'static str> {
    library_diagnostics(text)
        .into_iter()
        .filter_map(|diagnostic| diagnostic.code)
        .collect()
}

/// The diagnostic codes of a multi-module program, in order.
fn module_codes(text: &str, modules: &[(&str, &str)]) -> Vec<&'static str> {
    module_diagnostics(text, modules)
        .into_iter()
        .filter_map(|diagnostic| diagnostic.code)
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
    assert!(diagnostics("@Main function main() { for i in 0..5 { print(i) } return }").is_empty());
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
        diagnostics("@Main function main() { switch 9 { case 1 { print(1) } } return }").is_empty()
    );
    assert!(
        diagnostics(
            "@Main function main() { switch 1 { case 1 { print(1) } case 1 { print(2) } } return }"
        )
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
    let text =
        "function f(n: Int) -> Int { return n }\n@Main function main() { print(f(true)) return }";
    assert!(codes(text).contains(&"KSEM063"));
}

#[test]
fn arity_mismatch_is_reported() {
    let text =
        "function f(n: Int) -> Int { return n }\n@Main function main() { print(f(1, 2)) return }";
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

// ----- ownership ----------------------------------------------------
//
// Each case below is a program the oracle's own fail-corpus or harness
// pins, ported to this subset. They are grouped here rather than in
// `ownership.rs` so one `codes()` harness serves them all.

/// A struct is not trivially copyable, so handing a named one to a
/// consuming parameter must say `move`.
///
/// The oracle's `FsbOwnershipMissingMoveNamedNontrivial`.
#[test]
fn passing_a_named_struct_to_an_owned_parameter_requires_move() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) { return }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } consume(mesh) return }";
    assert_eq!(codes(text), vec!["KSEM108"]);
    // …and writing `move` is what makes it legal.
    let moved = "struct Mesh { let id: Int }\n\
                     function consume(mesh: Mesh) { return }\n\
                     @Main function main() { var mesh = Mesh { id: 1 } consume(move mesh) return }";
    assert!(diagnostics(moved).is_empty());
}

/// A `String` owns its bytes, so it is not trivially copyable either.
#[test]
fn passing_a_named_string_to_an_owned_parameter_requires_move() {
    let text = "function consume(s: String) { return }\n\
                    @Main function main() { let s = \"hi\" consume(s) return }";
    assert_eq!(codes(text), vec!["KSEM108"]);
}

/// A temporary has no binding to consume, so it needs no `move` — this is
/// why the rule is stated over *named* values.
#[test]
fn a_temporary_argument_never_needs_move() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) { return }\n\
                    @Main function main() { consume(Mesh { id: 1 }) return }";
    assert!(diagnostics(text).is_empty());
}

/// A scalar is trivially copyable, so it passes bare.
#[test]
fn a_scalar_argument_never_needs_move() {
    let text = "function consume(n: Int) { return }\n\
                    @Main function main() { let n = 1 consume(n) return }";
    assert!(diagnostics(text).is_empty());
}

/// The oracle's `FscOwnershipUseAfterMove`: reading a moved local is
/// KSEM107, the first of its three messages.
#[test]
fn using_a_moved_local_is_rejected() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 3 } \
                    print(consume(move mesh)) print(inspect(mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM107"]);
}

/// The oracle's `FsbOwnershipMoveTwice`.
#[test]
fn moving_the_same_local_twice_is_rejected() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) { return }\n\
                    @Main function main() { var mesh = Mesh { id: 5 } \
                    consume(move mesh) consume(move mesh) return }";
    assert_eq!(codes(text), vec!["KSEM110"]);
}

/// The oracle's `FsbOwnershipMoveBorrowedParam`: a borrow is not the
/// callee's to give away.
#[test]
fn a_borrowed_parameter_cannot_be_moved_onward() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) { return }\n\
                    function bad(mesh: borrow Mesh) { consume(move mesh) return }\n\
                    @Main function main() { let mesh = Mesh { id: 9 } bad(mesh) return }";
    assert_eq!(codes(text), vec!["KSEM111"]);
}

/// A `borrow` parameter does not take ownership, so `move` at the call
/// site is a contradiction rather than a redundancy.
#[test]
fn moving_into_a_borrow_parameter_is_rejected() {
    let text = "struct Mesh { let id: Int }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { let mesh = Mesh { id: 1 } print(inspect(move mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM114"]);
}

/// A `copy` parameter does not consume its source, so `move` is likewise
/// a contradiction.
#[test]
fn moving_into_a_copy_parameter_is_rejected() {
    let text = "function twice(n: copy Int) -> Int { return n + n }\n\
                    @Main function main() { let n = 2 print(twice(move n)) return }";
    assert_eq!(codes(text), vec!["KSEM115"]);
}

/// Passing a named non-trivial value to a `copy` parameter must say
/// `copy` — which then hits KSEM116, because there is no clone.
#[test]
fn a_copy_parameter_wants_an_explicit_copy_of_a_non_trivial_value() {
    let text = "struct Mesh { let id: Int }\n\
                    function duplicate(mesh: copy Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { let mesh = Mesh { id: 4 } print(duplicate(mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM113"]);
}

/// The oracle's `FsbOwnershipCopyNontrivialNotImplemented`. Kira reserves
/// `copy` but has no clone semantics, so writing it on a struct is an
/// error rather than a deep copy invented here.
#[test]
fn copying_a_non_trivial_value_is_not_implemented() {
    let text = "struct Mesh { let id: Int }\n\
                    function duplicate(mesh: copy Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { let mesh = Mesh { id: 4 } print(duplicate(copy mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM116"]);
    // A `String` is non-trivial for exactly the same reason.
    let string = "function echo(s: copy String) -> String { return s }\n\
                      @Main function main() { let s = \"x\" print(echo(copy s)) return }";
    assert_eq!(codes(string), vec!["KSEM116"]);
}

/// `copy` on a trivially-copyable value is legal and is a no-op.
#[test]
fn copying_a_scalar_is_allowed() {
    let text = "function twice(n: copy Int) -> Int { return n + n }\n\
                    @Main function main() { let n = 2 print(twice(copy n)) return }";
    assert!(diagnostics(text).is_empty());
}

/// A `borrow` parameter leaves the caller's binding usable — the whole
/// point of borrowing. The oracle's `StxoBorrowTwice` shape.
#[test]
fn a_borrow_does_not_consume_its_argument() {
    let text = "struct Mesh { let id: Int }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { let mesh = Mesh { id: 2 } \
                    print(inspect(mesh)) print(inspect(mesh)) print(mesh.id) return }";
    assert!(diagnostics(text).is_empty());
}

/// A struct still copies when bound, so `let w = v` leaves `v` usable.
/// This is the rule arrays will break, and it is pinned here so that when
/// they do, it is a deliberate change rather than a silent one.
#[test]
fn binding_a_struct_does_not_move_it() {
    let text = "struct Mesh { var id: Int }\n\
                    @Main function main() { let v = Mesh { id: 5 } var w = v w.id = 100 print(v.id) return }";
    assert!(diagnostics(text).is_empty());
}

/// `borrow mut` is the one mode that would be observable at run time, and
/// no backend carries it yet. It is refused rather than silently
/// miscompiled into a write the caller never sees.
#[test]
fn a_mutable_borrow_is_refused_rather_than_miscompiled() {
    let text = "struct Mesh { var id: Int }\n\
                    function bump(mesh: borrow mut Mesh) { mesh.id = mesh.id + 1 return }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } bump(mesh) return }";
    assert_eq!(codes(text), vec!["KSEM112"]);
}

/// `move` and `copy` are contextual identifiers, not reserved keywords:
/// a program may still name a variable `move` or `copy`.
#[test]
fn move_and_copy_remain_usable_as_names() {
    let text = "@Main function main() { let move = 1 let copy = 2 print(move + copy) return }";
    assert!(diagnostics(text).is_empty());
}

/// A method receiver borrows, so calling a method never demands `move`
/// and never consumes the receiver.
#[test]
fn a_method_call_does_not_consume_its_receiver() {
    let text = "struct Mesh { let id: Int\n function twice() -> Int { return id * 2 } }\n\
                    @Main function main() { let m = Mesh { id: 3 } print(m.twice()) print(m.twice()) return }";
    assert!(diagnostics(text).is_empty());
}

#[test]
fn analyzed_program_records_types_and_main() {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::application(
        &db,
        "@Main function main() { let x = 3 print(x) return }".to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
    );
    let program = analyzed(&db, source);
    assert!(program.main.is_some());
    assert_eq!(program.functions.len(), 1);
    assert_eq!(program.functions[0].locals[0].ty, Type::INT);
}
