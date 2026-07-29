//! Compiler-recognized compiler intrinsics.
//!
//! The primitives bundled Foundation's `Kira.Compiler` surface is built on. An
//! intrinsic rather than an ordinary function for the same reason the
//! file-system ones are: reaching a compiler is an effect the *engine* performs
//! through its host, not something a Kira function body can express — the VM
//! hands the request to whatever host it was given, and native code calls
//! `kira_rt_compiler_*`.
//!
//! One fixed signature, no overloading and no inference: `[String]` in and
//! `[String]` out, which is the one aggregate every backend already carries
//! across the engine seam. The shape *inside* those arrays is
//! [`kira_runtime_abi::CheckRequest`]'s, spelled once there and read by both
//! ends.

use kira_runtime_abi::CompilerOp;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Analyzes one compiler intrinsic, or returns `None` for another name.
    pub(super) fn analyze_compiler_intrinsic(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        let op = CompilerOp::from_intrinsic_name(name)?;
        self.reject_intrinsic_type_args(name, type_args, span);

        let values: Vec<HirExprId> = args
            .iter()
            .map(|arg| self.analyze_expr(ctx, arg.value))
            .collect();
        let expected = self.compiler_parameters(op);
        if values.len() != expected.len() {
            self.emit(
                span,
                "KSEM252",
                format!(
                    "`{name}` takes exactly {} argument{}, found {}",
                    expected.len(),
                    if expected.len() == 1 { "" } else { "s" },
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let mut refused = false;
        for (index, (&value, &want)) in values.iter().zip(expected.iter()).enumerate() {
            let got = self.program.expr(value).type_of();
            if !got.assignable_to(want) {
                self.emit(
                    span,
                    "KSEM253",
                    format!(
                        "argument {} of `{name}` expects `{}`, found `{}`",
                        index + 1,
                        self.type_name(want),
                        self.type_name(got)
                    ),
                );
                refused = true;
            }
        }
        if refused {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let ty = self.compiler_result(op);
        Some(self.program.exprs.alloc(HirExpr::Compiler {
            op,
            args: values,
            ty,
        }))
    }

    /// The parameter types of one intrinsic, in order.
    fn compiler_parameters(&mut self, op: CompilerOp) -> Vec<Type> {
        let strings = self.string_array();
        match op {
            CompilerOp::CheckPackages => vec![strings],
        }
    }

    /// The result type of one intrinsic.
    fn compiler_result(&mut self, op: CompilerOp) -> Type {
        match op {
            CompilerOp::CheckPackages => self.string_array(),
        }
    }

    /// The interned `[String]` type both ends of the seam speak.
    fn string_array(&mut self) -> Type {
        self.program.types.array_of(Type::String)
    }
}
