//! Minting function types, lifting closure literals, and capturing.

use kira_semantics_model::hir::{FuncId, HirExpr, HirExprId, HirFunction, HirStmt, LocalId};
use kira_semantics_model::{FieldDef, OwnershipMode, StructDef, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ClosureParam};

use super::{
    Capture, Captured, ClosureCtx, ClosureImpl, ClosureSite, FnTypeInfo, stable_impl_tag,
    unique_impl_tag,
};
use crate::analyze::{Analyzer, FnCtx};

#[path = "function_values.rs"]
mod function_values;

/// The shape a lifted function shares with the closure it came from.
///
/// One bundle for what every minting path needs about the function type:
/// its representation struct, parameter types, per-parameter ownership
/// modes, and result. Carried as a value so no helper here takes a
/// parameter list long enough to need an allowance.
pub(crate) struct ClosureShape<'a> {
    pub(crate) repr: StructId,
    pub(crate) params: &'a [Type],
    pub(crate) modes: &'a [OwnershipMode],
    pub(crate) result: Type,
}

impl Analyzer<'_> {
    /// Lifts one closure literal to a top-level function and builds its value.
    pub(super) fn lift_closure(
        &mut self,
        ctx: &mut FnCtx,
        shape: ClosureShape<'_>,
        params: &[ClosureParam],
        body: &Block,
    ) -> HirExprId {
        let repr = shape.repr;
        let param_types = shape.params;
        let modes = shape.modes;
        let result = shape.result;
        let function = self.reserve_synth();
        // The tag is claimed **before** the body is analyzed, not after. A
        // literal nested inside this one is lifted while this body is being
        // analyzed, and if the row were only appended afterwards both would
        // read the same `impls.len()` and take the same tag — so the dispatcher
        // would send every call to whichever body was registered second. The
        // function id is already reserved, so the row can be complete here.
        let display_tag = self
            .fn_types
            .get(repr)
            .map_or(0, |info| info.impls.len() as u32);
        let candidate = stable_impl_tag(self.source, body.span, "closure");
        let tag = self
            .fn_types
            .get(repr)
            .map_or(candidate, |info| unique_impl_tag(info, candidate));
        if let Some(info) = self.fn_types.get_mut(repr) {
            info.impls.push(ClosureImpl { tag, function });
        }

        let mut inner = FnCtx::new(result);
        inner.set_main_thread(ctx.main_thread);
        // A closure body is part of the same function's text, so it boxes its
        // own `var`s against the same set of mentioned names — which is what
        // makes a `var` declared in one closure and captured by a nested one
        // boxed too.
        inner.set_closure_mentions(ctx.closure_mentions());
        // Slot 0 is the closure value itself. It is bound to no name, so the
        // body cannot reach it: captures arrive through the prologue below,
        // which is the only thing that reads it.
        let env = inner.declare_hidden(Type::Struct(repr), false);
        // A closure body resolves no bare field names: `self` belongs to the
        // enclosing method's frame, and the lifted function has no receiver to
        // read one from. A body that writes one gets "undefined name", which is
        // accurate — the closure genuinely has no `self`.
        inner.receiver = None;
        // A closure parameter takes the mode its *type* declares, not `Owned`.
        // The literal never writes one, so the type is the only place it can
        // come from — and it has to arrive, or a body writing through a `borrow
        // mut` parameter would be refused for mutating an owned binding.
        for (index, (param, &ty)) in params.iter().zip(param_types).enumerate() {
            let name = self.interner.resolve(param.name).to_owned();
            let mode = modes.get(index).copied().unwrap_or(OwnershipMode::Owned);
            inner.declare_param(&name, ty, mode == OwnershipMode::BorrowMut, mode);
        }
        inner.closure = Some(ClosureCtx {
            repr,
            tag,
            captures: Vec::new(),
        });

        // The enclosing frame moves into the inner one for the duration, which
        // is what lets a name resolve outward through any depth of nesting and
        // thread a capture back through every frame it crossed.
        inner.enclosing = Some(Box::new(std::mem::replace(ctx, FnCtx::new(Type::Void))));
        let analyzed = self.analyze_block(&mut inner, body);
        // A closure owes its result exactly as a function does.
        //
        // Nothing checked this, so a body that fell off its end handed the
        // caller whatever the return slot happened to hold — and a caller that
        // read a struct out of it corrupted the heap, surfacing far away as a
        // garbage path inside an unrelated allocation. `{ in expr }` and the
        // terse `{ expr }` both reach it, because a bare expression is a
        // statement in a block and Kira returns by saying `return`.
        if result != Type::Void && result != Type::Error && !self.body_definitely_returns(&analyzed)
        {
            self.emit(
                body.span,
                "KSEM136",
                format!(
                    "this closure must return `{}` on every path, and this body can finish \
                     without returning; a bare expression is a statement, so write \
                     `return <value>`",
                    self.type_name(result)
                ),
            );
        }
        if let Some(outer) = inner.enclosing.take() {
            *ctx = *outer;
        }

        let closure = inner.closure.take().unwrap_or(ClosureCtx {
            repr,
            tag,
            captures: Vec::new(),
        });
        // Each capture is bound at entry by reading its field out of the
        // closure value. Building the prologue here rather than at each use is
        // what keeps a capture read as cheap as a local read.
        let mut stmts = Vec::with_capacity(closure.captures.len() + analyzed.len());
        for capture in &closure.captures {
            let ty = inner.local_type(capture.inner);
            let base = self.program.exprs.alloc(HirExpr::Local {
                local: env,
                ty: Type::Struct(repr),
            });
            let field_ty = if capture.boxed {
                self.program.types.array_of(ty)
            } else {
                ty
            };
            let read = self.program.exprs.alloc(HirExpr::Field {
                base,
                index: capture.field,
                ty: field_ty,
            });
            // A boxed capture travels as a one-element array; element zero is
            // the value.
            let init = if capture.boxed {
                let zero = self.program.exprs.alloc(HirExpr::Int(0));
                self.program.exprs.alloc(HirExpr::Index {
                    base: read,
                    index: zero,
                    ty,
                })
            } else {
                read
            };
            stmts.push(self.program.stmts.alloc(HirStmt::Let {
                local: capture.inner,
                init,
            }));
        }
        stmts.extend(analyzed);

        let param_count = 1 + params.len() as u32;
        let name = format!("{}#{display_tag}", self.type_name(Type::Struct(repr)));
        let execution = self.current_execution;
        self.fill_synth(
            function,
            HirFunction {
                name,
                param_count,
                return_type: result,
                locals: inner.locals,
                body: stmts,
                is_main: false,
                is_main_thread: false,
                is_async: false,
                execution,
                mutates_self: false,
                name_span: body.span,
            },
        );

        let capture_fields: Vec<u32> = closure.captures.iter().map(|c| c.field).collect();

        // The value: the tag plus this literal's captures, read in the frame
        // that is creating it. The remaining fields belong to other literals of
        // the same type and are filled with zeros once the list stops growing.
        let tag_expr = self.program.exprs.alloc(HirExpr::Int(i64::from(tag)));
        let mut capture_values = Vec::with_capacity(closure.captures.len());
        for capture in &closure.captures {
            let ty = ctx.local_type(capture.outer);
            let value = self.program.exprs.alloc(HirExpr::Local {
                local: capture.outer,
                ty,
            });
            capture_values.push(if capture.boxed {
                let array = self.program.types.array_of(ty);
                self.program.exprs.alloc(HirExpr::ArrayNew {
                    ty: array,
                    elements: vec![value],
                })
            } else {
                value
            });
        }
        let expr = self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: repr,
            fields: vec![tag_expr],
        });
        self.closure_sites.push(ClosureSite {
            expr,
            repr,
            tag,
            capture_fields,
            capture_values,
        });
        expr
    }

    /// Resolves `name` in `ctx`, capturing it out of an enclosing closure frame
    /// when that is where it lives.
    ///
    /// The recursion threads a capture through *every* frame it crosses, so a
    /// closure nested two deep reads a binding of the outermost function
    /// through the closure between them rather than reaching past it.
    pub(crate) fn resolve_capturing(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        span: Span,
    ) -> Captured {
        if let Some(local) = ctx.resolve(name) {
            return Captured::Local(local);
        }
        let Some(mut outer) = ctx.enclosing.take() else {
            return Captured::Absent;
        };
        let found = self.resolve_capturing(&mut outer, name, span);
        let outcome = match found {
            Captured::Local(outer_local) => self.capture(ctx, &mut outer, outer_local, name, span),
            other => other,
        };
        ctx.enclosing = Some(outer);
        outcome
    }

    /// Threads one binding of `outer` into `ctx` as a capture.
    fn capture(
        &mut self,
        ctx: &mut FnCtx,
        outer: &mut FnCtx,
        outer_local: LocalId,
        name: &str,
        span: Span,
    ) -> Captured {
        let Some(closure) = ctx.closure.as_ref() else {
            // A frame with an enclosing frame but no closure state is not a
            // closure body, so there is nothing to capture into.
            return Captured::Absent;
        };
        if let Some(existing) = closure
            .captures
            .iter()
            .find(|capture| capture.outer == outer_local)
        {
            return Captured::Local(existing.inner);
        }
        let repr = closure.repr;
        // A capture is a read of the enclosing binding, so it answers to the
        // move checker exactly as a bare mention of the name does. Checking
        // here rather than in `typeck` is what makes it reach the *outer*
        // binding: the body only ever sees the fresh inner one, which was never
        // moved out of.
        if !self.check_local_live(outer, outer_local, span) {
            return Captured::Refused;
        }
        let ty = outer.local_type(outer_local);
        // A mutable binding is captured by *sharing its box*, which is what the
        // declaration already put it in when this function's closures mention
        // the name. A mutable binding that has no box is one a cell cannot hold
        // — a `borrow mut` parameter, a recovered callback-state view — and is
        // refused, because capturing it by copy would run and quietly give a
        // different answer.
        if outer.is_mutable(outer_local) && !matches!(ty, Type::Cell(_)) {
            self.emit(
                span,
                "KSEM117",
                format!(
                    "closure captures `{name}`, which is declared `var` and cannot be moved \
                     into shared storage; a closure shares a captured `var` with the scope \
                     that declared it, and a value of type `{}` has no shared form",
                    self.type_name(ty)
                ),
            );
            return Captured::Refused;
        }
        // Said before the general answer below, which would name the cell type
        // rather than the value with the body: a captured `var` travels as a
        // share of its box, and every read out of that box is a value of what
        // the box holds. A value that runs a user `Drop` has no second copy.
        if let Some(inner) = self.program.types.cell_inner(ty)
            && self.program.types.runs_user_drop(inner)
        {
            self.emit(
                span,
                "KSEM302",
                format!(
                    "closure captures `{name}`, which is a `{}` and runs a user `Drop` body: a \
                     captured `var` is shared with the scope that declared it, and every read \
                     out of that share is a second value with the same body to run. Keep the \
                     value in its scope and capture what the closure needs from it.",
                    self.type_name(inner)
                ),
            );
            return Captured::Refused;
        }
        if !self.capture_is_trivially_copyable(ty) {
            self.emit(
                span,
                "KSEM117",
                format!(
                    "closure captures `{name}` of type `{}`, which is not trivially copyable",
                    self.type_name(ty)
                ),
            );
            return Captured::Refused;
        }
        // A capture becomes a *field* of the closure's representation struct, so
        // capturing a function value whose type reaches this one would make that
        // struct contain itself by value: a value of infinite size. The value is
        // put behind a one-element array instead — a heap handle, so the struct
        // has a fixed size again — and the prologue reads element zero back out.
        // Copying an array copies its elements, so the captured function value
        // has exactly the semantics an inline field would have given it.
        let boxed = self.capture_would_be_cyclic(repr, ty);
        let field_ty = if boxed {
            self.program.types.array_of(ty)
        } else {
            ty
        };
        // The literal's tag is in the name because one representation struct
        // holds the captures of *every* literal of its type: two literals that
        // each capture an `x` first would otherwise mint two fields named `x$0`
        // in one `StructDef`, and the rest of the workspace reads a struct's
        // field names as unique.
        let tag = ctx.closure.as_ref().map_or(0, |c| c.tag);
        let Some(field) = self.program.types.structs_mut().push_field(
            repr,
            FieldDef {
                name: format!(
                    "{name}${tag}_{}",
                    ctx.closure.as_ref().map_or(0, |c| c.captures.len())
                ),
                ty: field_ty,
                mutable: false,
            },
        ) else {
            return Captured::Refused;
        };
        // Bound in the closure's outermost scope, so it is visible everywhere
        // in the body *and* can be shadowed by an inner declaration — which is
        // what makes a name usable before an inner `let` of the same name and
        // rebound after it.
        let inner = ctx.declare_capture(name, ty);
        // The capture stands in for the outer binding, so a jump from a use
        // inside the closure lands where that binding was written.
        if let Some(binding) = outer.binding_span(outer_local) {
            ctx.note_binding_span(inner, binding);
        }
        if let Some(closure) = ctx.closure.as_mut() {
            closure.captures.push(Capture {
                outer: outer_local,
                inner,
                field,
                boxed,
            });
        }
        Captured::Local(inner)
    }
}
