//! Calls and construction: every expression that names something and hands it
//! arguments.
//!
//! Split out of [`super`] because it is a cohesive surface with one shared
//! question — *what is being called, and does the argument list fit its
//! signature* — and because four of the five kinds here share the argument
//! checking that [`Analyzer::analyze_user_call`] does. A method call, a
//! module-qualified call, and a bare call all end up there; a struct literal
//! and `print` are the two that do not, and they are the two that are not
//! calls to a user function.

use kira_semantics_model::OwnershipMode;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{
    Callee, FuncId, HirExpr, HirExprId, HirPlace, HirWriteback, LocalId,
};
use kira_syntax_model::ast::{CallArg, ExprId, FieldInit};

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;
use crate::typeck::overloads::OverloadFailure;

/// Written argument and child lists of one method call.
pub(super) struct MethodCallContent<'a> {
    pub(super) args: &'a [CallArg],
    pub(super) children: &'a [ExprId],
}

/// What a written call name resolved to.
enum CallTarget {
    /// The declaration the call means, and the parameter types its arguments
    /// are checked against.
    Chosen(FuncId, Vec<Type>),
    /// Several declarations fit the call equally well.
    Ambiguous(Vec<FuncId>),
    /// Nothing answers to the name here.
    Unknown,
}

impl Analyzer<'_> {
    /// Records a receiver-writeback place on a just-built call whose callee
    /// mutates its receiver, resolving the written `receiver` as a mutable
    /// place.
    ///
    /// A non-mutating callee — the common case — is left untouched, so an
    /// ordinary call carries no writeback and behaves exactly as before. A
    /// receiver that is not a mutable place turns the call into an error, the
    /// diagnostic already reported by place resolution (`KSEM021` for an
    /// immutable binding, `KSEM211` for a temporary).
    pub(crate) fn record_mut_receiver(
        &mut self,
        ctx: &mut FnCtx,
        call: HirExprId,
        receiver: ExprId,
    ) {
        if !self.callee_mutates(call) {
            return;
        }
        match self.resolve_place(ctx, receiver, PlacePurpose::MutCall) {
            Some((place, _)) => self.add_writeback(call, HirWriteback { param: 0, place }),
            None => self.program.exprs[call] = HirExpr::Error,
        }
    }

    /// Records the writeback for a call whose receiver is `self` — an implicit
    /// or parent-qualified call inside a method.
    ///
    /// `self` inside a mutating method is always a mutable place (the fixpoint
    /// marks the enclosing method mutating whenever it calls one on `self`), so
    /// the place is `self` with an empty path and no refusal is possible.
    pub(crate) fn record_mut_self(&mut self, call: HirExprId, self_local: LocalId) {
        if !self.callee_mutates(call) {
            return;
        }
        self.add_writeback(
            call,
            HirWriteback {
                param: 0,
                place: HirPlace {
                    local: self_local,
                    path: Vec::new(),
                },
            },
        );
    }

    /// Whether `call` is a real user call whose chosen callee mutates its
    /// receiver.
    ///
    /// The question is asked of the callee *the call resolved to* — overload
    /// resolution already picked the right declaration for this receiver's
    /// type — rather than of a display name looked up again: two packages may
    /// each declare a method of one name under one index key, and re-asking
    /// by name can read the other package's flag, losing or inventing a
    /// writeback.
    fn callee_mutates(&self, call: HirExprId) -> bool {
        let HirExpr::Call {
            callee: Callee::User(id),
            ..
        } = self.program.expr(call)
        else {
            return false;
        };
        self.mutates_self(*id)
    }

    /// Adds a writeback to a call node, keeping the list in parameter order.
    ///
    /// A method call records its receiver here *after* the call node exists,
    /// while a `borrow mut` argument records its own slot while the arguments
    /// are still being bound, so the two arrive out of order and the list is
    /// kept sorted rather than appended to. A slot already recorded is left
    /// alone: it names the same place, and a second entry would write it twice.
    fn add_writeback(&mut self, call: HirExprId, writeback: HirWriteback) {
        if let HirExpr::Call { writebacks, .. } = &mut self.program.exprs[call] {
            match writebacks.binary_search_by_key(&writeback.param, |entry| entry.param) {
                Ok(_) => {}
                Err(index) => writebacks.insert(index, writeback),
            }
        }
    }

    /// Type-checks `receiver.method(args)`.
    ///
    /// A method call is an ordinary call whose first argument is the receiver.
    /// Resolving it to that here is what keeps methods out of the IR and out of
    /// every backend: nothing downstream of analysis knows they exist.
    ///
    /// `expected` is carried only for the one shape that is not a call at all:
    /// `Result.Ok(1)` parses as a method call, and the instantiation it
    /// constructs comes from the position rather than from anything written.
    pub(super) fn analyze_method_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: kira_source::Span,
        content: MethodCallContent<'_>,
        expected: Option<Type>,
    ) -> HirExprId {
        let MethodCallContent { args, children } = content;
        // `Support.hello()` is a *module-qualified free call*, not a method
        // call, and it is recognized here because the parser cannot tell the
        // two apart: both are `<expr> . name ( args )`. What separates them is
        // that the receiver is a bare name which is no local and which this
        // file imported as a module — a question only the analyzer, holding the
        // file-scoped import table, can answer.
        if let Some(call) = self.analyze_qualified_call(ctx, receiver, method, method_span, args) {
            return call;
        }
        // `SizeMode.Fixed(3)` / `Foundation.SizeMode.Fixed(3)` is a
        // payload-carrying enum variant written with a qualified spelling: the
        // receiver names the enum, `method` names the variant, and the argument
        // is its payload. It parses as a method call because the parser cannot
        // tell an enum name from a value; the analyzer, holding the enum table
        // and the import table, can.
        match self.qualified_enum_at(ctx, receiver, expected) {
            crate::enums::QualifiedEnum::Enum(enum_id) => {
                let values = Some(Self::argument_values(args));
                return self.analyze_dot_member(
                    ctx,
                    method,
                    method_span,
                    &values,
                    method_span,
                    Some(Type::Enum(enum_id)),
                );
            }
            // `Result.Ok(1)` where the position asks for no instantiation of
            // `Result`. The receiver is a template, not a value.
            crate::enums::QualifiedEnum::Unanchored(template) => {
                let values = Some(Self::argument_values(args));
                return self.report_unanchored_generic_construction(
                    ctx,
                    &template,
                    method,
                    &values,
                    method_span,
                    expected,
                );
            }
            crate::enums::QualifiedEnum::NotAnEnum => {}
        }

        // Analyzing the receiver is how its type is known, and its type is what
        // decides which surface the call belongs to. For an array that is all
        // this pass is for: `append` needs the receiver as a *place*, which is
        // resolved from the syntax, not from the analyzed value.
        //
        // So the diagnostics are marked first and rolled back on the array
        // path, and the place resolution reports on its own. That keeps
        // `resolve_place` the single source of truth for what a bad receiver
        // says, instead of this pass and that one each having an opinion —
        // `grid[nope].append(1)` reports the undefined name exactly once.
        //
        // The probe is *effectful*, so its ownership effects are rolled back
        // too: analyzing `(move xs).append(1)`'s receiver marks `xs` moved, and
        // leaving that in place would report a phantom use-after-move on a later
        // `xs`. The array path re-resolves the receiver from syntax anyway, so
        // the probe's move is undone before it runs.
        let mark = self.diagnostics.len();
        let ownership = ctx.ownership_snapshot();
        let receiver_hir = self.analyze_expr(ctx, receiver);
        let receiver_ty = self.program.expr(receiver_hir).type_of();

        if receiver_ty.is_array() {
            self.diagnostics.truncate(mark);
            ctx.restore_ownership(ownership);
            // An array builtin binds its argument by shape, not by a parameter
            // name, so a label on one is a mistake.
            let name = self.interner.resolve(method).to_owned();
            let values = Self::argument_values(args);
            return self.analyze_array_method(ctx, receiver, &name, method_span, &values);
        }
        if let Type::Enum(family_id) = receiver_ty
            && self.is_construct_family_type(family_id)
        {
            let name = self.interner.resolve(method).to_owned();
            return self.analyze_construct_family_call(
                ctx,
                receiver_hir,
                family_id,
                &name,
                crate::constructs::ConstructCallContent { args, children },
                method_span,
            );
        }

        // A task handle's three operations are matched before anything tries to
        // resolve a method on it: it is not a struct, so a field-based lookup
        // would report a missing type rather than the opaque-handle rule.
        if matches!(receiver_ty, Type::Task(_)) {
            let name = self.interner.resolve(method).to_owned();
            return self.analyze_task_method(ctx, receiver_hir, &name, args, method_span);
        }
        if receiver_ty == Type::String {
            // A string builtin binds its arguments by shape, not by a parameter
            // name, so a label on one is a mistake.
            let name = self.interner.resolve(method).to_owned();
            let values = Self::argument_values(args);
            return self.analyze_string_method(ctx, receiver_hir, &name, method_span, &values);
        }
        // An error receiver already spoke; do not pile on.
        if receiver_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let name = self.interner.resolve(method).to_owned();
        if self.refuse_direct_drop_call(receiver_ty, &name, method_span) {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Type::Struct(_) = receiver_ty else {
            self.emit(
                method_span,
                "KSEM096",
                format!(
                    "type `{}` has no methods, so it has no method `{name}`",
                    self.type_name(receiver_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        // A field of function type is *called* through this same syntax, so a
        // closure in a field is tried before a method of that name is looked
        // up. A method wins if both exist, because a method is what the
        // receiver's type declares and a field only what it stores.
        let qualified = format!("{}.{name}", self.type_name(receiver_ty));
        if self.lookup_function(&qualified).is_none() {
            let values = Self::argument_values(args);
            if let Some(call) = self.analyze_field_closure_call(
                ctx,
                receiver_hir,
                receiver_ty,
                &name,
                &values,
                method_span,
            ) {
                // A closure stored in a field exposes no parameter names here.
                return call;
            }
        }
        if self.lookup_function(&qualified).is_none() {
            // A concrete construct value calls its family's `extend` modifiers
            // through the same syntax: the receiver has no method of this name,
            // but its family does. Upcast the receiver into the family value and
            // dispatch there — `Text(…).padding(8)` becomes a family call.
            if let Type::Struct(owner) = receiver_ty
                && let Some(family_id) = self.family_uniform_method(owner, &name)
            {
                let upcast = self.coerce_construct_value(receiver_hir, Some(Type::Enum(family_id)));
                return self.analyze_construct_family_call(
                    ctx,
                    upcast,
                    family_id,
                    &name,
                    crate::constructs::ConstructCallContent { args, children },
                    method_span,
                );
            }
            if let Type::Struct(owner) = receiver_ty
                && self.report_ambiguous_member(owner, &name, method_span, true)
            {
                return self.program.exprs.alloc(HirExpr::Error);
            }
            // A field holding a value is not callable, and saying so names the
            // likelier mistake than "no such method".
            let message = match self.resolve_field_quietly(receiver_ty, &name) {
                true => format!(
                    "`{name}` is a field of `{}`, not a method",
                    self.type_name(receiver_ty)
                ),
                false => format!(
                    "struct `{}` has no method `{name}`",
                    self.type_name(receiver_ty)
                ),
            };
            self.emit(method_span, "KSEM097", message);
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // A trailing block on a struct method fills that method's content
        // parameter, exactly as it does on a construction or a family modifier.
        // The children are already analyzed into one value, so they occupy the
        // last parameter slot rather than arriving as written arguments.
        let trailing = match self.lookup_function(&qualified).map(|(id, _, _)| id) {
            Some(id) if !children.is_empty() => match self.init_content_param(id) {
                Some(content) => vec![self.content_value(ctx, &content, children, method_span)],
                None => {
                    for &child in children {
                        self.analyze_expr(ctx, child);
                    }
                    self.emit(
                        method_span,
                        "KSEM278",
                        format!(
                            "`{qualified}` takes no trailing content; give it a last parameter                              of `some X` for one child or `[some X]` for a list of them"
                        ),
                    );
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        let call = self.analyze_user_call_from_syntax_with(
            ctx,
            &qualified,
            &[receiver_hir],
            args,
            &trailing,
            method_span,
        );
        // When the method mutates its receiver, the written receiver is resolved
        // as a mutable place so the mutation lands back in the caller's storage.
        self.record_mut_receiver(ctx, call, receiver);
        call
    }

    /// Type-checks a struct literal into a [`HirExpr::StructNew`] holding one
    /// initializer per declared field, in declaration order.
    ///
    /// A field the literal omits is filled from its declared default, so
    /// nothing downstream of analysis has to know that defaults exist. A field
    /// with neither an initializer nor a default is the one case that cannot be
    /// filled, and it is reported here.
    pub(super) fn analyze_struct_literal(
        &mut self,
        ctx: &mut FnCtx,
        name: kira_core::Symbol,
        name_span: kira_source::Span,
        inits: &[FieldInit],
    ) -> HirExprId {
        // A module-qualified literal (`Support.Point { … }`) resolves exactly as
        // a qualified *type* reference does: the qualifier is checked against
        // this file's imports and then decides which package's declaration is
        // meant. An unimported root is reported there and the literal's own
        // fields are still analyzed so their mistakes surface.
        let written = self.interner.resolve(name).to_owned();
        let Some(qualified) = self.split_module_qualifier(&written, name_span) else {
            for init in inits {
                self.analyze_expr(ctx, init.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let struct_name = qualified.text.clone();
        let Some(id) = self.visible_struct_qualified(&qualified) else {
            // A function of this name is the likely mistake, so say which.
            let message = if self.lookup_function(&struct_name).is_some() {
                format!("`{struct_name}` is a function, not a struct")
            } else {
                format!("unknown struct `{struct_name}`")
            };
            self.emit(name_span, "KSEM092", message);
            for init in inits {
                self.analyze_expr(ctx, init.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.link_type_name(&struct_name, name_span);
        // A C-layout struct's members are C storage, which is what lets a
        // `String` fill a `CString` member and an array fill a `RawPtr` one.
        let is_c_layout = self.ffi_c_layout_named(&struct_name).is_some();
        let field_count = self
            .program
            .types
            .structs()
            .get(id)
            .map_or(0, |def| def.fields.len());

        // Analyze each written initializer against the field it names, keeping
        // source order so diagnostics read in the order they were written.
        let mut slots: Vec<Option<HirExprId>> = vec![None; field_count];
        for init in inits {
            let field_name = self.interner.resolve(init.name).to_owned();
            // The field is resolved before its value, so the field's type is
            // the value's expected type: `H { values = [] }` needs it.
            let resolved = self.resolve_field(Type::Struct(id), &field_name, init.name_span);
            if resolved.is_some() {
                self.link_field_name(&struct_name, &field_name, init.name_span);
            }
            let value = self.analyze_expr_expecting(ctx, init.value, resolved.map(|(_, ty)| ty));
            let value_ty = self.program.expr(value).type_of();
            let Some((index, field_ty)) = resolved else {
                continue;
            };
            if slots[index as usize].is_some() {
                self.emit(
                    init.name_span,
                    "KSEM093",
                    format!("field `{field_name}` is initialized twice"),
                );
                continue;
            }
            // Two coercions fill a C-layout member with C storage this side
            // writes. A `String` filling a `CString` member copies its bytes
            // out; and a POINTER member — `RawPtr`, or an `@FFI.Pointer` naming
            // what it addresses — is filled from an array of seam scalars, from
            // the struct it points at, or from an `@FFI.Array` of that struct.
            // That is what a descriptor carrying a data pointer (`sg_range`) or
            // an item list beside a count (`WGPUVertexBufferLayout.attributes`)
            // is, and both are the shape a graphics API asks for.
            // The storage outlives the call, for the reason
            // `kira_runtime_abi::c_storage` gives: the callee may hold on to it.
            let pointer_fill = (is_c_layout
                && matches!(field_ty, Type::RawPtr | Type::ForeignPtr(_)))
            .then(|| self.foreign_pointer_fill(value, field_ty, init.span))
            .flatten();
            let value = if field_ty == Type::CString && value_ty == Type::String {
                self.program
                    .exprs
                    .alloc(HirExpr::CStringNew { text: value })
            } else if let Some(elements) = pointer_fill {
                elements
            } else {
                if !self.admits(value_ty, field_ty) {
                    self.emit(
                        init.span,
                        "KSEM094",
                        format!(
                            "field `{field_name}` of `{struct_name}` expects `{}`, found `{}`",
                            self.type_name(field_ty),
                            self.type_name(value_ty)
                        ),
                    );
                }
                self.coerce_into(value, field_ty)
            };
            slots[index as usize] = Some(value);
        }

        // Fill what the literal left out. A `@FFI.Struct { layout: c }` starts
        // from a zeroed value, so an omitted field with no default takes its
        // zero rather than being reported missing — the oracle's construction
        // rule.
        let mut fields = Vec::with_capacity(field_count);
        let mut missing: Vec<String> = Vec::new();
        for index in 0..field_count as u32 {
            if let Some(value) = slots[index as usize] {
                fields.push(value);
                continue;
            }
            match self.resolve_field_default_at(ctx, id, index) {
                Some(default) => fields.push(default),
                None if is_c_layout => {
                    fields.push(self.ffi_zero_field(id, index, name_span));
                }
                None => {
                    let field_name = self
                        .program
                        .types
                        .structs()
                        .get(id)
                        .and_then(|def| def.field(index))
                        .map_or_else(String::new, |field| field.name.clone());
                    missing.push(field_name);
                    fields.push(self.program.exprs.alloc(HirExpr::Error));
                }
            }
        }
        if !missing.is_empty() {
            self.emit(
                name_span,
                "KSEM095",
                format!(
                    "`{struct_name}` is missing {}: {} (no default is declared)",
                    if missing.len() == 1 {
                        "field"
                    } else {
                        "fields"
                    },
                    missing.join(", ")
                ),
            );
        }
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
        })
    }

    /// Type-checks a call whose arguments are still syntax.
    ///
    /// Ownership is the reason this exists. A parameter's [`OwnershipMode`]
    /// decides whether its argument must say `move`, may not say `move`, or
    /// must say `copy` — and answering that needs the *written* argument, not
    /// the analyzed one: `f(mesh)` and `f(move mesh)` produce the same HIR and
    /// differ only in what the source said. So each argument is analyzed
    /// against the mode its parameter declared, and only then handed to
    /// [`Analyzer::analyze_user_call`] for the type check.
    ///
    /// `leading` carries arguments already analyzed — a method's receiver —
    /// which occupy the first parameter slots.
    pub(crate) fn analyze_user_call_from_syntax(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        leading: &[HirExprId],
        args: &[CallArg],
        span: kira_source::Span,
    ) -> HirExprId {
        self.analyze_user_call_from_syntax_with(ctx, name, leading, args, &[], span)
    }

    /// [`Analyzer::analyze_user_call_from_syntax`], plus arguments that occupy
    /// the *last* parameter slots and are already analyzed.
    ///
    /// A construction's trailing children are the case: they fill an `init`'s
    /// content parameter, and no written expression stands for them, so they
    /// arrive as a value rather than as syntax to check an ownership mode
    /// against.
    pub(crate) fn analyze_user_call_from_syntax_with(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        leading: &[HirExprId],
        args: &[CallArg],
        trailing: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        let (id, params) = match self.resolve_call_target(ctx, name, leading, args, trailing) {
            CallTarget::Chosen(id, params) => (id, params),
            // Nothing decides which declaration this call means, so it has no
            // meaning. The arguments are still analyzed so their own mistakes
            // are reported beside the one about the call.
            CallTarget::Ambiguous(winners) => {
                let list = self.overload_list(&winners);
                self.emit(
                    span,
                    "KSEM275",
                    format!("this call of `{name}` fits {list} equally well"),
                );
                for arg in args {
                    self.analyze_expr(ctx, arg.value);
                }
                return self.program.exprs.alloc(HirExpr::Error);
            }
            // No signature to check against: still analyze every argument so
            // the mistakes inside them are reported alongside the bad call. A
            // label cannot bind to a callee that does not exist, so it is
            // dropped here and `analyze_user_call` reports the missing function.
            CallTarget::Unknown => {
                let mut all = leading.to_vec();
                all.extend(args.iter().map(|arg| self.analyze_expr(ctx, arg.value)));
                return self.analyze_user_call(name, &all, span);
            }
        };
        // Arguments bind by position; a label on one is decorative. See
        // `super::labels` for the measurement behind that.
        let positional = Self::argument_slots(args);
        let ownership = self.param_ownership(id);
        let mut all = leading.to_vec();
        // Where each `borrow mut` argument's final value has to land. Collected
        // while the arguments are bound, because that is the only point where a
        // parameter slot and the *syntax* the caller wrote for it are both in
        // hand; attached once the call node exists.
        let mut writebacks: Vec<HirWriteback> = Vec::new();
        for (index, slot_value) in positional.into_iter().enumerate() {
            let slot = index + leading.len();
            // A slot no argument filled takes its parameter's default, when one
            // was declared; otherwise the missing-argument diagnostic already
            // spoke (labeled) or the arity check will (positional), so stand in
            // with an error value that keeps the arity honest and cascades no
            // further.
            let Some(arg) = slot_value else {
                let filled = self
                    .resolve_param_default(id, slot)
                    .unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error));
                all.push(filled);
                continue;
            };
            // An arity mismatch leaves some argument with no parameter to
            // check against. `analyze_user_call` reports the count; here the
            // argument just analyzes plainly rather than being checked against
            // a mode that does not exist.
            match (params.get(slot), ownership.get(slot)) {
                (Some(&expected), Some(&mode)) => {
                    all.push(self.analyze_call_argument(ctx, arg, expected, mode, name));
                    if mode == OwnershipMode::BorrowMut {
                        self.record_borrow_mut_argument(
                            ctx,
                            arg,
                            slot,
                            slot as u32,
                            name,
                            &mut writebacks,
                        );
                    }
                }
                _ => all.push(self.analyze_expr(ctx, arg)),
            }
        }
        // Arguments the caller already analyzed take the slots after the written
        // ones, before any default is reached for.
        all.extend_from_slice(trailing);
        // A positional call that omitted trailing arguments fills them from
        // their defaults, left to right, stopping at the first parameter that
        // declares none — a genuine shortfall the arity check then reports.
        //
        // A `borrow mut` parameter is never filled this way: a default is a
        // value, and there is nowhere in the caller to write one back, so the
        // shortfall is reported instead.
        while all.len() < params.len() {
            if ownership.get(all.len()) == Some(&OwnershipMode::BorrowMut) {
                break;
            }
            match self.resolve_param_default(id, all.len()) {
                Some(default) => all.push(default),
                None => break,
            }
        }
        // The declaration was already chosen, from the argument types as
        // written. Re-choosing here would see them *after* each was coerced
        // into the parameter it was checked against, which is a rubber stamp
        // rather than a second opinion.
        let call = self.analyze_user_call_hinted(name, &all, span, Some(id));
        for writeback in writebacks {
            self.add_writeback(call, writeback);
        }
        call
    }

    /// The declaration a call names, and the parameter types its arguments are
    /// analyzed against.
    ///
    /// A name declared once answers immediately. An overloaded one is resolved
    /// from the types its arguments have on their own, because the parameter a
    /// given argument is checked against is exactly what is being decided —
    /// see [`Analyzer::try_argument_types`].
    fn resolve_call_target(
        &mut self,
        ctx: &FnCtx,
        name: &str,
        leading: &[HirExprId],
        args: &[CallArg],
        trailing: &[HirExprId],
    ) -> CallTarget {
        let candidates = self.visible_overloads(name);
        let id = match candidates.as_slice() {
            [] => return CallTarget::Unknown,
            [only] => *only,
            _ => {
                let mut actual = self.try_argument_types(ctx, leading, args);
                actual.extend(
                    trailing
                        .iter()
                        .map(|&value| self.program.expr(value).type_of()),
                );
                match self.resolve_overload(&candidates, &actual) {
                    Ok(id) => id,
                    Err(OverloadFailure::Ambiguous(winners)) => {
                        return CallTarget::Ambiguous(winners);
                    }
                    // A call that fits nothing still needs a signature to be
                    // checked against, so the first declaration speaks and
                    // reports the mismatch against what it expected.
                    Err(OverloadFailure::None) => candidates[0],
                }
            }
        };
        CallTarget::Chosen(id, self.param_types(id))
    }

    /// Resolves the caller storage a `borrow mut` argument names, recording
    /// where the callee's final value has to land.
    ///
    /// A `borrow mut` parameter is the callee writing through the caller's
    /// binding, so the argument has to *be* a binding: a temporary would be
    /// mutated and then discarded, which is why that is refused rather than
    /// silently accepted. Two arguments rooted at the same local are refused
    /// for the same reason in reverse — both writes would land in one place and
    /// the later one would erase the earlier.
    ///
    /// `slot` is the parameter as the *source* numbers it, which is what the
    /// diagnostic names; `param` is the slot on the function actually called,
    /// and the two differ by one through a function value, whose dispatcher
    /// carries the closure itself in slot 0.
    pub(crate) fn record_borrow_mut_argument(
        &mut self,
        ctx: &mut FnCtx,
        arg: ExprId,
        slot: usize,
        param: u32,
        callee: &str,
        writebacks: &mut Vec<HirWriteback>,
    ) {
        let span = self.tree.expr(arg).span();
        let Some((place, _)) = self.resolve_place(ctx, arg, PlacePurpose::BorrowMut) else {
            return;
        };
        if let Some(existing) = writebacks
            .iter()
            .find(|entry| crate::place::places_overlap(&entry.place, &place))
        {
            let name = ctx.local_name(place.local);
            // Reported in the source's numbering, which is the callee's shifted
            // back by however far this call's slot 0 sits from parameter 0.
            let other = existing.param as usize + slot - param as usize;
            self.emit(
                span,
                "KSEM247",
                format!(
                    "`{callee}` mutably borrows the same storage through `{name}` twice in \
                     one call (parameters {other} and {slot}); the two writes would land in \
                     the same place and the later one would erase the earlier"
                ),
            );
            return;
        }
        writebacks.push(HirWriteback { param, place });
    }

    /// The name of the copy specialized for these arguments' concrete classes.
    ///
    /// Built from the arguments rather than looked up by signature because the
    /// specialization *is* named after them — see `Analyzer::callable_name`. An
    /// argument whose type is the declared class contributes nothing, so a call
    /// that passes no subclass asks for the function as written and finds it.
    ///
    /// Returns the plain name when no specialization exists, which keeps a
    /// function past the specialization limit callable.
    fn specialized_name(&self, name: &str, args: &[HirExprId]) -> String {
        let mut suffix = String::new();
        for (index, arg) in args.iter().enumerate() {
            let Type::Struct(id) = self.program.expr(*arg).type_of() else {
                continue;
            };
            if !self.classes.contains_key(&id) {
                continue;
            }
            suffix.push_str(&format!(
                "${index}${}",
                self.program.types.type_name(Type::Struct(id))
            ));
        }
        let specialized = format!("{name}{suffix}");
        if !suffix.is_empty() && self.sig_index.contains_key(&specialized) {
            return specialized;
        }
        name.to_owned()
    }

    fn analyze_user_call(
        &mut self,
        name: &str,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        self.analyze_user_call_hinted(name, args, span, None)
    }

    /// Type-checks a call whose arguments are analyzed.
    ///
    /// `chosen` is the declaration an earlier pass already resolved this call
    /// to. It is honored unless class specialization renamed the callee, in
    /// which case the specialized copy is a different function and is looked up
    /// as one.
    fn analyze_user_call_hinted(
        &mut self,
        name: &str,
        args: &[HirExprId],
        span: kira_source::Span,
        chosen: Option<FuncId>,
    ) -> HirExprId {
        // A program may still declare a function called `sqrt`; a *call* of one
        // reaches the primitive only when nothing else answers to the name, so
        // the check runs after the user table below has been consulted.
        let specialized = self.specialized_name(name, args);
        let chosen = chosen.filter(|_| specialized == name);
        let name = &specialized;
        let candidates = self.visible_overloads(name);
        if candidates.is_empty() {
            if let Some(op) = kira_runtime_abi::MathOp::from_name(name) {
                return self.analyze_math_call(op, args, span);
            }
            if name == "scalarText" {
                return self.analyze_scalar_text_call(args, span);
            }
            self.emit(
                span,
                "KSEM061",
                format!("call to undefined function `{name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // The arguments are already analyzed here, so choosing among the
        // declarations of an overloaded name is a comparison rather than a
        // guess. A name declared once returns its one candidate untouched and
        // reports its own arity and type mistakes below, as it always did.
        let actual: Vec<Type> = args
            .iter()
            .map(|&arg| self.program.expr(arg).type_of())
            .collect();
        let id = match chosen
            .map(Ok)
            .unwrap_or_else(|| self.resolve_overload(&candidates, &actual))
        {
            Ok(id) => id,
            Err(OverloadFailure::Ambiguous(winners)) => {
                let list = self.overload_list(&winners);
                self.emit(
                    span,
                    "KSEM275",
                    format!("this call of `{name}` fits {list} equally well"),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            // Nothing fits. The first candidate carries the diagnostic, so an
            // overloaded name still says what it expected rather than only that
            // nothing matched.
            Err(OverloadFailure::None) => candidates[0],
        };
        let (params, ret) = {
            let (params, ret) = self.signature_of(id);
            (params.to_vec(), ret)
        };
        self.link_function(id, span);
        let mut args = args.to_vec();
        if args.len() != params.len() {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {} argument(s), found {}",
                    params.len(),
                    args.len()
                ),
            );
        } else {
            for (index, (arg, &expected)) in args.iter_mut().zip(params.iter()).enumerate() {
                let actual = self.program.expr(*arg).type_of();
                if !self.admits_argument(actual, expected) {
                    self.emit(
                        span,
                        "KSEM063",
                        format!(
                            "argument {} of `{name}` expects `{}`, found `{}`",
                            index + 1,
                            self.type_name(expected),
                            self.type_name(actual)
                        ),
                    );
                }
                // An `Any` parameter takes an erased value, and the erasure
                // belongs to the call site: the callee's body only ever sees the
                // boxed form.
                *arg = self.coerce_into(*arg, expected);
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(id),
            args,
            ty: ret,
            writebacks: Vec::new(),
        })
    }

    /// Analyzes `sqrt(x)` and the rest of the floating-point primitives.
    fn analyze_math_call(
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
    fn analyze_scalar_text_call(
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
