//! The async task spine: `async function`, `Task { … }`, `.await`,
//! `.requestCancel()`, `.detach()`, `taskYield()`, and `taskSleep(ms)`.
//!
//! Two rules shape everything here, and both are the language's, not this
//! file's:
//!
//! - A task **body** is a direct call to a named function taking scalars and
//!   returning a scalar or nothing, or a bare scalar literal. Anything else is
//!   `KSEM159`. The restriction is what makes a spawn's arguments evaluable at
//!   the spawn site and its body runnable later, with no closure and no capture
//!   analysis in between.
//! - A task **handle** is opaque. `.await`, `.requestCancel()`, and `.detach()`
//!   are the whole surface; every other use is `KSEM158`. That is enforced by
//!   the type — [`Type::Task`] is not an `Int` — rather than by a convention a
//!   later phase could forget.
//!
//! Nothing about *scheduling* lives here. Which task runs next, and when a join
//! traps, is the executor's, and the executor is generated Kira the IR
//! synthesizes — see `kira_ir`'s task lowering and
//! `kira_runtime_abi::TaskExecutor`.

use kira_semantics_model::hir::{Builtin, Callee, FuncId, HirExpr, HirExprId, TaskTarget};
use kira_semantics_model::{TaskResult, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, Expr, ExprId, UnaryOp};

use crate::analyze::{Analyzer, FnCtx};
use crate::traits::markers::Marker;

/// The scalar types a task body may take and hand back, before a `distinct` is
/// resolved to what it is.
///
/// `Bool` is deliberately absent: a task's arguments and result travel through
/// the executor as one machine word, and `Int`/`Float` are the two the compiler
/// can already move in and out of one without inventing a representation.
fn task_scalar(ty: Type) -> Option<TaskResult> {
    match ty {
        Type::Int(_) => Some(TaskResult::Int),
        Type::Float(_) => Some(TaskResult::Float),
        _ => None,
    }
}

impl Analyzer<'_> {
    /// The same answer as [`task_scalar`], with a `distinct` resolved to the
    /// scalar it is.
    ///
    /// A distinct type is erased before IR exists, so one crossing a task slot
    /// is already the word its representation is. Refusing it here would mean a
    /// channel end could not reach the task that uses it, which is the only
    /// place an end is ever going.
    fn task_slot_scalar(&self, ty: Type) -> Option<TaskResult> {
        match ty {
            Type::Distinct(id) => task_scalar(self.program.types.distincts().representation(id)?),
            other => task_scalar(other),
        }
    }

    /// Analyzes `Task { body }`.
    pub(crate) fn analyze_task_spawn(
        &mut self,
        ctx: &mut FnCtx,
        body: ExprId,
        span: Span,
    ) -> HirExprId {
        match self.tree.expr(body).clone() {
            Expr::Call {
                callee,
                callee_span,
                ref args,
                ref children,
                ..
            } if children.is_empty() => {
                let name = self.interner.resolve(callee).to_owned();
                self.analyze_task_call(ctx, &name, args, callee_span, span)
            }
            // `Task { 41 }` — a body already reduced to a value. Deferring it
            // changes nothing observable, because a literal has nothing to
            // observe, which is exactly why this is the one non-call body the
            // slice admits.
            Expr::Int { .. } | Expr::Float { .. } => {
                let value = self.analyze_expr(ctx, body);
                let Some(result) = task_scalar(self.program.expr(value).type_of()) else {
                    return self.refuse_task_body(span);
                };
                self.program.exprs.alloc(HirExpr::TaskSpawn {
                    target: TaskTarget::Value,
                    args: vec![value],
                    ty: Type::Task(result),
                })
            }
            Expr::Unary {
                op: UnaryOp::Neg,
                operand,
                ..
            } if matches!(
                self.tree.expr(operand),
                Expr::Int { .. } | Expr::Float { .. }
            ) =>
            {
                let value = self.analyze_expr(ctx, body);
                let Some(result) = task_scalar(self.program.expr(value).type_of()) else {
                    return self.refuse_task_body(span);
                };
                self.program.exprs.alloc(HirExpr::TaskSpawn {
                    target: TaskTarget::Value,
                    args: vec![value],
                    ty: Type::Task(result),
                })
            }
            _ => {
                // The body is still analyzed so its own mistakes surface next
                // to the refusal rather than behind it.
                self.analyze_expr(ctx, body);
                self.refuse_task_body(span)
            }
        }
    }

    /// Analyzes `Task { name(args) }`, the ordinary task body.
    fn analyze_task_call(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        callee_span: Span,
        span: Span,
    ) -> HirExprId {
        // The target is chosen the way an ordinary call chooses among
        // overloads: by the arguments as written, with defaults and labels
        // playing their usual part.
        let candidates = self.visible_overloads(name);
        if candidates.is_empty() {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(
                callee_span,
                "KSEM061",
                format!("call to undefined function `{name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let probed = self.try_argument_types(ctx, &[], args);
        let id = match self.resolve_overload(&candidates, &probed) {
            Ok(id) => id,
            Err(crate::typeck::overloads::OverloadFailure::Ambiguous(winners)) => {
                for arg in args {
                    self.analyze_expr(ctx, arg.value);
                }
                let list = self.overload_list(&winners);
                self.emit(
                    span,
                    "KSEM275",
                    format!("this task target `{name}` fits {list} equally well"),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            Err(crate::typeck::overloads::OverloadFailure::None) => candidates[0],
        };
        let (params, return_type) = {
            let sig = &self.sigs[id.0 as usize];
            (sig.params.clone(), sig.return_type)
        };
        self.link_function(id, callee_span);
        // A task runs a task entry point: an `async function`. An ordinary
        // function is called synchronously, or marked `async` if it is meant
        // to be scheduled.
        if !self.sigs[id.0 as usize].signature.is_async {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(
                callee_span,
                "KSEM352",
                format!(
                    "`{name}` is not `async`, so it cannot be a task target: mark it `async \
                     function` to schedule it, or call it directly"
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // A task owns what it runs on: nothing crosses into it borrowed, because
        // the caller's place may be gone, or written, by the time the task runs.
        if self.sigs[id.0 as usize].signature.borrows_any_parameter() {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(
                callee_span,
                "KSEM353",
                format!(
                    "`{name}` borrows a parameter, so it cannot be a task target: a task takes \
                     its arguments owned or copied, never borrowed"
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if let Some(refusal) = self.task_sends_everything_it_crosses(name, &params, return_type) {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(span, "KSEM312", refusal);
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // A `Void` body joins as `Int` `0`: the join is a sequencing point, and
        // sequencing still has to produce a value.
        let result = match return_type {
            Type::Void => Some(TaskResult::Int),
            other => self.task_slot_scalar(other),
        };
        let Some(result) = result else {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return self.refuse_task_body(span);
        };
        if params
            .iter()
            .any(|param| self.task_slot_scalar(*param).is_none())
        {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return self.refuse_task_body(span);
        }
        if args.len() != params.len() {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {} argument(s), but the task body passes {}",
                    params.len(),
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if params.len() > kira_runtime_abi::TASK_SLOTS {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return self.refuse_task_body(span);
        }
        // The arguments are evaluated **here**, at the spawn site. That is the
        // half of "deferred" the type checker gets to enforce: each one is
        // checked against its parameter exactly as an ordinary call's would be.
        let mut values = Vec::with_capacity(args.len());
        for (arg, param) in args.iter().zip(params.iter()) {
            let value = self.analyze_expr_expecting(ctx, arg.value, Some(*param));
            let actual = self.program.expr(value).type_of();
            if !actual.assignable_to(*param) {
                let expected_name = self.program.types.type_name(*param);
                let actual_name = self.program.types.type_name(actual);
                self.emit(
                    self.tree.expr(arg.value).span(),
                    "KSEM063",
                    format!("expected `{expected_name}`, found `{actual_name}`"),
                );
            }
            values.push(value);
        }
        self.program.exprs.alloc(HirExpr::TaskSpawn {
            target: TaskTarget::Call(id),
            args: values,
            ty: Type::Task(result),
        })
    }

    /// Why a task body may not take or return one of these types, or `None`
    /// when every value crossing the spawn is `Send`.
    ///
    /// A spawn is the one boundary in the language a value crosses without its
    /// spawner: what goes in is evaluated here and read there, and what comes
    /// out is read by whoever joins. So both directions must be movable.
    ///
    /// Asked of the resolved signature rather than of the arguments, so the
    /// answer is a fact about the function being spawned. Today the slot
    /// representation narrows the same signature further ([`Self::refuse_task_body`],
    /// `KSEM159`), and every type that rule admits is `Send`; this one is what
    /// stays correct when that narrowing lifts.
    fn task_sends_everything_it_crosses(
        &self,
        name: &str,
        params: &[Type],
        result: Type,
    ) -> Option<String> {
        let unsendable = |ty: Type| {
            let type_name = self.program.types.type_name(ty);
            self.marker_reason(&type_name, ty, Marker::Send)
                .map(|reason| (type_name, reason))
        };
        if let Some((type_name, reason)) = unsendable(result) {
            return Some(format!(
                "`{name}` returns `{type_name}`, which cannot cross into a task: {reason}"
            ));
        }
        for param in params {
            let Some((type_name, reason)) = unsendable(*param) else {
                continue;
            };
            return Some(format!(
                "`{name}` takes `{type_name}`, which cannot cross into a task: {reason}"
            ));
        }
        None
    }

    /// Reports a task body outside the executable slice.
    fn refuse_task_body(&mut self, span: Span) -> HirExprId {
        self.emit(
            span,
            "KSEM159",
            "a `Task { … }` body is a call to a named function taking `Int`/`Float` \
             parameters and returning `Int`, `Float`, or nothing, or a bare numeric \
             literal",
        );
        self.program.exprs.alloc(HirExpr::Error)
    }

    /// Analyzes a property read on a task handle: `handle.await` and nothing
    /// else.
    pub(crate) fn analyze_task_property(
        &mut self,
        handle: HirExprId,
        result: TaskResult,
        name: &str,
        span: Span,
    ) -> HirExprId {
        if name != "await" {
            return self.refuse_task_use(name, span);
        }
        let ty = match result {
            TaskResult::Int => Type::INT,
            TaskResult::Float => Type::FLOAT,
        };
        self.program.exprs.alloc(HirExpr::TaskJoin { handle, ty })
    }

    /// Analyzes a method call on a task handle: `.requestCancel()` and
    /// `.detach()`.
    pub(crate) fn analyze_task_method(
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
        if !args.is_empty() {
            self.emit(
                span,
                "KSEM062",
                format!("`{name}` on a task handle takes no arguments"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        match name {
            "requestCancel" => self.program.exprs.alloc(HirExpr::TaskCancel { handle }),
            "detach" => self.program.exprs.alloc(HirExpr::TaskDetach { handle }),
            // `.await` is a property, not a call: writing `handle.await()`
            // is refused by name rather than by arity, so the fix is obvious.
            _ => self.refuse_task_use(name, span),
        }
    }

    /// Reports a use of a task handle outside its three operations.
    pub(crate) fn refuse_task_use(&mut self, name: &str, span: Span) -> HirExprId {
        self.emit(
            span,
            "KSEM158",
            format!(
                "a task handle is opaque: `.await`, `.requestCancel()`, and `.detach()` \
                 are its whole surface, so `{name}` is not available on one"
            ),
        );
        self.program.exprs.alloc(HirExpr::Error)
    }

    /// Analyzes `taskYield()` / `taskSleep(ms)`, or answers `None` when `name`
    /// is neither.
    pub(crate) fn analyze_task_builtin(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let builtin = match name {
            "taskYield" => Builtin::TaskYield,
            "taskSleep" => Builtin::TaskSleep,
            _ => return None,
        };
        let expected = usize::from(builtin == Builtin::TaskSleep);
        let mut values: Vec<HirExprId> = args
            .iter()
            .map(|&arg| self.analyze_expr_expecting(ctx, arg, Some(Type::INT)))
            .collect();
        if values.len() != expected {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {expected} argument(s), but {} were passed",
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        if let Some(&value) = values.first() {
            let actual = self.program.expr(value).type_of();
            if !actual.assignable_to(Type::INT) {
                let actual_name = self.program.types.type_name(actual);
                self.emit(
                    span,
                    "KSEM063",
                    format!("`taskSleep` takes a duration in milliseconds as `Int`, found `{actual_name}`"),
                );
                values = vec![self.program.exprs.alloc(HirExpr::Error)];
            }
        }
        Some(self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(builtin),
            args: values,
            writebacks: Vec::new(),
            ty: Type::Void,
        }))
    }
}

impl Analyzer<'_> {
    /// Refuses a direct call of an `async function`, which is a task entry
    /// point rather than a function to call: `Task { name(…) }` schedules it.
    /// Returns whether the call was refused.
    pub(crate) fn refuse_direct_async_call(&mut self, id: FuncId, span: Span) -> bool {
        if !self.sigs[id.0 as usize].signature.is_async {
            return false;
        }
        let name = self.sigs[id.0 as usize].name.clone();
        self.emit(
            span,
            "KSEM354",
            format!(
                "`{name}` is `async`, so it cannot be called directly: write `Task {{ {name}(…) }}` \
                 to schedule it, and `.await` the handle for its result"
            ),
        );
        true
    }
}
