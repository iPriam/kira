//! Operator syntax backed by conventional struct methods.
//!
//! The reference language lowers `a + b`, `a - b`, `a * b`, and `a / b` to the
//! receiver's `add`, `subtract`, `multiply`, and `divide` methods when the left
//! side is a named value that declares one. Keeping this desugar in semantics
//! means the VM, LLVM, hybrid, and Web paths all receive the same ordinary call
//! and do not each invent an operator-overload ABI.

use kira_semantics_model::Type;
use kira_semantics_model::hir::HirExprId;
use kira_syntax_model::ast::{BinaryOp, ExprId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Lowers a supported struct operator to its conventional method, or
    /// returns `None` so the caller can report the ordinary operator refusal.
    pub(super) fn analyze_binary_operator_method(
        &mut self,
        ctx: &mut FnCtx,
        op: BinaryOp,
        lhs: ExprId,
        lhs_hir: HirExprId,
        rhs_hir: HirExprId,
        span: kira_source::Span,
    ) -> Option<HirExprId> {
        let lhs_ty = self.program.expr(lhs_hir).type_of();
        let rhs_ty = self.program.expr(rhs_hir).type_of();
        let method = match op {
            BinaryOp::Add => "add",
            BinaryOp::Sub => "subtract",
            BinaryOp::Mul => "multiply",
            BinaryOp::Div => "divide",
            _ => return None,
        };
        let Type::Struct(receiver) = lhs_ty else {
            return None;
        };
        let qualified = format!("{}.{}", self.member_owner_name(lhs_ty), method);
        let candidates: Vec<_> = self
            .visible_overloads(&qualified)
            .into_iter()
            .filter(|&id| self.param_types(id).first() == Some(&Type::Struct(receiver)))
            .collect();
        if candidates.is_empty() {
            return None;
        }

        // A method name may itself be overloaded. Resolve it with the same
        // specificity and default rules as a written method call, while the
        // receiver restriction above prevents a method from a different type
        // with the same display name from entering the set.
        let id = match self.resolve_overload(&candidates, &[lhs_ty, rhs_ty]) {
            Ok(id) => id,
            Err(crate::typeck::overloads::OverloadFailure::Ambiguous(winners)) => {
                let list = self.overload_list(&winners);
                self.emit(
                    span,
                    "KSEM275",
                    format!("this operator call of `{qualified}` fits {list} equally well"),
                );
                return Some(
                    self.program
                        .exprs
                        .alloc(kira_semantics_model::hir::HirExpr::Error),
                );
            }
            Err(crate::typeck::overloads::OverloadFailure::None) => candidates[0],
        };

        // The operands were already analyzed in source order. Reusing their
        // HIR handles makes this a true desugar: no operand is evaluated twice,
        // and the regular call checker still performs the final type coercions.
        let call = self.analyze_user_call_hinted(&qualified, &[lhs_hir, rhs_hir], span, Some(id));
        if self.mutates_self(id) {
            // A mutating operator is still a method call: preserve the same
            // writeback rule as `value.method(argument)`.
            self.record_mut_receiver(ctx, call, lhs);
        } else {
            // A non-mutating method borrows the receiver, so reading a struct
            // field or temporary as its left operand does not create a second
            // owner when the call returns.
            self.excuse_drop_extraction(lhs_hir);
        }
        Some(call)
    }
}
