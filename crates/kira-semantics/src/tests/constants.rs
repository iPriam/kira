//! Module-scope `let`: collection, namespace, dependency order, and the three
//! refusals it owns (KSEM316 clash, KSEM317 cycle, KSEM318 drop value).

use super::codes;

#[test]
fn a_constant_program_type_checks() {
    assert!(
        codes(
            "let answer = 6 * 7\n\
             let greeting: String = \"hi\"\n\
             @Main function main() { print(answer) print(greeting) return }"
        )
        .is_empty()
    );
}

#[test]
fn declaration_order_does_not_bind_evaluation_order() {
    // `first` reads a constant declared later, through a function declared
    // later still; the dependency order puts `base` first regardless.
    assert!(
        codes(
            "let first = base + bonus()\n\
             let base = 40\n\
             function bonus() -> Int { return 2 }\n\
             @Main function main() { print(first) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_duplicate_constant_is_refused() {
    assert_eq!(
        codes(
            "let twice = 1\n\
             let twice = 2\n\
             @Main function main() { print(twice) return }"
        ),
        vec!["KSEM316"]
    );
}

#[test]
fn a_constant_clashing_with_a_function_is_refused() {
    assert_eq!(
        codes(
            "let clash = 1\n\
             function clash() -> Int { return 2 }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM316"]
    );
}

#[test]
fn a_dependency_cycle_is_refused() {
    assert_eq!(
        codes(
            "let a = b + 1\n\
             let b = a + 1\n\
             @Main function main() { return }"
        ),
        vec!["KSEM317"]
    );
}

#[test]
fn a_self_cycle_is_refused() {
    assert_eq!(
        codes("let a = a + 1\n@Main function main() { return }"),
        vec!["KSEM317"]
    );
}

#[test]
fn a_cycle_through_a_function_is_refused() {
    // The dependency walk is transitive through called functions: `a` calls a
    // function whose body reads `a`.
    assert_eq!(
        codes(
            "let a = again()\n\
             function again() -> Int { return a }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM317"]
    );
}

#[test]
fn an_annotation_the_value_cannot_fill_is_refused() {
    assert_eq!(
        codes("let wrong: String = 1\n@Main function main() { return }"),
        vec!["KSEM020"]
    );
}

#[test]
fn a_drop_value_cannot_be_a_constant() {
    assert_eq!(
        codes(
            "struct Handle: Drop {\n    let id: Int\n\
             \n    function drop(borrow mut self) { return }\n}\n\
             let held = Handle { id: 1 }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM318"]
    );
}

#[test]
fn an_initializer_with_an_undefined_name_reports_it_once() {
    assert_eq!(
        codes("let broken = missing\n@Main function main() { return }"),
        vec!["KSEM060"]
    );
}

#[test]
fn a_field_default_may_read_a_constant() {
    assert!(
        codes(
            "let base = 10\n\
             struct Holder { var value: Int = base }\n\
             @Main function main() { let h = Holder {} print(h.value) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_local_shadows_a_constant() {
    assert!(
        codes(
            "let named = 1\n\
             @Main function main() { let named = 2 print(named) return }"
        )
        .is_empty()
    );
}
