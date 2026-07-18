//! Binary-operator lowering: arithmetic, comparison, division, and
//! short-circuit.
//!
//! Split from [`super`] on the file-size ladder, and cohesive on its own: this
//! is every place wasm's own semantics differ from Kira's and Kira wins.
//! Integer arithmetic wraps, which wasm already does — but `i64.div_s` traps on
//! `Int` minimum divided by `-1` where the VM wraps, division by zero is a Kira
//! trap with Kira's message rather than the engine's, and `&&`/`||` evaluate
//! their right operand only when the left decides nothing.
//!
//! Signedness is the one thing an integer's *written* width decides, so the
//! `U8`..`U64` spellings lower through `_u` instructions here. Their division
//! is deliberately shorter than the signed form: no unsigned pair overflows, so
//! there is no `MIN / -1` case to branch around.

use kira_ir::{IrBinOp, IrExprId, IrFunction};

use super::Lowering;
use crate::encode::ValType;
use crate::error::WasmError;
use crate::func::{BlockType, BlockType::Empty, Func};

impl Lowering<'_> {
    /// Lowers a binary operation.
    pub(super) fn binary(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<(), WasmError> {
        // `&&` and `||` decide whether the right operand runs at all, so they
        // are branches rather than operators.
        match op {
            IrBinOp::And => {
                self.expr(func, function, lhs)?;
                func.if_(BlockType::Value(ValType::I32));
                self.expr(func, function, rhs)?;
                func.else_();
                func.i32_const(0);
                func.end();
                return Ok(());
            }
            IrBinOp::Or => {
                self.expr(func, function, lhs)?;
                func.if_(BlockType::Value(ValType::I32));
                func.i32_const(1);
                func.else_();
                self.expr(func, function, rhs)?;
                func.end();
                return Ok(());
            }
            IrBinOp::DivInt | IrBinOp::RemInt | IrBinOp::DivUInt | IrBinOp::RemUInt => {
                return self.int_division(func, function, op, lhs, rhs);
            }
            _ => {}
        }

        self.expr(func, function, lhs)?;
        self.expr(func, function, rhs)?;

        match op {
            IrBinOp::AddInt => func.i64_add(),
            IrBinOp::SubInt => func.i64_sub(),
            IrBinOp::MulInt => func.i64_mul(),
            IrBinOp::AddFloat => func.f64_add(),
            IrBinOp::SubFloat => func.f64_sub(),
            IrBinOp::MulFloat => func.f64_mul(),
            IrBinOp::DivFloat => func.f64_div(),
            IrBinOp::EqInt => func.i64_eq(),
            IrBinOp::NeInt => func.i64_ne(),
            IrBinOp::LtInt => func.i64_lt_s(),
            IrBinOp::LeInt => func.i64_le_s(),
            IrBinOp::GtInt => func.i64_gt_s(),
            IrBinOp::GeInt => func.i64_ge_s(),
            IrBinOp::LtUInt => func.i64_lt_u(),
            IrBinOp::LeUInt => func.i64_le_u(),
            IrBinOp::GtUInt => func.i64_gt_u(),
            IrBinOp::GeUInt => func.i64_ge_u(),
            IrBinOp::EqFloat => func.f64_eq(),
            IrBinOp::NeFloat => func.f64_ne(),
            IrBinOp::LtFloat => func.f64_lt(),
            IrBinOp::LeFloat => func.f64_le(),
            IrBinOp::GtFloat => func.f64_gt(),
            IrBinOp::GeFloat => func.f64_ge(),
            IrBinOp::EqBool => func.i32_eq(),
            IrBinOp::NeBool => func.i32_ne(),
            IrBinOp::ConcatStr => func.call(self.runtime.str_concat),
            IrBinOp::EqStr => func.call(self.runtime.str_eq),
            IrBinOp::NeStr => func.call(self.runtime.str_eq).i32_eqz(),
            // wasm's shifts already take the amount modulo 64, which is the
            // rule the VM and the native backend are made to match, so no
            // masking is needed here.
            IrBinOp::BitAnd => func.i64_and(),
            IrBinOp::BitOr => func.i64_or(),
            IrBinOp::BitXor => func.i64_xor(),
            IrBinOp::Shl => func.i64_shl(),
            IrBinOp::ShrInt => func.i64_shr_s(),
            IrBinOp::ShrUInt => func.i64_shr_u(),
            // Handled above; listed so a new operator cannot fall through.
            IrBinOp::And
            | IrBinOp::Or
            | IrBinOp::DivInt
            | IrBinOp::RemInt
            | IrBinOp::DivUInt
            | IrBinOp::RemUInt => {
                return Err(WasmError::UnsupportedOperator);
            }
        };
        Ok(())
    }

    /// Lowers `/` or `%` on integers, with Kira's answers for the two cases
    /// wasm would decide differently.
    fn int_division(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<(), WasmError> {
        let left = func.local(ValType::I64);
        let right = func.local(ValType::I64);

        self.expr(func, function, lhs)?;
        self.expr(func, function, rhs)?;
        func.local_set(right);
        func.local_set(left);

        // By zero is a Kira trap, and it is raised before the engine can raise
        // its own — so a Web user reads the same sentence a VM user does.
        func.local_get(right).i64_eqz();
        func.if_(Empty);
        func.call(self.runtime.trap_div_zero).unreachable();
        func.end();

        // Unsigned division cannot overflow — there is no unsigned pair whose
        // quotient leaves the range — so once zero is excluded the wasm
        // instruction is already Kira's answer, with no guard around it.
        if matches!(op, IrBinOp::DivUInt | IrBinOp::RemUInt) {
            func.local_get(left).local_get(right);
            match op {
                IrBinOp::DivUInt => func.i64_div_u(),
                _ => func.i64_rem_u(),
            };
            return Ok(());
        }

        // `Int::MIN / -1` overflows: wasm traps, the VM wraps to `Int::MIN`,
        // and `Int::MIN % -1` is zero rather than a trap.
        func.local_get(left)
            .i64_const(i64::MIN)
            .i64_eq()
            .local_get(right)
            .i64_const(-1)
            .i64_eq()
            .i32_and();
        func.if_(BlockType::Value(ValType::I64));
        match op {
            IrBinOp::DivInt => func.i64_const(i64::MIN),
            _ => func.i64_const(0),
        };
        func.else_();
        func.local_get(left).local_get(right);
        match op {
            IrBinOp::DivInt => func.i64_div_s(),
            _ => func.i64_rem_s(),
        };
        func.end();
        Ok(())
    }
}
