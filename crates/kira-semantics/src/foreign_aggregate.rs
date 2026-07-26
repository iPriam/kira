//! Building the program's C-layout aggregate table from the structs an
//! `@FFI.Extern` signature names.
//!
//! A struct crosses the C seam by value when it is a `@FFI.Struct { layout: c }`
//! whose every field is a fixed-width seam scalar or another such struct, to any
//! depth. This module turns one of those into a [`ForeignAggregateId`] — an
//! index into [`HirProgram::foreign_aggregates`] — building the entry, and every
//! nested entry beneath it, the first time it is named.
//!
//! # Why the annotation is required
//!
//! An ordinary Kira struct is not admitted, even when its fields would all map.
//! The annotation is the author's statement that this type mirrors a C
//! declaration field for field, and the generated shim redeclares it in C on
//! exactly that basis. Without it, adding a field to a Kira struct would
//! silently change what a C function receives.
//!
//! # Ordering and cycles
//!
//! The table's invariant is that a member's id is always lower than the id of
//! the aggregate containing it, which is what makes layout a single forward pass
//! and a cycle unrepresentable. Recursion maintains it for free: a nested
//! aggregate is pushed on the way down, so it holds an id before its container
//! is built. A struct that reaches itself is caught on the way down by the
//! in-progress set and refused by name — the alternative is
//! [`ForeignAggregates::push`] rejecting it later with a message about table
//! indices, which names nothing the author wrote.

use std::collections::HashMap;

use kira_runtime_abi::{
    ForeignAggregate, ForeignAggregateId, ForeignArrayElement, ForeignMember, ForeignType,
};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;

use crate::analyze::Analyzer;
use crate::ffi_types::FfiStructKind;

/// The per-analysis state that keeps each aggregate in the table exactly once.
#[derive(Debug, Default)]
pub(crate) struct ForeignAggregateBuilder {
    /// The id already built for a struct, so naming it twice adds one row.
    built: HashMap<StructId, ForeignAggregateId>,
    /// The structs currently being built, innermost last — a struct reached
    /// while it is in here contains itself.
    in_progress: Vec<StructId>,
}

impl Analyzer<'_> {
    /// The aggregate id a C-layout struct crosses as, building its table entry
    /// on first use.
    ///
    /// `None` when the struct is not a `@FFI.Struct { layout: c }`, when it
    /// contains itself, or when a field cannot cross — each refused by name at
    /// `span`, so the author is told which field is the problem rather than that
    /// "an aggregate" failed.
    pub(crate) fn aggregate_seam_of(
        &mut self,
        id: StructId,
        span: Span,
    ) -> Option<ForeignAggregateId> {
        if let Some(built) = self.foreign_aggregates.built.get(&id) {
            return Some(*built);
        }
        match self.ffi_struct_kind(id) {
            // A callback's one `RawPtr` field walks the same generic path: a
            // struct holding one pointer has a pointer's size, alignment, and
            // ABI class, so the row describes the C member exactly.
            Some(FfiStructKind::CLayout | FfiStructKind::Callback) => {}
            // An `@FFI.Array` type is a C array typedef, and a struct holding
            // only that array has the array's own size and alignment — so it
            // crosses as a one-member row, and every place that names one is an
            // ordinary aggregate reference.
            Some(FfiStructKind::Array) => return self.array_seam_of(id, span),
            _ => return None,
        }
        if self.foreign_aggregates.in_progress.contains(&id) {
            self.emit(
                span,
                "KSEM182",
                format!(
                    "`{}` contains itself, so it has no C layout: a struct crossing the \
                     C seam by value must be finite",
                    self.struct_name(id)
                ),
            );
            return None;
        }

        let fields = self
            .program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.clone())?;

        self.foreign_aggregates.in_progress.push(id);
        let mut members = Vec::with_capacity(fields.len());
        let mut ok = true;
        for field in &fields {
            match self.aggregate_member_of(field.ty, id, &field.name, span) {
                Some(member) => members.push(member),
                None => ok = false,
            }
        }
        self.foreign_aggregates.in_progress.pop();
        if !ok {
            return None;
        }

        // A push can still fail on the table's own invariant. It cannot here —
        // every member was pushed while this struct was in progress, so each
        // holds a lower id — but the table owns that rule, and reporting its
        // refusal is honest where asserting it would not be.
        match self
            .program
            .foreign_aggregates
            .push(ForeignAggregate::new(members))
        {
            Ok(built) => {
                self.foreign_aggregates.built.insert(id, built);
                Some(built)
            }
            Err(error) => {
                self.emit(
                    span,
                    "KSEM182",
                    format!(
                        "`{}` cannot be described at the C seam: {error}",
                        self.struct_name(id)
                    ),
                );
                None
            }
        }
    }

    /// The aggregate id an `@FFI.Array` type crosses as: a row holding one
    /// inline array member.
    ///
    /// The element is a seam scalar or another C-layout aggregate, to any depth
    /// — including another `@FFI.Array`, which is how a C array of arrays is
    /// spelled.
    fn array_seam_of(&mut self, id: StructId, span: Span) -> Option<ForeignAggregateId> {
        let count = self.ffi_array_count(id)?;
        let element = self
            .program
            .types
            .structs()
            .get(id)
            .and_then(|def| def.fields.first())
            .map(|field| field.ty)
            .and_then(|ty| self.program.types.element_of(ty))?;
        if self.foreign_aggregates.in_progress.contains(&id) {
            self.emit(
                span,
                "KSEM182",
                format!(
                    "`{}` contains itself, so it has no C layout: a struct crossing the \
                     C seam by value must be finite",
                    self.struct_name(id)
                ),
            );
            return None;
        }

        self.foreign_aggregates.in_progress.push(id);
        let member = self.array_element_of(element, id, span);
        self.foreign_aggregates.in_progress.pop();
        let member = member?;

        match self
            .program
            .foreign_aggregates
            .push(ForeignAggregate::new(vec![ForeignMember::Array {
                element: member,
                count,
            }])) {
            Ok(built) => {
                self.foreign_aggregates.built.insert(id, built);
                Some(built)
            }
            Err(error) => {
                self.emit(
                    span,
                    "KSEM182",
                    format!(
                        "`{}` cannot be described at the C seam: {error}",
                        self.struct_name(id)
                    ),
                );
                None
            }
        }
    }

    /// One array element's entry, or `None` after reporting why it cannot cross.
    fn array_element_of(
        &mut self,
        ty: Type,
        container: StructId,
        span: Span,
    ) -> Option<ForeignArrayElement> {
        if ty == Type::Error {
            return None;
        }
        if let Some(scalar) = member_foreign_type(ty) {
            return Some(ForeignArrayElement::Scalar(scalar));
        }
        if let Type::Struct(nested) = ty {
            return self
                .aggregate_seam_of(nested, span)
                .map(ForeignArrayElement::Aggregate);
        }
        self.emit(
            span,
            "KSEM182",
            format!(
                "`{}` holds `{}`, which cannot cross the C seam: an inline C array's elements \
                 are fixed-width scalars, `Bool`, `RawPtr`, `CString`, and `@FFI.Struct {{ layout: c }}` \
                 or `@FFI.Array` types",
                self.struct_name(container),
                self.type_name(ty),
            ),
        );
        None
    }

    /// One field's member entry, or `None` after reporting why it cannot cross.
    fn aggregate_member_of(
        &mut self,
        ty: Type,
        container: StructId,
        field: &str,
        span: Span,
    ) -> Option<ForeignMember> {
        // Whatever produced an `Error` type already spoke.
        if ty == Type::Error {
            return None;
        }
        if let Some(scalar) = member_foreign_type(ty) {
            return Some(ForeignMember::Scalar(scalar));
        }
        if let Type::Struct(nested) = ty
            && matches!(
                self.ffi_struct_kind(nested),
                Some(FfiStructKind::CLayout | FfiStructKind::Array | FfiStructKind::Callback)
            )
        {
            return self
                .aggregate_seam_of(nested, span)
                .map(ForeignMember::Aggregate);
        }
        self.emit(
            span,
            "KSEM182",
            format!(
                "field `{field}` of `{}` cannot cross the C seam as `{}`: a C-layout \
                 struct's fields are fixed-width scalars, `Bool`, `RawPtr`, `CString`, and \
                 other `@FFI.Struct {{ layout: c }}`, `@FFI.Array`, or `@FFI.Callback` types",
                self.struct_name(container),
                self.type_name(ty),
            ),
        );
        None
    }

    /// A struct's written name, for a diagnostic.
    fn struct_name(&self, id: StructId) -> String {
        self.type_name(Type::Struct(id))
    }
}

/// The [`ForeignType`] a C-layout struct's field or a C array's element sits in
/// the layout as.
///
/// This is [`crate::foreign::scalar_foreign_type`] plus `CString`, and the extra
/// case is the whole reason it exists. At a *signature* `CString` is a direction
/// — a borrowed C string the seam builds for the duration of one call — so a
/// result position has to refuse it. In a **layout** it is not a direction at
/// all: `const char* window_title` is a pointer word, laid out and aligned
/// exactly as a `RawPtr` member is, and a binding file that mirrors a C struct
/// has to be able to say so. Nothing is loosened by admitting it: a value of
/// such a struct still cannot be constructed with a `CString` field, which the
/// zero-fill rule refuses where the value would be minted.
fn member_foreign_type(ty: Type) -> Option<ForeignType> {
    match ty {
        Type::CString => Some(ForeignType::CString),
        other => crate::foreign::scalar_foreign_type(other),
    }
}
