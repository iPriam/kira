//! Compiler-recognized opaque native callback-state intrinsics.

use std::collections::HashSet;

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{EnumId, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Analyzes one callback-state intrinsic, or returns `None` for another name.
    pub(super) fn analyze_native_state_intrinsic(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        Some(match name {
            "nativeState" => self.analyze_native_state(ctx, type_args, args, span),
            "nativeUserData" => self.analyze_native_user_data(ctx, type_args, args, span),
            "nativeRecover" => self.analyze_native_recover(ctx, type_args, args, span),
            "nativeStateFree" => self.analyze_native_state_free(ctx, type_args, args, span),
            _ => return None,
        })
    }

    fn analyze_native_state(
        &mut self,
        ctx: &mut FnCtx,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        self.reject_intrinsic_type_args("nativeState", type_args, span);
        let Some(value) = self.one_intrinsic_arg(ctx, "nativeState", args, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let ty = self.program.expr(value).type_of();
        if ty != Type::Error && !self.native_state_eligible(ty) {
            self.emit(
                span,
                "KSEM214",
                format!(
                    "`nativeState` requires a Kira-owned value, found `{}`",
                    self.type_name(ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Some(type_id) = self.program.types.native_state_type_id(ty) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let state_ty = self.program.types.native_state_of(ty);
        self.program.exprs.alloc(HirExpr::NativeState {
            value,
            type_id,
            ty: state_ty,
        })
    }

    fn analyze_native_user_data(
        &mut self,
        ctx: &mut FnCtx,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        self.reject_intrinsic_type_args("nativeUserData", type_args, span);
        let Some(state) = self.one_intrinsic_arg(ctx, "nativeUserData", args, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let ty = self.program.expr(state).type_of();
        if ty != Type::Error && self.program.types.native_state_target(ty).is_none() {
            self.emit(
                span,
                "KSEM215",
                format!(
                    "`nativeUserData` expects callback state, found `{}`",
                    self.type_name(ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // Handing the token out is where the compiler loses the thread. The
        // raw word can be stored in a slot, returned inside a struct, or given
        // to a C callback as its user data, and any of those owners may free
        // it — so this body is no longer the one that must. It is NOT a move:
        // the handle is still usable here, and the ordinary idiom hands the
        // token out and then frees the handle on its way out of the function.
        if let Some(arg) = args.first()
            && let Some(local) = self.named_local(ctx, arg.value)
        {
            ctx.mark_handed_out(local);
        }
        self.program.exprs.alloc(HirExpr::NativeUserData { state })
    }

    fn analyze_native_recover(
        &mut self,
        ctx: &mut FnCtx,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        let target = match type_args {
            [target] => self.resolve_type_ref(*target),
            _ => {
                self.emit(
                    span,
                    "KSEM216",
                    format!(
                        "`nativeRecover` takes exactly one type argument, found {}",
                        type_args.len()
                    ),
                );
                Type::Error
            }
        };
        let Some(raw) = self.one_intrinsic_arg(ctx, "nativeRecover", args, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let raw_ty = self.program.expr(raw).type_of();
        if raw_ty != Type::RawPtr && raw_ty != Type::Error {
            self.emit(
                span,
                "KSEM217",
                format!(
                    "`nativeRecover` expects `RawPtr`, found `{}`",
                    self.type_name(raw_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if target != Type::Error && !self.native_state_eligible(target) {
            self.emit(
                span,
                "KSEM214",
                format!(
                    "`nativeRecover` requires a Kira-owned type, found `{}`",
                    self.type_name(target)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if let Some(boxed) = self.statically_boxed_type(raw)
            && target != Type::Error
            && boxed != target
        {
            self.emit(
                span,
                "KSEM218",
                format!(
                    "`nativeRecover<{}>` cannot recover state boxed as `{}`",
                    self.type_name(target),
                    self.type_name(boxed)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Some(type_id) = self.program.types.native_state_type_id(target) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::NativeRecover {
            raw,
            type_id,
            ty: target,
        })
    }

    fn analyze_native_state_free(
        &mut self,
        ctx: &mut FnCtx,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        self.reject_intrinsic_type_args("nativeStateFree", type_args, span);
        let Some(token) = self.one_intrinsic_arg(ctx, "nativeStateFree", args, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let ty = self.program.expr(token).type_of();
        if ty != Type::RawPtr
            && ty != Type::Error
            && self.program.types.native_state_target(ty).is_none()
        {
            self.emit(
                span,
                "KSEM219",
                format!(
                    "`nativeStateFree` expects callback state or `RawPtr`, found `{}`",
                    self.type_name(ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // Freeing CONSUMES the handle. The binding still holds the same bits,
        // but they now name a box the runtime has torn down, and reading them
        // again is a use-after-free the box's magic word would catch at run
        // time. Marking it moved is what makes the existing `KSEM107` say so at
        // compile time instead, and it is what tells the end-of-body check this
        // handle was accounted for.
        if let Some(arg) = args.first()
            && let Some(local) = self.named_local(ctx, arg.value)
        {
            ctx.mark_moved(local, span);
        }
        self.program.exprs.alloc(HirExpr::NativeStateFree { token })
    }

    fn one_intrinsic_arg(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        let values: Vec<HirExprId> = args
            .iter()
            .map(|arg| self.analyze_expr(ctx, arg.value))
            .collect();
        if values.len() != 1 {
            self.emit(
                span,
                "KSEM220",
                format!(
                    "`{name}` takes exactly one value argument, found {}",
                    values.len()
                ),
            );
            None
        } else {
            values.first().copied()
        }
    }

    pub(super) fn reject_intrinsic_type_args(
        &mut self,
        name: &str,
        type_args: &[TypeRefId],
        span: Span,
    ) {
        if !type_args.is_empty() {
            self.emit(
                span,
                "KSEM221",
                format!("`{name}` does not take type arguments"),
            );
        }
    }

    fn statically_boxed_type(&self, raw: HirExprId) -> Option<Type> {
        let HirExpr::NativeUserData { state } = self.program.expr(raw) else {
            return None;
        };
        self.program
            .types
            .native_state_target(self.program.expr(*state).type_of())
    }

    /// Re-answers every callback-state identity once the program's shapes are
    /// final.
    ///
    /// A type id fingerprints a declaration's shape, and one shape is not final
    /// while bodies are still being analyzed: a closure's representation struct
    /// gains a field per capture, so a function type's repr grows as literals of
    /// it are found. A `nativeState` in the first file analyzed and a
    /// `nativeRecover<T>` in the last would fingerprint two different shapes of
    /// one type, and a correct program's recovery would be refused at run time.
    ///
    /// So an id is written twice: where the call is analyzed, so a type with no
    /// identity is refused at its own line, and again here, when every shape is
    /// final and the two sites cannot disagree.
    pub(crate) fn finalize_native_state_type_ids(&mut self) {
        let types = &self.program.types;
        for (_, expr) in self.program.exprs.iter_mut() {
            match expr {
                HirExpr::NativeState { type_id, ty, .. } => {
                    if let Some(target) = types.native_state_target(*ty)
                        && let Some(final_id) = types.native_state_type_id(target)
                    {
                        *type_id = final_id;
                    }
                }
                HirExpr::NativeRecover { type_id, ty, .. } => {
                    if let Some(final_id) = types.native_state_type_id(*ty) {
                        *type_id = final_id;
                    }
                }
                _ => {}
            }
        }
    }

    fn native_state_eligible(&self, ty: Type) -> bool {
        self.native_state_eligible_inner(ty, &mut HashSet::new())
    }

    fn native_state_eligible_inner(&self, ty: Type, visiting: &mut HashSet<Type>) -> bool {
        if !visiting.insert(ty) {
            return true;
        }
        let eligible = match ty {
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            | Type::RawPtr
            | Type::ForeignPtr(_) => true,
            Type::Struct(id) => self.native_state_struct_eligible(id, visiting),
            Type::Array(id) => self
                .program
                .types
                .arrays()
                .element(id)
                .is_some_and(|element| self.native_state_eligible_inner(element, visiting)),
            Type::Enum(id) => self.native_state_enum_eligible(id, visiting),
            // A capture cell goes in *shared*, which is the only way it could
            // go in at all: a closure inside the state and the frame that
            // declared the `var` are two holders of one box, and a copy would
            // give them a box each. The state holds a share like any other
            // holder, and gives it back when the state is freed — nothing is
            // handed to a host, which only ever sees an opaque token. What the
            // box holds still answers this question on its own terms.
            Type::Cell(id) => self
                .program
                .types
                .cells()
                .inner(id)
                .is_some_and(|inner| self.native_state_eligible_inner(inner, visiting)),
            // Recovering callback state is *typed*: `nativeRecover<T>` checks a
            // runtime identity against `T`. `Any` has no identity to check
            // (`TypeTable::native_state_type_id` gives it none), so boxing one
            // would produce state nothing could ever recover.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::NativeState(_)
            | Type::Task(_)
            | Type::Any => false,
        };
        visiting.remove(&ty);
        eligible
    }

    /// Whether a struct may be boxed as callback state.
    ///
    /// A **function type**'s representation struct qualifies on its own terms
    /// rather than by exception: it holds a tag and the captures of every
    /// closure literal of that type, and a capture already had to be trivially
    /// copyable to exist — so the generic field walk below answers `true` for it
    /// without a special case, and boxing a struct that holds a frame handler
    /// works because that is what an application's runtime state *is*.
    ///
    /// A C-layout struct still does not: its bytes are C's, and a box that
    /// recovered one would be handing back storage the box never owned.
    fn native_state_struct_eligible(&self, id: StructId, visiting: &mut HashSet<Type>) -> bool {
        let Some(def) = self.program.types.structs().get(id) else {
            return false;
        };
        if self.ffi_c_layout_named(&def.name).is_some() {
            return false;
        }
        def.fields
            .iter()
            .all(|field| self.native_state_eligible_inner(field.ty, visiting))
    }

    /// Whether an enum may be boxed as callback state.
    ///
    /// A variant's payload answers by the same rule as any other value.
    /// [`kira_runtime_abi::NativeStateValue`] carries a tag beside an optional
    /// payload, so struct and array payloads use the same boxed representation
    /// as direct fields.
    fn native_state_enum_eligible(&self, id: EnumId, visiting: &mut HashSet<Type>) -> bool {
        self.program.types.enums().get(id).is_some_and(|def| {
            def.variants.iter().all(|variant| {
                variant
                    .payload
                    .is_none_or(|payload| self.native_state_eligible_inner(payload, visiting))
            })
        })
    }
}
