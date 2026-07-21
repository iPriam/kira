//! Declared struct shapes and the program's struct table.

use super::Type;

/// Index of a declared struct within a [`StructTable`].
///
/// Only a [`StructTable`] mints one, so an id always names a row of the table
/// it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StructId(u32);

impl StructId {
    /// This id as an index, for a backend keying its own per-struct data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// One declared struct: its name and its stored fields, in declaration order.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDef {
    /// The struct's name, as written.
    pub name: String,
    /// The stored fields, in declaration order. Field order is layout order.
    pub fields: Vec<FieldDef>,
}

impl StructDef {
    /// The index of the field named `name`, or `None` when there is no such
    /// field.
    pub fn field_index(&self, name: &str) -> Option<u32> {
        self.fields
            .iter()
            .position(|field| field.name == name)
            .map(|index| index as u32)
    }

    /// The field at `index`, or `None` when out of range.
    pub fn field(&self, index: u32) -> Option<&FieldDef> {
        self.fields.get(index as usize)
    }
}

/// One stored field of a [`StructDef`].
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    /// The field's name, as written.
    pub name: String,
    /// The field's resolved type.
    pub ty: Type,
    /// Whether the field may be reassigned through a mutable place (`var`).
    pub mutable: bool,
}

/// Every struct a program declares, indexed by [`StructId`].
///
/// The table is the one owner of struct shapes: the HIR, the IR, and every
/// backend read layout and names from here rather than carrying their own copy.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructTable {
    defs: Vec<StructDef>,
    // Kept in step with `defs` by `declare`, which is the only way to add one.
    index: std::collections::HashMap<String, StructId>,
}

impl StructTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a struct, returning its id, or `None` when the name is taken.
    ///
    /// Rejecting the duplicate here rather than overwriting keeps the name
    /// index and the rows in step: every id resolves, and every name resolves
    /// to the first declaration.
    pub fn declare(&mut self, def: StructDef) -> Option<StructId> {
        if self.index.contains_key(&def.name) {
            return None;
        }
        let id = StructId(u32::try_from(self.defs.len()).ok()?);
        self.index.insert(def.name.clone(), id);
        self.defs.push(def);
        Some(id)
    }

    /// The struct `name` declares, or `None` when no struct has that name.
    pub fn lookup(&self, name: &str) -> Option<StructId> {
        self.index.get(name).copied()
    }

    /// The definition behind an id.
    pub fn get(&self, id: StructId) -> Option<&StructDef> {
        self.defs.get(id.0 as usize)
    }

    /// Appends a field to an already-declared struct, returning its index.
    ///
    /// Exists for the *synthesized* struct a function type becomes: the closure
    /// literals of one type are discovered while bodies are analyzed, one after
    /// another, and each contributes its captures — so that struct's field list
    /// is only complete once analysis is. Growing it in place is what lets a
    /// function type be a `Type` (and so be written in a parameter, a field, or
    /// an annotation) before anything is known about the closures that will
    /// inhabit it.
    ///
    /// Never used on a struct the source declared: those are complete the
    /// moment they are declared, and appending to one would silently change a
    /// layout that construction sites already agreed on.
    pub fn push_field(&mut self, id: StructId, field: FieldDef) -> Option<u32> {
        let def = self.defs.get_mut(id.0 as usize)?;
        let index = u32::try_from(def.fields.len()).ok()?;
        def.fields.push(field);
        Some(index)
    }

    /// Fills the fields of a struct declared as an empty header.
    ///
    /// The frontend's type collection runs in two passes: it declares every
    /// struct's name first — so a field may name a sibling declared later —
    /// and resolves the field lists in a second pass, once every name is in
    /// the table. This writes the resolved list back into the row the header
    /// minted. Returns `false` when `id` names no row.
    ///
    /// Unlike [`StructTable::push_field`], this replaces the whole list, and is
    /// meant only for that collection pass, on a row declared empty a moment
    /// earlier — not for reshaping a struct construction sites already agreed
    /// on.
    pub fn set_fields(&mut self, id: StructId, fields: Vec<FieldDef>) -> bool {
        match self.defs.get_mut(id.0 as usize) {
            Some(def) => {
                def.fields = fields;
                true
            }
            None => false,
        }
    }

    /// Every declared struct, in declaration order.
    pub fn defs(&self) -> &[StructDef] {
        &self.defs
    }

    /// How many structs the program declares.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the program declares no structs.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_point() -> (StructTable, StructId) {
        let mut table = StructTable::new();
        let id = table
            .declare(StructDef {
                name: "Point".to_owned(),
                fields: vec![
                    FieldDef {
                        name: "x".to_owned(),
                        ty: Type::INT,
                        mutable: true,
                    },
                    FieldDef {
                        name: "y".to_owned(),
                        ty: Type::INT,
                        mutable: false,
                    },
                ],
            })
            .expect("a fresh name declares");
        (table, id)
    }

    #[test]
    fn a_duplicate_name_is_rejected_rather_than_overwriting() {
        let (mut table, id) = table_with_point();
        let again = table.declare(StructDef {
            name: "Point".to_owned(),
            fields: Vec::new(),
        });
        assert_eq!(again, None);
        // The first declaration still owns the name and the row.
        assert_eq!(table.lookup("Point"), Some(id));
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn an_empty_header_takes_its_fields_in_a_second_pass() {
        let mut table = StructTable::new();
        // First pass: the name is declared with no fields, minting the id.
        let id = table
            .declare(StructDef {
                name: "World".to_owned(),
                fields: Vec::new(),
            })
            .expect("a fresh name declares");
        assert!(table.get(id).expect("the id resolves").fields.is_empty());
        // Second pass: the resolved fields are written back into that row.
        let filled = table.set_fields(
            id,
            vec![FieldDef {
                name: "tick".to_owned(),
                ty: Type::INT,
                mutable: true,
            }],
        );
        assert!(filled);
        let def = table.get(id).expect("the id still resolves");
        assert_eq!(def.field_index("tick"), Some(0));
        // An id no row carries is reported rather than panicking.
        assert!(!table.set_fields(StructId(99), Vec::new()));
    }

    #[test]
    fn field_lookup_is_by_declaration_order() {
        let (table, id) = table_with_point();
        let def = table.get(id).expect("the id resolves");
        assert_eq!(def.field_index("x"), Some(0));
        assert_eq!(def.field_index("y"), Some(1));
        assert_eq!(def.field_index("z"), None);
        assert!(def.field(0).expect("x").mutable);
        assert!(!def.field(1).expect("y").mutable);
    }
}
