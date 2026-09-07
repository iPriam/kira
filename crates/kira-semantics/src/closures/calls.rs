//! Calling a closure value, and finishing the desugar once analysis is done.

use kira_semantics_model::hir::{
    CallableSignature, Callee, FieldOrder, FuncId, HirBinaryOp, HirExpr, HirExprId, HirFunction,
    HirPlace, HirStmt, HirStmtId, HirWriteback, LocalId, TaskTarget,
};
use kira_semantics_model::{OwnershipMode, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::ExprId;

use crate::analyze::{Analyzer, FnCtx};
use crate::closures::ClosureImpl;
use crate::closures::lift::ClosureShape;

impl Analyzer<'_> {
    /// Type-checks `f(args)` when `f` names a binding of function type.
    ///
    /// `None` when it names no such binding, so the caller carries on treating
    /// the call as a class construction or a free function call.
    pub(crate) fn analyze_local_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let local = match self.resolve_capturing(ctx, name, span) {
            super::Captured::Local(local) => local,
            // A refused capture already spoke; calling it would say it twice.
            super::Captured::Refused => return Some(self.program.exprs.alloc(HirExpr::Error)),
            super::Captured::Absent => return None,
        };
        let ty = ctx.local_type(local);
        let repr = self.as_function_type(ty)?;
        if !self.check_local_live(ctx, local, span) {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        let expr = self.program.exprs.alloc(HirExpr::Local { local, ty });
        Some(self.analyze_closure_call(ctx, expr, repr, args, span))
    }

    /// Type-checks `f(args)` when `f` names a module constant of function
    /// type.
    ///
    /// `None` when no visible constant answers to the name or the one that
    /// does holds no function, so the caller carries on to classes, constructs
    /// and free functions.
    pub(crate) fn analyze_constant_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let ty = self.constant_type(name)?;
        let repr = self.as_function_type(ty)?;
        let expr = self.constant_read(name, span)?;
        Some(self.analyze_closure_call(ctx, expr, repr, args, span))
    }

    /// Type-checks `receiver.name(args)` when `name` is a field of function
    /// type rather than a method.
    ///
    /// A closure stored in a field is called through the field, which is the
    /// same syntax a method call uses — so this is tried before "no such
    /// method" is reported.
    pub(crate) fn analyze_field_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        receiver_ty: Type,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let Type::Struct(owner) = receiver_ty else {
            return None;
        };
        let def = self.program.types.structs().get(owner)?;
        let index = def.field_index(name)?;
        let field_ty = def.field(index)?.ty;
        let repr = self.as_function_type(field_ty)?;
        let expr = self.program.exprs.alloc(HirExpr::Field {
            base: receiver,
            index,
            ty: field_ty,
        });
        Some(self.analyze_closure_call(ctx, expr, repr, args, span))
    }

    /// Type-checks a call *through* a closure value.
    ///
    /// The value becomes the dispatcher's first argument, exactly as a method's
    /// receiver becomes its function's first parameter — which is what keeps
    /// closures out of the IR and out of every backend.
    fn analyze_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        expr: HirExprId,
        repr: StructId,
        args: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let Some((params, modes, result)) = self.fn_types.get(repr).map(|info| {
            (
                info.params.clone(),
                info.param_ownership.clone(),
                info.result,
            )
        }) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let dispatcher = self.dispatcher_for(repr);
        if args.len() != params.len() {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{}` takes {} argument(s), found {}",
                    self.type_name(Type::Struct(repr)),
                    params.len(),
                    args.len()
                ),
            );
        }
        let mut all = vec![expr];
        // Where each `borrow mut` argument's final value lands. The dispatcher
        // carries the closure value in slot 0, so a source parameter `index` is
        // the dispatcher's `index + 1` — the writeback names the dispatcher's
        // numbering, because that is the call the instruction actually makes.
        let mut writebacks: Vec<HirWriteback> = Vec::new();
        for (index, &arg) in args.iter().enumerate() {
            match params.get(index) {
                // Each argument is checked against the mode the *type* declares
                // — a `borrow` parameter takes no `move`, an unannotated one
                // does — exactly as a declared parameter's is.
                Some(&expected) => {
                    let name = self.type_name(Type::Struct(repr));
                    let mode = modes.get(index).copied().unwrap_or(OwnershipMode::Owned);
                    all.push(self.analyze_call_argument(ctx, arg, expected, mode, &name));
                    if mode == OwnershipMode::BorrowMut {
                        self.record_borrow_mut_argument(
                            ctx,
                            arg,
                            index,
                            index as u32 + 1,
                            &name,
                            &mut writebacks,
                        );
                    }
                }
                None => all.push(self.analyze_expr(ctx, arg)),
            }
        }
        for (index, &expected) in params.iter().enumerate() {
            let Some(arg) = all.get(index + 1).copied() else {
                break;
            };
            let actual = self.program.expr(arg).type_of();
            if !self.admits(actual, expected) {
                self.emit(
                    span,
                    "KSEM063",
                    format!(
                        "argument {} of `{}` expects `{}`, found `{}`",
                        index + 1,
                        self.type_name(Type::Struct(repr)),
                        self.type_name(expected),
                        self.type_name(actual)
                    ),
                );
            }
            // A closure with an `Any` parameter takes the erased form, at the
            // call site like every other crossing.
            all[index + 1] = self.coerce_into(arg, expected);
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: all,
            ty: result,
            writebacks,
        })
    }

    /// The dispatcher for a function type, reserving its id on first need.
    fn dispatcher_for(&mut self, repr: StructId) -> FuncId {
        if let Some(existing) = self.fn_types.get(repr).and_then(|info| info.dispatcher) {
            return existing;
        }
        let id = self.reserve_synth();
        if let Some(info) = self.fn_types.get_mut(repr) {
            info.dispatcher = Some(id);
        }
        id
    }

    /// Whether `ty` is a function type, and its representation struct.
    pub(crate) fn as_function_type(&self, ty: Type) -> Option<StructId> {
        match ty {
            Type::Struct(id) if self.fn_types.get(id).is_some() => Some(id),
            _ => None,
        }
    }

    /// Finishes the desugar: pads every closure value's field list and builds
    /// every dispatcher body.
    ///
    /// Runs once, after every body is analyzed, because both depend on facts
    /// that are only final then — how many literals each function type has, and
    /// how wide its representation struct grew.
    pub(crate) fn finalize_closures(&mut self) {
        self.finalize_closure_values();
        self.build_dispatchers();
        // Generic function bodies are already in the ordinary prefix. Rewrite
        // the temporary synthesized ids in every HIR expression before those
        // bodies are appended at their final contiguous positions.
        let final_base = self.program.functions.len() as u32;
        self.remap_synth_calls(final_base);
        for constant in &mut self.program.constants {
            remap_synth_id(
                &mut constant.init,
                self.synth_base,
                self.synth.len() as u32,
                final_base,
            );
        }
        let synth = std::mem::take(&mut self.synth);
        for function in synth {
            if let Some(function) = function {
                self.program.functions.push(function);
            } else {
                // A reserved id is always filled: a dispatcher is built above,
                // and a lifted body is filled where it is lifted. A hole would
                // shift every later id, so it is replaced by an empty function
                // rather than dropped.
                self.program.functions.push(HirFunction {
                    name: "<unreachable closure>".to_owned(),
                    param_count: 0,
                    return_type: Type::Void,
                    locals: Vec::new(),
                    body: Vec::new(),
                    is_main: false,
                    is_main_thread: false,
                    is_async: false,
                    execution: kira_semantics_model::Execution::Inherited,
                    mutates_self: false,
                    name_span: Span::new(0, 0),
                    signature: CallableSignature::synthesized(&[], Type::Void),
                });
            }
        }
    }

    /// Replaces temporary synthesized ids with their final function-vector
    /// positions. Calls can be nested in ordinary, generic, or synthesized
    /// bodies, so the expression arena is the one complete place to rewrite.
    fn remap_synth_calls(&mut self, final_base: u32) {
        let temporary_base = self.synth_base;
        let temporary_count = self.synth.len() as u32;
        for (_, expr) in self.program.exprs.iter_mut() {
            match expr {
                HirExpr::Call {
                    callee: Callee::User(id),
                    ..
                } => remap_synth_id(id, temporary_base, temporary_count, final_base),
                HirExpr::TaskSpawn {
                    target: TaskTarget::Call(id),
                    ..
                } => remap_synth_id(id, temporary_base, temporary_count, final_base),
                _ => {}
            }
        }
    }

    /// Pads each closure literal's `StructNew` out to its representation
    /// struct's full field list.
    fn finalize_closure_values(&mut self) {
        let sites = std::mem::take(&mut self.closure_sites);
        for site in &sites {
            let field_types: Vec<Type> = match self.program.types.structs().get(site.repr) {
                Some(def) => def.fields.iter().map(|field| field.ty).collect(),
                None => continue,
            };
            let mut fields = Vec::with_capacity(field_types.len());
            for (index, &ty) in field_types.iter().enumerate() {
                let index = index as u32;
                if index == 0 {
                    fields.push(self.program.exprs.alloc(HirExpr::Int(i64::from(site.tag))));
                    continue;
                }
                match site.capture_fields.iter().position(|&field| field == index) {
                    Some(slot) => fields.push(site.capture_values[slot]),
                    // A field another literal of the same type owns. The
                    // dispatcher never reads it for this tag.
                    None => fields.push(match ty {
                        Type::Cell(_) => self.program.exprs.alloc(HirExpr::CellNull { ty }),
                        _ => self.default_value(ty),
                    }),
                }
            }
            if let HirExpr::StructNew { fields: slot, .. } = &mut self.program.exprs[site.expr] {
                *slot = fields;
            }
        }
    }

    /// One dispatcher branch: forward the closure value and every parameter to
    /// the lifted body, and return what it returns.
    ///
    /// A `borrow mut` parameter is forwarded *and written back*: the arm passes
    /// the dispatcher's own parameter, and the value the arm's target leaves in
    /// it lands back in that same slot — which the dispatcher, itself declaring
    /// the slot `borrow mut`, then carries out to its caller. That chain is what
    /// makes a mutable borrow survive a call through a function value, where the
    /// callee is not known until the tag is read.
    fn dispatch_arm(
        &mut self,
        target: FuncId,
        env: LocalId,
        shape: ClosureShape<'_>,
        param_locals: &[LocalId],
    ) -> Vec<HirStmtId> {
        let repr = shape.repr;
        let params = shape.params;
        let modes = shape.modes;
        let result = shape.result;
        let mut args = vec![self.program.exprs.alloc(HirExpr::Local {
            local: env,
            ty: Type::Struct(repr),
        })];
        let mut writebacks = Vec::new();
        for (index, (&local, &ty)) in param_locals.iter().zip(params.iter()).enumerate() {
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
            if modes.get(index) == Some(&OwnershipMode::BorrowMut) {
                writebacks.push(HirWriteback {
                    param: index as u32 + 1,
                    place: HirPlace {
                        local,
                        path: Vec::new(),
                    },
                });
            }
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(target),
            args,
            ty: result,
            writebacks,
        });
        if result == Type::Void {
            vec![
                self.program.stmts.alloc(HirStmt::Expr { expr: call }),
                self.program.stmts.alloc(HirStmt::Return { value: None }),
            ]
        } else {
            vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(call) }),
            ]
        }
    }

    /// A well-typed value of `ty`, for a slot nothing reads.
    ///
    /// Every arm builds a value the backends agree is of `ty`, because a
    /// backend type-checks what it is handed: an `Int(0)` standing in for a
    /// `String` passes the VM and is rejected by the LLVM verifier, which is
    /// exactly the parity break this exists to make impossible.
    ///
    /// A struct cannot contain itself by value — `KSEM052` breaks that cycle
    /// and reports it — but a type may still reach itself *through an enum*,
    /// which is the very escape `KSEM052` tells an author to use. So the walk
    /// carries the types it is inside of and an enum picks the first variant
    /// that does not lead back into one of them, rather than picking variant
    /// zero and recursing until the stack is gone. A widget tree whose first
    /// variant is the branching one is exactly that shape.
    pub(crate) fn default_value(&mut self, ty: Type) -> HirExprId {
        match self.default_value_inside(ty, &mut Vec::new()) {
            Some(value) => value,
            // Every variant led back into the cycle, so the type has no finite
            // value at all. [`Analyzer::check_enum_terminates`] runs before this
            // and has already reported the enum against its declaration and
            // broken its payloads — except for an enum with no variants at all,
            // which is uninhabited by declaration and reaches here silently.
            None => self.program.exprs.alloc(HirExpr::Error),
        }
    }

    /// Whether a finite value of `ty` can be built at all.
    ///
    /// Answered by building one and discarding it, rather than by a second walk
    /// over the same shape beside [`Analyzer::default_value`]: two answers to
    /// one question, with nothing checking they agree, is the shape this
    /// repository removes wherever it finds it. What it costs is a few arena
    /// nodes for each enum [`Analyzer::check_enum_terminates`] asks about.
    pub(crate) fn has_finite_value(&mut self, ty: Type) -> bool {
        self.default_value_inside(ty, &mut Vec::new()).is_some()
    }

    /// [`Analyzer::default_value`], tracking the types the walk is inside of.
    ///
    /// `None` means no finite value of `ty` can be built along this path,
    /// which an enum's variant search reads as "try the next variant".
    fn default_value_inside(&mut self, ty: Type, visiting: &mut Vec<Type>) -> Option<HirExprId> {
        if visiting.contains(&ty) {
            return None;
        }
        // A distinct type's placeholder is its representation's, crossed into
        // the distinct type — the same value with the type the slot declares.
        // Built here rather than as an arm below because the crossing wraps a
        // node that has to exist first.
        if let Type::Distinct(_) = ty {
            let representation = self.program.types.representation(ty);
            let value = self.default_value_inside(representation, visiting)?;
            return Some(self.program.exprs.alloc(HirExpr::Distinct { value, ty }));
        }
        let node = match ty {
            Type::Float(_) => HirExpr::Float(0.0),
            Type::Bool => HirExpr::Bool(false),
            Type::String => HirExpr::Str(String::new()),
            Type::Array(_) => HirExpr::ArrayNew {
                ty,
                elements: Vec::new(),
            },
            Type::Struct(id) => {
                let field_types: Vec<Type> = match self.program.types.structs().get(id) {
                    Some(def) => def.fields.iter().map(|field| field.ty).collect(),
                    None => Vec::new(),
                };
                visiting.push(ty);
                let mut fields = Vec::with_capacity(field_types.len());
                for field_ty in field_types {
                    // A field with no finite value makes the whole struct one:
                    // there is no other field to fall back on.
                    let Some(field) = self.default_value_inside(field_ty, visiting) else {
                        visiting.pop();
                        return None;
                    };
                    fields.push(field);
                }
                visiting.pop();
                HirExpr::StructNew {
                    struct_id: id,
                    fields,
                    order: FieldOrder::Declared,
                }
            }
            Type::Enum(id) => {
                // The first variant that terminates, not simply the first one.
                // A payload-less variant always does; a variant carrying the
                // enum's own tree does not, and taking it would recurse for as
                // long as there is stack.
                let variants: Vec<Option<Type>> = match self.program.types.enums().get(id) {
                    Some(def) => (0..def.variants.len() as u32)
                        .filter_map(|tag| def.variant(tag))
                        .map(|variant| variant.payload)
                        .collect(),
                    None => Vec::new(),
                };
                visiting.push(ty);
                let chosen =
                    variants
                        .into_iter()
                        .enumerate()
                        .find_map(|(tag, payload)| match payload {
                            None => Some((tag as u32, None)),
                            Some(payload_ty) => self
                                .default_value_inside(payload_ty, visiting)
                                .map(|value| (tag as u32, Some(value))),
                        });
                visiting.pop();
                let (tag, payload) = chosen?;
                HirExpr::EnumNew {
                    enum_id: id,
                    tag,
                    payload,
                }
            }
            // A padding slot of cell type gets a real, empty box rather than a
            // zero word. Nothing reads it, but a copy of the representation
            // struct walks every field, and a copy that bumped a share count
            // through a null handle would be a crash rather than a wasted
            // allocation.
            Type::Cell(id) => {
                let inner = self.program.types.cells().inner(id).unwrap_or(Type::Error);
                visiting.push(ty);
                let value = self.default_value_inside(inner, visiting);
                visiting.pop();
                HirExpr::CellNew { value: value?, ty }
            }
            // `Void` never reaches here (its callers return without a value)
            // and `Error` means the program is already rejected. `RawPtr` and
            // `CString` are C-seam types that never reach a closure slot in this
            // subset — a foreign value does not flow into a closure — so they
            // fall to the same zero-word placeholder rather than growing a HIR
            // node nothing constructs.
            // `Any` has no default either, and for a reason of its own: every
            // erased value is *some* concrete value, and there is no concrete
            // type this could pick without the type system having chosen one.
            // A capture slot of type `Any` is filled by the erasure that put a
            // value there, never by a placeholder.
            // A distinct type is answered above, before the match, because its
            // placeholder wraps one that has to exist first.
            Type::Distinct(_)
            | Type::Int(_)
            | Type::Void
            | Type::Error
            | Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::CString
            | Type::CBlock
            | Type::RuntimeType
            | Type::Any
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::NativeState(_) => HirExpr::Int(0),
        };
        Some(self.program.exprs.alloc(node))
    }

    /// Builds the body of every dispatcher a call site reserved.
    fn build_dispatchers(&mut self) {
        // Sorted by id rather than taken in table order: the table is a hash
        // map, and building bodies in its iteration order would allocate this
        // program's arena nodes in a different order on every run.
        let mut rows: Vec<(StructId, FuncId)> = self
            .fn_types
            .rows()
            .filter_map(|(id, info)| info.dispatcher.map(|dispatcher| (id, dispatcher)))
            .collect();
        rows.sort_by_key(|&(_, dispatcher)| dispatcher.0);
        for (repr, dispatcher) in rows {
            let function = self.dispatcher_body(repr);
            self.fill_synth(dispatcher, function);
        }
    }

    /// The engine a function type's dispatcher runs on.
    ///
    /// Native when any implementation it dispatches to is, because the branch
    /// that reaches a native body has to be on the same engine as the body —
    /// a `borrow mut` parameter is a writeback, and a writeback call cannot
    /// cross the seam. Taking it from `current_execution` instead would take it
    /// from whichever call site happened to mint the dispatcher first, which is
    /// analysis order rather than a property of the program.
    ///
    /// A function type's literals must share an execution engine: a split would
    /// require one dispatcher per engine, while this representation carries one
    /// execution choice for the whole type.
    fn dispatcher_execution(&self, impls: &[ClosureImpl]) -> kira_semantics_model::Execution {
        for entry in impls {
            let index = (entry.function.0 - self.synth_base) as usize;
            if let Some(Some(function)) = self.synth.get(index)
                && function.execution == kira_semantics_model::Execution::Native
            {
                return kira_semantics_model::Execution::Native;
            }
        }
        kira_semantics_model::Execution::Inherited
    }

    /// The dispatcher for one function type: a branch per closure literal.
    fn dispatcher_body(&mut self, repr: StructId) -> HirFunction {
        let Some((params, modes, result, impls)) = self.fn_types.get(repr).map(|info| {
            (
                info.params.clone(),
                info.param_ownership.clone(),
                info.result,
                info.impls.clone(),
            )
        }) else {
            return HirFunction {
                name: "<unreachable dispatcher>".to_owned(),
                param_count: 0,
                return_type: Type::Void,
                locals: Vec::new(),
                body: Vec::new(),
                is_main: false,
                is_main_thread: false,
                is_async: false,
                execution: kira_semantics_model::Execution::Inherited,
                mutates_self: false,
                name_span: Span::new(0, 0),
                signature: CallableSignature::synthesized(&[], Type::Void),
            };
        };
        let execution = self.dispatcher_execution(&impls);
        let mut ctx = FnCtx::new(result);
        let env = ctx.declare_hidden(Type::Struct(repr), false);
        let mut param_locals = Vec::with_capacity(params.len());
        for (index, &ty) in params.iter().enumerate() {
            let mode = modes.get(index).copied().unwrap_or(OwnershipMode::Owned);
            // A `borrow mut` slot is declared mutable as well as borrowing: the
            // arm writes its result back into it, and a slot nothing may write
            // is one the writeback cannot land in.
            let mutable = mode == OwnershipMode::BorrowMut;
            param_locals.push(ctx.declare_hidden_as(ty, mutable, mode));
        }

        // The last literal is the chain's unconditional tail rather than one
        // more tested branch. That is not an optimization: a tested last branch
        // leaves a fall-through the dispatcher would have to return *some*
        // value from, and no value of an arbitrary result type can be conjured
        // that every backend agrees is of that type.
        let mut body: Vec<HirStmtId> = Vec::with_capacity(impls.len().max(1));
        for (index, closure) in impls.iter().enumerate() {
            let arm = self.dispatch_arm(
                closure.function,
                env,
                ClosureShape {
                    repr,
                    params: &params,
                    modes: &modes,
                    result,
                },
                &param_locals,
            );
            if index + 1 == impls.len() {
                body.extend(arm);
                break;
            }
            let tag_read = {
                let base = self.program.exprs.alloc(HirExpr::Local {
                    local: env,
                    ty: Type::Struct(repr),
                });
                self.program.exprs.alloc(HirExpr::Field {
                    base,
                    index: 0,
                    ty: Type::INT,
                })
            };
            let wanted = self
                .program
                .exprs
                .alloc(HirExpr::Int(i64::from(closure.tag)));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::EqInt,
                lhs: tag_read,
                rhs: wanted,
                ty: Type::Bool,
            });
            body.push(self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body: arm,
                else_body: Vec::new(),
            }));
        }
        if impls.is_empty() {
            // A function type mentioned and called, but with no literal
            // anywhere in the program: nothing can build a value of it, so no
            // call can reach this. It still needs a well-typed terminator,
            // because a backend type-checks a body it can prove is dead.
            let tail = if result == Type::Void {
                HirStmt::Return { value: None }
            } else {
                let value = self.default_value(result);
                HirStmt::Return { value: Some(value) }
            };
            body.push(self.program.stmts.alloc(tail));
        }

        HirFunction {
            name: format!("{}$call", self.type_name(Type::Struct(repr))),
            param_count: 1 + params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            is_main_thread: false,
            is_async: false,
            execution,
            mutates_self: false,
            name_span: Span::new(0, 0),
            signature: CallableSignature::synthesized(&[], result),
        }
    }
}

/// Maps a temporary synthesized id to the slot it occupies after all ordinary
/// and generic functions have been appended.
fn remap_synth_id(id: &mut FuncId, temporary_base: u32, count: u32, final_base: u32) {
    let Some(index) = id.0.checked_sub(temporary_base) else {
        return;
    };
    if index < count {
        *id = FuncId(final_base + index);
    }
}
