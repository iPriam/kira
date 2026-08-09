//! Foreign pointer types and the program's interning table.
//!
//! An `@FFI.Pointer { target: T; }` names a C pointer. At the wire it is one
//! pointer word and nothing more — which is exactly what [`Type::RawPtr`]
//! already says, and why every such typedef used to erase to it.
//!
//! What that lost is the target. `const sapp_event*` arriving in a callback is a
//! pointer Kira can hold but not read: `event.kind` asks a targetless pointer
//! for a field, and the only way to get the value was a C accessor per field.
//! This type keeps the target, so a field read resolves against the struct's
//! C layout and lowers to a load at that member's offset.
//!
//! A foreign pointer is still a pointer word everywhere else: it crosses the
//! seam as one, boxes as one, and compares as one, so adding this type changed
//! no wire format.
//!
//! The table interns exactly as [`super::cells::CellTable`] does, and for the
//! same reason: it keeps [`Type`] `Copy` while still letting the type carry what
//! it points at.

use std::collections::HashMap;

use crate::StructId;

/// Index of a foreign pointer type within a [`ForeignPtrTable`].
///
/// Only a [`ForeignPtrTable`] mints one, so an id always names a row of the
/// table it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ForeignPtrId(u32);

impl ForeignPtrId {
    /// This id as an index, for a backend keying its own per-pointer-type data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// Every foreign pointer type a program mentions, indexed by [`ForeignPtrId`].
///
/// Interning makes two pointers to the same struct one type, so
/// `Type::ForeignPtr(a) == Type::ForeignPtr(b)` compares two `u32`s.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ForeignPtrTable {
    targets: Vec<StructId>,
    // Kept in step with `targets` by `intern`, the only way to add a row.
    index: HashMap<StructId, ForeignPtrId>,
}

impl ForeignPtrTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of the pointer type addressing `target`, minting one if this is
    /// the first mention, or `None` when the id space is exhausted.
    pub fn intern(&mut self, target: StructId) -> Option<ForeignPtrId> {
        if let Some(&id) = self.index.get(&target) {
            return Some(id);
        }
        let id = ForeignPtrId(u32::try_from(self.targets.len()).ok()?);
        self.targets.push(target);
        self.index.insert(target, id);
        Some(id)
    }

    /// What the pointer named by `id` addresses.
    pub fn target(&self, id: ForeignPtrId) -> Option<StructId> {
        self.targets.get(id.0 as usize).copied()
    }

    /// How many pointer types the program mentions.
    pub fn len(&self) -> usize {
        self.targets.len()
    }

    /// Whether the program mentions no foreign pointer type.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_pointers_to_one_struct_are_one_type() {
        let mut table = ForeignPtrTable::new();
        let target = StructId::new(7);
        let first = table.intern(target).expect("first");
        let again = table.intern(target).expect("again");
        assert_eq!(first, again);
        assert_eq!(table.len(), 1);
        assert_eq!(table.target(first), Some(target));
    }

    #[test]
    fn pointers_to_different_structs_are_different_types() {
        let mut table = ForeignPtrTable::new();
        let first = table.intern(StructId::new(1)).expect("first");
        let second = table.intern(StructId::new(2)).expect("second");
        assert_ne!(first, second);
    }
}
