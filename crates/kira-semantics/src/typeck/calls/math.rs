use kira_semantics_model::hir::HirBinaryOp;
use kira_syntax_model::ast::BinaryOp;

use super::*;

/// The wrapping arithmetic builtin `name` spells, if it spells one.
pub(super) fn wrapping_operator(name: &str) -> Option<HirBinaryOp> {
    Some(match name {
        "wrappingAdd" => HirBinaryOp::WrappingAddInt,
        "wrappingSub" => HirBinaryOp::WrappingSubInt,
        "wrappingMul" => HirBinaryOp::WrappingMulInt,
        _ => return None,
    })
}

impl Analyzer<'_> {
    /// Analyzes `sqrt(x)` and the rest of the floating-point primitives.
    pub(super) fn analyze_math_call(
        &mut self,
        op: kira_runtime_abi::MathOp,
        args: &[HirExprId],
        span: Span,
    ) -> HirExprId {
        let name = op.name();
        let expected = op.argument_count();
        if args.len() != expected {
            let arguments = if expected == 1 {
                "argument"
            } else {
                "arguments"
            };
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {expected} {arguments}, and this call passes {}",
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // Every operand is checked before any is coerced, so a two-operand call
        // that is wrong in its second argument says so about that argument
        // rather than about the call.
        let mut operands = Vec::with_capacity(expected);
        for &arg in args {
            let actual = self.program.expr(arg).type_of();
            if !actual.assignable_to(Type::FLOAT) {
                self.emit(
                    span,
                    "KSEM063",
                    format!(
                        "`{name}` takes a `Float`, and this call passes a `{}`",
                        self.type_name(actual)
                    ),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            operands.push(self.coerce_into(arg, Type::FLOAT));
        }
        self.program
            .exprs
            .alloc(HirExpr::MathOperation { op, operands })
    }

    /// Analyzes `scalarText(codePoint)` — one Unicode scalar as text.
    pub(super) fn analyze_scalar_text_call(&mut self, args: &[HirExprId], span: Span) -> HirExprId {
        let [value] = args else {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`scalarText` takes one argument, and this call passes {}",
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let actual = self.program.expr(*value).type_of();
        if !actual.assignable_to(Type::INT) {
            self.emit(
                span,
                "KSEM063",
                format!(
                    "`scalarText` takes an `Int` code point, and this call passes a `{}`",
                    self.type_name(actual)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let value = self.coerce_into(*value, Type::INT);
        self.program.exprs.alloc(HirExpr::ScalarText { value })
    }

    /// Analyzes `wrappingAdd(a, b)`, `wrappingSub`, and `wrappingMul`: integer
    /// arithmetic that wraps at the operands' written width where `+`, `-`,
    /// and `*` trap.
    ///
    /// The operands agree on a spelling the way `+`'s do, and the result has
    /// that spelling. A fold that wants the wrap — a hash, a checksum, a
    /// pseudo-random step — spells it here, so ordinary arithmetic can keep
    /// refusing to lose a value.
    pub(super) fn analyze_wrapping_call(
        &mut self,
        name: &str,
        op: HirBinaryOp,
        args: &[HirExprId],
        span: Span,
    ) -> HirExprId {
        let [lhs, rhs] = args else {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes 2 arguments, and this call passes {}",
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let (lt, rt) = (
            self.program.expr(*lhs).type_of(),
            self.program.expr(*rhs).type_of(),
        );
        let Some((_, ty)) = crate::operators::resolve_binary(BinaryOp::Add, lt, rt)
            .filter(|(_, ty)| matches!(ty, Type::Int(_)))
        else {
            self.emit(
                span,
                "KSEM063",
                format!(
                    "`{name}` takes two integers of one spelling, and this call passes `{}` and `{}`",
                    self.type_name(lt),
                    self.type_name(rt)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::Binary {
            op,
            lhs: *lhs,
            rhs: *rhs,
            ty,
        })
    }
}
