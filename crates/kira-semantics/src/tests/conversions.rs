//! Semantic analysis of scalar type-conversion calls `Target(expr)`: every supported
//! conversion type-checks, and every invalid one is refused with a typed code
//! rather than reported as an undefined function.

use super::codes;

/// Each of the four numeric conversion directions type-checks cleanly, across a
/// spread of width spellings.
#[test]
fn every_numeric_conversion_direction_checks_clean() {
    assert!(
        codes(
            r#"@Main function main() {
                let i: Int = 5
                let f: Float = 2.5
                print(Int(f))
                print(Float(i))
                print(I8(i))
                print(U64(i))
                print(F32(f))
                print(Int(i))
                print(Float(f))
                return
            }"#,
        )
        .is_empty()
    );
}

/// A conversion result carries the target's width spelling, so it flows into a
/// width-typed context and drives width-sensitive operators. `U64(...)` on the
/// left of `/` is unsigned division — this checks clean where a signed result
/// would too, but the point is that the width annotation reaches the checker.
#[test]
fn a_conversion_result_carries_the_target_width() {
    assert!(
        codes(
            r#"@Main function main() {
                let n: Int = 10
                let big: U64 = U64(n)
                let two: U64 = 2
                print(big / two)
                let small: F32 = F32(n)
                print(small)
                return
            }"#,
        )
        .is_empty()
    );
}

/// A conversion whose operand is not numeric is refused with `KSEM209`, not the
/// `KSEM061` undefined-function error the call form used to produce.
#[test]
fn a_non_numeric_operand_is_refused() {
    assert_eq!(
        codes(r#"@Main function main() { print(Int("hello")) return }"#),
        vec!["KSEM209"],
    );
}

/// A boolean operand is refused the same way: `Bool` is not a numeric type, so
/// it cannot be the source of a numeric conversion.
#[test]
fn a_boolean_operand_is_refused() {
    assert_eq!(
        codes(r#"@Main function main() { print(Int(true)) return }"#),
        vec!["KSEM209"],
    );
}

/// A numeric conversion takes exactly one argument; more or fewer is `KSEM210`.
#[test]
fn a_conversion_with_the_wrong_arity_is_refused() {
    assert_eq!(
        codes(r#"@Main function main() { print(Int(1, 2)) return }"#),
        vec!["KSEM210"],
    );
    assert_eq!(
        codes(r#"@Main function main() { print(Float()) return }"#),
        vec!["KSEM210"],
    );
}

/// A conversion binds its operand by position, so a label on it is refused
/// (`KSEM191`) exactly as on the other unlabeled surfaces.
#[test]
fn a_labeled_conversion_argument_is_refused() {
    assert_eq!(
        codes(r#"@Main function main() { print(Int(value: 2.5)) return }"#),
        vec!["KSEM191"],
    );
}

/// `Bool` is not a numeric conversion, so `Bool(x)` is *not* recognized here and
/// keeps its ordinary meaning — an undefined function, since no `Bool` function
/// exists. This pins that the conversion path claims only the numeric names.
#[test]
fn bool_is_not_a_numeric_conversion() {
    assert_eq!(
        codes(r#"@Main function main() { let n: Int = 1 print(Bool(n)) return }"#),
        vec!["KSEM061"],
    );
}

/// A local of the same name shadows the type name: `Int` bound as a value is
/// called as that value, not treated as a conversion. Here the local is not a
/// function, so the call is refused for that reason rather than as a cast.
#[test]
fn a_local_named_like_a_scalar_type_shadows_the_conversion() {
    // A local `Int` holding an integer is not callable; the conversion path
    // must step aside so the shadowing local is what gets diagnosed.
    let reported = codes(r#"@Main function main() { let Int = 5 print(Int(2.5)) return }"#);
    assert!(
        !reported.contains(&"KSEM209") && !reported.contains(&"KSEM210"),
        "a shadowing local must not be analyzed as a conversion, got {reported:?}",
    );
}
