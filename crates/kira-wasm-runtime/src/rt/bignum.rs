//! Generated arbitrary-precision integers, in five fixed registers.
//!
//! These exist for one caller: the float formatter. Shortest-round-trip
//! rendering of an `f64` is a question about exact rationals — "is this
//! numerator, scaled, closer to the value than its neighbours?" — and answering
//! it in `f64` or `i64` arithmetic answers a different, rounded question. So the
//! module carries exact integers and pays for them only when a float is
//! printed.
//!
//! A register is a length word then [`layout::BIGNUM_LIMBS`] little-endian
//! 32-bit limbs. The length counts significant limbs, so zero is length zero and
//! no value carries a leading zero limb — which makes comparison a length check
//! and then a walk down from the top.

use crate::encode::ValType;
use crate::func::{BlockType::Empty, BlockType::Value, Func, FuncIdx};
use crate::layout;
use crate::module::Module;

/// The shift and power-of-ten helpers, which are half this file's bulk.
mod shift;

/// The handles to the generated big-integer helpers.
#[derive(Debug, Clone, Copy)]
pub struct Bignum {
    /// `set_u64(register, value)`
    pub set: FuncIdx,
    /// `copy(destination, source)`
    pub copy: FuncIdx,
    /// `cmp(left, right) -> -1 | 0 | 1`
    pub cmp: FuncIdx,
    /// `add(left, right)`: `left += right`
    pub add: FuncIdx,
    /// `sub(left, right)`: `left -= right`, for `left >= right`
    pub sub: FuncIdx,
    /// `mul_small(register, multiplier)`
    pub mul_small: FuncIdx,
    /// `shl1(register)`: `register *= 2`
    pub shl1: FuncIdx,
    /// `shl(register, bits)`
    pub shl: FuncIdx,
    /// `mul_pow10(register, power)`
    pub mul_pow10: FuncIdx,
    /// `trim(register)`: drop leading zero limbs
    pub trim: FuncIdx,
}

impl Bignum {
    /// Reserves an index for each helper.
    pub fn declare(module: &mut Module) -> Self {
        let addr = module.addr().val();
        Self {
            set: module.declare(vec![addr, ValType::I64], Vec::new()),
            copy: module.declare(vec![addr, addr], Vec::new()),
            cmp: module.declare(vec![addr, addr], vec![ValType::I32]),
            add: module.declare(vec![addr, addr], Vec::new()),
            sub: module.declare(vec![addr, addr], Vec::new()),
            mul_small: module.declare(vec![addr, ValType::I32], Vec::new()),
            shl1: module.declare(vec![addr], Vec::new()),
            shl: module.declare(vec![addr, ValType::I32], Vec::new()),
            mul_pow10: module.declare(vec![addr, ValType::I32], Vec::new()),
            trim: module.declare(vec![addr], Vec::new()),
        }
    }

    /// Emits every helper's body.
    ///
    /// `overflow` is called when a value would run past its register — a
    /// bound the layout argues is unreachable, guarded anyway, because a
    /// silently truncated numerator would print a plausible wrong number.
    pub fn define(&self, module: &mut Module, overflow: FuncIdx) -> bool {
        self.define_set(module)
            && self.define_copy(module)
            && self.define_cmp(module)
            && self.define_add(module, overflow)
            && self.define_sub(module)
            && self.define_mul_small(module, overflow)
            && self.define_shl1(module, overflow)
            && self.define_shl(module, overflow)
            && self.define_mul_pow10(module)
            && self.define_trim(module)
    }

    /// Pushes the address of limb `index` of the register in `register`.
    ///
    /// Both operands come off the stack: this is the one place limb addressing
    /// is spelled, so a register's shape is described once.
    pub(super) fn limb(
        func: &mut Func,
        register: crate::func::LocalIdx,
        index: crate::func::LocalIdx,
    ) {
        func.local_get(register);
        func.local_get(index)
            .i32_to_addr()
            .addr_const(4)
            .addr_mul()
            .addr_add();
    }

    /// `set_u64(register, value)`
    fn define_set(&self, module: &mut Module) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, ValType::I64], Vec::new());
        let (Some(register), Some(value)) = (func.param(0), func.param(1)) else {
            return false;
        };

        func.local_get(register)
            .local_get(value)
            .i32_wrap_i64()
            .i32_store(4);
        func.local_get(register)
            .local_get(value)
            .i64_const(32)
            .i64_shr_u()
            .i32_wrap_i64()
            .i32_store(8);

        // The length is what the value needs: two limbs, one, or none at all.
        func.local_get(register);
        func.local_get(value)
            .i64_const(32)
            .i64_shr_u()
            .i64_eqz()
            .i32_eqz();
        func.if_(Value(ValType::I32));
        func.i32_const(2);
        func.else_();
        func.local_get(value).i64_eqz();
        func.if_(Value(ValType::I32));
        func.i32_const(0);
        func.else_();
        func.i32_const(1);
        func.end();
        func.end();
        func.i32_store(0);

        module.define(self.set, func)
    }

    /// `copy(destination, source)`
    fn define_copy(&self, module: &mut Module) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, addr], Vec::new());
        let (Some(destination), Some(source)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let length = func.local(ValType::I32);
        let index = func.local(ValType::I32);

        func.local_get(source).i32_load(0).local_set(length);
        func.local_get(destination).local_get(length).i32_store(0);

        func.i32_const(0).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).local_get(length).i32_ge_u().br_if(1);
            Self::limb(&mut func, destination, index);
            Self::limb(&mut func, source, index);
            func.i32_load(4);
            func.i32_store(4);
            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        module.define(self.copy, func)
    }

    /// `cmp(left, right) -> -1 | 0 | 1`
    fn define_cmp(&self, module: &mut Module) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, addr], vec![ValType::I32]);
        let (Some(left), Some(right)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let left_len = func.local(ValType::I32);
        let right_len = func.local(ValType::I32);
        let index = func.local(ValType::I32);

        // No leading zero limbs, so a longer value is a larger value.
        func.local_get(left).i32_load(0).local_set(left_len);
        func.local_get(right).i32_load(0).local_set(right_len);
        func.local_get(left_len).local_get(right_len).i32_ne();
        func.if_(Empty);
        func.local_get(left_len)
            .local_get(right_len)
            .i32_gt_u()
            .if_(Value(ValType::I32));
        func.i32_const(1);
        func.else_();
        func.i32_const(-1);
        func.end();
        func.return_();
        func.end();

        func.local_get(left_len).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).i32_eqz().br_if(1);
            func.local_get(index)
                .i32_const(1)
                .i32_sub()
                .local_set(index);

            Self::limb(&mut func, left, index);
            func.i32_load(4);
            Self::limb(&mut func, right, index);
            func.i32_load(4);
            func.i32_ne();
            func.if_(Empty);
            {
                Self::limb(&mut func, left, index);
                func.i32_load(4);
                Self::limb(&mut func, right, index);
                func.i32_load(4);
                func.i32_gt_u();
                func.if_(Value(ValType::I32));
                func.i32_const(1);
                func.else_();
                func.i32_const(-1);
                func.end();
                func.return_();
            }
            func.end();
            func.br(0);
        }
        func.end();
        func.end();

        func.i32_const(0);
        module.define(self.cmp, func)
    }

    /// `add(left, right)`: `left += right`
    fn define_add(&self, module: &mut Module, overflow: FuncIdx) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, addr], Vec::new());
        let (Some(left), Some(right)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let left_len = func.local(ValType::I32);
        let right_len = func.local(ValType::I32);
        let length = func.local(ValType::I32);
        let index = func.local(ValType::I32);
        let carry = func.local(ValType::I64);
        let sum = func.local(ValType::I64);

        func.local_get(left).i32_load(0).local_set(left_len);
        func.local_get(right).i32_load(0).local_set(right_len);

        func.local_get(left_len)
            .local_get(right_len)
            .i32_gt_u()
            .if_(Value(ValType::I32));
        func.local_get(left_len);
        func.else_();
        func.local_get(right_len);
        func.end();
        func.local_set(length);

        func.i64_const(0).local_set(carry);
        func.i32_const(0).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).local_get(length).i32_ge_u().br_if(1);

            // A limb past a value's length reads as zero rather than as
            // whatever the register last held.
            func.local_get(carry);
            Self::limb_or_zero(&mut func, left, index, left_len);
            func.i64_add();
            Self::limb_or_zero(&mut func, right, index, right_len);
            func.i64_add();
            func.local_set(sum);

            Self::limb(&mut func, left, index);
            func.local_get(sum).i32_wrap_i64();
            func.i32_store(4);

            func.local_get(sum)
                .i64_const(32)
                .i64_shr_u()
                .local_set(carry);
            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        func.local_get(carry).i64_eqz().i32_eqz();
        func.if_(Empty);
        {
            Self::guard(&mut func, length, overflow);
            Self::limb(&mut func, left, length);
            func.local_get(carry).i32_wrap_i64();
            func.i32_store(4);
            func.local_get(length)
                .i32_const(1)
                .i32_add()
                .local_set(length);
        }
        func.end();

        func.local_get(left).local_get(length).i32_store(0);
        module.define(self.add, func)
    }

    /// `sub(left, right)`: `left -= right`, for `left >= right`
    fn define_sub(&self, module: &mut Module) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, addr], Vec::new());
        let (Some(left), Some(right)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let left_len = func.local(ValType::I32);
        let right_len = func.local(ValType::I32);
        let index = func.local(ValType::I32);
        let borrow = func.local(ValType::I64);
        let difference = func.local(ValType::I64);

        func.local_get(left).i32_load(0).local_set(left_len);
        func.local_get(right).i32_load(0).local_set(right_len);

        func.i64_const(0).local_set(borrow);
        func.i32_const(0).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index)
                .local_get(left_len)
                .i32_ge_u()
                .br_if(1);

            Self::limb_or_zero(&mut func, left, index, left_len);
            Self::limb_or_zero(&mut func, right, index, right_len);
            func.i64_sub();
            func.local_get(borrow).i64_sub();
            func.local_set(difference);

            Self::limb(&mut func, left, index);
            func.local_get(difference).i32_wrap_i64();
            func.i32_store(4);

            // The subtraction wrapped exactly when it needed a borrow, and the
            // wrap sets every bit above the low limb.
            func.local_get(difference)
                .i64_const(32)
                .i64_shr_u()
                .i64_eqz()
                .i32_eqz()
                .i64_extend_i32_u()
                .local_set(borrow);

            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        func.local_get(left).local_get(left_len).i32_store(0);
        func.local_get(left).call(self.trim);
        module.define(self.sub, func)
    }

    /// `mul_small(register, multiplier)`
    fn define_mul_small(&self, module: &mut Module, overflow: FuncIdx) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, ValType::I32], Vec::new());
        let (Some(register), Some(multiplier)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let length = func.local(ValType::I32);
        let index = func.local(ValType::I32);
        let carry = func.local(ValType::I64);
        let product = func.local(ValType::I64);

        func.local_get(register).i32_load(0).local_set(length);
        func.i64_const(0).local_set(carry);
        func.i32_const(0).local_set(index);

        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).local_get(length).i32_ge_u().br_if(1);

            Self::limb(&mut func, register, index);
            func.i32_load(4)
                .i64_extend_i32_u()
                .local_get(multiplier)
                .i64_extend_i32_u()
                .i64_mul()
                .local_get(carry)
                .i64_add()
                .local_set(product);

            Self::limb(&mut func, register, index);
            func.local_get(product).i32_wrap_i64();
            func.i32_store(4);

            func.local_get(product)
                .i64_const(32)
                .i64_shr_u()
                .local_set(carry);
            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        // A 32x32 product carries at most another 32 bits, but the carry is
        // drained in a loop anyway so the shape does not depend on that.
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(carry).i64_eqz().br_if(1);
            Self::guard(&mut func, length, overflow);

            Self::limb(&mut func, register, length);
            func.local_get(carry).i32_wrap_i64();
            func.i32_store(4);

            func.local_get(carry)
                .i64_const(32)
                .i64_shr_u()
                .local_set(carry);
            func.local_get(length)
                .i32_const(1)
                .i32_add()
                .local_set(length);
            func.br(0);
        }
        func.end();
        func.end();

        func.local_get(register).local_get(length).i32_store(0);
        func.local_get(register).call(self.trim);
        module.define(self.mul_small, func)
    }

    /// `trim(register)`: drop leading zero limbs.
    ///
    /// Every operation that can shorten a value ends here, which is what lets
    /// `cmp` decide on lengths alone.
    fn define_trim(&self, module: &mut Module) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr], Vec::new());
        let Some(register) = func.param(0) else {
            return false;
        };
        let length = func.local(ValType::I32);
        let index = func.local(ValType::I32);

        func.local_get(register).i32_load(0).local_set(length);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(length).i32_eqz().br_if(1);
            func.local_get(length)
                .i32_const(1)
                .i32_sub()
                .local_set(index);
            Self::limb(&mut func, register, index);
            func.i32_load(4).i32_eqz().i32_eqz().br_if(1);
            func.local_get(index).local_set(length);
            func.br(0);
        }
        func.end();
        func.end();

        func.local_get(register).local_get(length).i32_store(0);
        module.define(self.trim, func)
    }

    /// Pushes limb `index` of `register` as a `u64`, or zero past its length.
    fn limb_or_zero(
        func: &mut Func,
        register: crate::func::LocalIdx,
        index: crate::func::LocalIdx,
        length: crate::func::LocalIdx,
    ) {
        func.local_get(index).local_get(length).i32_lt_u();
        func.if_(Value(ValType::I64));
        Self::limb(func, register, index);
        func.i32_load(4).i64_extend_i32_u();
        func.else_();
        func.i64_const(0);
        func.end();
    }

    /// Traps when limb `index` would fall outside a register.
    pub(super) fn guard(func: &mut Func, index: crate::func::LocalIdx, overflow: FuncIdx) {
        func.local_get(index)
            .i32_const(layout::BIGNUM_LIMBS as i32)
            .i32_ge_u();
        func.if_(Empty);
        func.call(overflow).unreachable();
        func.end();
    }
}
