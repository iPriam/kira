//! Doubling, shifting, and scaling by powers of ten.
//!
//! Split from the core operations for size, but they share a theme: each is a
//! multiplication the float formatter needs in bulk, and each is written for
//! the shape of the work rather than for the shortest code. `shl` moves limbs
//! instead of doubling and `mul_pow10` takes nine digits a pass — a setup that
//! did neither ran hundreds of passes over a whole register to print one float.

use crate::encode::ValType;
use crate::func::{BlockType::Empty, FuncIdx};
use crate::module::Module;

use super::Bignum;

impl Bignum {
    /// `shl1(register)`: `register *= 2`
    pub(super) fn define_shl1(&self, module: &mut Module, overflow: FuncIdx) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr], Vec::new());
        let Some(register) = func.param(0) else {
            return false;
        };
        let length = func.local(ValType::I32);
        let index = func.local(ValType::I32);
        let carry = func.local(ValType::I32);
        let limb = func.local(ValType::I32);

        func.local_get(register).i32_load(0).local_set(length);
        func.i32_const(0).local_set(carry);
        func.i32_const(0).local_set(index);

        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).local_get(length).i32_ge_u().br_if(1);

            Self::limb(&mut func, register, index);
            func.i32_load(4).local_set(limb);

            Self::limb(&mut func, register, index);
            func.local_get(limb)
                .i32_const(1)
                .i32_shl()
                .local_get(carry)
                .i32_or();
            func.i32_store(4);

            func.local_get(limb)
                .i32_const(31)
                .i32_shr_u()
                .local_set(carry);
            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        func.local_get(carry);
        func.if_(Empty);
        {
            Self::guard(&mut func, length, overflow);
            Self::limb(&mut func, register, length);
            func.i32_const(1);
            func.i32_store(4);
            func.local_get(length)
                .i32_const(1)
                .i32_add()
                .local_set(length);
            func.local_get(register).local_get(length).i32_store(0);
        }
        func.end();

        module.define(self.shl1, func)
    }

    /// `shl(register, bits)`
    ///
    /// Moves whole limbs, then the leftover bits — not `bits` doublings.
    ///
    /// The difference is not academic. A subnormal's denominator is `2^1075`,
    /// so the doubling version ran about 1100 passes over the whole register to
    /// set up a single printed float. That is invisible under a JIT and brutal
    /// under an interpreter, and every engine paid it; this is one pass.
    pub(super) fn define_shl(&self, module: &mut Module, overflow: FuncIdx) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, ValType::I32], Vec::new());
        let (Some(register), Some(bits)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let length = func.local(ValType::I32);
        let limbs = func.local(ValType::I32);
        let rest = func.local(ValType::I32);
        let grown = func.local(ValType::I32);
        let index = func.local(ValType::I32);
        let target = func.local(ValType::I32);
        let limb = func.local(ValType::I32);
        let top = func.local(ValType::I32);

        // Zero shifts nothing, and zero *is* nothing: both leave the register
        // alone, and both would otherwise index limb `-1` below.
        func.local_get(bits).i32_eqz();
        func.if_(Empty);
        func.return_();
        func.end();
        func.local_get(register).i32_load(0).local_tee(length);
        func.i32_eqz();
        func.if_(Empty);
        func.return_();
        func.end();

        func.local_get(bits)
            .i32_const(5)
            .i32_shr_u()
            .local_set(limbs);
        func.local_get(bits).i32_const(31).i32_and().local_set(rest);

        // A partial shift can spill into one more limb than the whole ones.
        func.local_get(length).local_get(limbs).i32_add();
        func.local_get(rest)
            .i32_eqz()
            .i32_eqz()
            .i32_add()
            .local_set(grown);
        func.local_get(grown).i32_const(1).i32_sub().local_set(top);
        Self::guard(&mut func, top, overflow);

        // The limbs the value is about to occupy start empty, so the high half
        // of a spilling limb can be merged in without reading stale bits.
        func.local_get(length).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).local_get(grown).i32_ge_u().br_if(1);
            Self::limb(&mut func, register, index);
            func.i32_const(0);
            func.i32_store(4);
            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        // High to low, so a limb is read before the limb below it overwrites
        // the place it is going.
        func.local_get(length).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).i32_eqz().br_if(1);
            func.local_get(index)
                .i32_const(1)
                .i32_sub()
                .local_set(index);
            func.local_get(index)
                .local_get(limbs)
                .i32_add()
                .local_set(target);

            Self::limb(&mut func, register, index);
            func.i32_load(4).local_set(limb);

            func.local_get(rest).i32_eqz();
            func.if_(Empty);
            {
                Self::limb(&mut func, register, target);
                func.local_get(limb);
                func.i32_store(4);
            }
            func.else_();
            {
                // The bits that do not fit join the limb above, which already
                // holds the low half of the limb that was above this one.
                func.local_get(target).i32_const(1).i32_add().local_set(top);
                Self::limb(&mut func, register, top);
                Self::limb(&mut func, register, top);
                func.i32_load(4);
                func.local_get(limb)
                    .i32_const(32)
                    .local_get(rest)
                    .i32_sub()
                    .i32_shr_u()
                    .i32_or();
                func.i32_store(4);

                Self::limb(&mut func, register, target);
                func.local_get(limb).local_get(rest).i32_shl();
                func.i32_store(4);
            }
            func.end();
            func.br(0);
        }
        func.end();
        func.end();

        // Everything below the shift is now zero.
        func.i32_const(0).local_set(index);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(index).local_get(limbs).i32_ge_u().br_if(1);
            Self::limb(&mut func, register, index);
            func.i32_const(0);
            func.i32_store(4);
            func.local_get(index)
                .i32_const(1)
                .i32_add()
                .local_set(index);
            func.br(0);
        }
        func.end();
        func.end();

        func.local_get(register).local_get(grown).i32_store(0);
        // The spill limb may have taken nothing, so the length is trimmed
        // rather than assumed.
        func.local_get(register).call(self.trim);
        module.define(self.shl, func)
    }

    /// `mul_pow10(register, power)`
    ///
    /// Nine powers at a time: `10^9` is the largest power of ten that fits a
    /// limb, so one pass does the work of nine. A float's setup scales by up to
    /// `10^340`, which is 38 passes rather than 340.
    pub(super) fn define_mul_pow10(&self, module: &mut Module) -> bool {
        let addr = module.addr().val();
        let mut func = module.func(vec![addr, ValType::I32], Vec::new());
        let (Some(register), Some(power)) = (func.param(0), func.param(1)) else {
            return false;
        };
        let remaining = func.local(ValType::I32);

        func.local_get(power).local_set(remaining);
        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(remaining).i32_const(9).i32_lt_u().br_if(1);
            func.local_get(register)
                .i32_const(1_000_000_000)
                .call(self.mul_small);
            func.local_get(remaining)
                .i32_const(9)
                .i32_sub()
                .local_set(remaining);
            func.br(0);
        }
        func.end();
        func.end();

        func.block(Empty);
        func.loop_(Empty);
        {
            func.local_get(remaining).i32_eqz().br_if(1);
            func.local_get(register).i32_const(10).call(self.mul_small);
            func.local_get(remaining)
                .i32_const(1)
                .i32_sub()
                .local_set(remaining);
            func.br(0);
        }
        func.end();
        func.end();

        module.define(self.mul_pow10, func)
    }
}
