//! Semantics for the fixed-width scalar spellings: what a width makes
//! distinct, and the one place it deliberately does not.
//!
//! The whole rule is in [`kira_semantics_model::Type::assignable_to`]: a
//! *named* width must match exactly, while bare `Int`/`Float` is a wildcard
//! matching any width in its kind. These tests pin both halves, because
//! dropping either one breaks a different set of programs — losing the
//! exactness lets `U8` flow into an `I64`, and losing the wildcard makes
//! `let x: U8 = 5` fail to check.

use super::{codes, diagnostics};

#[test]
fn every_fixed_width_name_resolves_as_a_type() {
    assert!(
        diagnostics(
            "@Main function main() {
                 let a: I8 = 1
                 let b: I16 = 2
                 let c: I32 = 3
                 let d: I64 = 4
                 let e: U8 = 5
                 let f: U16 = 6
                 let g: U32 = 7
                 let h: U64 = 8
                 let i: F32 = 1.5
                 let j: F64 = 2.5
                 print(a + 1)
                 print(b + 1)
                 print(c + 1)
                 print(d + 1)
                 print(e + 1)
                 print(f + 1)
                 print(g + 1)
                 print(h + 1)
                 print(i + 0.5)
                 print(j + 0.5)
                 return
             }"
        )
        .is_empty()
    );
}

#[test]
fn an_integer_literal_checks_at_every_width() {
    // The wildcard half of the rule. A literal is spelled plain `Int`, so it is
    // assignable to every width — this is what stands in for the implicit
    // conversion the language does not have.
    assert!(
        diagnostics(
            "@Main function main() {
                 let a: U8 = 5
                 let b: I64 = 5
                 print(a + 1)
                 print(b - 1)
                 print(a > 0)
                 return
             }"
        )
        .is_empty()
    );
}

#[test]
fn two_different_widths_do_not_mix() {
    // The exactness half. There is no widening in this language, so this is a
    // type error rather than a promotion to the wider operand.
    assert_eq!(
        codes(
            "@Main function main() {
                 let a: U8 = 1
                 let b: I64 = 2
                 let c: I64 = a
                 print(c + b)
                 return
             }"
        ),
        vec!["KSEM020"]
    );
}

#[test]
fn arithmetic_on_two_different_widths_is_rejected() {
    assert_eq!(
        codes(
            "@Main function main() {
                 let a: U8 = 1
                 let b: I64 = 2
                 print(a + b)
                 return
             }"
        ),
        vec!["KSEM071"]
    );
}

#[test]
fn an_integer_width_and_a_float_width_do_not_mix() {
    // Across kinds there is not even a wildcard: `Int` and `Float` are
    // different types, so no spelling of one reaches the other.
    assert_eq!(
        codes(
            "@Main function main() {
                 let a: I32 = 1
                 let b: F32 = 2.0
                 print(a + b)
                 return
             }"
        ),
        vec!["KSEM071"]
    );
}

#[test]
fn two_float_widths_do_not_mix() {
    assert_eq!(
        codes(
            "@Main function main() {
                 let a: F32 = 1.0
                 let b: F64 = 2.0
                 print(a + b)
                 return
             }"
        ),
        vec!["KSEM071"]
    );
}

#[test]
fn a_width_is_checked_at_a_call_and_at_a_return() {
    assert_eq!(
        codes(
            "function take(value: U8) -> U8 { return value }
             @Main function main() {
                 let wide: I64 = 1
                 print(take(wide))
                 return
             }"
        ),
        vec!["KSEM063"]
    );
}

#[test]
fn a_width_is_checked_on_a_struct_field() {
    assert_eq!(
        codes(
            "struct Holder { var value: U32 }
             @Main function main() {
                 let wide: I64 = 1
                 let h = Holder { value: wide }
                 print(h.value)
                 return
             }"
        ),
        vec!["KSEM094"]
    );
}

#[test]
fn a_bare_int_accepts_and_is_accepted_by_any_width() {
    // The wildcard is symmetric, and deliberately makes assignability
    // non-transitive: `U8` -> `Int` and `Int` -> `I64` both hold while
    // `U8` -> `I64` does not. That is the language's rule, not an artifact.
    assert!(
        diagnostics(
            "function widen(value: Int) -> Int { return value }
             @Main function main() {
                 let small: U8 = 5
                 let plain: Int = 6
                 print(widen(small))
                 let back: U8 = plain
                 print(back)
                 return
             }"
        )
        .is_empty()
    );
}

#[test]
fn a_mixed_operation_takes_its_type_from_the_left_operand() {
    // The left operand decides, and this is *asymmetric*: `1 + i32Value` is a
    // plain `Int` (wildcard-assignable to `I64`), while `i32Value + 1` is an
    // `I32` (which is not). Verified against the oracle in both directions —
    // a symmetric "the written width wins" rule accepts the second and passes
    // every backend-parity test while still being wrong.
    assert!(
        diagnostics(
            "@Main function main() {
                 let narrow: I32 = 3
                 let wide: I64 = 1 + narrow
                 print(wide)
                 return
             }"
        )
        .is_empty()
    );
    assert_eq!(
        codes(
            "@Main function main() {
                 let narrow: I32 = 3
                 let wide: I64 = narrow + 1
                 print(wide)
                 return
             }"
        ),
        vec!["KSEM020"]
    );
}

#[test]
fn a_for_range_bound_accepts_any_integer_width() {
    // KSEM043 is a *kind* check, not an exact-spelling one: the oracle runs
    // `for i in 0..u8Count`. Reading `Type::INT` as a pattern here would reject
    // it, because `Type::INT` is only the plain spelling.
    assert!(
        diagnostics(
            "@Main function main() {
                 let count: U8 = 3
                 for i in 0..count {
                     print(i)
                 }
                 return
             }"
        )
        .is_empty()
    );
}

#[test]
fn a_for_range_bound_still_rejects_a_non_integer() {
    assert_eq!(
        codes(
            "@Main function main() {
                 let limit: Float = 3.0
                 for i in 0..limit {
                     print(i)
                 }
                 return
             }"
        ),
        vec!["KSEM043"]
    );
}

#[test]
fn an_array_index_accepts_any_integer_width() {
    // Same kind-not-spelling rule as the `for` bound: `xs[u8Index]` runs on the
    // oracle. An index is consumed as a position, so its width means nothing.
    assert!(
        diagnostics(
            "@Main function main() {
                 var xs: [Int] = []
                 xs.append(10)
                 xs.append(20)
                 let at: U8 = 1
                 print(xs[at])
                 return
             }"
        )
        .is_empty()
    );
}

#[test]
fn an_array_index_still_rejects_a_non_integer() {
    assert_eq!(
        codes(
            "@Main function main() {
                 var xs: [Int] = []
                 xs.append(10)
                 let at: Float = 0.0
                 print(xs[at])
                 return
             }"
        ),
        vec!["KSEM102"]
    );
}

#[test]
fn there_is_no_byte_builtin() {
    // `Byte` is a library alias — `type Byte = U8` — not a builtin, so an
    // unaliased use is an unknown type. Hardcoding it here would invent a
    // ninth integer name the language does not have.
    assert_eq!(
        codes(
            "@Main function main() {
                 let b: Byte = 1
                 print(b)
                 return
             }"
        ),
        vec!["KSEM050"]
    );
}

#[test]
fn a_byte_alias_declares_the_name_the_language_actually_uses() {
    assert!(
        diagnostics(
            "type Byte = U8
             @Main function main() {
                 let b: Byte = 250
                 let d: Byte = 5
                 print(b / d)
                 return
             }"
        )
        .is_empty()
    );
}

#[test]
fn there_is_no_128_bit_width_and_no_char() {
    // The corpus census has neither, so neither resolves.
    for absent in ["I128", "U128", "Char"] {
        let source = format!(
            "@Main function main() {{
                 let value: {absent} = 1
                 print(value)
                 return
             }}"
        );
        assert_eq!(codes(&source), vec!["KSEM050"], "{absent} must not resolve");
    }
}
