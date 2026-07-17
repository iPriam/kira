//! The v0 type lattice and the program's struct table.
//!
//! The subset is monomorphic and closed: four scalar types, `Void`, an `Error`
//! type that absorbs mismatches so one type error does not cascade, and
//! user-declared structs. A [`Type`] stays `Copy` because a struct type is a
//! [`StructId`] into a [`StructTable`] rather than an inline shape.

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

/// A resolved Kira type in the v0 subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// The 64-bit signed integer type (`Int`).
    Int,
    /// The 64-bit floating-point type (`Float`).
    Float,
    /// The boolean type (`Bool`).
    Bool,
    /// The heap string type (`String`).
    String,
    /// The unit type of statements and value-less returns (`Void`).
    Void,
    /// The absorbing error type; assignable to and from anything.
    Error,
    /// A declared struct, named by its row in the program's [`StructTable`].
    Struct(StructId),
}

impl Type {
    /// Resolves a written *builtin* type name, or `None` when it is not one.
    ///
    /// A struct name is not a builtin, so resolving one needs the program's
    /// [`StructTable`]; the analyzer tries this first and the table second.
    pub fn from_name(name: &str) -> Option<Type> {
        Some(match name {
            "Int" => Type::Int,
            "Float" => Type::Float,
            "Bool" => Type::Bool,
            "String" => Type::String,
            "Void" => Type::Void,
            _ => return None,
        })
    }

    /// Whether a value of `self` may be used where `target` is expected.
    ///
    /// v0 requires exact matches (no implicit `Int`->`Float` widening); the
    /// `Error` type is compatible in both directions to stop cascades.
    pub fn assignable_to(self, target: Type) -> bool {
        self == Type::Error || target == Type::Error || self == target
    }

    /// Whether this is one of the numeric types (`Int` or `Float`).
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// Whether values of this type can be passed to the `print` builtin.
    ///
    /// A struct is not printable: what `print` renders for one is not pinned
    /// by the language corpus, and inventing a format here would be inventing
    /// language surface. A struct prints through its own accessors until the
    /// format is settled.
    pub fn is_printable(self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Bool | Type::String)
    }

    /// Whether a value of this type owns heap storage that a copy must clone
    /// and a drop must release.
    ///
    /// Scalars are `Copy` and own nothing. A `String` owns its bytes. A struct
    /// owns whatever its fields own, so the answer is the table's to give —
    /// see [`StructTable::owns_heap`].
    pub fn is_scalar(self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Bool | Type::Void)
    }

    /// Whether a value of this type reaches an owned parameter without an
    /// explicit `move`.
    ///
    /// This is the predicate that decides whether passing a *named* local to a
    /// consuming parameter needs `move` written at the call site. It is a
    /// property of the type alone and deliberately narrow: `Void` and the
    /// three scalars, plus (once they exist) the C-seam types `CString` and
    /// `RawPtr`.
    ///
    /// `String` is **not** trivially copyable — it owns its bytes — so
    /// `f(name)` on a `String` local is `KSEM108` and `f(move name)` is how it
    /// is written. A **struct is not trivially copyable either**, which is the
    /// rule most worth stating out loud, because a struct nonetheless does
    /// *not* implicitly move when bound ([`Type::moves_on_bind`]). The two
    /// predicates answer different questions and a struct answers them
    /// differently: it needs `move` into an owned parameter, and it still
    /// copies on `let w = v`.
    pub fn is_trivially_copyable(self) -> bool {
        match self {
            Type::Int | Type::Float | Type::Bool | Type::Void => true,
            // An expression that already failed to analyze must not also
            // collect an ownership diagnostic on top of its type error.
            Type::Error => true,
            Type::String | Type::Struct(_) => false,
        }
    }

    /// Whether binding a value of this type *consumes* the binding it was read
    /// from (Rust-style implicit move on bind).
    ///
    /// True for exactly the types that a binding would otherwise **alias**:
    /// arrays, enum instances, and construct existentials all lower to a
    /// shared heap handle, so `let alias = values` would leave two owners
    /// pointing at one object. Marking the source moved turns that aliasing
    /// into `KSEM107` instead of a use-after-free.
    ///
    /// **Every type in today's lattice answers `false`**, and that is the
    /// correct answer rather than a stub: a struct deep-copies when bound and
    /// a `String` clones its bytes, so neither can alias and neither has
    /// anything to enforce. The predicate exists — and is spelled as the type
    /// question rather than inlined as `false` — because arrays are the next
    /// feature and are the first type to answer `true`. When `Type::Array`
    /// lands, this arm is the whole ownership change it needs.
    pub fn moves_on_bind(self) -> bool {
        match self {
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::Void
            | Type::Error
            | Type::String
            | Type::Struct(_) => false,
        }
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
        let id = StructId(self.defs.len() as u32);
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

    /// The canonical spelling of `ty`, for diagnostics.
    pub fn type_name(&self, ty: Type) -> &str {
        match ty {
            Type::Int => "Int",
            Type::Float => "Float",
            Type::Bool => "Bool",
            Type::String => "String",
            Type::Void => "Void",
            Type::Error => "<error>",
            Type::Struct(id) => match self.get(id) {
                Some(def) => &def.name,
                None => "<unknown struct>",
            },
        }
    }

    /// Whether a value of `ty` owns heap storage, so that copying it must
    /// clone and dropping it must release.
    ///
    /// A struct owns heap storage when any field does, transitively. Field
    /// types are resolved against this same table and a struct's fields can
    /// only name structs declared before it, so the walk terminates.
    pub fn owns_heap(&self, ty: Type) -> bool {
        match ty {
            Type::String => true,
            Type::Struct(id) => match self.get(id) {
                Some(def) => def.fields.iter().any(|field| self.owns_heap(field.ty)),
                None => false,
            },
            _ => false,
        }
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
                        ty: Type::Int,
                        mutable: true,
                    },
                    FieldDef {
                        name: "y".to_owned(),
                        ty: Type::Int,
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
    fn a_struct_type_names_itself_in_diagnostics() {
        let (table, id) = table_with_point();
        assert_eq!(table.type_name(Type::Struct(id)), "Point");
        assert_eq!(table.type_name(Type::Int), "Int");
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

    #[test]
    fn a_struct_needs_move_but_does_not_move_on_bind() {
        let (_, id) = table_with_point();
        // The two predicates answer differently for a struct, and that split
        // is the whole point: `f(p)` into an owned param is KSEM108, while
        // `let q = p` still copies.
        assert!(!Type::Struct(id).is_trivially_copyable());
        assert!(!Type::Struct(id).moves_on_bind());
    }

    #[test]
    fn a_string_needs_move_and_a_scalar_does_not() {
        assert!(!Type::String.is_trivially_copyable());
        assert!(Type::Int.is_trivially_copyable());
        assert!(Type::Float.is_trivially_copyable());
        assert!(Type::Bool.is_trivially_copyable());
        assert!(Type::Void.is_trivially_copyable());
    }

    #[test]
    fn nothing_in_todays_lattice_moves_on_bind() {
        // Arrays are the first type that will answer `true`. Until then this
        // pins that no current type silently acquired implicit-move.
        let (_, id) = table_with_point();
        for ty in [
            Type::Int,
            Type::Float,
            Type::Bool,
            Type::Void,
            Type::Error,
            Type::String,
            Type::Struct(id),
        ] {
            assert!(!ty.moves_on_bind(), "{ty:?} must not move on bind");
        }
    }

    #[test]
    fn an_error_type_never_collects_an_ownership_diagnostic() {
        // A type error already reported must not also produce KSEM108.
        assert!(Type::Error.is_trivially_copyable());
    }

    #[test]
    fn heap_ownership_is_transitive_through_fields() {
        let mut table = StructTable::new();
        let scalars = table
            .declare(StructDef {
                name: "Pair".to_owned(),
                fields: vec![FieldDef {
                    name: "w".to_owned(),
                    ty: Type::Int,
                    mutable: true,
                }],
            })
            .expect("declares");
        assert!(!table.owns_heap(Type::Struct(scalars)));

        let labelled = table
            .declare(StructDef {
                name: "Labelled".to_owned(),
                fields: vec![FieldDef {
                    name: "label".to_owned(),
                    ty: Type::String,
                    mutable: true,
                }],
            })
            .expect("declares");
        assert!(table.owns_heap(Type::Struct(labelled)));

        // A struct owning a struct that owns a string owns heap storage too.
        let nested = table
            .declare(StructDef {
                name: "Nested".to_owned(),
                fields: vec![FieldDef {
                    name: "inner".to_owned(),
                    ty: Type::Struct(labelled),
                    mutable: true,
                }],
            })
            .expect("declares");
        assert!(table.owns_heap(Type::Struct(nested)));
        assert!(!table.owns_heap(Type::Int));
    }
}
