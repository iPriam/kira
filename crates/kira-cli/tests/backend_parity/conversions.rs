//! Parity for scalar type-conversion calls `Target(expr)`: a numeric cast must run
//! byte-identically on the vm, llvm, and hybrid backends.
//!
//! The oracle pins these semantics, and the three backends must agree with it
//! and with each other:
//!
//! - **float -> int** truncates toward zero, saturates an out-of-range input to
//!   `i64::MIN`/`i64::MAX`, and maps NaN to zero — never rounds, never traps.
//! - **int -> float** is a signed conversion, round to nearest.
//! - **int -> int** and **float -> float** are identity copies: a width is a
//!   type annotation over one runtime representation, so nothing is truncated or
//!   extended. `I8(300)` stays `300`, matching the language's deliberate refusal
//!   to narrow — the same rule `widths.rs` pins for arithmetic.
//!
//! A backend that emitted a bare `fptosi` (poison out of range), rounded
//! instead of truncating, or masked an int-to-int cast to the target width would
//! produce a different number here rather than the same one.

use crate::assert_parity;

#[test]
fn float_to_int_truncates_toward_zero() {
    // Not floor, not round-half: `2.9 -> 2`, `-2.9 -> -2`, `2.5 -> 2`.
    let output = assert_parity(
        r#"@Main function main() {
            print(Int(2.9))
            print(Int(0.0 - 2.9))
            print(Int(2.5))
            print(Int(0.0 - 2.5))
            return
        }"#,
    );
    assert_eq!(output, "2\n-2\n2\n-2\n");
}

#[test]
fn float_to_int_saturates_and_never_traps() {
    // A magnitude far past `i64`'s range clamps to the endpoints rather than
    // wrapping or invoking undefined behavior. The literal is `1e20`, written
    // without an exponent because the lexer has none, and it exceeds
    // `i64::MAX` (~9.2e18).
    let output = assert_parity(
        r#"@Main function main() {
            print(Int(100000000000000000000.0))
            print(Int(0.0 - 100000000000000000000.0))
            return
        }"#,
    );
    assert_eq!(output, "9223372036854775807\n-9223372036854775808\n");
}

#[test]
fn int_to_float_is_a_signed_conversion() {
    let output = assert_parity(
        r#"@Main function main() {
            print(Float(7) == 7.0)
            print(Float(10) / 4.0)
            print(Float(0) - Float(3))
            return
        }"#,
    );
    assert_eq!(output, "true\n2.5\n-3\n");
}

#[test]
fn a_round_trip_through_float_and_back_holds() {
    // `Int(3.99) -> 3`, then `Float(3) -> 3.0`, compared to `3.0`.
    let output = assert_parity(
        r#"@Main function main() {
            print(Float(Int(3.99)) == 3.0)
            print(Int(Float(5)))
            return
        }"#,
    );
    assert_eq!(output, "true\n5\n");
}

#[test]
fn int_to_int_is_identity_across_widths() {
    // A width is an annotation over one 64-bit representation, so a conversion
    // neither truncates nor extends: `I8(300)` is `300`, and `U64(-1)` is the
    // same bit pattern the source held — printed signed, as every width is.
    let output = assert_parity(
        r#"@Main function main() {
            let big: U64 = -1
            print(U64(big))
            print(I8(300))
            print(U8(300))
            let n: Int = 5000000000
            print(I32(n))
            return
        }"#,
    );
    assert_eq!(output, "-1\n300\n300\n5000000000\n");
}

#[test]
fn float_to_float_is_identity_and_math_runs_at_full_width() {
    // `F32`/`F64` share one representation; the cast copies the value, and all
    // float arithmetic runs at `f64`, so no precision is lost at the cast site.
    let output = assert_parity(
        r#"@Main function main() {
            let d: F64 = 2.25
            print(F32(d))
            print(F32(d) + 1.0)
            print(F64(F32(0.5)))
            return
        }"#,
    );
    assert_eq!(output, "2.25\n3.25\n0.5\n");
}

#[test]
fn a_conversion_result_drives_unsigned_operators() {
    // The target width reaches the operator selector: `U64(...)` on the left of
    // `/` and `>` is unsigned, so `-1` reads as the large magnitude, not `-1`.
    let output = assert_parity(
        r#"@Main function main() {
            let neg: Int = 0 - 1
            let two: U64 = 2
            print(U64(neg) / two)
            print(U64(neg) > two)
            return
        }"#,
    );
    assert_eq!(output, "9223372036854775807\ntrue\n");
}

#[test]
fn conversions_flow_through_functions_and_loops() {
    let output = assert_parity(
        r#"function scaled(value: Float) -> Int {
            return Int(value * 10.0)
        }
        @Main function main() {
            var sum: Int = 0
            for i in 0..5 {
                sum = sum + Int(1.5)
            }
            print(sum)
            print(scaled(4.29))
            return
        }"#,
    );
    assert_eq!(output, "5\n42\n");
}
