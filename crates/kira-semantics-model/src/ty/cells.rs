//! Capture-cell types and the program's interning cell table.
//!
//! A **cell** is the one genuinely shared, mutable storage in the language: the
//! box a `var` moves into when a closure captures it. Every other value here has
//! value semantics — a struct copies deeply, an array copies on the first write
//! — so a cell is not an optimization of an existing shape but a new one, and it
//! is deliberately *not* surface. No source text spells `Cell`; the analyzer
//! mints one when a mutable binding is captured, and nothing else ever produces
//! the type.
//!
//! The table interns exactly as [`super::arrays::ArrayTable`] does, and for the
//! same reason: it keeps [`Type`] `Copy` while still letting a cell carry the
//! type it holds.

use std::collections::HashMap;

use super::Type;

/// Index of a capture-cell type within a [`CellTable`].
///
/// Only a [`CellTable`] mints one, so an id always names a row of the table it
/// came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellId(u32);

impl CellId {
    /// This id as an index, for a backend keying its own per-cell-type data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// Every capture-cell type a program mentions, indexed by [`CellId`].
///
/// Interning makes two cells of `Int` one type, so `Type::Cell(a) ==
/// Type::Cell(b)` compares two `u32`s.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CellTable {
    inners: Vec<Type>,
    // Kept in step with `inners` by `intern`, the only way to add a row.
    index: HashMap<Type, CellId>,
}

impl CellTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of the cell type holding `inner`, minting one if this is the
    /// first mention, or `None` when the id space is exhausted.
    pub fn intern(&mut self, inner: Type) -> Option<CellId> {
        if let Some(&id) = self.index.get(&inner) {
            return Some(id);
        }
        let id = CellId(u32::try_from(self.inners.len()).ok()?);
        self.inners.push(inner);
        self.index.insert(inner, id);
        Some(id)
    }

    /// The held type behind an id.
    pub fn inner(&self, id: CellId) -> Option<Type> {
        self.inners.get(id.0 as usize).copied()
    }

    /// Every cell type, as `(id, held type)`, in interning order.
    pub fn rows(&self) -> impl Iterator<Item = (CellId, Type)> + '_ {
        self.inners
            .iter()
            .enumerate()
            .map(|(index, inner)| (CellId(index as u32), *inner))
    }

    /// How many distinct cell types the program mentions.
    pub fn len(&self) -> usize {
        self.inners.len()
    }

    /// Whether the program mentions no cell types.
    pub fn is_empty(&self) -> bool {
        self.inners.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_held_type_interns_to_the_same_id() {
        let mut table = CellTable::new();
        let first = table.intern(Type::INT).expect("interns");
        let again = table.intern(Type::INT).expect("interns");
        assert_eq!(first, again, "two `Int` cells are one type");
        assert_eq!(table.len(), 1);
        assert_eq!(Type::Cell(first), Type::Cell(again));
    }

    #[test]
    fn different_held_types_are_different_cells() {
        let mut table = CellTable::new();
        let ints = table.intern(Type::INT).expect("interns");
        let strings = table.intern(Type::String).expect("interns");
        assert_ne!(ints, strings);
        assert_eq!(table.inner(ints), Some(Type::INT));
        assert_eq!(table.inner(strings), Some(Type::String));
    }
}
