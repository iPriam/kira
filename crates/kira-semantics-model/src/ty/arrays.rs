//! Array types and the program's interning array table.

use std::collections::HashMap;

use super::Type;

/// Index of an array type within an [`ArrayTable`].
///
/// Only an [`ArrayTable`] mints one, so an id always names a row of the table
/// it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArrayId(u32);

impl ArrayId {
    /// This id as an index, for a backend keying its own per-array-type data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// Every array type a program mentions, indexed by [`ArrayId`].
///
/// The table **interns**: `[Int]` written twice resolves to one [`ArrayId`], so
/// `Type::Array(a) == Type::Array(b)` decides array type equality by comparing
/// two `u32`s. That is what keeps [`Type`] `Copy` while still letting an array
/// carry an element type — the alternative, an inline `Box<Type>`, would cost
/// `Copy` on every type in the lattice to express a handful of array types.
///
/// Nesting falls out of the same mechanism: `[[Int]]` is a row whose element is
/// the `Type::Array` naming the `[Int]` row. Interning terminates because the
/// element of a new row is always a type that already resolves.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArrayTable {
    elements: Vec<Type>,
    // Kept in step with `elements` by `intern`, the only way to add a row.
    index: HashMap<Type, ArrayId>,
}

impl ArrayTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The id of the array type with element `element`, minting one if this is
    /// the first mention, or `None` when the id space is exhausted.
    ///
    /// Idempotent: the same element always yields the same id, which is what
    /// makes `[Int] == [Int]`.
    pub fn intern(&mut self, element: Type) -> Option<ArrayId> {
        if let Some(&id) = self.index.get(&element) {
            return Some(id);
        }
        let id = ArrayId(u32::try_from(self.elements.len()).ok()?);
        self.elements.push(element);
        self.index.insert(element, id);
        Some(id)
    }

    /// The element type behind an id.
    pub fn element(&self, id: ArrayId) -> Option<Type> {
        self.elements.get(id.0 as usize).copied()
    }

    /// Every interned array type's element, in interning order.
    pub fn elements(&self) -> &[Type] {
        &self.elements
    }

    /// Every array type, as `(id, element)`, in interning order.
    ///
    /// A backend emitting one helper per array type needs the id *and* the
    /// element, and reconstructing the id from the position would be a second
    /// place that knows how ids are minted. This is the one place.
    pub fn rows(&self) -> impl Iterator<Item = (ArrayId, Type)> + '_ {
        self.elements
            .iter()
            .enumerate()
            .map(|(index, element)| (ArrayId(index as u32), *element))
    }

    /// Rewrites every row's element through `visit`, then rebuilds the intern
    /// index over the rewritten elements.
    ///
    /// Rewriting can make two rows equal — `[TabId]` and `[U32]` are one shape
    /// once the distinct type is erased — so the index is rebuilt rather than
    /// patched, and the earliest row wins the key. The duplicate row is kept:
    /// an id already handed out has to keep naming an array of the right
    /// element, and a backend that emits one helper per row emits one helper
    /// too many rather than the wrong one.
    pub fn visit_elements_mut(&mut self, visit: &dyn Fn(&mut Type)) {
        for element in &mut self.elements {
            visit(element);
        }
        self.index.clear();
        for (index, element) in self.elements.iter().enumerate() {
            self.index.entry(*element).or_insert(ArrayId(index as u32));
        }
    }

    /// How many distinct array types the program mentions.
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Whether the program mentions no array types.
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_element_interns_to_the_same_id() {
        let mut table = ArrayTable::new();
        let first = table.intern(Type::INT).expect("interns");
        let again = table.intern(Type::INT).expect("interns");
        assert_eq!(first, again, "`[Int]` written twice is one type");
        assert_eq!(table.len(), 1);
        // Which is what makes the type-level equality work.
        assert_eq!(Type::Array(first), Type::Array(again));
    }

    #[test]
    fn different_elements_are_different_types() {
        let mut table = ArrayTable::new();
        let ints = table.intern(Type::INT).expect("interns");
        let strings = table.intern(Type::String).expect("interns");
        assert_ne!(ints, strings);
        assert_ne!(Type::Array(ints), Type::Array(strings));
        assert_eq!(table.element(ints), Some(Type::INT));
        assert_eq!(table.element(strings), Some(Type::String));
    }

    #[test]
    fn nesting_is_a_row_whose_element_is_an_array() {
        let mut table = ArrayTable::new();
        let inner = table.intern(Type::INT).expect("interns");
        let outer = table.intern(Type::Array(inner)).expect("interns");
        assert_ne!(inner, outer, "`[Int]` and `[[Int]]` are different types");
        assert_eq!(table.element(outer), Some(Type::Array(inner)));
        // …and `[[Int]]` still interns to itself.
        assert_eq!(table.intern(Type::Array(inner)), Some(outer));
    }
}
