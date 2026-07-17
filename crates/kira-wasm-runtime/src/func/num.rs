//! The numeric instructions, named as the format names them.
//!
//! Split from the builder itself only for size: these are one method per
//! opcode, they carry no state, and they are the half of the surface that grows
//! every time a lowering needs an operator it did not need before.

use super::{Func, op};

/// Numeric instructions, named as the format names them.
impl Func {
    /// Emits a single-byte numeric opcode.
    ///
    /// The wrappers below are the vocabulary; this is the shared spelling.
    fn simple(&mut self, opcode: u8) -> &mut Self {
        self.code.byte(opcode);
        self
    }

    /// Emits `i32.eqz`.
    pub fn i32_eqz(&mut self) -> &mut Self {
        self.simple(op::I32_EQZ)
    }
    /// Emits `i32.eq`.
    pub fn i32_eq(&mut self) -> &mut Self {
        self.simple(op::I32_EQ)
    }
    /// Emits `i32.ne`.
    pub fn i32_ne(&mut self) -> &mut Self {
        self.simple(op::I32_NE)
    }
    /// Emits `i32.lt_s`.
    pub fn i32_lt_s(&mut self) -> &mut Self {
        self.simple(op::I32_LT_S)
    }
    /// Emits `i32.lt_u`.
    pub fn i32_lt_u(&mut self) -> &mut Self {
        self.simple(op::I32_LT_U)
    }
    /// Emits `i32.gt_s`.
    pub fn i32_gt_s(&mut self) -> &mut Self {
        self.simple(op::I32_GT_S)
    }
    /// Emits `i32.gt_u`.
    pub fn i32_gt_u(&mut self) -> &mut Self {
        self.simple(op::I32_GT_U)
    }
    /// Emits `i32.le_s`.
    pub fn i32_le_s(&mut self) -> &mut Self {
        self.simple(op::I32_LE_S)
    }
    /// Emits `i32.ge_s`.
    pub fn i32_ge_s(&mut self) -> &mut Self {
        self.simple(op::I32_GE_S)
    }
    /// Emits `i32.ge_u`.
    pub fn i32_ge_u(&mut self) -> &mut Self {
        self.simple(op::I32_GE_U)
    }
    /// Emits `i32.add`.
    pub fn i32_add(&mut self) -> &mut Self {
        self.simple(op::I32_ADD)
    }
    /// Emits `i32.sub`.
    pub fn i32_sub(&mut self) -> &mut Self {
        self.simple(op::I32_SUB)
    }
    /// Emits `i32.mul`.
    pub fn i32_mul(&mut self) -> &mut Self {
        self.simple(op::I32_MUL)
    }
    /// Emits `i32.div_u`.
    pub fn i32_div_u(&mut self) -> &mut Self {
        self.simple(op::I32_DIV_U)
    }
    /// Emits `i32.and`.
    pub fn i32_and(&mut self) -> &mut Self {
        self.simple(op::I32_AND)
    }
    /// Emits `i32.or`.
    pub fn i32_or(&mut self) -> &mut Self {
        self.simple(op::I32_OR)
    }
    /// Emits `i32.shl`.
    pub fn i32_shl(&mut self) -> &mut Self {
        self.simple(op::I32_SHL)
    }
    /// Emits `i32.shr_u`.
    pub fn i32_shr_u(&mut self) -> &mut Self {
        self.simple(op::I32_SHR_U)
    }
    /// Emits `i32.wrap_i64`.
    pub fn i32_wrap_i64(&mut self) -> &mut Self {
        self.simple(op::I32_WRAP_I64)
    }

    /// Emits `i64.clz`.
    pub fn i64_clz(&mut self) -> &mut Self {
        self.simple(op::I64_CLZ)
    }
    /// Emits `i64.eqz`.
    pub fn i64_eqz(&mut self) -> &mut Self {
        self.simple(op::I64_EQZ)
    }
    /// Emits `i64.eq`.
    pub fn i64_eq(&mut self) -> &mut Self {
        self.simple(op::I64_EQ)
    }
    /// Emits `i64.ne`.
    pub fn i64_ne(&mut self) -> &mut Self {
        self.simple(op::I64_NE)
    }
    /// Emits `i64.lt_s`.
    pub fn i64_lt_s(&mut self) -> &mut Self {
        self.simple(op::I64_LT_S)
    }
    /// Emits `i64.gt_s`.
    pub fn i64_gt_s(&mut self) -> &mut Self {
        self.simple(op::I64_GT_S)
    }
    /// Emits `i64.gt_u`.
    pub fn i64_gt_u(&mut self) -> &mut Self {
        self.simple(op::I64_GT_U)
    }
    /// Emits `i64.ge_u`.
    pub fn i64_ge_u(&mut self) -> &mut Self {
        self.simple(op::I64_GE_U)
    }
    /// Emits `i64.le_s`.
    pub fn i64_le_s(&mut self) -> &mut Self {
        self.simple(op::I64_LE_S)
    }
    /// Emits `i64.ge_s`.
    pub fn i64_ge_s(&mut self) -> &mut Self {
        self.simple(op::I64_GE_S)
    }
    /// Emits `i64.add`.
    pub fn i64_add(&mut self) -> &mut Self {
        self.simple(op::I64_ADD)
    }
    /// Emits `i64.sub`.
    pub fn i64_sub(&mut self) -> &mut Self {
        self.simple(op::I64_SUB)
    }
    /// Emits `i64.mul`.
    pub fn i64_mul(&mut self) -> &mut Self {
        self.simple(op::I64_MUL)
    }
    /// Emits `i64.div_s`.
    pub fn i64_div_s(&mut self) -> &mut Self {
        self.simple(op::I64_DIV_S)
    }
    /// Emits `i64.div_u`.
    pub fn i64_div_u(&mut self) -> &mut Self {
        self.simple(op::I64_DIV_U)
    }
    /// Emits `i64.rem_s`.
    pub fn i64_rem_s(&mut self) -> &mut Self {
        self.simple(op::I64_REM_S)
    }
    /// Emits `i64.rem_u`.
    pub fn i64_rem_u(&mut self) -> &mut Self {
        self.simple(op::I64_REM_U)
    }
    /// Emits `i64.and`.
    pub fn i64_and(&mut self) -> &mut Self {
        self.simple(op::I64_AND)
    }
    /// Emits `i64.or`.
    pub fn i64_or(&mut self) -> &mut Self {
        self.simple(op::I64_OR)
    }
    /// Emits `i64.shl`.
    pub fn i64_shl(&mut self) -> &mut Self {
        self.simple(op::I64_SHL)
    }
    /// Emits `i64.shr_u`.
    pub fn i64_shr_u(&mut self) -> &mut Self {
        self.simple(op::I64_SHR_U)
    }
    /// Emits `i64.extend_i32_u`.
    pub fn i64_extend_i32_u(&mut self) -> &mut Self {
        self.simple(op::I64_EXTEND_I32_U)
    }
    /// Emits `i64.reinterpret_f64`.
    pub fn i64_reinterpret_f64(&mut self) -> &mut Self {
        self.simple(op::I64_REINTERPRET_F64)
    }

    /// Emits `f64.eq`.
    pub fn f64_eq(&mut self) -> &mut Self {
        self.simple(op::F64_EQ)
    }
    /// Emits `f64.ne`.
    pub fn f64_ne(&mut self) -> &mut Self {
        self.simple(op::F64_NE)
    }
    /// Emits `f64.lt`.
    pub fn f64_lt(&mut self) -> &mut Self {
        self.simple(op::F64_LT)
    }
    /// Emits `f64.gt`.
    pub fn f64_gt(&mut self) -> &mut Self {
        self.simple(op::F64_GT)
    }
    /// Emits `f64.le`.
    pub fn f64_le(&mut self) -> &mut Self {
        self.simple(op::F64_LE)
    }
    /// Emits `f64.ge`.
    pub fn f64_ge(&mut self) -> &mut Self {
        self.simple(op::F64_GE)
    }
    /// Emits `f64.add`.
    pub fn f64_add(&mut self) -> &mut Self {
        self.simple(op::F64_ADD)
    }
    /// Emits `f64.sub`.
    pub fn f64_sub(&mut self) -> &mut Self {
        self.simple(op::F64_SUB)
    }
    /// Emits `f64.mul`.
    pub fn f64_mul(&mut self) -> &mut Self {
        self.simple(op::F64_MUL)
    }
    /// Emits `f64.div`.
    pub fn f64_div(&mut self) -> &mut Self {
        self.simple(op::F64_DIV)
    }
    /// Emits `f64.ceil`.
    pub fn f64_ceil(&mut self) -> &mut Self {
        self.simple(op::F64_CEIL)
    }
    /// Emits `f64.convert_i32_s`.
    pub fn f64_convert_i32_s(&mut self) -> &mut Self {
        self.simple(op::F64_CONVERT_I32_S)
    }
    /// Emits `i32.trunc_f64_s`.
    pub fn i32_trunc_f64_s(&mut self) -> &mut Self {
        self.simple(op::I32_TRUNC_F64_S)
    }
    /// Emits `f64.abs`.
    pub fn f64_abs(&mut self) -> &mut Self {
        self.simple(op::F64_ABS)
    }
    /// Emits `f64.neg`.
    pub fn f64_neg(&mut self) -> &mut Self {
        self.simple(op::F64_NEG)
    }
}
