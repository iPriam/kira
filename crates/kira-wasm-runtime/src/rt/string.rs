//! The generated string runtime: construction, concatenation, comparison,
//! integer and boolean rendering, printing, and traps.
//!
//! Every function here is emitted into the module. `print` and `trap` are the
//! only things a host supplies, and both take bytes the module already built —
//! so what a Kira program says on the Web is decided by Kira's code, not by the
//! embedder's idea of how a number looks.

use crate::encode::ValType;
use crate::func::{BlockType, BlockType::Empty, FuncIdx};
use crate::layout;
use crate::module::Module;
use crate::rt::Runtime;

/// The byte offset of a string object's first character.
pub const TEXT: u64 = 4;

/// Emits `str_new(length) -> address`: an uninitialized string of `length`
/// bytes, with its header written.
pub fn define_str_new(module: &mut Module, rt: &Runtime) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![ValType::I32], vec![addr]);

    let Some(length) = func.param(0) else {
        return false;
    };
    let object = func.local(addr);

    func.local_get(length)
        .i32_const(TEXT as i32)
        .i32_add()
        .i32_to_addr();
    func.call(rt.alloc).local_tee(object);
    func.local_get(length).i32_store(0);
    func.local_get(object);

    module.define(rt.str_new, func)
}

/// Emits `str_concat(left, right) -> address`.
///
/// Concatenation always allocates: the result is a new value, and neither
/// operand is disturbed. That is the VM's rule too, which is why `a + b` cannot
/// be observed to change `a` on either engine.
pub fn define_str_concat(module: &mut Module, rt: &Runtime) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![addr, addr], vec![addr]);

    let (Some(left), Some(right)) = (func.param(0), func.param(1)) else {
        return false;
    };
    let left_len = func.local(ValType::I32);
    let right_len = func.local(ValType::I32);
    let result = func.local(addr);

    func.local_get(left).i32_load(0).local_set(left_len);
    func.local_get(right).i32_load(0).local_set(right_len);

    func.local_get(left_len)
        .local_get(right_len)
        .i32_add()
        .call(rt.str_new)
        .local_set(result);

    func.local_get(result).addr_const(TEXT).addr_add();
    func.local_get(left).addr_const(TEXT).addr_add();
    func.local_get(left_len);
    func.call(rt.memcpy);

    func.local_get(result)
        .addr_const(TEXT)
        .addr_add()
        .local_get(left_len)
        .i32_to_addr()
        .addr_add();
    func.local_get(right).addr_const(TEXT).addr_add();
    func.local_get(right_len);
    func.call(rt.memcpy);

    func.local_get(result);

    module.define(rt.str_concat, func)
}

/// Emits `str_eq(left, right) -> i32`: byte equality.
pub fn define_str_eq(module: &mut Module, rt: &Runtime) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![addr, addr], vec![ValType::I32]);

    let (Some(left), Some(right)) = (func.param(0), func.param(1)) else {
        return false;
    };
    let length = func.local(ValType::I32);
    let cursor = func.local(ValType::I32);

    // Different lengths cannot be equal, and settling that first means the loop
    // below only ever reads inside both strings.
    func.local_get(left).i32_load(0).local_tee(length);
    func.local_get(right).i32_load(0).i32_ne();
    func.if_(Empty);
    func.i32_const(0).return_();
    func.end();

    func.i32_const(0).local_set(cursor);
    func.block(Empty);
    func.loop_(Empty);
    {
        func.local_get(cursor).local_get(length).i32_ge_u().br_if(1);

        func.local_get(left)
            .local_get(cursor)
            .i32_to_addr()
            .addr_add()
            .i32_load8_u(TEXT);
        func.local_get(right)
            .local_get(cursor)
            .i32_to_addr()
            .addr_add()
            .i32_load8_u(TEXT);
        func.i32_ne();
        func.if_(Empty);
        func.i32_const(0).return_();
        func.end();

        func.local_get(cursor)
            .i32_const(1)
            .i32_add()
            .local_set(cursor);
        func.br(0);
    }
    func.end();
    func.end();

    func.i32_const(1);

    module.define(rt.str_eq, func)
}

/// Emits `str_from_bool(value) -> address`.
///
/// Hands back a shared literal: nothing mutates a string, so `true` need only
/// exist once in the module.
pub fn define_str_from_bool(module: &mut Module, rt: &Runtime, yes: u64, no: u64) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![ValType::I32], vec![addr]);

    let Some(value) = func.param(0) else {
        return false;
    };

    func.local_get(value);
    func.if_(BlockType::Value(addr));
    func.addr_const(yes);
    func.else_();
    func.addr_const(no);
    func.end();

    module.define(rt.str_from_bool, func)
}

/// Emits `str_from_i64(value) -> address`, matching the VM's `Int` rendering.
///
/// The magnitude is taken as *unsigned*, which is what makes `Int` minimum
/// print as `-9223372036854775808` rather than trapping or wrapping: negating
/// it in two's complement lands back on itself, and its bit pattern read
/// unsigned is exactly the magnitude wanted.
pub fn define_str_from_i64(module: &mut Module, rt: &Runtime, zero: u64) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![ValType::I64], vec![addr]);

    let Some(value) = func.param(0) else {
        return false;
    };
    let magnitude = func.local(ValType::I64);
    let cursor = func.local(addr);
    let length = func.local(ValType::I32);
    let result = func.local(addr);

    func.local_get(value).i64_eqz();
    func.if_(Empty);
    func.addr_const(zero).return_();
    func.end();

    func.local_get(value)
        .i64_const(0)
        .i64_lt_s()
        .if_(BlockType::Value(ValType::I64));
    func.i64_const(0).local_get(value).i64_sub();
    func.else_();
    func.local_get(value);
    func.end();
    func.local_set(magnitude);

    func.addr_const(u64::from(layout::DIGITS_END))
        .local_set(cursor);
    func.loop_(Empty);
    {
        func.local_get(cursor)
            .addr_const(1)
            .addr_sub()
            .local_tee(cursor);
        func.i32_const(b'0' as i32)
            .local_get(magnitude)
            .i64_const(10)
            .i64_rem_u()
            .i32_wrap_i64()
            .i32_add();
        func.i32_store8(0);

        func.local_get(magnitude)
            .i64_const(10)
            .i64_div_u()
            .local_tee(magnitude);
        func.i64_eqz().i32_eqz().br_if(0);
    }
    func.end();

    func.local_get(value).i64_const(0).i64_lt_s();
    func.if_(Empty);
    func.local_get(cursor)
        .addr_const(1)
        .addr_sub()
        .local_tee(cursor);
    func.i32_const(b'-' as i32);
    func.i32_store8(0);
    func.end();

    func.addr_const(u64::from(layout::DIGITS_END))
        .local_get(cursor)
        .addr_sub()
        .addr_to_i32()
        .local_tee(length);
    func.call(rt.str_new).local_set(result);

    func.local_get(result).addr_const(TEXT).addr_add();
    func.local_get(cursor);
    func.local_get(length);
    func.call(rt.memcpy);

    func.local_get(result);

    module.define(rt.str_from_i64, func)
}

/// Emits `print_str(string)`: hands the host the bytes and their length.
pub fn define_print_str(module: &mut Module, rt: &Runtime) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![addr], Vec::new());

    let Some(string) = func.param(0) else {
        return false;
    };

    func.local_get(string).addr_const(TEXT).addr_add();
    func.local_get(string).i32_load(0);
    func.call(rt.print);

    module.define(rt.print_str, func)
}

/// Emits a trap: tells the host why, then makes the module unreachable.
///
/// The `unreachable` is what stops the program. A host that returned from
/// `trap` anyway must not see execution continue — a trapped Kira program has
/// no defined behavior left to produce, and finishing quietly would be the
/// worst possible answer.
pub fn define_trap(module: &mut Module, rt: &Runtime, index: FuncIdx, message: u64) -> bool {
    let mut func = module.func(Vec::new(), Vec::new());
    func.addr_const(message + TEXT);
    func.addr_const(message);
    func.i32_load(0);
    func.call(rt.trap);
    func.unreachable();
    module.define(index, func)
}
