//! Semantic-analysis tests: the diagnostics `kira check` reports, driven
//! through the same salsa `analyzed` query the CLI and the LSP use.

use kira_diagnostics::Diagnostic;

mod aliases;
mod any;
mod arrays;
mod attempts;
mod calls;
mod channels;
mod classes;
mod closures;
mod collectors;
mod compiler;
mod constants;
mod constructs;
mod conversions;
mod copyable;
mod definitions;
mod distincts;
mod drop;
mod enums;
mod exports;
mod ffi_types;
mod file_system;
mod foreign;
mod foreign_field;
mod generics;
mod imports;
mod libraries;
mod main_thread;
mod markers;
mod matches;
mod memberwise;
mod mutation;
mod native_state;
mod operators;
mod overloads;
mod raw_pointers;
mod repro_dep_enum;
mod reuse;
mod specializations;
mod strings;
mod syscall;
mod tasks;
mod traits;
mod widths;

use super::*;
use crate::BuildMachine;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirProgram, HirStmt};

fn diagnostics(text: &str) -> Vec<Diagnostic> {
    module_diagnostics(text, &[])
}

/// The analyzed HIR of a single-file program, for the few cases that check the
/// shape analysis produced rather than the diagnostics it did not.
fn analyze_text(text: &str) -> HirProgram {
    let db = salsa::DatabaseImpl::new();
    let source =
        SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), Vec::new());
    analyzed(&db, source).clone()
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
        PrecompiledShaders::default(),
        BuildMachine::host(),
        // Not a lint run.
        false,
    );
    analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0.clone())
        .collect()
}

/// The diagnostic codes of a library, in order.
fn library_codes(text: &str) -> Vec<String> {
    library_diagnostics(text)
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect()
}

/// The diagnostic codes of a multi-module program, in order.
fn module_codes(text: &str, modules: &[(&str, &str)]) -> Vec<String> {
    module_diagnostics(text, modules)
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect()
}

fn codes(text: &str) -> Vec<String> {
    diagnostics(text)
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
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
    assert!(
        codes("function f() { return }")
            .iter()
            .any(|code| code == "KSEM011")
    );
}

#[test]
fn duplicate_main_is_reported() {
    let text = "@Main function a() { return }\n@Main function b() { return }";
    assert!(codes(text).iter().any(|code| code == "KSEM010"));
}

#[test]
fn undefined_name_is_reported() {
    assert!(
        codes("@Main function main() { print(x) return }")
            .iter()
            .any(|code| code == "KSEM060")
    );
}

#[test]
fn wrong_argument_type_is_reported() {
    let text =
        "function f(n: Int) -> Int { return n }\n@Main function main() { print(f(true)) return }";
    assert!(codes(text).iter().any(|code| code == "KSEM063"));
}

#[test]
fn arity_mismatch_is_reported() {
    let text =
        "function f(n: Int) -> Int { return n }\n@Main function main() { print(f(1, 2)) return }";
    assert!(codes(text).iter().any(|code| code == "KSEM062"));
}

#[test]
fn assigning_to_let_is_reported() {
    let text = "@Main function main() { let x = 1 x = 2 return }";
    assert!(codes(text).iter().any(|code| code == "KSEM021"));
}

#[test]
fn missing_return_on_some_paths_is_reported() {
    // The review's reproduced hole: only the `n > 100` path returns.
    let text = "function f(n: Int) -> Int { if n > 100 { return 1 } }\n\
                    @Main function main() { print(f(5)) return }";
    assert!(codes(text).iter().any(|code| code == "KSEM033"));
}

#[test]
fn if_else_where_both_arms_return_is_accepted() {
    let text = "function f(n: Int) -> Int { if n > 0 { return 1 } else { return 2 } }\n\
                    @Main function main() { print(f(5)) return }";
    assert!(!codes(text).iter().any(|code| code == "KSEM033"));
}

#[test]
fn else_if_chain_with_full_coverage_is_accepted() {
    let text = "function f(n: Int) -> Int {\n\
                        if n > 0 { return 1 } else if n < 0 { return 2 } else { return 3 }\n\
                    }\n\
                    @Main function main() { print(f(5)) return }";
    assert!(!codes(text).iter().any(|code| code == "KSEM033"));
}

#[test]
fn else_if_chain_missing_final_else_is_reported() {
    let text = "function f(n: Int) -> Int {\n\
                        if n > 0 { return 1 } else if n < 0 { return 2 }\n\
                    }\n\
                    @Main function main() { print(f(5)) return }";
    assert!(codes(text).iter().any(|code| code == "KSEM033"));
}

#[test]
fn while_containing_return_does_not_count_as_definite() {
    // A while body may run zero times, so it can never satisfy the check.
    let text = "function f(n: Int) -> Int { while n > 0 { return n } }\n\
                    @Main function main() { print(f(5)) return }";
    assert!(codes(text).iter().any(|code| code == "KSEM033"));
}

#[test]
fn trailing_return_after_if_is_accepted() {
    let text = "function f(n: Int) -> Int { if n > 100 { return 1 } return 0 }\n\
                    @Main function main() { print(f(5)) return }";
    assert!(!codes(text).iter().any(|code| code == "KSEM033"));
}

#[test]
fn void_functions_are_exempt_from_definite_return() {
    let text = "function f() { print(1) }\n@Main function main() { f() return }";
    assert!(!codes(text).iter().any(|code| code == "KSEM033"));
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

/// Assigning to a moved-out binding gives it a value again.
///
/// The shape a program threads an owned value through a sequence of steps
/// with: each step takes the value and hands back the next one, and the
/// binding names whichever one is current.
#[test]
fn assigning_to_a_moved_local_makes_it_live_again() {
    let text = "struct Mesh { let id: Int }\n\
                    function step(mesh: Mesh) -> Mesh { return Mesh { id: mesh.id + 1 } }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } \
                    mesh = step(move mesh) mesh = step(move mesh) print(inspect(mesh)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// The value is analyzed before the binding is restored, so a *second* read of
/// the moved local in the same assignment is still a use-after-move.
#[test]
fn an_assignment_does_not_excuse_reading_the_moved_local_in_its_own_value() {
    let text = "struct Mesh { let id: Int }\n\
                    function step(mesh: Mesh, n: Int) -> Mesh { return Mesh { id: mesh.id + n } }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } \
                    mesh = step(move mesh, inspect(mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM107"]);
}

/// A loop body runs more than once, so a value it gives away is already gone
/// when it starts again. The second `close` here would free a handle the first
/// one already freed.
#[test]
fn a_move_a_loop_repeats_is_rejected() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } var n = 0 \
                    while n < 3 { print(consume(move mesh)) n = n + 1 } return }";
    assert_eq!(codes(text), vec!["KSEM270"]);
}

/// The rule is about the back edge, not about `move` in a loop: a binding the
/// body gives a new value to has one again when the body starts over. This is
/// the idiom for threading an owned value through a loop, and it stays legal.
#[test]
fn a_move_the_loop_body_reassigns_is_accepted() {
    let text = "struct Mesh { let id: Int }\n\
                    function step(mesh: Mesh) -> Mesh { return Mesh { id: mesh.id + 1 } }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } var n = 0 \
                    while n < 3 { mesh = step(move mesh) n = n + 1 } \
                    print(inspect(mesh)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A body that always returns never reaches the back edge, so its move runs at
/// most once and is as sound as one in straight-line code.
#[test]
fn a_move_in_a_loop_body_that_always_returns_is_accepted() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() -> Int { var mesh = Mesh { id: 1 } var n = 0 \
                    while n < 3 { return consume(move mesh) } return 0 }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// The same for a body that always breaks out: `break` leaves the loop rather
/// than jumping back to its head.
#[test]
fn a_move_in_a_loop_body_that_always_breaks_is_accepted() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } var n = 0 \
                    while n < 3 { print(consume(move mesh)) break } return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A local the body declares is bound afresh on every iteration, so its move
/// is spent inside one and never crosses the back edge.
#[test]
fn a_move_of_a_local_the_loop_body_declares_is_accepted() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var n = 0 \
                    while n < 3 { let mesh = Mesh { id: n } print(consume(move mesh)) n = n + 1 } \
                    return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A `for` variable is rebound at the top of each iteration by the desugar, so
/// it is a body-declared local for this purpose whatever its slot number says.
#[test]
fn a_move_of_a_for_loop_variable_is_accepted() {
    let text = "function consume(s: String) -> Int { return s.count }\n\
                    @Main function main() { let names = [\"ada\", \"grace\"] \
                    for name in names { print(consume(move name)) } return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A `for` loop's back edge is the `while` desugar's, so a move of an outer
/// binding is caught there too.
#[test]
fn a_move_a_for_loop_repeats_is_rejected() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } \
                    for i in 0..3 { print(consume(move mesh)) } return }";
    assert_eq!(codes(text), vec!["KSEM270"]);
}

/// One mistake, one diagnostic: every loop around the offending one sees the
/// same local go from live to moved, and only the innermost says so.
#[test]
fn a_move_nested_loops_repeat_is_reported_once() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } \
                    for i in 0..3 { for j in 0..3 { print(consume(move mesh)) } } return }";
    assert_eq!(codes(text), vec!["KSEM270"]);
}

/// A move on one path through the body still reaches the back edge on that
/// path, so a conditional move is refused exactly as an unconditional one is.
#[test]
fn a_conditional_move_inside_a_loop_is_rejected() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } var n = 0 \
                    while n < 3 { if n == 1 { print(consume(move mesh)) } n = n + 1 } return }";
    assert_eq!(codes(text), vec!["KSEM270"]);
}

/// The loop reports the repeat; code *after* it reports its own use-after-move,
/// because the state the body ended in is what follows the loop.
#[test]
fn a_read_after_a_loop_that_moved_is_still_a_use_after_move() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } var n = 0 \
                    while n < 3 { print(consume(move mesh)) n = n + 1 } \
                    print(inspect(mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM270", "KSEM107"]);
}

/// Writing *through* a moved-out binding is not a reinitialization: the field
/// write needs the value that is gone.
#[test]
fn assigning_to_a_field_of_a_moved_local_is_still_rejected() {
    let text = "struct Mesh { var id: Int }\n\
                    function consume(mesh: Mesh) { return }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } \
                    consume(move mesh) mesh.id = 2 return }";
    assert_eq!(codes(text), vec!["KSEM107"]);
}

/// Two arms of a branch are alternatives, so each may move the same value.
///
/// The shape the corpus writes: one `match` over a platform enum, every arm
/// handing the same value to its own handler and returning.
#[test]
fn sibling_branches_may_each_move_the_same_local() {
    let text = "enum Platform { Metal  Vulkan }\n\
                    struct Mesh { let id: Int }\n\
                    function metal(mesh: Mesh) -> Int { return mesh.id }\n\
                    function vulkan(mesh: Mesh) -> Int { return mesh.id + 1 }\n\
                    function pick(p: Platform, mesh: Mesh) -> Int { \
                    match p { Metal -> return metal(move mesh) \
                    Vulkan -> return vulkan(move mesh) } }\n\
                    @Main function main() { print(pick(Platform.Metal, Mesh { id: 1 })) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn a_move_in_one_branch_still_poisons_a_later_read() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } \
                    if true { print(consume(move mesh)) } print(inspect(mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM107"]);
}

/// An arm that definitely returns contributes nothing to the join: its move
/// happened on a path that never rejoins.
#[test]
fn a_move_in_a_returning_branch_does_not_reach_the_code_after_it() {
    let text = "struct Mesh { let id: Int }\n\
                    function consume(mesh: Mesh) -> Int { return mesh.id }\n\
                    function inspect(mesh: borrow Mesh) -> Int { return mesh.id }\n\
                    function pick(flag: Bool, mesh: Mesh) -> Int { \
                    if flag { return consume(move mesh) } return inspect(mesh) }\n\
                    @Main function main() { print(pick(true, Mesh { id: 1 })) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
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

/// Passing a named non-trivial value to a `copy` parameter must say `copy`.
#[test]
fn a_copy_parameter_wants_an_explicit_copy_of_a_non_trivial_value() {
    let text = "struct Mesh { let id: Int }\n\
                    function duplicate(mesh: copy Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { let mesh = Mesh { id: 4 } print(duplicate(mesh)) return }";
    assert_eq!(codes(text), vec!["KSEM113"]);
}

/// Explicit `copy` is defined for every runtime value. Structs and strings
/// copy their owned storage; arrays copy their handle and detach on the first
/// write, matching the VM and native value operations.
#[test]
fn copying_a_non_trivial_value_is_allowed() {
    let text = "struct Mesh { let id: Int }\n\
                    function duplicate(mesh: copy Mesh) -> Int { return mesh.id }\n\
                    @Main function main() { let mesh = Mesh { id: 4 } print(duplicate(copy mesh)) return }";
    assert!(diagnostics(text).is_empty());
    // A `String` owns bytes, so this also exercises an actual heap copy.
    let string = "function echo(s: copy String) -> String { return s }\n\
                      @Main function main() { let s = \"x\" print(echo(copy s)) return }";
    assert!(diagnostics(string).is_empty());
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

/// `borrow mut` is the one mode observable at run time: the callee writes
/// through the caller's binding, and the call site records where that write
/// lands.
#[test]
fn a_mutable_borrow_is_accepted_and_carries_a_writeback() {
    let text = "struct Mesh { var id: Int }\n\
                    function bump(mesh: borrow mut Mesh) { mesh.id = mesh.id + 1 return }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } bump(mesh) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = analyze_text(text);
    let main = program
        .functions
        .iter()
        .find(|function| function.is_main)
        .expect("the program declares a main");
    let call = main
        .body
        .iter()
        .find_map(|&stmt| match program.stmt(stmt) {
            HirStmt::Expr { expr } => Some(*expr),
            _ => None,
        })
        .expect("main evaluates the call");
    let HirExpr::Call { writebacks, .. } = program.expr(call) else {
        panic!("expected a call expression");
    };
    assert_eq!(writebacks.len(), 1, "{writebacks:?}");
    assert_eq!(writebacks[0].param, 0);
    assert!(writebacks[0].place.path.is_empty());
}

/// A `borrow mut` argument has to name storage: a temporary would be mutated
/// and then discarded, which is a write nobody can observe.
#[test]
fn a_mutable_borrow_of_a_temporary_is_refused() {
    let text = "struct Mesh { var id: Int }\n\
                    function bump(mesh: borrow mut Mesh) { mesh.id = mesh.id + 1 return }\n\
                    @Main function main() { bump(Mesh { id: 1 }) return }";
    assert_eq!(codes(text), vec!["KSEM248"]);
}

/// One call cannot mutably borrow the same binding twice: both writes would
/// land in one place and the later one would erase the earlier.
#[test]
fn mutably_borrowing_one_binding_twice_in_a_call_is_refused() {
    let text = "struct Mesh { var id: Int }\n\
                    function merge(a: borrow mut Mesh, b: borrow mut Mesh) { a.id = b.id return }\n\
                    @Main function main() { var mesh = Mesh { id: 1 } merge(mesh, mesh) return }";
    assert_eq!(codes(text), vec!["KSEM247"]);
}

/// Two sibling fields of one binding are two distinct places.
///
/// The shape the corpus writes: `sceneApplyEdit(ws.doc, ws.world, op)`. Keying
/// the refusal on the root binding alone rejected it, which was wrong — neither
/// write can reach the other's storage.
#[test]
fn mutably_borrowing_two_fields_of_one_binding_is_accepted() {
    let text = "struct Doc { var n: Int }\n\
                    struct World { var n: Int }\n\
                    struct State { var doc: Doc  var world: World }\n\
                    function apply(d: borrow mut Doc, w: borrow mut World) { \
                    d.n = d.n + 1 w.n = w.n + 1 return }\n\
                    @Main function main() { \
                    var state = State { doc: Doc { n: 1 }, world: World { n: 2 } } \
                    apply(state.doc, state.world) print(state.doc.n) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A place that *contains* another is still one place: the outer write would
/// erase the inner one.
#[test]
fn mutably_borrowing_a_binding_and_its_own_field_is_refused() {
    let text = "struct Doc { var n: Int }\n\
                    struct State { var doc: Doc }\n\
                    function apply(s: borrow mut State, d: borrow mut Doc) { \
                    d.n = s.doc.n return }\n\
                    @Main function main() { var state = State { doc: Doc { n: 1 } } \
                    apply(state, state.doc) return }";
    assert_eq!(codes(text), vec!["KSEM247"]);
}

/// Two elements of one array cannot be told apart without evaluating the
/// indices, so the pair is refused rather than assumed distinct.
#[test]
fn mutably_borrowing_two_elements_of_one_array_is_refused() {
    let text = "struct Cell { var n: Int }\n\
                    function apply(a: borrow mut Cell, b: borrow mut Cell) { \
                    a.n = b.n return }\n\
                    @Main function main() { var cells = [Cell { n: 1 }, Cell { n: 2 }] \
                    apply(cells[0], cells[1]) return }";
    assert_eq!(codes(text), vec!["KSEM247"]);
}

/// Two distinct bindings are two distinct places, so the same call is fine.
#[test]
fn mutably_borrowing_two_bindings_in_one_call_is_accepted() {
    let text = "struct Mesh { var id: Int }\n\
                    function merge(a: borrow mut Mesh, b: borrow mut Mesh) { a.id = b.id return }\n\
                    @Main function main() { var one = Mesh { id: 1 } var two = Mesh { id: 2 } \
                    merge(one, two) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
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
