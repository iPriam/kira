use super::*;

impl Analyzer<'_> {
    /// Analyzes `sqrt(x)` and the rest of the floating-point primitives.
    pub(super) fn analyze_math_call(
        &mut self,
        op: kira_runtime_abi::MathOp,
        args: &[HirExprId],
        span: kira_source::Span,
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
    pub(super) fn analyze_scalar_text_call(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
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
}
