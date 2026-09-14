//! Compiler-recognized opaque native callback-state intrinsics.

use std::collections::HashSet;

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{EnumId, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;

/// Which way a userdata token's owner count moves.
#[derive(Debug, Clone, Copy)]
enum Counting {
    Retain,
    Release,
}

impl Counting {
    fn name(self) -> &'static str {
        match self {
            Self::Retain => "nativeUserDataRetain",
            Self::Release => "nativeUserDataRelease",
        }
    }
}

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
            "nativeUserData" => self.analyze_native_user_data(ctx, type_args, args, span, false),
            "nativeUserDataBorrow" => {
                self.analyze_native_user_data(ctx, type_args, args, span, true)
            }
            "nativeRecover" => self.analyze_native_recover(ctx, type_args, args, span),
            "nativeUserDataRetain" => {
                self.analyze_native_state_count(ctx, Counting::Retain, type_args, args, span)
            }
            "nativeUserDataRelease" => {
                self.analyze_native_state_count(ctx, Counting::Release, type_args, args, span)
            }
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
        if self.program.types.native_state_target(ty).is_some() {
            self.emit(
                span,
                "KSEM377",
                "`nativeState` cannot box a `NativeState` handle as the root value; move the handle as a field of an owning aggregate instead",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
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
        borrowed: bool,
    ) -> HirExprId {
        let name = if borrowed {
            "nativeUserDataBorrow"
        } else {
            "nativeUserData"
        };
        self.reject_intrinsic_type_args(name, type_args, span);
        let Some(state) = self.one_intrinsic_arg(ctx, name, args, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let state_ty = self.program.expr(state).type_of();
        if state_ty != Type::Error && self.program.types.native_state_target(state_ty).is_none() {
            self.emit(
                span,
                "KSEM215",
                format!(
                    "`{name}` expects callback state, found `{}`",
                    self.type_name(state_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if borrowed
            && self
                .resolve_place(ctx, args[0].value, PlacePurpose::UserDataBorrow)
                .is_none()
        {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if borrowed {
            // The intrinsic borrows the owner in place. A field such as
            // `wrapper.storage` therefore does not extract a second affine
            // owner; claim the deferred field read exactly like any borrowed
            // call argument does.
            self.excuse_drop_extraction(state);
        } else {
            // `nativeUserData(owner)` is the explicit clone operation for a
            // typed state owner. It may therefore copy an affine field on
            // purpose; ordinary field reads are still refused.
            self.excuse_drop_extraction(state);
        }
        self.program.exprs.alloc(HirExpr::NativeUserData {
            state,
            borrowed,
            ty: if borrowed { Type::RawPtr } else { state_ty },
        })
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
        let owned_state = self.program.types.native_state_target(raw_ty).is_some();
        if raw_ty != Type::RawPtr && !owned_state && raw_ty != Type::Error {
            self.emit(
                span,
                "KSEM217",
                format!(
                    "`nativeRecover` expects an owned userdata handle or borrowed `RawPtr`, found `{}`",
                    self.type_name(raw_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // Recovery borrows an owner; it never manufactures another one. An
        // owned state therefore has to be a real place whose token remains
        // rooted for the duration of the borrow. Refusing owner temporaries is
        // deliberately stricter than trying to invent an implicit lifetime for
        // a retained temporary during lowering.
        if owned_state
            && self
                .resolve_place(ctx, args[0].value, PlacePurpose::NativeRecoverBorrow)
                .is_none()
        {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if owned_state {
            // Recovery is a borrow of the owner place, not an extraction of a
            // second NativeState value.
            self.excuse_drop_extraction(raw);
        }
        if self.program.types.native_state_target(target).is_some() {
            self.emit(
                span,
                "KSEM377",
                "`nativeRecover<NativeState<...>>` is not a root recovery; recover the owning aggregate and borrow its state field instead",
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

    /// Explicit callback-state owner counting.
    ///
    /// Kira code may release a tracked `NativeState<T>` owner early, but it may
    /// never mint or destroy ownership through a `RawPtr`. A raw userdata word
    /// is a borrow only. Ownership may cross into C only through a `retains:`
    /// foreign parameter, whose `move` is visible to ownership analysis; C then
    /// balances that transferred reference through the runtime ABI.
    fn analyze_native_state_count(
        &mut self,
        ctx: &mut FnCtx,
        counting: Counting,
        type_args: &[TypeRefId],
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        let name = counting.name();
        if !type_args.is_empty() {
            self.reject_intrinsic_type_args(name, type_args, span);
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Some(token) = self.one_intrinsic_arg(ctx, name, args, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let ty = self.program.expr(token).type_of();
        let owned = self.program.types.native_state_target(ty).is_some();

        if matches!(counting, Counting::Retain) {
            self.emit(
                span,
                "KSEM378",
                "`nativeUserDataRetain` is not available to Kira code: manufacturing an owner without a typed affine value would make the release obligation invisible. Clone a tracked owner with `nativeUserData(owner)`, or transfer one to C with a `retains:` parameter and `move`.",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }

        if ty == Type::RawPtr {
            self.emit(
                span,
                "KSEM379",
                "`nativeUserDataRelease` cannot consume `RawPtr`: raw userdata is a borrow and carries no ownership. Release a tracked `NativeState<T>` owner, or let C balance an owner explicitly transferred through a `retains:` parameter.",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if ty != Type::Error && !owned {
            self.emit(
                span,
                "KSEM361",
                format!(
                    "`{name}` expects a tracked `NativeState<T>` owner, found `{}`.",
                    self.type_name(ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // Releasing an affine userdata owner consumes that owner. Its scope must
        // not release the same reference a second time.
        if owned
            && let Some(arg) = args.first()
            && let Some(local) = self.named_local(ctx, arg.value)
        {
            ctx.mark_moved(local, span);
        }
        self.program.exprs.alloc(HirExpr::NativeStateRelease {
            token,
            target: None,
        })
    }

    /// `nativeStateFree(handle)`: the pre-1.9.1 spelling of one release,
    /// kept so that programs keep compiling and warned about (`KSEM360`).
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
        if ty == Type::RawPtr {
            self.emit(
                span,
                "KSEM379",
                "`nativeStateFree` cannot consume `RawPtr`: raw userdata is a borrow and carries no ownership.",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if ty != Type::Error && self.program.types.native_state_target(ty).is_none() {
            self.emit(
                span,
                "KSEM219",
                format!(
                    "`nativeStateFree` expects tracked callback state, found `{}`",
                    self.type_name(ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.emit_warning(
            span,
            "KSEM360",
            "`nativeStateFree` is deprecated: it releases one reference, not the state. Let \
             the handle go out of scope to release its reference. Raw userdata is borrowed \
             and cannot be released from Kira code.",
        );
        // Releasing through the handle consumes it: the handle's reference is
        // the one given up, so the binding no longer owns anything to release
        // at the end of its scope, and reading it again is a use after move.
        if let Some(arg) = args.first()
            && let Some(local) = self.named_local(ctx, arg.value)
        {
            ctx.mark_moved(local, span);
        }
        self.program.exprs.alloc(HirExpr::NativeStateRelease {
            token,
            target: None,
        })
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
        let raw_ty = self.program.expr(raw).type_of();
        if let Some(target) = self.program.types.native_state_target(raw_ty) {
            return Some(target);
        }
        let HirExpr::NativeUserData { state, .. } = self.program.expr(raw) else {
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
            // A distinct type is eligible exactly when the scalar it is would
            // be, and its runtime identity is its own — see
            // `TypeTable::native_state_type_id`, which fingerprints the name so
            // a recovery cannot confuse a `TabId` state with a `U32` one.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.native_state_eligible_inner(representation, visiting)
            }
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
            // A nested NativeState remains an affine owner inside the portable
            // tree; every backend preserves its token identity and release.
            Type::NativeState(id) => self
                .program
                .types
                .native_state_target(Type::NativeState(id))
                .is_some_and(|target| self.native_state_eligible_inner(target, visiting)),
            // Recovering callback state is *typed*: `nativeRecover<T>` checks a
            // runtime identity against `T`. `Any` has no identity to check
            // (`TypeTable::native_state_type_id` gives it none), so boxing one
            // would produce state nothing could ever recover.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::Number
            | Type::RuntimeType
            | Type::Task(_)
            | Type::MainThreadTask(_)
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
