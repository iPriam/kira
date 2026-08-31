//! Semantic lowering for the host main-thread capability.
//!
//! `MainThread.invoke { f(args) }`, `spawn`, and `post` are deliberately kept
//! as a small compiler surface. The block contains one direct call to a named
//! `@MainThread` function; argument evaluation stays in the requesting
//! context, and the resolved target plus those values become one HIR request.
//! A future pure-Kira executor can build on the host capability without being
//! named by the compiler.

use super::*;

use crate::traits::markers::Marker;
use kira_runtime_abi::MainThreadOp;
use kira_semantics_model::hir::Callee;
use kira_semantics_model::{MainThreadTaskResult, Type};
use kira_syntax_model::ast::{Expr, ExprId};

impl Analyzer<'_> {
    /// Recognizes `MainThread.<operation> { directCall() }` before ordinary
    /// receiver analysis tries to resolve `MainThread` as a Kira value.
    pub(super) fn analyze_main_thread_namespace(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: Span,
        args: &[CallArg],
        children: &[ExprId],
    ) -> Option<HirExprId> {
        let Expr::Name { symbol, .. } = self.tree.expr(receiver) else {
            return None;
        };
        let namespace = self.interner.resolve(*symbol);
        if namespace != "MainThread" || ctx.resolve(namespace).is_some() {
            return None;
        }
        let name = self.interner.resolve(method);
        let operation = match name {
            "invoke" => MainThreadOp::Invoke,
            "spawn" => MainThreadOp::Spawn,
            "post" => MainThreadOp::Post,
            _ => return None,
        };
        Some(self.analyze_main_thread_call(ctx, operation, method_span, args, children))
    }

    /// Type-checks one main-thread operation and turns its direct call into a
    /// host request.
    fn analyze_main_thread_call(
        &mut self,
        ctx: &mut FnCtx,
        operation: MainThreadOp,
        span: Span,
        args: &[CallArg],
        children: &[ExprId],
    ) -> HirExprId {
        if !args.is_empty() {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(
                span,
                "KSEM331",
                format!(
                    "`MainThread.{}` takes its call in the trailing block and no parenthesized arguments",
                    operation.label()
                ),
            );
        }
        let Some(&body) = children.first() else {
            self.emit(
                span,
                "KSEM331",
                format!(
                    "`MainThread.{}` needs one direct function call in its trailing block",
                    operation.label()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        for &extra in children.iter().skip(1) {
            self.analyze_expr(ctx, extra);
            self.emit(
                span,
                "KSEM331",
                format!(
                    "`MainThread.{}` accepts one direct function call, found more than one expression",
                    operation.label()
                ),
            );
        }

        let inner = match self.tree.expr(body).clone() {
            Expr::Call {
                callee,
                callee_span,
                ref args,
                ref children,
                trailing_closure,
                ..
            } => {
                if !children.is_empty() || trailing_closure.is_some() {
                    self.analyze_expr(ctx, body);
                    self.emit(
                        span,
                        "KSEM332",
                        format!(
                            "`MainThread.{}` requires a direct call without a nested trailing block",
                            operation.label()
                        ),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                let name = self.interner.resolve(callee).to_owned();
                self.analyze_user_call_from_syntax_with_type_args(CallSyntax {
                    ctx,
                    name: &name,
                    leading: &[],
                    type_args: &[],
                    args,
                    trailing: &[],
                    span: callee_span,
                    allow_main_thread_target: true,
                })
            }
            Expr::MethodCall {
                receiver,
                method,
                method_span,
                ref args,
                ref children,
                ..
            } if children.is_empty() => {
                let receiver_hir = self.analyze_expr(ctx, receiver);
                let receiver_ty = self.program.expr(receiver_hir).type_of();
                let method_name = self.interner.resolve(method).to_owned();
                let qualified = format!("{}.{method_name}", self.type_name(receiver_ty));
                if self.lookup_function(&qualified).is_none() {
                    self.analyze_expr(ctx, body);
                    self.emit(
                        method_span,
                        "KSEM332",
                        format!(
                            "`MainThread.{}` requires a direct call to a named `@MainThread` function",
                            operation.label()
                        ),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                let inner = self.analyze_user_call_from_syntax_on_main_thread(
                    ctx,
                    &qualified,
                    &[receiver_hir],
                    args,
                    method_span,
                );
                self.record_mut_receiver(ctx, inner, receiver);
                inner
            }
            Expr::MethodCall { .. } => {
                self.analyze_expr(ctx, body);
                self.emit(
                    span,
                    "KSEM332",
                    format!(
                        "`MainThread.{}` requires a direct call without a nested trailing block",
                        operation.label()
                    ),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            _ => {
                self.analyze_expr(ctx, body);
                self.emit(
                    span,
                    "KSEM332",
                    format!(
                        "`MainThread.{}` requires a direct call to a named `@MainThread` function",
                        operation.label()
                    ),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
        };
        let HirExpr::Call {
            callee: Callee::User(function),
            args: values,
            ty,
            writebacks,
        } = self.program.expr(inner).clone()
        else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let name = self.sigs[function.0 as usize].name.clone();
        if !self.sigs[function.0 as usize].is_main_thread {
            self.emit(
                span,
                "KSEM332",
                format!(
                    "`{name}` is not marked `@MainThread`; annotate the target or call it normally"
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if !writebacks.is_empty() {
            self.emit(
                span,
                "KSEM333",
                format!(
                    "`MainThread.{}` cannot route a call with `borrow mut` parameters because its caller storage stays on the requesting context",
                    operation.label()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        for value in &values {
            self.refuse_main_thread_value(self.program.expr(*value).type_of(), span);
        }

        let result = match operation {
            MainThreadOp::Invoke => {
                self.refuse_main_thread_value(ty, span);
                ty
            }
            MainThreadOp::Post | MainThreadOp::LifecycleStart => Type::Void,
            MainThreadOp::Spawn => {
                self.refuse_main_thread_value(ty, span);
                let Some(result) = main_thread_task_result(ty) else {
                    self.emit(
                        span,
                        "KSEM334",
                        "`MainThread.spawn` targets must return an owned `Send` value or nothing",
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                };
                Type::MainThreadTask(result)
            }
        };
        self.program.exprs.alloc(HirExpr::MainThreadCall {
            operation,
            function,
            args: values,
            ty: result,
        })
    }

    /// Reports a value that cannot be copied through the main-thread request.
    fn refuse_main_thread_value(&mut self, ty: Type, span: Span) {
        if ty == Type::Error {
            return;
        }
        let name = self.program.types.type_name(ty);
        if let Some(reason) = self.marker_reason(&name, ty, Marker::Send) {
            self.emit(
                span,
                "KSEM335",
                format!("`{name}` cannot cross to the main thread: {reason}"),
            );
        }
    }

    /// Analyzes the only property on a spawned main-thread task.
    pub(in crate::typeck) fn analyze_main_thread_task_property(
        &mut self,
        handle: HirExprId,
        result: MainThreadTaskResult,
        name: &str,
        span: Span,
    ) -> HirExprId {
        if name != "await" {
            self.emit(
                span,
                "KSEM336",
                format!(
                    "a main-thread task handle is opaque: `.await` is its only operation, so `{name}` is not available on one"
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let ty = result.value_type();
        self.program
            .exprs
            .alloc(HirExpr::MainThreadJoin { handle, ty })
    }

    /// Analyzes a method-shaped use of a spawned main-thread task.
    pub(in crate::typeck) fn analyze_main_thread_task_method(
        &mut self,
        ctx: &mut FnCtx,
        handle: HirExprId,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        for arg in args {
            self.analyze_expr(ctx, arg.value);
        }
        self.emit(
            span,
            "KSEM336",
            format!(
                "a main-thread task handle only has `.await`; `{name}()` is not available on one"
            ),
        );
        let _ = handle;
        self.program.exprs.alloc(HirExpr::Error)
    }
}

/// Maps a source result type to the type of a main-thread task handle.
fn main_thread_task_result(ty: Type) -> Option<MainThreadTaskResult> {
    MainThreadTaskResult::from_type(ty)
}
