//! Parity for the conditional expression and the bitwise operators.
//!
//! wasm gets both nearly for free — a `? :` is its value-typed `if`, and its
//! shifts already take the count modulo 64 — which is exactly why these cases
//! matter: "nearly free" is where a wrong block type or a missing depth walker
//! hides, and a wrong branch target is a silent miscompile rather than a
//! validation failure.

use crate::assert_parity;

#[test]
fn a_conditional_expression_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            let n = 7
            print(n > 5 ? "big" : "small")
            print(n > 5 ? 1 : 2)
            print(n < 5 ? 1.5 : 2.5)
            print(n > 5 ? true : false)
            return
        }"#,
    );
}

/// Only the taken branch runs. Each untaken arm divides by zero, so evaluating
/// both would turn a clean run into a trap.
#[test]
fn a_conditional_evaluates_only_the_taken_branch() {
    assert_parity(
        r#"@Main function main() {
            var zero = 0
            print(true ? 10 : 1 / zero)
            print(false ? 1 / zero : 20)
            return
        }"#,
    );
}

/// Nesting is where the block type and the branch depth both have to be right:
/// each arm opens another block, so a constant depth anywhere would jump to the
/// wrong label.
#[test]
fn nested_conditionals_match_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var i = 0
            while i < 4 {
                print(i == 0 ? "zero" : i == 1 ? "one" : i == 2 ? "two" : "many")
                i = i + 1
            }
            return
        }"#,
    );
}

/// A conditional nested inside a loop inside a conditional: the `break` still
/// belongs to the loop, and the value blocks a `? :` opens must not be counted
/// as loop labels.
#[test]
fn a_conditional_inside_a_loop_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var i = 0
            var total = 0
            while true {
                if i >= 5 {
                    break
                }
                total = total + (i % 2 == 0 ? i : -i)
                i = i + 1
            }
            print(total)
            return
        }"#,
    );
}

/// A conditional yielding a heap handle rather than a scalar: the block's
/// result type is an address, and both arms must agree on it.
#[test]
fn a_conditional_over_arrays_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var xs: [Int] = [1, 2, 3]
            var ys: [Int] = [9]
            let pick = xs.count > ys.count ? xs.count : ys.count
            print(pick)
            return
        }"#,
    );
}

#[test]
fn bitwise_operators_match_the_vm() {
    assert_parity(
        r#"@Main function main() {
            let a = 12
            let b = 10
            print(a & b)
            print(a | b)
            print(a ^ b)
            print(~a)
            print(~0)
            print(~(-1))
            return
        }"#,
    );
}

#[test]
fn shifts_match_the_vm() {
    assert_parity(
        r#"@Main function main() {
            print(1 << 10)
            print(1024 >> 3)
            print(-16 >> 2)
            print(-1 >> 1)
            print(9223372036854775807 << 1)
            return
        }"#,
    );
}

/// The unsigned right shift zero-fills where the signed one propagates the sign
/// bit — the same bits in, a different number out.
#[test]
fn an_unsigned_shift_right_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            let signed: I64 = -1
            var unsigned: U64 = 0
            unsigned = unsigned - 1
            print(signed >> 60)
            print(unsigned >> 60)
            return
        }"#,
    );
}

/// A shift count of 64 or more is taken modulo 64 rather than trapping.
#[test]
fn an_oversized_shift_count_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var count = 64
            print(1 << count)
            print(1 << (count + 1))
            print(256 >> count)
            print(1 << (count * 2))
            return
        }"#,
    );
}

/// A negative shift count is defined too: only the low six bits of the count
/// are read, so `-1` shifts by 63 rather than trapping or shifting the other
/// way.
#[test]
fn a_negative_shift_count_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var back = -1
            print(1 << back)
            print(-1 >> back)
            return
        }"#,
    );
}

#[test]
fn bitwise_precedence_matches_the_vm() {
    assert_parity(
        r#"@Main function main() {
            print(1 | 2 ^ 3 & 6)
            print(1 + 2 << 3)
            print(1 << 2 + 3)
            print(~1 & 15)
            return
        }"#,
    );
}
