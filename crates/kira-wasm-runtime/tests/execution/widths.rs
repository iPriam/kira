//! Parity for the fixed-width scalar spellings, VM against wasm32 and wasm64.
//!
//! Every integer spelling lowers to `i64` in wasm, so a width is invisible here
//! except for one thing: signedness picks a different instruction.
//! `i64.div_u` versus `i64.div_s`, and `i64.gt_u` versus `i64.gt_s`, are the
//! whole observable surface, so each program below chooses operands whose two
//! answers differ.
//!
//! The unsigned division lowering is also structurally different from the
//! signed one — it skips the `MIN / -1` guard, because no unsigned pair
//! overflows — which makes it a separate code path in `lower.rs` that needs its
//! own coverage rather than inheriting the signed path's.

use crate::assert_parity;

#[test]
fn unsigned_division_uses_the_unsigned_instruction() {
    // As a `U64`, `-1` is 18446744073709551615. Signed division gives 0 here;
    // unsigned gives 9223372036854775807. A wrong instruction cannot tie.
    assert_parity(
        r#"@Main function main() {
            let big: U64 = -1
            let two: U64 = 2
            print(big / two)
            print(big % two)
            return
        }"#,
    );
}

#[test]
fn unsigned_ordering_uses_the_unsigned_instructions() {
    // Covers all four orderings, including `lt_u` and `le_u`, which the wasm
    // encoder had no constants for until this feature needed them.
    assert_parity(
        r#"@Main function main() {
            let big: U64 = -1
            let two: U64 = 2
            print(big < two)
            print(big <= two)
            print(big > two)
            print(big >= two)
            return
        }"#,
    );
}

#[test]
fn signed_widths_still_use_the_signed_instructions() {
    assert_parity(
        r#"@Main function main() {
            let neg: I64 = -8
            let three: I64 = 3
            print(neg / three)
            print(neg % three)
            print(neg < three)
            print(neg <= three)
            print(neg > three)
            print(neg >= three)
            return
        }"#,
    );
}

#[test]
fn every_spelling_round_trips_through_wasm() {
    assert_parity(
        r#"@Main function main() {
            let a: I8 = 7
            let b: I16 = 300
            let c: I32 = 70000
            let d: I64 = 5000000000
            let e: U8 = 200
            let f: U16 = 60000
            let g: U32 = 4000000000
            let h: U64 = 9
            print(a + b)
            print(c + d)
            print(e + f)
            print(g + h)
            return
        }"#,
    );
}

#[test]
fn float_spellings_round_trip_through_wasm() {
    assert_parity(
        r#"@Main function main() {
            let a: F32 = 1.5
            let b: F64 = 2.25
            print(a + b)
            print(b / a)
            print(a < b)
            return
        }"#,
    );
}

#[test]
fn arithmetic_wraps_at_64_bits_not_at_the_written_width() {
    // wasm has no 8-bit arithmetic in play here, and must not grow any: a `U8`
    // sum of 250 and 10 is 260 on every backend.
    assert_parity(
        r#"@Main function main() {
            let big: U8 = 250
            let ten: U8 = 10
            print(big + ten)
            return
        }"#,
    );
}

#[test]
fn an_unsigned_width_works_inside_a_loop_and_a_condition() {
    // Ordering drives control flow, so an unsigned comparison lowered as
    // signed would change which branch a loop takes rather than only what it
    // prints — and a branch target in wasm is named by label depth, which is
    // what makes this worth exercising rather than assuming.
    assert_parity(
        r#"@Main function main() {
            var i: U32 = 0
            var total: U32 = 0
            while i < 5 {
                if i > 2 {
                    total = total + i
                }
                i = i + 1
            }
            print(total)
            return
        }"#,
    );
}
