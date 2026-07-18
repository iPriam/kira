//! Parity for the fixed-width scalar spellings `I8`..`I64`, `U8`..`U64`,
//! `F32`, and `F64`.
//!
//! Most of a width is a *frontend* distinction — every integer spelling shares
//! one 64-bit representation, so `I32` and `I64` reach the backends as the same
//! value. The one thing a width decides that a backend can get wrong is
//! **signedness**, and that is what these programs are built to catch: each
//! picks operands whose signed and unsigned answers differ, so a backend that
//! emitted `sdiv` where it owed `udiv` produces a different number rather than
//! the same one.
//!
//! `-1` is the workhorse. As a `U64` it is `18446744073709551615`, so `-1 / 2`
//! is `9223372036854775807` unsigned and `0` signed, and `-1 > 2` is `true`
//! unsigned and `false` signed. A backend cannot pass these by accident.

use crate::{assert_parity, assert_trap_parity};

#[test]
fn unsigned_division_differs_from_signed_on_every_backend() {
    // Signed, these are 0 and -1; unsigned they are huge. Every backend must
    // agree on the unsigned answers.
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
fn unsigned_ordering_differs_from_signed_on_every_backend() {
    assert_parity(
        r#"@Main function main() {
            let big: U64 = -1
            let two: U64 = 2
            print(big > two)
            print(big >= two)
            print(big < two)
            print(big <= two)
            return
        }"#,
    );
}

#[test]
fn signed_widths_keep_signed_division_and_ordering() {
    // The same shape as the unsigned cases, spelled `I64`, to prove the
    // signedness switch is driven by the spelling rather than applied to every
    // integer.
    assert_parity(
        r#"@Main function main() {
            let neg: I64 = -8
            let three: I64 = 3
            print(neg / three)
            print(neg % three)
            print(neg < three)
            return
        }"#,
    );
}

#[test]
fn equality_is_the_same_under_either_signedness() {
    // `==` has no unsigned twin because it needs none: the same 64 bits
    // compare equal whichever way they are read. This pins that the compiler
    // does not accidentally route equality through an ordering opcode.
    assert_parity(
        r#"@Main function main() {
            let big: U64 = -1
            let same: U64 = -1
            let two: U64 = 2
            print(big == same)
            print(big != two)
            return
        }"#,
    );
}

#[test]
fn every_integer_spelling_carries_a_value_across_the_backends() {
    // A width is a frontend distinction, so the point here is that all nine
    // spellings *work* and agree, not that they differ.
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
            let i: Int = 1
            print(a + 1)
            print(b + 1)
            print(c + 1)
            print(d + 1)
            print(e + 1)
            print(f + 1)
            print(g + 1)
            print(h + 1)
            print(i + 1)
            return
        }"#,
    );
}

#[test]
fn float_spellings_carry_a_value_across_the_backends() {
    assert_parity(
        r#"@Main function main() {
            let a: F32 = 1.5
            let b: F64 = 2.25
            let c: Float = 0.25
            print(a + b)
            print(b / c)
            print(a < b)
            return
        }"#,
    );
}

#[test]
fn arithmetic_wraps_at_64_bits_for_every_spelling() {
    // The deliberate non-feature. A `U8` sum of 250 and 10 is 260, not 4:
    // arithmetic wraps at the representation's 64 bits, never at the written
    // width. Narrowing is behavior the language does not define, so no backend
    // may invent it — and a backend that masked to 8 bits would fail here
    // rather than pass quietly.
    assert_parity(
        r#"@Main function main() {
            let big: U8 = 250
            let ten: U8 = 10
            print(big + ten)
            let wide: I8 = 127
            print(wide + wide)
            return
        }"#,
    );
}

#[test]
fn a_literal_is_usable_at_every_width() {
    // An integer literal carries the plain spelling, which is the wildcard in
    // assignability — this is what lets `let x: U8 = 5` check with no
    // conversion rule, and what lets `x + 1` mix a literal with a width.
    assert_parity(
        r#"@Main function main() {
            let a: U8 = 5
            let b: I32 = 5
            print(a + 1)
            print(b + 1)
            print(a > 1)
            return
        }"#,
    );
}

#[test]
fn unsigned_division_by_zero_traps_on_every_backend() {
    // The unsigned path is a *separate* lowering in every backend — it has no
    // overflow branch, because no unsigned pair overflows — so its trap has to
    // be pinned separately from the signed one.
    assert_trap_parity(
        r#"@Main function main() {
            let big: U64 = 7
            let zero: U64 = 0
            print("before")
            print(big / zero)
            return
        }"#,
        "before\n",
    );
}

#[test]
fn unsigned_remainder_by_zero_traps_on_every_backend() {
    assert_trap_parity(
        r#"@Main function main() {
            let big: U64 = 7
            let zero: U64 = 0
            print("before")
            print(big % zero)
            return
        }"#,
        "before\n",
    );
}

#[test]
fn an_alias_carries_its_targets_signedness() {
    // `type Byte = U8` is how the language spells `Byte` — it is a library
    // alias, not a builtin. An alias that lost the target's signedness would
    // silently emit signed division, so this pins that it does not.
    assert_parity(
        r#"type Word = U64
        @Main function main() {
            let big: Word = -1
            let two: Word = 2
            print(big / two)
            print(big > two)
            return
        }"#,
    );
}

#[test]
fn widths_pass_through_functions_and_struct_fields() {
    assert_parity(
        r#"struct Counter {
            var total: U32
        }
        function halve(value: U64) -> U64 {
            let two: U64 = 2
            return value / two
        }
        @Main function main() {
            let big: U64 = -1
            print(halve(big))
            var c = Counter { total: 4000000000 }
            c.total = c.total + 1
            print(c.total)
            return
        }"#,
    );
}

#[test]
fn a_plain_left_operand_makes_a_mixed_operation_signed() {
    // The left operand decides signedness, so a plain `Int` on the left keeps
    // the operation signed even when the right side is a `U8`. The oracle
    // prints -3, -1, and true here.
    //
    // Every earlier case in this file puts the written width on the left, and
    // that gap is exactly how a wrong unification rule survived review: all
    // four backends agreed with each other while all four disagreed with the
    // oracle. Backend parity is not oracle parity.
    assert_parity(
        r#"@Main function main() {
            let neg: Int = 0 - 10
            let three: U8 = 3
            print(neg / three)
            print(neg % three)
            print(neg < three)
            return
        }"#,
    );
}

#[test]
fn swapping_the_operands_swaps_the_signedness() {
    // The same two values with the sides exchanged: now the `U8` is on the
    // left, so this is unsigned and prints 0 rather than -3.
    assert_parity(
        r#"@Main function main() {
            let neg: U8 = 0 - 10
            let three: Int = 3
            print(neg / three)
            print(neg < three)
            return
        }"#,
    );
}

#[test]
fn a_width_typed_for_bound_and_array_index_run_on_every_backend() {
    assert_parity(
        r#"@Main function main() {
            let count: U8 = 3
            for i in 0..count {
                print(i)
            }
            var xs: [Int] = []
            xs.append(10)
            xs.append(20)
            let at: U16 = 1
            print(xs[at])
            return
        }"#,
    );
}
