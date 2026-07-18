//! Semantic-analysis tests for closures: function types, capture rules, and
//! what calling a closure value checks.

use super::codes;

#[test]
fn a_function_type_checks_in_every_position() {
    assert!(
        codes(
            "function apply(f: borrow (Int) -> Int, x: Int) -> Int { return f(x) }\n\
             function make(step: Int): (Int) -> Int { return { v in return v + step } }\n\
             @Main function main() { let add = make(2) print(apply(add, 3)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_closure_with_no_expected_type_is_refused() {
    // Nothing at a `print` argument says what the parameters are, so there is
    // no signature to check the body against — and guessing one is exactly what
    // this refuses to do.
    assert_eq!(
        codes("@Main function main() { print({ v in return v }) return }"),
        vec!["KSEM134"]
    );
}

#[test]
fn a_closure_whose_parameter_count_is_wrong_is_refused() {
    assert_eq!(
        codes(
            "function run(f: (Int) -> Int) -> Int { return f(1) }\n\
             @Main function main() { print(run { a, b in return a }) return }"
        ),
        vec!["KSEM135"]
    );
}

#[test]
fn capturing_a_var_is_refused() {
    // The oracle *borrows* a mutable capture, which needs shared storage.
    // Nothing in this runtime shares storage, so copying it would run and give
    // the wrong answer — it is refused instead of silently diverging.
    assert_eq!(
        codes(
            "function run(f: () -> Int) -> Int { return f() }\n\
             @Main function main() { var total = 0 print(run { in return total }) return }"
        ),
        vec!["KSEM117"]
    );
}

#[test]
fn assigning_to_a_captured_binding_is_refused() {
    let reported = codes(
        "function run(f: () -> Void) { f() return }\n\
         @Main function main() { var total = 0 run { in total = total + 1 } print(total) return }",
    );
    assert!(
        reported.contains(&"KSEM117"),
        "a closure that writes an enclosing `var` is refused, got {reported:?}"
    );
}

#[test]
fn capturing_a_non_trivially_copyable_value_is_refused() {
    // `isTriviallyCopyable` admits only the scalars: a `String` capture is the
    // "non-Copy owned capture" KSEM117 names.
    assert_eq!(
        codes(
            "function run(f: () -> String) -> String { return f() }\n\
             @Main function main() { let label = \"hi\" print(run { in return label }) return }"
        ),
        vec!["KSEM117"]
    );

    // An array is a heap object too, and refused by the same rule.
    assert_eq!(
        codes(
            "function run(f: () -> Int) -> Int { return f() }\n\
             @Main function main() { let xs: [Int] = [1] print(run { in return xs.count }) return }"
        ),
        vec!["KSEM117"]
    );
}

#[test]
fn a_closure_argument_is_type_checked() {
    assert_eq!(
        codes(
            "@Main function main() { let f: (Int) -> Int = { v in return v } print(f(\"no\")) return }"
        ),
        vec!["KSEM063"]
    );
}

#[test]
fn a_closure_call_checks_its_argument_count() {
    assert_eq!(
        codes(
            "@Main function main() { let f: (Int) -> Int = { v in return v } print(f(1, 2)) return }"
        ),
        vec!["KSEM062"]
    );
}

#[test]
fn a_closure_body_is_checked_against_its_result_type() {
    assert_eq!(
        codes(
            "function run(f: () -> Int) -> Int { return f() }\n\
             @Main function main() { print(run { in return \"no\" }) return }"
        ),
        vec!["KSEM032"]
    );
}

#[test]
fn two_spellings_of_one_function_type_are_one_type() {
    // `(Int) -> Int` written in a parameter, an annotation, and a return type
    // interns to a single type, so a closure made for one fits all three.
    assert!(
        codes(
            "function apply(f: borrow (Int) -> Int) -> Int { return f(1) }\n\
             function make(): (Int) -> Int { return { v in return v } }\n\
             @Main function main() {\n\
               let a: (Int) -> Int = { v in return v }\n\
               let b = make()\n\
               print(apply(a) + apply(b))\n\
               return\n\
             }"
        )
        .is_empty()
    );
}

#[test]
fn a_closure_has_no_receiver_to_read_a_bare_field_from() {
    // A closure lifted out of a method has no `self`, so a bare field name in
    // its body resolves to nothing — which is what it is, not a capture.
    assert_eq!(
        codes(
            "class Counter {\n\
               let step: Int = 1\n\
               function run(f: () -> Int) -> Int { return f() }\n\
               function go() -> Int { return self.run({ in return step }) }\n\
             }\n\
             @Main function main() { print(Counter().go()) return }"
        ),
        vec!["KSEM060"]
    );
}

#[test]
fn a_closure_may_be_declared_but_never_called() {
    // A function type with no call site mints no dispatcher; a value of it is
    // still built and still copied and dropped like any other struct.
    assert!(
        codes("@Main function main() { let f: (Int) -> Int = { v in return v } return }")
            .is_empty()
    );
}
