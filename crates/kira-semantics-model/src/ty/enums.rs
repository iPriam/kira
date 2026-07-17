//! Declared enum shapes and the program's enum table.
//!
//! An enum is a tagged value: one of a fixed set of named variants, each
//! optionally carrying a single payload value. The table is the one owner of
//! enum shapes — the HIR, the IR, and every backend read a variant's tag and
//! its payload type from here rather than carrying their own copy.

use super::Type;

/// Index of a declared enum within an [`EnumTable`].
///
/// Only an [`EnumTable`] mints one, so an id always names a row of the table it
/// came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(u32);

impl EnumId {
    /// This id as an index, for a backend keying its own per-enum data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// One declared enum: its name and its variants, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDef {
    /// The enum's name, as written.
    pub name: String,
    /// The variants, in declaration order. A variant's index is its **tag** —
    /// the discriminant compared by `==` and stored in the runtime value.
    pub variants: Vec<VariantDef>,
}

impl EnumDef {
    /// The tag (declaration index) of the variant named `name`, or `None` when
    /// there is no such variant.
    pub fn variant_index(&self, name: &str) -> Option<u32> {
        self.variants
            .iter()
            .position(|variant| variant.name == name)
            .map(|index| index as u32)
    }

    /// The variant at `tag`, or `None` when out of range.
    pub fn variant(&self, tag: u32) -> Option<&VariantDef> {
        self.variants.get(tag as usize)
    }
}

/// One variant of an [`EnumDef`].
#[derive(Debug, Clone, PartialEq)]
pub struct VariantDef {
    /// The variant's name, as written.
    pub name: String,
    /// The payload's resolved type, or `None` for a payload-less variant.
    ///
    /// The v0 subset carries a payload of a single value, and only a scalar or
    /// a `String`: those cross the enum box's type-erased slot cleanly, while a
    /// struct/enum/array payload has no representation there yet and is refused
    /// at the declaration.
    pub payload: Option<Type>,
}

/// Every enum a program declares, indexed by [`EnumId`].
///
/// The table is the one owner of enum shapes: the HIR, the IR, and every
/// backend read tags and payload types from here rather than carrying their own
/// copy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnumTable {
    defs: Vec<EnumDef>,
    // Kept in step with `defs` by `declare`, which is the only way to add one.
    index: std::collections::HashMap<String, EnumId>,
}

impl EnumTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an enum, returning its id, or `None` when the name is taken.
    ///
    /// Rejecting the duplicate here rather than overwriting keeps the name
    /// index and the rows in step: every id resolves, and every name resolves
    /// to the first declaration.
    pub fn declare(&mut self, def: EnumDef) -> Option<EnumId> {
        if self.index.contains_key(&def.name) {
            return None;
        }
        let id = EnumId(u32::try_from(self.defs.len()).ok()?);
        self.index.insert(def.name.clone(), id);
        self.defs.push(def);
        Some(id)
    }

    /// The enum `name` declares, or `None` when no enum has that name.
    pub fn lookup(&self, name: &str) -> Option<EnumId> {
        self.index.get(name).copied()
    }

    /// The definition behind an id.
    pub fn get(&self, id: EnumId) -> Option<&EnumDef> {
        self.defs.get(id.0 as usize)
    }

    /// Every declared enum, in declaration order.
    pub fn defs(&self) -> &[EnumDef] {
        &self.defs
    }

    /// How many enums the program declares.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the program declares no enums.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Whether any variant of an enum carries a payload that owns heap storage
    /// (a `String`), so a copy must clone it and a drop must release it.
    ///
    /// A payload-less enum, or one whose payloads are all scalars, owns no heap
    /// beyond its own box; a variant with a `String` payload does. The answer
    /// is the whole enum's, not one variant's, because a value's static type is
    /// the enum and any of its variants could be the one it holds.
    pub fn owns_heap_payload(&self, id: EnumId) -> bool {
        self.get(id).is_some_and(|def| {
            def.variants
                .iter()
                .any(|variant| matches!(variant.payload, Some(Type::String)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_color() -> (EnumTable, EnumId) {
        let mut table = EnumTable::new();
        let id = table
            .declare(EnumDef {
                name: "Color".to_owned(),
                variants: vec![
                    VariantDef {
                        name: "Red".to_owned(),
                        payload: None,
                    },
                    VariantDef {
                        name: "Labelled".to_owned(),
                        payload: Some(Type::String),
                    },
                ],
            })
            .expect("a fresh name declares");
        (table, id)
    }

    #[test]
    fn a_variant_tag_is_its_declaration_index() {
        let (table, id) = table_with_color();
        let def = table.get(id).expect("the id resolves");
        assert_eq!(def.variant_index("Red"), Some(0));
        assert_eq!(def.variant_index("Labelled"), Some(1));
        assert_eq!(def.variant_index("Green"), None);
        assert!(def.variant(0).expect("Red").payload.is_none());
        assert_eq!(
            def.variant(1).expect("Labelled").payload,
            Some(Type::String)
        );
    }

    #[test]
    fn a_duplicate_name_is_rejected_rather_than_overwriting() {
        let (mut table, id) = table_with_color();
        let again = table.declare(EnumDef {
            name: "Color".to_owned(),
            variants: Vec::new(),
        });
        assert_eq!(again, None);
        assert_eq!(table.lookup("Color"), Some(id));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn a_string_payload_owns_heap_but_a_scalar_one_does_not() {
        let (table, id) = table_with_color();
        assert!(table.owns_heap_payload(id));

        let mut scalars = EnumTable::new();
        let axis = scalars
            .declare(EnumDef {
                name: "Axis".to_owned(),
                variants: vec![
                    VariantDef {
                        name: "Horizontal".to_owned(),
                        payload: None,
                    },
                    VariantDef {
                        name: "At".to_owned(),
                        payload: Some(Type::Int),
                    },
                ],
            })
            .expect("declares");
        assert!(!scalars.owns_heap_payload(axis));
    }
}
