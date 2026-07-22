//! Labeled (named) call arguments: binding an argument to the parameter it
//! names, and the refusals that binding earns.

use super::codes;
use super::diagnostics;

#[test]
fn a_labeled_call_in_declaration_order_type_checks() {
    assert!(
        diagnostics(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(tree: 1, index: 2)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_labeled_call_may_reorder_its_arguments() {
    // The label names the parameter, so the written order need not match the
    // declared one — the argument still binds where its name says.
    assert!(
        diagnostics(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(index: 2, tree: 1)) return }"
        )
        .is_empty()
    );
}

#[test]
fn both_binders_are_accepted_in_a_call() {
    // `=` is canonical, `:` transitional; both bind the same argument.
    assert!(
        diagnostics(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(tree = 1, index = 2)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_labeled_method_call_type_checks() {
    assert!(
        diagnostics(
            "struct Grid {\n\
             var w: Int\n\
             function at(row: Int, col: Int) -> Int { return row * self.w + col }\n\
             }\n\
             @Main function main() { let g = Grid { w = 10 } print(g.at(col: 2, row: 1)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_labeled_call_still_checks_argument_types_after_reordering() {
    // Reordering binds `index` to the `String`; the type error lands on the
    // parameter the label chose, not the position it was written in.
    assert_eq!(
        codes(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(index: \"x\", tree: 1)) return }"
        ),
        vec!["KSEM063"]
    );
}

#[test]
fn an_unknown_label_is_refused() {
    assert_eq!(
        codes(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(tree: 1, nope: 2)) return }"
        ),
        vec!["KSEM187"]
    );
}

#[test]
fn a_duplicate_label_is_refused() {
    assert_eq!(
        codes(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(tree: 1, tree: 2)) return }"
        ),
        vec!["KSEM188"]
    );
}

#[test]
fn mixing_labeled_and_positional_arguments_is_refused() {
    assert_eq!(
        codes(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(tree: 1, 2)) return }"
        ),
        vec!["KSEM189"]
    );
}

#[test]
fn a_missing_labeled_argument_is_refused_by_name_only() {
    // The named refusal stands alone: no second, unnamed arity error follows.
    assert_eq!(
        codes(
            "function measure(tree: Int, index: Int) -> Int { return tree + index }\n\
             @Main function main() { print(measure(tree: 1)) return }"
        ),
        vec!["KSEM190"]
    );
}

#[test]
fn a_label_on_the_print_builtin_is_refused() {
    assert_eq!(
        codes("@Main function main() { print(value: 1) return }"),
        vec!["KSEM191"]
    );
}

// ----- parameter defaults ------------------------------------------------

#[test]
fn a_positional_call_may_omit_a_trailing_defaulted_argument() {
    // `add(1)` fills `step` from its default, so the call type-checks with no
    // arity error.
    assert!(
        diagnostics(
            "function add(base: Int, step: Int = 3) -> Int { return base + step }\n\
             @Main function main() { print(add(1)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_positional_call_may_still_pass_a_defaulted_argument() {
    assert!(
        diagnostics(
            "function add(base: Int, step: Int = 3) -> Int { return base + step }\n\
             @Main function main() { print(add(1, 5)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_labeled_call_may_omit_a_defaulted_argument() {
    // The omitted `step` is filled from its default rather than earning the
    // missing-argument refusal a default-less parameter would.
    assert!(
        diagnostics(
            "function add(base: Int, step: Int = 3) -> Int { return base + step }\n\
             @Main function main() { print(add(base: 1)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_default_still_type_checks_a_passed_argument() {
    // Supplying the wrong type for a defaulted parameter is still a type error.
    assert!(
        codes(
            "function add(base: Int, step: Int = 3) -> Int { return base + step }\n\
             @Main function main() { print(add(1, true)) return }"
        )
        .contains(&"KSEM063")
    );
}

#[test]
fn a_default_less_missing_argument_is_still_an_arity_error() {
    // Only the defaulted trailing parameter may be omitted; a bare `add(1)`
    // against two default-less parameters is still KSEM062.
    assert!(
        codes(
            "function add(base: Int, step: Int) -> Int { return base + step }\n\
             @Main function main() { print(add(1)) return }"
        )
        .contains(&"KSEM062")
    );
}

#[test]
fn a_default_may_call_a_helper_in_its_declaring_file() {
    // The default resolves in the declaring module's scope, so it may name a
    // sibling function.
    assert!(
        diagnostics(
            "function base() -> Int { return 7 }\n\
             function add(x: Int, y: Int = base()) -> Int { return x + y }\n\
             @Main function main() { print(add(1)) return }"
        )
        .is_empty()
    );
}

#[test]
fn parameter_defaults_that_fill_each_other_are_refused() {
    // `f`'s default calls `g` omitting its argument, whose default calls `f`
    // omitting its argument: the cycle has no finite value.
    assert!(
        codes(
            "function g(a: Int = f()) -> Int { return a }\n\
             function f(b: Int = g()) -> Int { return b }\n\
             @Main function main() { print(f()) return }"
        )
        .contains(&"KSEM240")
    );
}
