//! Parity for the conditional expression and the bitwise operators.
//!
//! The two features that most easily drift apart between backends, for opposite
//! reasons. A conditional is control flow that LLVM would happily lower as a
//! `select` — which evaluates both arms — and shifts are the one arithmetic
//! where LLVM's answer for an out-of-range count is *poison* while the VM and
//! wasm both take it modulo 64. Both divergences are silent, and both are what
//! these cases exist to catch.

use crate::assert_parity;

#[test]
fn a_conditional_expression_agrees() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let n = 7
    print(n > 5 ? "big" : "small")
    print(n > 5 ? 1 : 2)
    print(n < 5 ? 1.5 : 2.5)
    print(n > 5 ? true : false)
    return
}
"#,
    );
    assert_eq!(output, "big\n1\n2.5\ntrue\n");
}

/// The whole reason a conditional is a branch and not a `select`: the branch
/// that is not taken must never run. Each untaken arm here would trap, so a
/// backend that evaluated both would change the exit status rather than just
/// the output.
#[test]
fn a_conditional_evaluates_only_the_taken_branch() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var zero = 0
    print(true ? 10 : 1 / zero)
    print(false ? 1 / zero : 20)
    return
}
"#,
    );
    assert_eq!(output, "10\n20\n", "the untaken branch must never run");
}

/// Right-associative, and nestable in either branch.
#[test]
fn nested_conditionals_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var i = 0
    while i < 4 {
        print(i == 0 ? "zero" : i == 1 ? "one" : i == 2 ? "two" : "many")
        i = i + 1
    }
    return
}
"#,
    );
    assert_eq!(output, "zero\none\ntwo\nmany\n");
}

/// A conditional is an expression, so it composes: inside a call argument,
/// inside arithmetic, and as a returned value.
#[test]
fn a_conditional_composes_as_an_expression() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let flag = true
    print(1 + (flag ? 10 : 20) * 2)
    print(pick(flag))
    print(pick(false))
    return
}

function pick(flag: Bool) -> Int {
    return flag ? 100 : 200
}
"#,
    );
    assert_eq!(output, "21\n100\n200\n");
}

#[test]
fn bitwise_operators_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let a = 12
    let b = 10
    print(a & b)
    print(a | b)
    print(a ^ b)
    print(~a)
    print(~0)
    return
}
"#,
    );
    assert_eq!(output, "8\n14\n6\n-13\n-1\n");
}

#[test]
fn shifts_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(1 << 10)
    print(1024 >> 3)
    print(-16 >> 2)
    print(-1 >> 1)
    return
}
"#,
    );
    // The last two are the signed rule: `>>` on a signed spelling propagates
    // the sign bit, so `-1 >> 1` stays `-1` rather than becoming a huge
    // positive number.
    assert_eq!(output, "1024\n128\n-4\n-1\n");
}

/// `>>` on an unsigned spelling fills with zeros instead of the sign bit. The
/// same 64 bits go in — all ones — and a different number comes out, which is
/// the whole reason the operator has two forms.
///
/// The all-ones value is built by wrapping rather than written as a literal:
/// `U64`'s maximum does not fit in the `i64` a literal is read as, and
/// two's-complement subtraction is bit-identical under either signedness.
#[test]
fn an_unsigned_shift_right_zero_fills() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let signed: Int = -1
    var unsigned: U64 = 0
    unsigned = unsigned - 1
    print(signed >> 60)
    print(unsigned >> 60)
    return
}
"#,
    );
    assert_eq!(output, "-1\n15\n");
}

/// Which branch decides a conditional's width is observable, because the width
/// picks signed or unsigned `>>`. The rule is that the **then** branch decides,
/// mirroring the left-operand rule for `+`, so the same bits shift two
/// different ways depending on the order they are written in. Pinned here
/// rather than left to a semantics assertion that only checks for silence: a
/// future change to branch-type selection has to fail a test instead of quietly
/// flipping shift signedness.
#[test]
fn a_conditional_takes_its_width_from_the_then_branch() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var wide: U64 = 0
    wide = wide - 1
    print((true ? wide : 0) >> 60)
    print((false ? 0 : wide) >> 60)
    return
}
"#,
    );
    assert_eq!(output, "15\n-1\n");
}

/// A shift count of 64 or more is defined, not undefined: every backend takes
/// it modulo 64. LLVM would emit poison here without an explicit mask, so this
/// is the case that pins the mask.
#[test]
fn an_oversized_shift_count_wraps_modulo_sixty_four() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var count = 64
    print(1 << count)
    print(1 << (count + 1))
    print(256 >> count)
    return
}
"#,
    );
    assert_eq!(output, "1\n2\n256\n");
}

/// The precedence ladder, run rather than merely parsed. `&` binds tighter than
/// `^`, which binds tighter than `|`; shifts bind tighter than the orderings but
/// looser than `+`. That is C's ladder; a backend reading Go's or Swift's
/// instead prints different numbers here rather than failing to compile.
///
/// The one rung this cannot exercise at run time is bitwise-below-equality:
/// grouping `1 | (2 == 3)` makes the program a *type* error rather than a
/// different answer, so it is pinned in the parser and semantics tests instead.
#[test]
fn bitwise_precedence_agrees() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(1 | 2 ^ 3 & 6)
    print(1 + 2 << 3)
    print(1 << 2 + 3)
    return
}
"#,
    );
    assert_eq!(output, "1\n24\n32\n");
}

/// Both features together across an execution boundary: the hybrid backend
/// splits this program on `@Runtime`/`@Native`, so the bitwise work runs
/// natively while the caller runs on the VM, and the answer must not change.
///
/// Note the parentheses in `(bits & 8) == 8`: without them Kira's ladder groups
/// this as `bits & (8 == 8)`, which is a type error rather than a different
/// number. That is the bitwise-below-equality rule doing its job.
#[test]
fn bitwise_and_conditionals_agree_across_the_seam() {
    let output = assert_parity(
        r#"
@Native
function mask(seed: Int) -> Int {
    let bits = seed | 12
    return (bits & 8) == 8 ? bits << 2 : ~bits
}

@Runtime
function classify(seed: Int) -> Int {
    return mask(seed) >> 1
}

@Main
function main() {
    print(mask(0))
    print(mask(1))
    print(classify(0))
    return
}
"#,
    );
    // `0 | 12` is `12`; `12 & 8 == 8` holds, so `12 << 2` is `48`, and `48 >> 1`
    // is `24`. `1 | 12` is `13`, whose low bit changes nothing about the test.
    assert_eq!(output, "48\n52\n24\n");
}
