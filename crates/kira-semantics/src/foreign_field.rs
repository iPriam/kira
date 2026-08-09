//! Reading a member through an `@FFI.Pointer`.
//!
//! [`super::foreign_aggregate`] describes a C-layout struct so it can be *sent*;
//! this is the other direction. A callback receives `const sapp_event*` — a
//! pointer into storage C owns — and the fields behind it are the whole payload.
//! Without a read, every one of them needs a C accessor compiled into a shim,
//! which is exactly the shape a binding is supposed to remove.
//!
//! What makes the read possible is that the pointer keeps its target: an
//! `@FFI.Pointer { target: T; }` whose `T` resolves to a C-layout struct becomes
//! a [`Type::ForeignPtr`], and the aggregate row already built for `T` says
//! where each member sits.
//!
//! A member is one of two things. A scalar reads back as a value — that is a
//! load. A nested struct or an inline array has its bytes *inside* the
//! container, so it names a place, and what a read of it produces is that
//! place's address. `event.at.x` and `event.touches[2].pos_x` both fall out of
//! that: each step is an address until the last, which is the load.
//!
//! Offsets and element sizes are computed per backend, not here: a C pointer is
//! four bytes on `wasm32` and eight elsewhere, so a struct with a pointer member
//! ahead of this one lays out differently per target.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{ForeignPtrId, StructId, Type};
use kira_source::Span;

use crate::analyze::Analyzer;
use crate::ffi_types::FfiStructKind;

impl Analyzer<'_> {
    /// Analyzes `pointer.name` — a member reached through an `@FFI.Pointer`.
    pub(crate) fn analyze_foreign_field(
        &mut self,
        base: HirExprId,
        pointer: ForeignPtrId,
        name: &str,
        span: Span,
    ) -> HirExprId {
        let Some(target) = self.program.types.foreign_ptr_target(pointer) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let owner = self.type_name(Type::Struct(target));
        let member = self
            .program
            .types
            .structs()
            .get(target)
            .and_then(|def| def.field_index(name).map(|index| (index, def)))
            .and_then(|(index, def)| def.field(index).map(|field| (index, field.ty)));
        let Some((index, ty)) = member else {
            self.emit(
                span,
                "KSEM090",
                format!("`{owner}` has no field `{name}`, so a pointer to it has none to read"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let Some(aggregate) = self.aggregate_seam_of(target, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.link_field_name(&owner, name, span);
        // A member whose bytes live inside the container is a place, so what the
        // read produces is its address. Which place it is decides what the
        // pointer addresses: a nested struct addresses itself, and an inline
        // array addresses its first element, the way C's array-to-pointer decay
        // does — that is what makes `touches[2]` the next step rather than a
        // second spelling.
        if let Some(addressed) = self.foreign_storage_member(ty) {
            let pointer = self.program.types.foreign_ptr_to(addressed);
            return self.program.exprs.alloc(HirExpr::ForeignMemberAddress {
                base,
                aggregate,
                member: index,
                ty: pointer,
            });
        }
        self.program.exprs.alloc(HirExpr::ForeignField {
            base,
            aggregate,
            member: index,
            ty,
        })
    }

    /// Analyzes `pointer[index]` — one element of a C array.
    pub(crate) fn analyze_foreign_element(
        &mut self,
        base: HirExprId,
        pointer: ForeignPtrId,
        index: HirExprId,
        span: Span,
    ) -> HirExprId {
        let Some(target) = self.program.types.foreign_ptr_target(pointer) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let Some(aggregate) = self.aggregate_seam_of(target, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let ty = self.program.types.foreign_ptr_to(target);
        self.program.exprs.alloc(HirExpr::ForeignElement {
            base,
            aggregate,
            index,
            ty,
        })
    }

    /// The struct a storage member's address points at, if the member is one.
    ///
    /// A nested C-layout struct addresses itself. An `@FFI.Array` addresses its
    /// element, which is what an array's name means in C. Anything else is a
    /// value and is loaded rather than addressed.
    fn foreign_storage_member(&mut self, ty: Type) -> Option<StructId> {
        let Type::Struct(id) = ty else {
            return None;
        };
        match self.ffi_struct_kind(id)? {
            FfiStructKind::CLayout => Some(id),
            FfiStructKind::Array => self.foreign_array_element(id),
            _ => None,
        }
    }

    /// The struct an `@FFI.Array`'s elements are, when they are aggregates.
    ///
    /// An array of scalars addresses its element type, which has no struct to
    /// name; those are reached as the one-member aggregate the array itself
    /// crosses as, so the array's own row is what the address points at.
    fn foreign_array_element(&mut self, id: StructId) -> Option<StructId> {
        let element = self
            .program
            .types
            .structs()
            .get(id)
            .and_then(|def| def.fields.first())
            .map(|field| field.ty)
            .and_then(|ty| self.program.types.element_of(ty))?;
        match element {
            Type::Struct(element) => Some(element),
            _ => Some(id),
        }
    }
}
