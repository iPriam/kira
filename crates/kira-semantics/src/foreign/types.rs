//! Mapping Kira types onto the foreign ones a signature can carry.
//!
//! Every refusal here names the shape rather than the position, because a
//! declaration that cannot cross is wrong wherever it appears.

use super::*;

impl<'a> Analyzer<'a> {
    /// Maps a foreign declaration's written signature to its wire
    /// [`ForeignSignature`] and the per-position wrapper structs, or `None` when
    /// any parameter or the result cannot cross the seam.
    pub(super) fn map_foreign_signature(&mut self, function: &Function) -> Option<MappedForeign> {
        let mut params = Vec::with_capacity(function.params.len());
        let mut param_wrappers = Vec::with_capacity(function.params.len());
        let mut param_pointees = Vec::with_capacity(function.params.len());
        let mut ok = true;
        for param in &function.params {
            match self.map_foreign_param(param) {
                Some(seam) => {
                    params.push(seam.spec);
                    param_wrappers.push(seam.wrapper);
                    param_pointees.push(seam.pointee);
                }
                None => ok = false,
            }
        }
        let result = self.map_foreign_result(function);
        match (ok, result) {
            (true, Some(result)) => Some(MappedForeign {
                signature: ForeignSignature::new(params, result.spec),
                param_wrappers: param_wrappers.into(),
                param_pointees: param_pointees.into(),
                result_pointee: result.pointee.map(|pointee| pointee.struct_id),
                result_wrapper: result.wrapper,
            }),
            _ => None,
        }
    }

    /// Maps one written parameter to its seam type, reporting why it cannot
    /// cross when it cannot.
    pub(super) fn map_foreign_param(&mut self, param: &Param) -> Option<ForeignSeam> {
        let span = self.tree.type_ref(param.ty).span();
        if let Some(()) = self.refuse_written_shape(param.ty, span) {
            return None;
        }
        let ty = self.resolve_foreign_type(param.ty);
        let mut seam = self.foreign_seam_of(ty, span, Position::Param)?;
        // A parameter written as an `@FFI.Pointer` to a C-layout struct is a
        // pointer word at the wire and also accepts the struct itself, which the
        // call hands over by address.
        //
        // A pointer whose target resolved carries it on the type; one whose
        // target did not is a plain `RawPtr`, and the name is looked up in case
        // the target became visible after the alias resolved. A pointer to a C
        // type nobody declared is an opaque handle, not a mistake, so neither
        // path reports.
        let target = match ty {
            Type::ForeignPtr(pointer) => self.program.types.foreign_ptr_target(pointer),
            Type::RawPtr => {
                let written = self.written_type_name(param.ty);
                self.pointer_targets
                    .get(&written)
                    .cloned()
                    .and_then(|target| self.visible_struct(&target))
            }
            _ => None,
        };
        if let Some(struct_id) = target
            && self.ffi_struct_kind(struct_id) == Some(FfiStructKind::CLayout)
            && let Some(aggregate) = self.aggregate_seam_of(struct_id, span)
        {
            seam.pointee = Some(kira_semantics_model::hir::ForeignPointee {
                struct_id,
                aggregate,
            });
        }
        Some(seam)
    }

    /// Maps the written result to its seam type; an absent result is
    /// [`ForeignType::Void`].
    pub(super) fn map_foreign_result(&mut self, function: &Function) -> Option<ForeignSeam> {
        let Some(type_ref) = function.return_type else {
            return Some(ForeignSeam::scalar(ForeignType::Void));
        };
        let span = self.tree.type_ref(type_ref).span();
        if let Some(()) = self.refuse_written_shape(type_ref, span) {
            return None;
        }
        let ty = self.resolve_foreign_type(type_ref);
        let mut seam = self.foreign_seam_of(ty, span, Position::Result)?;
        // A result written as an `@FFI.Pointer` to a C-layout struct is a
        // pointer word at the wire, and the call hands back a pointer that still
        // knows its target so members can be read through it.
        if let Type::ForeignPtr(pointer) = ty
            && let Some(struct_id) = self.program.types.foreign_ptr_target(pointer)
        {
            seam.pointee = Some(kira_semantics_model::hir::ForeignPointee {
                struct_id,
                // The target's own row, which the read needs to find member
                // offsets. A target that cannot be described has no readable
                // members, and the pointer stays a plain word.
                aggregate: self.aggregate_seam_of(struct_id, span)?,
            });
        }
        Some(seam)
    }

    /// The seam a written parameter or result crosses as.
    ///
    /// Three struct shapes are tried in order, and the order matters. A
    /// single-scalar-field struct (a C handle like `sg_image { id: U32 }`)
    /// crosses as its field's scalar — checked first because it stays the
    /// cheaper crossing even when the struct also carries the C-layout
    /// annotation, and because it is the shape already proven end to end. A
    /// `@FFI.Struct { layout: c }` of any other shape crosses by value as a
    /// C-layout aggregate. Everything else falls to [`Self::foreign_type_of`],
    /// which maps the scalars and refuses the rest — including an `@FFI.Array`
    /// on its own, whose refusal names the form.
    ///
    /// A `@FFI.Callback` needs no case of its own: its storage is one `RawPtr`
    /// field, so the single-scalar-field rule crosses it as the pointer a C
    /// function pointer is.
    pub(super) fn foreign_seam_of(
        &mut self,
        ty: Type,
        span: Span,
        position: Position,
    ) -> Option<ForeignSeam> {
        if let Type::Struct(id) = ty
            && !self.ffi_struct_kind(id).is_some_and(is_deferred_ffi)
        {
            if let Some(field_ty) = self.single_scalar_field_seam(id) {
                return Some(ForeignSeam {
                    pointee: None,
                    spec: ForeignTypeSpec::Scalar(field_ty),
                    wrapper: Some(id),
                });
            }
            if self.ffi_struct_kind(id) == Some(FfiStructKind::CLayout) {
                // A `Void` result is the only position an aggregate cannot take,
                // and it cannot be written as one, so no position check is
                // needed here: a C function may both take and return a struct.
                let _ = position;
                return self
                    .aggregate_seam_of(id, span)
                    .map(|aggregate| ForeignSeam {
                        pointee: None,
                        spec: ForeignTypeSpec::Aggregate(aggregate),
                        wrapper: Some(id),
                    });
            }
        }
        self.foreign_type_of(ty, span, position)
            .map(ForeignSeam::scalar)
    }

    /// The [`ForeignType`] a struct crosses as when it has exactly one field and
    /// that field is a seam scalar — the C single-member-struct handle, passed
    /// in a register exactly like its member. `None` for any other shape (no
    /// field, many fields, or a field that is itself an aggregate).
    pub(super) fn single_scalar_field_seam(&self, id: StructId) -> Option<ForeignType> {
        let def = self.program.types.structs().get(id)?;
        let [field] = def.fields.as_slice() else {
            return None;
        };
        scalar_foreign_type(field.ty)
    }

    /// Resolves a type inside a foreign signature, where `CString` is permitted
    /// to resolve without the seam-only refusal that guards every other
    /// position.
    pub(crate) fn resolve_foreign_type(
        &mut self,
        type_ref: kira_syntax_model::ast::TypeRefId,
    ) -> Type {
        self.in_foreign_signature = true;
        let ty = self.resolve_type_ref(type_ref);
        self.in_foreign_signature = false;
        ty
    }

    /// Refuses a written function type with a message precise to the shape.
    /// Returns `Some(())` when it refused.
    ///
    /// Caught from the written [`TypeRef`] rather than the resolved [`Type`]
    /// because the shape names the fix: a function type has no resolved-type
    /// spelling to blame, and `@FFI.Callback` is the form that carries one.
    ///
    pub(super) fn refuse_written_shape(
        &mut self,
        type_ref: kira_syntax_model::ast::TypeRefId,
        span: Span,
    ) -> Option<()> {
        match self.tree.type_ref(type_ref) {
            TypeRef::Function { .. } => {
                self.emit(
                    span,
                    "KSEM182",
                    "a function pointer cannot cross the C seam: `@FFI.Extern` supports \
                     no callback parameter or result",
                );
                Some(())
            }
            _ => None,
        }
    }

    /// Whether an enum is a set of named numbers, which is what a C enum is.
    ///
    /// Every case payload-less. One that carries a payload is a tagged union,
    /// and C's own is a different shape with a different layout — so it has no
    /// crossing here rather than a lossy one.
    pub(super) fn enum_crosses_as_a_number(&self, id: kira_semantics_model::EnumId) -> bool {
        self.program
            .types
            .enums()
            .get(id)
            .is_some_and(|def| def.variants.iter().all(|variant| variant.payload.is_none()))
    }
}
