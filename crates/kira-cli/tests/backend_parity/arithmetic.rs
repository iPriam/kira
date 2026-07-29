//! Parity for arithmetic, integer division, overflow, and float formatting.

use crate::{assert_parity, assert_trap_parity};

#[test]
fn arithmetic_and_integer_division_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(1 + 2 * 3 - 4)
    print(7 / 2)
    print(-7 % 2)
    print(17 % 5)
    print(-(3 + 4))
    return
}
"#,
    );
    assert_eq!(output, "3\n3\n-1\n2\n-7\n");
}

/// The case LLVM would get wrong on its own: `sdiv i64 MIN, -1` is poison, but
/// the VM's `wrapping_div` defines it as `MIN`. The backend branches around it,
/// and this proves the branch is really there.
#[test]
fn integer_overflow_in_division_wraps_like_the_vm() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var min = -9223372036854775807
    min = min - 1
    print(min / -1)
    print(min % -1)
    return
}
"#,
    );
    assert_eq!(output, "-9223372036854775808\n0\n");
}

/// Signed arithmetic wraps rather than trapping or being poison, matching the
/// VM's `wrapping_*` operators.
#[test]
fn signed_arithmetic_wraps_on_overflow() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var max = 9223372036854775807
    print(max + 1)
    var min = -9223372036854775807
    min = min - 1
    print(min - 1)
    return
}
"#,
    );
    assert_eq!(output, "-9223372036854775808\n9223372036854775807\n");
}

/// Division by zero is a trap in Kira, not UB: every backend must refuse it the
/// same way — the output before the trap is kept, the trap itself reaches no
/// stdout, and no run succeeds.
#[test]
fn division_by_zero_traps_on_every_backend() {
    assert_trap_parity(
        r#"
@Main
function main() {
    var zero = 0
    print(1)
    print(10 / zero)
    return
}
"#,
        "1\n",
    );
}

/// Float formatting is where a hand-written native runtime would drift from the
/// VM. Both format with the same standard library, so a whole float prints
/// without a decimal point on both.
#[test]
fn float_arithmetic_and_formatting_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let a = 1.5
    let b = 2.0
    print(a + b)
    print(b)
    print(a * b)
    print(a / b)
    print(a < b)
    print(b == 2.0)
    print(-a)
    return
}
"#,
    );
    assert_eq!(output, "3.5\n2\n3\n0.75\ntrue\ntrue\n-1.5\n");
}

/// A hexadecimal literal is the same value on every backend, including the
/// full-width bit pattern.
///
/// The corpus writes GUIDs and masks in hex, so the interesting cases are a
/// mask that would not fit as a positive `i64`, one at each width boundary, and
/// hex mixed into ordinary arithmetic.
#[test]
fn hexadecimal_literals_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(0xff)
    print(0x1bc6ea02)
    print(0X10 + 1)
    print(0x7fffffffffffffff)
    print(0xffffffffffffffff)
    print(0xff == 255)
    return
}
"#,
    );
    assert_eq!(
        output,
        "255\n466020866\n17\n9223372036854775807\n-1\ntrue\n"
    );
}

/// Float remainder is *truncated*: the sign follows the dividend.
///
/// The VM computes it with Rust's `%` on `f64` and the native backend with
/// LLVM's `frem`; both are `fmod`, and the negative case is what proves it —
/// a floored remainder would answer `3` where this answers `-1`.
#[test]
fn float_remainder_truncates_toward_zero() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(9.0 % 4.0)
    print((0.0 - 9.0) % 4.0)
    print(8.5 % 2.0)
    print(9.0 % (0.0 - 4.0))
    return
}
"#,
    );
    assert_eq!(output, "1\n-1\n0.5\n1\n");
}

/// An integer literal opposite a `Float` branch of `? :` is the float it
/// spells.
///
/// A property of the *literal*, not a widening rule: a named `Int` on one side
/// still disagrees with a `Float` on the other, exactly as `let f: Float = 1`
/// still does.
#[test]
fn a_conditional_reads_an_integer_literal_against_a_float_branch_as_a_float() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let mixed = false ? 1 : 2.5
    print(mixed)
    print(mixed == 2.5)
    print(true ? 7 : 2.5)
    print(false ? 2.5 : 7)
    return
}
"#,
    );
    assert_eq!(output, "2.5\ntrue\n7\n7\n");
}
