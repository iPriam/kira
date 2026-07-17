//! The program's one table of type shapes: structs and array types together.

use super::Type;
use super::arrays::ArrayTable;
use super::enums::EnumTable;
use super::structs::StructTable;

/// Every shape a program's types can name: its structs and its array types.
///
/// One table rather than two loose fields, because the questions that need
/// answering — what is this type called, does it own heap storage — cannot be
/// answered by either table alone once `[SomeStruct]` exists. Anchoring them
/// here keeps one owner of the answer instead of a struct table and an array
/// table that could disagree.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypeTable {
    structs: StructTable,
    arrays: ArrayTable,
    enums: EnumTable,
}

impl TypeTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The declared structs.
    pub fn structs(&self) -> &StructTable {
        &self.structs
    }

    /// The declared structs, mutably, for the analyzer that fills them.
    pub fn structs_mut(&mut self) -> &mut StructTable {
        &mut self.structs
    }

    /// The interned array types.
    pub fn arrays(&self) -> &ArrayTable {
        &self.arrays
    }

    /// The declared enums.
    pub fn enums(&self) -> &EnumTable {
        &self.enums
    }

    /// The declared enums, mutably, for the analyzer that fills them.
    pub fn enums_mut(&mut self) -> &mut EnumTable {
        &mut self.enums
    }

    /// The array type `[element]`, interning it if this is its first mention,
    /// or [`Type::Error`] when the id space is exhausted.
    ///
    /// `Error` rather than a `Result` because every caller is analysis, whose
    /// job is to keep going: a program that names four billion distinct array
    /// types has one more error node in it, not a compiler that stopped.
    pub fn array_of(&mut self, element: Type) -> Type {
        match self.arrays.intern(element) {
            Some(id) => Type::Array(id),
            None => Type::Error,
        }
    }

    /// The element type of an array type, or `None` when `ty` is not one.
    pub fn element_of(&self, ty: Type) -> Option<Type> {
        match ty {
            Type::Array(id) => self.arrays.element(id),
            _ => None,
        }
    }

    /// The canonical spelling of `ty`, for diagnostics.
    ///
    /// Owned rather than borrowed because an array's name is *built* — `[Int]`
    /// is nowhere in the table to point at.
    pub fn type_name(&self, ty: Type) -> String {
        match ty {
            Type::Int => "Int".to_owned(),
            Type::Float => "Float".to_owned(),
            Type::Bool => "Bool".to_owned(),
            Type::String => "String".to_owned(),
            Type::Void => "Void".to_owned(),
            Type::Error => "<error>".to_owned(),
            Type::Struct(id) => match self.structs.get(id) {
                Some(def) => def.name.clone(),
                None => "<unknown struct>".to_owned(),
            },
            Type::Array(id) => match self.arrays.element(id) {
                Some(element) => format!("[{}]", self.type_name(element)),
                None => "<unknown array>".to_owned(),
            },
            Type::Enum(id) => match self.enums.get(id) {
                Some(def) => def.name.clone(),
                None => "<unknown enum>".to_owned(),
            },
        }
    }

    /// Whether a value of `ty` owns heap storage, so that copying it must
    /// clone and dropping it must release.
    ///
    /// A struct owns heap storage when any field does, transitively. An array
    /// always does: it *is* a heap object, whatever it holds, so `[Int]` owns
    /// storage even though `Int` does not.
    ///
    /// The walk terminates: a struct's fields can only name structs declared
    /// before it, and an array type's element always resolves to a type
    /// interned before it.
    pub fn owns_heap(&self, ty: Type) -> bool {
        match ty {
            // An enum, like an array, always *is* a heap object — a boxed tag
            // plus its optional payload — so a copy allocates a fresh box and a
            // drop frees it, whatever the variant carries.
            Type::String | Type::Array(_) | Type::Enum(_) => true,
            Type::Struct(id) => match self.structs.get(id) {
                Some(def) => def.fields.iter().any(|field| self.owns_heap(field.ty)),
                None => false,
            },
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::structs::{FieldDef, StructDef};
    use super::*;

    #[test]
    fn an_array_names_itself_by_its_element() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::Int);
        assert_eq!(table.type_name(ints), "[Int]");
        let nested = table.array_of(ints);
        assert_eq!(table.type_name(nested), "[[Int]]");
    }

    #[test]
    fn an_array_of_a_struct_names_the_struct() {
        let mut table = TypeTable::new();
        let point = table
            .structs_mut()
            .declare(StructDef {
                name: "Point".to_owned(),
                fields: vec![FieldDef {
                    name: "x".to_owned(),
                    ty: Type::Int,
                    mutable: true,
                }],
            })
            .expect("declares");
        let points = table.array_of(Type::Struct(point));
        assert_eq!(table.type_name(points), "[Point]");
    }

    #[test]
    fn an_array_owns_heap_storage_whatever_it_holds() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::Int);
        // `Int` owns nothing, but the array holding them is itself an object.
        assert!(!table.owns_heap(Type::Int));
        assert!(table.owns_heap(ints));
    }

    #[test]
    fn heap_ownership_is_transitive_through_fields() {
        let mut table = TypeTable::new();
        let scalars = table
            .structs_mut()
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
            .structs_mut()
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
            .structs_mut()
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

    #[test]
    fn a_struct_holding_an_array_owns_heap_storage() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::Int);
        let holder = table
            .structs_mut()
            .declare(StructDef {
                name: "Holder".to_owned(),
                fields: vec![FieldDef {
                    name: "values".to_owned(),
                    ty: ints,
                    mutable: true,
                }],
            })
            .expect("declares");
        assert!(table.owns_heap(Type::Struct(holder)));
    }

    #[test]
    fn an_array_type_resolves_to_its_element() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::Int);
        assert_eq!(table.element_of(ints), Some(Type::Int));
        assert_eq!(table.element_of(Type::Int), None);
    }
}
