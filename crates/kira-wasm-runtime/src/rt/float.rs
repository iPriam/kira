//! Shortest-round-trip decimal digits for an `f64`, generated into the module.
//!
//! This is Steele & White's Dragon4 in the Burger–Dybvig formulation: keep the
//! value as an exact rational `R/S`, carry the distances `M+`/`M-` to the
//! neighbouring representable values, and emit digits until the number printed
//! so far is closer to this `f64` than to either neighbour. The first such
//! prefix is the shortest string that reads back as this exact value, which is
//! precisely what Rust's `f64` `Display` promises — so the VM, native code, and
//! this module print the same float because they answer the same question, not
//! because three formatters were tuned to agree.
//!
//! `digits(value) -> exponent` writes the digits at [`layout::DIGITS`], leaves
//! the count in a global, and returns `k` such that the value is
//! `0.d1d2...dn × 10^k`. Sign, zero, and the non-finite cases are settled by
//! the caller — this only runs on a positive finite value.

use crate::encode::ValType;
use crate::func::{BlockType::Empty, BlockType::Value, Func, FuncIdx, GlobalIdx, LocalIdx};
use crate::layout;
use crate::module::Module;
use crate::rt::bignum::Bignum;

/// Emits `digits(value) -> exponent`.
pub fn define(
    module: &mut Module,
    index: FuncIdx,
    bignum: &Bignum,
    digit_count: GlobalIdx,
) -> bool {
    let mut func = module.func(vec![ValType::F64], vec![ValType::I32]);
    let Some(value) = func.param(0) else {
        return false;
    };

    let bits = func.local(ValType::I64);
    let mantissa = func.local(ValType::I64);
    let significand = func.local(ValType::I64);
    let exponent_bits = func.local(ValType::I32);
    let exponent = func.local(ValType::I32);
    let inclusive = func.local(ValType::I32);
    let asymmetric = func.local(ValType::I32);
    let k = func.local(ValType::I32);
    let digit = func.local(ValType::I32);
    let low = func.local(ValType::I32);
    let high = func.local(ValType::I32);
    let ordering = func.local(ValType::I32);
    let cursor = func.local(module.addr().val());

    func.local_get(value).i64_reinterpret_f64().local_set(bits);
    func.local_get(bits)
        .i64_const(52)
        .i64_shr_u()
        .i64_const(0x7ff)
        .i64_and()
        .i32_wrap_i64()
        .local_set(exponent_bits);
    func.local_get(bits)
        .i64_const(0x000f_ffff_ffff_ffff)
        .i64_and()
        .local_set(mantissa);

    // A subnormal has no implicit leading one and a fixed exponent; a normal
    // value carries the one and biases its exponent by the significand width.
    func.local_get(exponent_bits).i32_eqz();
    func.if_(Empty);
    {
        func.local_get(mantissa).local_set(significand);
        func.i32_const(-1074).local_set(exponent);
    }
    func.else_();
    {
        func.local_get(mantissa)
            .i64_const(1i64 << 52)
            .i64_or()
            .local_set(significand);
        func.local_get(exponent_bits)
            .i32_const(1075)
            .i32_sub()
            .local_set(exponent);
    }
    func.end();

    // An even significand owns its boundaries: a decimal landing exactly on one
    // reads back as this value under round-to-even, so the test is inclusive.
    func.local_get(significand)
        .i64_const(1)
        .i64_and()
        .i64_eqz()
        .local_set(inclusive);

    // A power of two is closer to its lower neighbour than its upper one — the
    // significand steps down to a finer exponent below it. The smallest normal
    // is the exception: its lower neighbour is a subnormal at the same spacing.
    func.local_get(mantissa)
        .i64_eqz()
        .local_get(exponent_bits)
        .i32_const(1)
        .i32_gt_u()
        .i32_and()
        .local_set(asymmetric);

    initial_registers(
        &mut func,
        bignum,
        significand,
        exponent,
        asymmetric,
        module.addr().val(),
    );

    estimate_and_prescale(&mut func, bignum, significand, exponent, k);

    // Scale up while the value's upper boundary still reaches the denominator:
    // each step moves one digit out of the integer part.
    func.block(Empty);
    func.loop_(Empty);
    {
        sum_into_t(&mut func, bignum);
        compare_t_to_s(&mut func, bignum, inclusive, ordering, Bound::High);
        func.i32_eqz().br_if(1);
        func.addr_const(u64::from(layout::BIGNUM_S))
            .i32_const(10)
            .call(bignum.mul_small);
        func.local_get(k).i32_const(1).i32_add().local_set(k);
        func.br(0);
    }
    func.end();
    func.end();

    // Scale down while a further digit would still fit under the denominator.
    func.block(Empty);
    func.loop_(Empty);
    {
        sum_into_t(&mut func, bignum);
        func.addr_const(u64::from(layout::BIGNUM_T))
            .i32_const(10)
            .call(bignum.mul_small);
        compare_t_to_s(&mut func, bignum, inclusive, ordering, Bound::Low);
        func.i32_eqz().br_if(1);
        for register in [layout::BIGNUM_R, layout::BIGNUM_MP, layout::BIGNUM_MM] {
            func.addr_const(u64::from(register))
                .i32_const(10)
                .call(bignum.mul_small);
        }
        func.local_get(k).i32_const(1).i32_sub().local_set(k);
        func.br(0);
    }
    func.end();
    func.end();

    func.addr_const(u64::from(layout::DIGITS)).local_set(cursor);

    func.block(Empty);
    func.loop_(Empty);
    {
        for register in [layout::BIGNUM_R, layout::BIGNUM_MP, layout::BIGNUM_MM] {
            func.addr_const(u64::from(register))
                .i32_const(10)
                .call(bignum.mul_small);
        }

        // The digit is how many times the denominator fits: at most nine, so
        // repeated subtraction is a division without a division routine.
        func.i32_const(0).local_set(digit);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.addr_const(u64::from(layout::BIGNUM_R))
                .addr_const(u64::from(layout::BIGNUM_S))
                .call(bignum.cmp)
                .i32_const(0)
                .i32_lt_s()
                .br_if(1);
            func.addr_const(u64::from(layout::BIGNUM_R))
                .addr_const(u64::from(layout::BIGNUM_S))
                .call(bignum.sub);
            func.local_get(digit)
                .i32_const(1)
                .i32_add()
                .local_set(digit);
            func.br(0);
        }
        func.end();
        func.end();

        // Below the lower boundary: what has been emitted already rounds back
        // to this value, so stopping here is safe.
        func.addr_const(u64::from(layout::BIGNUM_R))
            .addr_const(u64::from(layout::BIGNUM_MM))
            .call(bignum.cmp);
        test_bound(&mut func, inclusive, ordering, Bound::Low);
        func.local_set(low);

        sum_into_t(&mut func, bignum);
        compare_t_to_s(&mut func, bignum, inclusive, ordering, Bound::High);
        func.local_set(high);

        // Neither boundary reached: this digit is forced, and another follows.
        // Label 0 is this `if`, 1 is the digit loop — continuing it is the
        // whole point, so the depth is worth stating.
        func.local_get(low)
            .local_get(high)
            .i32_or()
            .i32_eqz()
            .if_(Empty);
        {
            store_digit(&mut func, cursor, digit);
            func.br(1);
        }
        func.end();

        // One boundary reached picks the side; both reached is a tie broken by
        // which of the two the remainder is actually nearer.
        func.local_get(low).i32_eqz();
        func.if_(Empty);
        {
            func.local_get(digit)
                .i32_const(1)
                .i32_add()
                .local_set(digit);
        }
        func.else_();
        {
            func.local_get(high);
            func.if_(Empty);
            {
                func.addr_const(u64::from(layout::BIGNUM_T))
                    .addr_const(u64::from(layout::BIGNUM_R))
                    .call(bignum.copy);
                func.addr_const(u64::from(layout::BIGNUM_T))
                    .call(bignum.shl1);
                func.addr_const(u64::from(layout::BIGNUM_T))
                    .addr_const(u64::from(layout::BIGNUM_S))
                    .call(bignum.cmp)
                    .i32_const(0)
                    .i32_ge_s();
                func.if_(Empty);
                func.local_get(digit)
                    .i32_const(1)
                    .i32_add()
                    .local_set(digit);
                func.end();
            }
            func.end();
        }
        func.end();

        store_digit(&mut func, cursor, digit);
    }
    func.end();
    func.end();

    // Rounding the last digit up can carry: `0.99` rounded at the second digit
    // is `1.0`, which is one digit at a higher exponent. Nothing else in the
    // loop can produce a ten, so the carry is settled once, here.
    carry(&mut func, cursor, k, module.addr().val());

    // The cursor advanced past the last digit written, so the count is how far
    // it moved — not how far the buffer is from it.
    func.local_get(cursor)
        .addr_const(u64::from(layout::DIGITS))
        .addr_sub()
        .addr_to_i32()
        .global_set(digit_count);
    func.local_get(k);

    module.define(index, func)
}

/// Jumps `k` to roughly the right decimal exponent and scales to match.
///
/// The loops that follow are correct from any starting point — they only ever
/// step one digit at a time — but stepping there from zero is how a subnormal
/// paid for 320 passes over a big integer before its first digit. The value's
/// own exponent already says roughly where its decimal point is:
/// `log10(f × 2^e)` is within a digit of `(e + bitlen(f)) × log10(2)`, and the
/// loops close that last digit.
///
/// So this is a shortcut, not a decision: an estimate that was wildly wrong
/// would be slow and still correct, which is why it needs no proof of accuracy.
fn estimate_and_prescale(
    func: &mut Func,
    bignum: &Bignum,
    significand: LocalIdx,
    exponent: LocalIdx,
    k: LocalIdx,
) {
    let bits_used = func.local(ValType::I32);

    // The significand is never zero here — a zero value never reaches this —
    // so its bit length is what `clz` leaves.
    func.i32_const(64);
    func.local_get(significand).i64_clz().i32_wrap_i64();
    func.i32_sub().local_set(bits_used);

    // ceil((e + bitlen) × log10(2))
    func.local_get(exponent)
        .local_get(bits_used)
        .i32_add()
        .f64_convert_i32_s();
    func.f64_const(std::f64::consts::LOG10_2);
    func.f64_mul();
    func.f64_ceil();
    func.i32_trunc_f64_s();
    func.local_set(k);

    // A positive exponent means the value is large, so the denominator is what
    // grows; a negative one means the numerator and both boundaries do.
    func.local_get(k).i32_const(0).i32_gt_s();
    func.if_(Empty);
    {
        func.addr_const(u64::from(layout::BIGNUM_S))
            .local_get(k)
            .call(bignum.mul_pow10);
    }
    func.else_();
    {
        func.local_get(k).i32_const(0).i32_lt_s();
        func.if_(Empty);
        for register in [layout::BIGNUM_R, layout::BIGNUM_MP, layout::BIGNUM_MM] {
            func.addr_const(u64::from(register));
            func.i32_const(0).local_get(k).i32_sub();
            func.call(bignum.mul_pow10);
        }
        func.end();
    }
    func.end();
}

/// Which side of the value a boundary comparison is testing.
#[derive(Debug, Clone, Copy)]
enum Bound {
    /// At or above the upper boundary — `cmp >= 0`, or `> 0` when exclusive.
    High,
    /// At or below the lower boundary — `cmp <= 0`, or `< 0` when exclusive.
    Low,
}

/// Emits `T = R + M+`.
fn sum_into_t(func: &mut Func, bignum: &Bignum) {
    func.addr_const(u64::from(layout::BIGNUM_T))
        .addr_const(u64::from(layout::BIGNUM_R))
        .call(bignum.copy);
    func.addr_const(u64::from(layout::BIGNUM_T))
        .addr_const(u64::from(layout::BIGNUM_MP))
        .call(bignum.add);
}

/// Pushes whether a `cmp` result has reached `bound`, at the inclusiveness the
/// significand dictates.
///
/// The comparison lands in a local first because a block's operand stack starts
/// empty: an `if` cannot reach a value pushed before it, so the result has to be
/// named to be used in both arms.
fn test_bound(func: &mut Func, inclusive: LocalIdx, ordering: LocalIdx, bound: Bound) {
    func.local_set(ordering);
    func.local_get(inclusive);
    func.if_(Value(ValType::I32));
    func.local_get(ordering).i32_const(0);
    match bound {
        Bound::High => func.i32_ge_s(),
        Bound::Low => func.i32_le_s(),
    };
    func.else_();
    func.local_get(ordering).i32_const(0);
    match bound {
        Bound::High => func.i32_gt_s(),
        Bound::Low => func.i32_lt_s(),
    };
    func.end();
}

/// Pushes whether `T` has reached `S` on the `High` side.
fn compare_t_to_s(
    func: &mut Func,
    bignum: &Bignum,
    inclusive: LocalIdx,
    ordering: LocalIdx,
    bound: Bound,
) {
    func.addr_const(u64::from(layout::BIGNUM_T))
        .addr_const(u64::from(layout::BIGNUM_S))
        .call(bignum.cmp);
    test_bound(func, inclusive, ordering, bound);
}

/// Emits `store cursor++ = '0' + digit`.
fn store_digit(func: &mut Func, cursor: LocalIdx, digit: LocalIdx) {
    func.local_get(cursor);
    func.i32_const(b'0' as i32).local_get(digit).i32_add();
    func.i32_store8(0);
    func.local_get(cursor)
        .addr_const(1)
        .addr_add()
        .local_set(cursor);
}

/// Propagates a ten in the last digit back through the digits already emitted.
fn carry(func: &mut Func, cursor: LocalIdx, k: LocalIdx, addr: ValType) {
    let scan = func.local(addr);
    func.local_get(cursor).local_set(scan);

    func.block(Empty);
    func.loop_(Empty);
    {
        // Nothing to carry into once the scan reaches the front: the value
        // became a one at the next exponent up.
        func.local_get(scan)
            .addr_const(u64::from(layout::DIGITS))
            .addr_eq();
        func.if_(Empty);
        {
            func.addr_const(u64::from(layout::DIGITS));
            func.i32_const(b'1' as i32);
            func.i32_store8(0);
            func.addr_const(u64::from(layout::DIGITS))
                .addr_const(1)
                .addr_add()
                .local_set(cursor);
            func.local_get(k).i32_const(1).i32_add().local_set(k);
            func.br(2);
        }
        func.end();

        func.local_get(scan)
            .addr_const(1)
            .addr_sub()
            .local_set(scan);

        func.local_get(scan)
            .i32_load8_u(0)
            .i32_const(b'9' as i32)
            .i32_gt_u()
            .i32_eqz()
            .br_if(1);

        // This digit overflowed: it becomes a zero and the carry moves left.
        func.local_get(scan);
        func.i32_const(b'0' as i32);
        func.i32_store8(0);
        func.br(0);
    }
    func.end();
    func.end();
}

/// Emits the initial `R`, `S`, `M+`, and `M-` for a positive finite value.
///
/// The four cases are the cross of "does the exponent scale the numerator or
/// the denominator" and "is the value a power of two". Getting the second wrong
/// is invisible on almost every input and prints one digit too many on exactly
/// the values a user is most likely to type.
fn initial_registers(
    func: &mut Func,
    bignum: &Bignum,
    significand: LocalIdx,
    exponent: LocalIdx,
    asymmetric: LocalIdx,
    addr: ValType,
) {
    let shift = func.local(ValType::I32);
    let _ = addr;

    func.addr_const(u64::from(layout::BIGNUM_R))
        .local_get(significand)
        .call(bignum.set);

    func.local_get(exponent).i32_const(0).i32_ge_s();
    func.if_(Empty);
    {
        // R = f << (e + 1 + extra); S = 2 << extra; M+ = 1 << (e + extra);
        // M- = 1 << e, with `extra` one for a power of two.
        func.local_get(asymmetric)
            .if_(Value(ValType::I32))
            .i32_const(1);
        func.else_().i32_const(0);
        func.end();
        func.local_set(shift);

        func.addr_const(u64::from(layout::BIGNUM_R));
        func.local_get(exponent)
            .i32_const(1)
            .i32_add()
            .local_get(shift)
            .i32_add();
        func.call(bignum.shl);

        func.addr_const(u64::from(layout::BIGNUM_S))
            .i64_const(2)
            .call(bignum.set);
        func.addr_const(u64::from(layout::BIGNUM_S))
            .local_get(shift)
            .call(bignum.shl);

        func.addr_const(u64::from(layout::BIGNUM_MP))
            .i64_const(1)
            .call(bignum.set);
        func.addr_const(u64::from(layout::BIGNUM_MP))
            .local_get(exponent)
            .local_get(shift)
            .i32_add()
            .call(bignum.shl);

        func.addr_const(u64::from(layout::BIGNUM_MM))
            .i64_const(1)
            .call(bignum.set);
        func.addr_const(u64::from(layout::BIGNUM_MM))
            .local_get(exponent)
            .call(bignum.shl);
    }
    func.else_();
    {
        // R = f << (1 + extra); S = 1 << (1 - e + extra); M+ = 1 << extra;
        // M- = 1.
        func.local_get(asymmetric)
            .if_(Value(ValType::I32))
            .i32_const(1);
        func.else_().i32_const(0);
        func.end();
        func.local_set(shift);

        func.addr_const(u64::from(layout::BIGNUM_R));
        func.i32_const(1).local_get(shift).i32_add();
        func.call(bignum.shl);

        func.addr_const(u64::from(layout::BIGNUM_S))
            .i64_const(1)
            .call(bignum.set);
        func.addr_const(u64::from(layout::BIGNUM_S));
        func.i32_const(1)
            .local_get(exponent)
            .i32_sub()
            .local_get(shift)
            .i32_add();
        func.call(bignum.shl);

        func.addr_const(u64::from(layout::BIGNUM_MP))
            .i64_const(1)
            .call(bignum.set);
        func.addr_const(u64::from(layout::BIGNUM_MP))
            .local_get(shift)
            .call(bignum.shl);

        func.addr_const(u64::from(layout::BIGNUM_MM))
            .i64_const(1)
            .call(bignum.set);
    }
    func.end();
}
