//! `str_from_f64`: the digits from Dragon4, placed the way Rust places them.
//!
//! Shortest-round-trip gives digits and an exponent; a formatter still has to
//! decide where the point goes. Rust's `f64` `Display` never uses exponent
//! notation — `1e300` prints as a one and three hundred zeros, and `1e-300` as
//! a zero, a point, and three hundred more — so this positions the digits and
//! pads with zeros rather than reaching for a shorter, different answer. That
//! choice is the whole reason a float prints the same here as on the VM.

use crate::encode::ValType;
use crate::func::{BlockType::Empty, BlockType::Value, Func, LocalIdx};
use crate::layout;
use crate::literals::Literals;
use crate::module::Module;
use crate::rt::Runtime;
use crate::rt::string::TEXT;

/// Emits `str_from_f64(value) -> address`.
pub fn define(module: &mut Module, rt: &Runtime, literals: &mut Literals) -> bool {
    let nan = literals.intern("NaN");
    let infinity = literals.intern("inf");
    let negative_infinity = literals.intern("-inf");
    let zero = literals.intern("0");
    let negative_zero = literals.intern("-0");

    let addr = module.addr().val();
    let mut func = module.func(vec![ValType::F64], vec![addr]);
    let Some(value) = func.param(0) else {
        return false;
    };

    let bits = func.local(ValType::I64);
    let mantissa = func.local(ValType::I64);
    let exponent_bits = func.local(ValType::I32);
    let negative = func.local(ValType::I32);
    let k = func.local(ValType::I32);
    let count = func.local(ValType::I32);
    let length = func.local(ValType::I32);
    let result = func.local(addr);
    let out = func.local(addr);
    let position = func.local(ValType::I32);
    let from = func.local(ValType::I32);
    let span = func.local(ValType::I32);
    let index = func.local(ValType::I32);

    func.local_get(value).i64_reinterpret_f64().local_set(bits);
    func.local_get(bits)
        .i64_const(63)
        .i64_shr_u()
        .i32_wrap_i64()
        .local_set(negative);
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

    // Not finite: a NaN has no sign to show, an infinity does.
    func.local_get(exponent_bits).i32_const(0x7ff).i32_eq();
    func.if_(Empty);
    {
        func.local_get(mantissa).i64_eqz().i32_eqz();
        func.if_(Empty);
        func.addr_const(nan).return_();
        func.end();

        func.local_get(negative).if_(Value(addr));
        func.addr_const(negative_infinity);
        func.else_();
        func.addr_const(infinity);
        func.end();
        func.return_();
    }
    func.end();

    // Zero: signed, because the sign of a zero is observable and Rust shows it.
    func.local_get(exponent_bits)
        .i32_eqz()
        .local_get(mantissa)
        .i64_eqz()
        .i32_and();
    func.if_(Empty);
    {
        func.local_get(negative).if_(Value(addr));
        func.addr_const(negative_zero);
        func.else_();
        func.addr_const(zero);
        func.end();
        func.return_();
    }
    func.end();

    // The digits are the magnitude's; the sign is put back on at the front.
    func.local_get(value).f64_abs().call(rt.float_digits);
    func.local_set(k);
    func.global_get(rt.digit_count).local_set(count);

    // `0.000ddd` needs a zero, a point, and the gap; `ddd000` needs the run of
    // zeros out to the point; anything else is the digits plus the point.
    func.local_get(k).i32_const(0).i32_le_s();
    func.if_(Value(ValType::I32));
    {
        func.i32_const(2)
            .i32_const(0)
            .local_get(k)
            .i32_sub()
            .i32_add()
            .local_get(count)
            .i32_add();
    }
    func.else_();
    {
        func.local_get(k).local_get(count).i32_ge_s();
        func.if_(Value(ValType::I32));
        func.local_get(k);
        func.else_();
        func.local_get(count).i32_const(1).i32_add();
        func.end();
    }
    func.end();
    func.local_get(negative).i32_add();
    func.local_tee(length);

    func.call(rt.str_new).local_set(result);
    func.local_get(result)
        .addr_const(TEXT)
        .addr_add()
        .local_set(out);
    func.i32_const(0).local_set(position);

    func.local_get(negative);
    func.if_(Empty);
    {
        store_byte(&mut func, out, position, b'-');
    }
    func.end();

    func.local_get(k).i32_const(0).i32_le_s();
    func.if_(Empty);
    {
        store_byte(&mut func, out, position, b'0');
        store_byte(&mut func, out, position, b'.');
        func.i32_const(0).local_get(k).i32_sub().local_set(span);
        fill_zeros(&mut func, out, position, span, index);
        func.i32_const(0).local_set(from);
        func.local_get(count).local_set(span);
        copy_digits(&mut func, out, position, from, span, index);
    }
    func.else_();
    {
        func.local_get(k).local_get(count).i32_ge_s();
        func.if_(Empty);
        {
            func.i32_const(0).local_set(from);
            func.local_get(count).local_set(span);
            copy_digits(&mut func, out, position, from, span, index);
            func.local_get(k).local_get(count).i32_sub().local_set(span);
            fill_zeros(&mut func, out, position, span, index);
        }
        func.else_();
        {
            func.i32_const(0).local_set(from);
            func.local_get(k).local_set(span);
            copy_digits(&mut func, out, position, from, span, index);
            store_byte(&mut func, out, position, b'.');
            func.local_get(k).local_set(from);
            func.local_get(count).local_get(k).i32_sub().local_set(span);
            copy_digits(&mut func, out, position, from, span, index);
        }
        func.end();
    }
    func.end();

    func.local_get(result);
    module.define(rt.str_from_f64, func)
}

/// Emits `out[position++] = byte`.
fn store_byte(func: &mut Func, out: LocalIdx, position: LocalIdx, byte: u8) {
    func.local_get(out)
        .local_get(position)
        .i32_to_addr()
        .addr_add();
    func.i32_const(i32::from(byte));
    func.i32_store8(0);
    func.local_get(position)
        .i32_const(1)
        .i32_add()
        .local_set(position);
}

/// Emits a run of `span` zeros at `out[position..]`.
fn fill_zeros(func: &mut Func, out: LocalIdx, position: LocalIdx, span: LocalIdx, index: LocalIdx) {
    func.i32_const(0).local_set(index);
    func.block(Empty);
    func.loop_(Empty);
    {
        func.local_get(index).local_get(span).i32_ge_s().br_if(1);
        store_byte(func, out, position, b'0');
        func.local_get(index)
            .i32_const(1)
            .i32_add()
            .local_set(index);
        func.br(0);
    }
    func.end();
    func.end();
}

/// Emits a copy of `span` generated digits, starting at `from`, to
/// `out[position..]`.
fn copy_digits(
    func: &mut Func,
    out: LocalIdx,
    position: LocalIdx,
    from: LocalIdx,
    span: LocalIdx,
    index: LocalIdx,
) {
    func.i32_const(0).local_set(index);
    func.block(Empty);
    func.loop_(Empty);
    {
        func.local_get(index).local_get(span).i32_ge_s().br_if(1);

        func.local_get(out)
            .local_get(position)
            .i32_to_addr()
            .addr_add();
        func.addr_const(u64::from(layout::DIGITS))
            .local_get(from)
            .i32_to_addr()
            .addr_add()
            .local_get(index)
            .i32_to_addr()
            .addr_add()
            .i32_load8_u(0);
        func.i32_store8(0);

        func.local_get(position)
            .i32_const(1)
            .i32_add()
            .local_set(position);
        func.local_get(index)
            .i32_const(1)
            .i32_add()
            .local_set(index);
        func.br(0);
    }
    func.end();
    func.end();
}
