//! Parity for the fixed-width scalar spellings `I8`..`Int`, `U8`..`U64`,
//! `F32`, and `Float`.
//!
//! Every integer spelling shares one 64-bit representation, so what a
//! backend can get wrong is what the spelling *means*: signedness, and the
//! range arithmetic may not leave. Each program picks operands whose signed
//! and unsigned answers differ, so a backend that emitted `sdiv` where it
//! owed `udiv` produces a different number rather than the same one; and the
//! trap cases pick sums that fit 64 bits but not the written width.
//!
//! `0xffffffffffffffff` is the workhorse. As a `U64` it is
//! `18446744073709551615`, so divided by 2 it is `9223372036854775807`
//! unsigned and `0` signed, and it is greater than 2 unsigned and less
//! signed. A backend cannot pass these by accident.

use crate::{assert_parity, assert_trap_parity};

#[test]
fn unsigned_division_differs_from_signed_on_every_backend() {
    // Signed, these are 0 and -1; unsigned they are huge. Every backend must
    // agree on the unsigned answers.
    assert_parity(
        r#"@Main function main() {
            let big: U64 = 0xffffffffffffffff
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
            let big: U64 = 0xffffffffffffffff
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
    // The same shape as the unsigned cases, spelled `Int`, to prove the
    // signedness switch is driven by the spelling rather than applied to every
    // integer.
    assert_parity(
        r#"@Main function main() {
            let neg: Int = -8
            let three: Int = 3
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
            let big: U64 = 0xffffffffffffffff
            let same: U64 = 0xffffffffffffffff
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
            let d: Int = 5000000000
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
            let b: Float = 2.25
            let c: Float = 0.25
            print(a + b)
            print(b / c)
            print(a < b)
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
            let big: Word = 0xffffffffffffffff
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
            let big: U64 = 0xffffffffffffffff
            print(halve(big))
            var c = Counter { total: 4000000000 }
            c.total = c.total + 1
            print(c.total)
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

/// Arithmetic at a narrow width is exact while it fits, and `U64` reads its
/// whole range.
#[test]
fn narrow_arithmetic_agrees_while_in_range() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let a: U8 = 200
    let b: U8 = 55
    print(a + b)
    let c: I8 = -100
    print(c - 27)
    let d: U64 = 0xffffffffffffffff
    print(d / 2)
    print(d > 1)
    let e: U32 = 4000000000
    print(e + 294967295)
    return
}
"#,
    );
    assert_eq!(output, "255\n-127\n9223372036854775807\ntrue\n4294967295\n");
}

/// A `U8` sum that leaves `0..=255` traps: not 260, not 4.
#[test]
fn a_narrow_sum_traps_when_it_leaves_the_width() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let a: U8 = 250
    print(a + 5)
    print(a + 10)
    return
}
"#,
        "255\n",
    );
}

/// Negating the most negative value of a signed width traps.
#[test]
fn negating_the_minimum_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let a: I8 = -128
    print(0 - 127)
    print(-a)
    return
}
"#,
        "-127\n",
    );
}

/// `U64` overflow is judged unsigned: `U64` max plus one traps although the
/// same 64 bits, read signed, would not.
#[test]
fn unsigned_sixty_four_bit_overflow_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let a: U64 = 0xffffffffffffffff
    print(a - 1 == 0xfffffffffffffffe)
    print(a + 1)
    return
}
"#,
        "true\n",
    );
}

/// A value converts between spellings unchanged when it fits, and the
/// conversion is the only place a wide value meets a narrow slot.
#[test]
fn conversions_between_spellings_agree_when_the_value_fits() {
    let output = assert_parity(
        r#"
function narrow(x: Int) -> U8 {
    return x
}

@Main
function main() {
    let wide: Int = 200
    let byte: U8 = wide
    print(byte)
    print(narrow(255))
    let small: I8 = -5
    let bigger: I32 = I32(small)
    print(bigger)
    let unsigned: U32 = 4294967295
    let back: Int = unsigned
    print(back)
    return
}
"#,
    );
    assert_eq!(output, "200\n255\n-5\n4294967295\n");
}

/// A conversion whose value the destination cannot hold traps at the
/// conversion, on every backend.
#[test]
fn a_narrowing_conversion_traps_when_the_value_does_not_fit() {
    assert_trap_parity(
        r#"
function narrow(x: Int) -> U8 {
    return x
}

@Main
function main() {
    print(narrow(255))
    print(narrow(256))
    return
}
"#,
        "255\n",
    );
}

/// A negative `Int` cannot become a `U64`, and a `U64` past the signed range
/// cannot become an `Int`.
#[test]
fn signedness_changes_are_checked() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let big: U64 = 9223372036854775807
    let signed: Int = big
    print(signed)
    let bigger: U64 = 0x8000000000000000
    let refused: Int = bigger
    print(refused)
    return
}
"#,
        "9223372036854775807\n",
    );
}

/// The wrapping builtins wrap at the written width, not at 64 bits.
#[test]
fn the_wrapping_builtins_wrap_at_the_written_width() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let byte: U8 = 255
    print(wrappingAdd(byte, 1))
    let signed: I8 = 127
    print(wrappingAdd(signed, 1))
    let half: U16 = 65535
    print(wrappingMul(half, 2))
    print(wrappingSub(byte, 256))
    return
}
"#,
    );
    assert_eq!(output, "0\n-128\n65534\n255\n");
}

/// A shift count is measured against the written width; a left shift drops
/// the bits it pushes past it, and a right shift follows the signedness.
#[test]
fn shifts_are_measured_against_the_width() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let byte: U8 = 129
    print(byte << 1)
    print(byte >> 1)
    let signed: I8 = -128
    print(signed >> 7)
    print(signed << 1)
    let wide: U64 = 0xffffffffffffffff
    print(wide >> 63)
    return
}
"#,
    );
    assert_eq!(output, "2\n64\n-1\n0\n1\n");
}

/// A shift count at or past the width traps.
#[test]
fn a_shift_count_past_the_width_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let byte: U8 = 1
    print(byte << 7)
    print(byte << 8)
    return
}
"#,
        "128\n",
    );
}

/// `Int(f)` truncates toward zero and traps where no integer exists; a
/// `U64` converts to `Float` as the value it is.
#[test]
fn float_conversions_truncate_and_read_unsigned() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(Int(2.9))
    print(Int(-2.9))
    let big: U64 = 0xffffffffffffffff
    print(Float(big))
    print(Float(9007199254740993))
    return
}
"#,
    );
    assert_eq!(output, "2\n-2\n18446744073709552000\n9007199254740992\n");
}

/// NaN has no integer value, and neither does a float past the range.
#[test]
fn a_float_without_an_integer_value_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    print(Int(1000000000000000000.0))
    print(Int(10000000000000000000.0))
    return
}
"#,
        "1000000000000000000\n",
    );
}

/// A bare literal adapts to the written width on the other side, whichever
/// side that is.
#[test]
fn a_literal_adapts_from_either_side() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let three: U8 = 3
    print(250 + three)
    print(three + 250)
    let big: U64 = 0xffffffffffffffff
    print(1 < big)
    return
}
"#,
    );
    assert_eq!(output, "253\n253\ntrue\n");
}
