//! The program's one table of type shapes: structs and array types together.

use kira_runtime_abi::NativeStateTypeId;

use super::arrays::ArrayTable;
use super::cells::CellTable;
use super::enums::EnumTable;
use super::native_state::NativeStateTable;
use super::structs::StructTable;
use super::{FloatSpelling, IntSpelling, Type};

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
    native_states: NativeStateTable,
    cells: CellTable,
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

    /// The interned capture-cell types.
    pub fn cells(&self) -> &CellTable {
        &self.cells
    }

    /// The cell type holding `inner`, interning it on first mention, or
    /// [`Type::Error`] when the id space is exhausted.
    ///
    /// `Error` rather than a `Result` for the same reason [`TypeTable::array_of`]
    /// gives one: every caller is analysis, whose job is to keep going.
    pub fn cell_of(&mut self, inner: Type) -> Type {
        match self.cells.intern(inner) {
            Some(id) => Type::Cell(id),
            None => Type::Error,
        }
    }

    /// The type a cell holds, or `None` when `ty` is not a cell.
    pub fn cell_inner(&self, ty: Type) -> Option<Type> {
        match ty {
            Type::Cell(id) => self.cells.inner(id),
            _ => None,
        }
    }

    /// The element type of an array type, or `None` when `ty` is not one.
    pub fn element_of(&self, ty: Type) -> Option<Type> {
        match ty {
            Type::Array(id) => self.arrays.element(id),
            _ => None,
        }
    }

    /// The opaque callback-state handle type for `target`.
    pub fn native_state_of(&mut self, target: Type) -> Type {
        match self.native_states.intern(target) {
            Some(id) => Type::NativeState(id),
            None => Type::Error,
        }
    }

    /// The Kira value type boxed by an opaque callback-state handle.
    pub fn native_state_target(&self, ty: Type) -> Option<Type> {
        match ty {
            Type::NativeState(id) => self.native_states.target(id),
            _ => None,
        }
    }

    /// The collision-free runtime identity of a callback-state value type.
    pub fn native_state_type_id(&self, ty: Type) -> Option<NativeStateTypeId> {
        let (tag, payload) = match ty {
            Type::Int(spelling) => (1_u64, int_code(spelling)),
            Type::Float(spelling) => (2, float_code(spelling)),
            Type::Bool => (3, 0),
            Type::String => (4, 0),
            Type::Struct(id) => (5, u64::from(id.index())),
            Type::Array(id) => (6, u64::from(id.index())),
            Type::Enum(id) => (7, u64::from(id.index())),
            Type::RawPtr => (8, 0),
            // `Any` has no identity to give: the whole point of the type is
            // that the value inside it kept its own and this one has none, so
            // there is nothing for a recovery to check against.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::NativeState(_)
            | Type::Task(_)
            // A cell never crosses into opaque callback state: it is shared
            // mutable storage this runtime manages the count of, and handing
            // one to a host that does not would leave the count wrong.
            | Type::Cell(_)
            | Type::Any => {
                return None;
            }
        };
        Some(NativeStateTypeId::new((tag << 56) | payload))
    }

    /// The canonical spelling of `ty`, for diagnostics.
    ///
    /// Owned rather than borrowed because an array's name is *built* — `[Int]`
    /// is nowhere in the table to point at.
    pub fn type_name(&self, ty: Type) -> String {
        match ty {
            // A width names itself: a mismatch between `U8` and `I64` has to
            // say which two types it means, not report both as "Int".
            Type::Int(spelling) => spelling.name().to_owned(),
            Type::Float(spelling) => spelling.name().to_owned(),
            Type::Bool => "Bool".to_owned(),
            Type::String => "String".to_owned(),
            Type::Any => "Any".to_owned(),
            Type::Void => "Void".to_owned(),
            Type::RawPtr => "RawPtr".to_owned(),
            Type::CString => "CString".to_owned(),
            Type::NativeState(id) => match self.native_states.target(id) {
                Some(target) => format!("NativeState<{}>", self.type_name(target)),
                None => "<unknown native state>".to_owned(),
            },
            Type::Task(result) => format!("Task<{}>", result.label()),
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
            // Named for a diagnostic that should never reach a reader: a cell
            // is not surface, so anything printing one is reporting on the
            // desugar rather than on what was written.
            Type::Cell(id) => match self.cells.inner(id) {
                Some(inner) => format!("<captured {}>", self.type_name(inner)),
                None => "<unknown capture cell>".to_owned(),
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
    /// The walk terminates: the frontend breaks any by-value struct cycle to
    /// `Error` before this runs, so a struct can never reach itself through its
    /// fields, and an array type's element always resolves to a type interned
    /// before it.
    pub fn owns_heap(&self, ty: Type) -> bool {
        match ty {
            // An enum, like an array, always *is* a heap object — a boxed tag
            // plus its optional payload — so a copy allocates a fresh box and a
            // drop frees it, whatever the variant carries.
            // `Any` always is, whatever it erased. The box carries the tag that
            // says what it owns, so a copy and a drop go through it even when
            // the value inside was a scalar that owned nothing.
            // A cell always is: it is a share-counted box, and the last holder
            // releases what is inside it.
            Type::String | Type::Array(_) | Type::Enum(_) | Type::Any | Type::Cell(_) => true,
            Type::Struct(id) => match self.structs.get(id) {
                Some(def) => def.fields.iter().any(|field| self.owns_heap(field.ty)),
                None => false,
            },
            _ => false,
        }
    }
}

fn int_code(spelling: IntSpelling) -> u64 {
    match spelling {
        IntSpelling::Plain => 0,
        IntSpelling::I8 => 1,
        IntSpelling::I16 => 2,
        IntSpelling::I32 => 3,
        IntSpelling::I64 => 4,
        IntSpelling::U8 => 5,
        IntSpelling::U16 => 6,
        IntSpelling::U32 => 7,
        IntSpelling::U64 => 8,
    }
}

fn float_code(spelling: FloatSpelling) -> u64 {
    match spelling {
        FloatSpelling::Plain => 0,
        FloatSpelling::F32 => 1,
        FloatSpelling::F64 => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::super::structs::{FieldDef, StructDef};
    use super::*;

    #[test]
    fn an_array_names_itself_by_its_element() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::INT);
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
                    ty: Type::INT,
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
        let ints = table.array_of(Type::INT);
        // `Int` owns nothing, but the array holding them is itself an object.
        assert!(!table.owns_heap(Type::INT));
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
                    ty: Type::INT,
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
        assert!(!table.owns_heap(Type::INT));
    }

    #[test]
    fn a_struct_holding_an_array_owns_heap_storage() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::INT);
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
        let ints = table.array_of(Type::INT);
        assert_eq!(table.element_of(ints), Some(Type::INT));
        assert_eq!(table.element_of(Type::INT), None);
    }
}
