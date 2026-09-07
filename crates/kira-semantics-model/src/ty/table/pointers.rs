//! Where a pointer word sits inside a type.
//!
//! A rule that refuses a pointer has to ask about the whole value, not its
//! outermost shape. `RawPtr` is refused at a seam because an address means
//! nothing in the context that receives it, and that is just as true when the
//! address is a field of the struct that crossed, an element of the array, or
//! the payload of the enum variant. Asking only about the outer type lets the
//! same address across inside a wrapper.

use std::collections::HashSet;

use super::TypeTable;
use crate::ty::Type;

impl TypeTable {
    /// The first pointer word this type carries, at any depth, or `None`.
    ///
    /// Answers the pointer's own type rather than a bare `bool` so a refusal
    /// can name what it found: `RawPtr` and a `ForeignPtr` read differently to
    /// an author, and a distinct over either reads differently again.
    ///
    /// The walk mirrors [`TypeTable::native_state_type_id`]'s: the same shapes
    /// are opened, in the same order, because it is the same question about
    /// the same value — that one asks what the store can hold and this asks
    /// what the value would mean on the far side.
    pub fn pointer_word_within(&self, ty: Type) -> Option<Type> {
        let mut visiting = HashSet::new();
        self.find_pointer_word(ty, &mut visiting)
    }

    fn find_pointer_word(&self, ty: Type, visiting: &mut HashSet<(u8, u32)>) -> Option<Type> {
        match ty {
            Type::RawPtr | Type::ForeignPtr(_) => Some(ty),
            // A distinct over a pointer is the pointer, and it is the spelling
            // the author wrote, so it is the one worth naming.
            Type::Distinct(id) => {
                let representation = self.distincts.get(id)?.representation;
                if matches!(representation, Type::RawPtr | Type::ForeignPtr(_)) {
                    return Some(ty);
                }
                if !visiting.insert((10, id.index())) {
                    return None;
                }
                let found = self.find_pointer_word(representation, visiting);
                visiting.remove(&(10, id.index()));
                found
            }
            Type::Struct(id) => {
                let def = self.structs.get(id)?;
                if !visiting.insert((5, id.index())) {
                    return None;
                }
                let found = def
                    .fields
                    .iter()
                    .find_map(|field| self.find_pointer_word(field.ty, visiting));
                visiting.remove(&(5, id.index()));
                found
            }
            Type::Array(id) => {
                let element = self.arrays.element(id)?;
                if !visiting.insert((6, id.index())) {
                    return None;
                }
                let found = self.find_pointer_word(element, visiting);
                visiting.remove(&(6, id.index()));
                found
            }
            Type::Enum(id) => {
                let def = self.enums.get(id)?;
                if !visiting.insert((7, id.index())) {
                    return None;
                }
                let found = def
                    .variants
                    .iter()
                    .filter_map(|variant| variant.payload)
                    .find_map(|payload| self.find_pointer_word(payload, visiting));
                visiting.remove(&(7, id.index()));
                found
            }
            Type::Cell(id) => {
                let inner = self.cells.inner(id)?;
                if !visiting.insert((9, id.index())) {
                    return None;
                }
                let found = self.find_pointer_word(inner, visiting);
                visiting.remove(&(9, id.index()));
                found
            }
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            | Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::NativeState(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::RuntimeType
            | Type::Any => None,
        }
    }
}
