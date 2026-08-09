//! Typing rules for the conditional expression and the bitwise operators.
//!
//! Both were added together because both were the frontend-only corner of the
//! oracle's surface: the conditional appears in its docs with no corpus call
//! site, and `& | ^ ~ << >>` have tokens with none either. What settled that
//! they *execute* rather than being refused is the oracle's own lowering
//! boundary, which lists conditional expressions among the executable subset
//! and carries every bitwise operator through its IR into both backends. So
//! these are typing tests for real operators, not refusal tests.

use super::*;

/// The condition is `Bool` and nothing else. There is no truthiness.
#[test]
fn a_conditional_condition_must_be_a_bool() {
    assert!(
        diagnostics("@Main function main() { let x = true ? 1 : 2 print(x) return }").is_empty()
    );
    assert_eq!(
        codes("@Main function main() { let x = 1 ? 1 : 2 print(x) return }"),
        vec!["KSEM131"]
    );
    assert_eq!(
        codes(r#"@Main function main() { let x = "s" ? 1 : 2 print(x) return }"#),
        vec!["KSEM131"]
    );
}

/// Both branches must agree, with no widening and no common supertype.
#[test]
fn conditional_branches_must_agree_on_a_type() {
    assert!(
        diagnostics(r#"@Main function main() { let x = true ? "a" : "b" print(x) return }"#)
            .is_empty()
    );
    assert_eq!(
        codes(r#"@Main function main() { let x = true ? 1 : "b" print(x) return }"#),
        vec!["KSEM132"]
    );
    // An integer *literal* opposite a `Float` branch is the float it spells:
    // a literal has no width of its own until a position gives it one, and the
    // other branch is that position.
    assert!(
        diagnostics("@Main function main() { let x = true ? 1 : 2.0 print(x) return }").is_empty()
    );
    // That is a property of the literal and not a widening rule, so a value
    // that already has a width still does not meet a `Float`.
    assert_eq!(
        codes("@Main function main() { let n = 1 let x = true ? n : 2.0 print(x) return }"),
        vec!["KSEM132"]
    );
}

/// A bare literal is a wildcard against a written width, exactly as it is for
/// `+`, so a mask or a default constant needs no conversion. Which of the two
/// widths the conditional then *takes* is not visible in a diagnostic; the
/// then-branch rule is pinned by its observable effect on shift signedness in
/// `backend_parity::bitwise::a_conditional_takes_its_width_from_the_then_branch`.
#[test]
fn a_bare_literal_branch_agrees_with_a_written_width() {
    assert!(
        diagnostics("@Main function main() { let w: U8 = 7 let x = true ? 0 : w print(x) return }")
            .is_empty()
    );
    // Two *different* written widths still agree on nothing.
    assert_eq!(
        codes(
            "@Main function main() { let a: U8 = 7 let b: U32 = 9 let x = true ? a : b print(x) return }"
        ),
        vec!["KSEM132"]
    );
}

/// A conditional is an expression, so it must leave a value behind. Two `Void`
/// branches type-agree yet produce nothing, which is rejected here rather than
/// reaching a backend that cannot represent it.
#[test]
fn a_conditional_over_two_void_branches_is_rejected() {
    assert_eq!(
        codes("@Main function main() { let x = true ? print(1) : print(2) return }"),
        vec!["KSEM133"]
    );
}

/// A bad condition is reported once, not once per branch as well.
#[test]
fn a_conditional_reports_its_condition_error_once() {
    assert_eq!(
        codes(r#"@Main function main() { let x = 1 ? "a" : 2 print(x) return }"#),
        vec!["KSEM131"]
    );
}

/// `&`, `|`, and `^` want two integers and yield one.
#[test]
fn bitwise_operators_require_integers() {
    assert!(
        diagnostics("@Main function main() { print(6 & 3) print(6 | 3) print(6 ^ 3) return }")
            .is_empty()
    );
    assert_eq!(
        codes("@Main function main() { print(1.0 & 3) return }"),
        vec!["KSEM071"]
    );
    // There is no bitwise `Bool`: `&&`/`||` already mean that, and they
    // short-circuit where these do not.
    assert_eq!(
        codes("@Main function main() { print(true & false) return }"),
        vec!["KSEM071"]
    );
    assert_eq!(
        codes(r#"@Main function main() { print("a" | "b") return }"#),
        vec!["KSEM071"]
    );
}

/// `~` complements an integer and rejects everything else — notably `Bool`,
/// which has `!`.
#[test]
fn complement_requires_an_integer() {
    assert!(diagnostics("@Main function main() { print(~5) return }").is_empty());
    assert_eq!(
        codes("@Main function main() { print(~true) return }"),
        vec!["KSEM070"]
    );
    assert_eq!(
        codes("@Main function main() { print(~1.0) return }"),
        vec!["KSEM070"]
    );
}

/// A shift is the one binary operator whose operands need not agree: the right
/// side is a count, not a value of the same kind.
#[test]
fn a_shift_accepts_any_integer_count() {
    assert!(
        diagnostics("@Main function main() { let w: U8 = 7 let n: Int = 2 print(w << n) return }")
            .is_empty()
    );
    assert_eq!(
        codes("@Main function main() { print(1 << 2.0) return }"),
        vec!["KSEM071"]
    );
    assert_eq!(
        codes("@Main function main() { print(1.0 >> 2) return }"),
        vec!["KSEM071"]
    );
}

/// The mixed-width rule that shifts relax, `&`/`|`/`^` keep: those two operands
/// really are the same kind of thing.
#[test]
fn bitwise_operands_must_agree_on_a_width() {
    assert_eq!(
        codes("@Main function main() { let a: U8 = 7 let b: U32 = 9 print(a & b) return }"),
        vec!["KSEM071"]
    );
    // A bare literal still pairs with any width.
    assert!(diagnostics("@Main function main() { let a: U8 = 7 print(a & 3) return }").is_empty());
}
