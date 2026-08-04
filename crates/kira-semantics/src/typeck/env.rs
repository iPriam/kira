//! Compiler-recognized environment intrinsics.
//!
//! The primitives Foundation's `Env` surface is built on. An intrinsic rather
//! than an ordinary function for the same reason the file-system ones are:
//! reading the process environment is an effect the *engine* performs through
//! its host, not something a Kira function body can express — the VM asks
//! whatever host it was given, and native code calls `kira_rt_env_*`.
//!
//! One fixed signature each, no overloading and no inference: a `String` name
//! in, and either the value or whether there is one.

use kira_runtime_abi::EnvOp;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Analyzes one environment intrinsic, or returns `None` for another name.
    pub(super) fn analyze_env_intrinsic(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        let op = EnvOp::from_intrinsic_name(name)?;
        self.reject_intrinsic_type_args(name, type_args, span);

        let values: Vec<HirExprId> = args
            .iter()
            .map(|arg| self.analyze_expr(ctx, arg.value))
            .collect();
        if values.len() != op.arity() {
            self.emit(
                span,
                "KSEM252",
                format!(
                    "`{name}` takes exactly {} argument{}, found {}",
                    op.arity(),
                    if op.arity() == 1 { "" } else { "s" },
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let mut refused = false;
        for (index, &value) in values.iter().enumerate() {
            let got = self.program.expr(value).type_of();
            if !got.assignable_to(Type::String) {
                self.emit(
                    span,
                    "KSEM253",
                    format!(
                        "argument {} of `{name}` expects `String`, found `{}`",
                        index + 1,
                        self.type_name(got)
                    ),
                );
                refused = true;
            }
        }
        if refused {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let ty = match op {
            EnvOp::Text => Type::String,
            EnvOp::IsSet => Type::Bool,
        };
        Some(self.program.exprs.alloc(HirExpr::Env {
            op,
            args: values,
            ty,
        }))
    }
}
