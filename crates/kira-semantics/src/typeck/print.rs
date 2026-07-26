//! `print(x)`: the one builtin that takes any printable value.
//!
//! Its own file because it answers a question no other call does — what can be
//! *rendered* — and that set is the contract `String(x)` mirrors.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{Builtin, Callee, HirExpr, HirExprId};

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    pub(super) fn analyze_print(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        if args.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`print` takes exactly one argument, found {}", args.len()),
            );
        } else {
            let arg_ty = self.program.expr(args[0]).type_of();
            if arg_ty != Type::Error && !arg_ty.is_printable() {
                self.emit(
                    span,
                    "KSEM081",
                    format!(
                        "`print` cannot format a value of type `{}`",
                        self.type_name(arg_ty)
                    ),
                );
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::Print),
            args: args.to_vec(),
            ty: Type::Void,
            writebacks: Vec::new(),
        })
    }
}
