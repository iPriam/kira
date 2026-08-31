//! The program's one table of type shapes: structs and array types together.

use std::collections::HashSet;

use kira_runtime_abi::NativeStateTypeId;

use super::arrays::ArrayTable;
use super::cells::CellTable;
use super::enums::EnumTable;
use super::foreign_ptr::{ForeignPtrId, ForeignPtrTable};
use super::native_state::NativeStateTable;
use super::structs::{StructId, StructTable};
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
    foreign_ptrs: ForeignPtrTable,
}

impl TypeTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// The type of a C pointer addressing `target`, minting it on first
    /// mention.
    ///
    /// Returns [`Type::RawPtr`] when the id space is exhausted: a pointer that
    /// cannot carry its target is still a correct pointer word, and degrading to
    /// one loses field reads rather than correctness.
    pub fn foreign_ptr_to(&mut self, target: StructId) -> Type {
        match self.foreign_ptrs.intern(target) {
            Some(id) => Type::ForeignPtr(id),
            None => Type::RawPtr,
        }
    }

    /// What the foreign pointer named by `id` addresses.
    pub fn foreign_ptr_target(&self, id: ForeignPtrId) -> Option<StructId> {
        self.foreign_ptrs.target(id)
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
            // Table indices are compilation-local. A live VM keeps callback
            // state across a rebuild, so using a struct/array/enum index here
            // would make an unrelated declaration invalidate every state
            // token after it. Fingerprinting the declaration shape keeps the
            // type id stable while still refusing a changed state schema.
            Type::Struct(_) => (5, self.native_state_fingerprint(ty)),
            Type::Array(_) => (6, self.native_state_fingerprint(ty)),
            Type::Enum(_) => (7, self.native_state_fingerprint(ty)),
            // The same runtime word a `RawPtr` is, so the same identity: what
            // it points at is a compile-time fact, not a runtime one.
            Type::RawPtr | Type::ForeignPtr(_) => (8, 0),
            // A capture cell is one box, and what recovery has to agree about
            // is what the box holds: two cells of different inner types are
            // different state schemas exactly as two structs are.
            Type::Cell(_) => (9, self.native_state_fingerprint(ty)),
            // `Any` has no identity to give: the whole point of the type is
            // that the value inside it kept its own and this one has none, so
            // there is nothing for a recovery to check against.
            // A C block is seam-local storage; state that kept one across a
            // rebuild would keep a pointer into a program that no longer
            // exists, so it has no recovery identity either.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::NativeState(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::Any => {
                return None;
            }
        };
        const PAYLOAD_MASK: u64 = 0x00ff_ffff_ffff_ffff;
        Some(NativeStateTypeId::new(
            (tag << 56) | (payload & PAYLOAD_MASK),
        ))
    }

    /// Stable shape identity for an aggregate callback-state type.
    ///
    /// The recursive walk deliberately uses declaration names and field
    /// shapes, never the table's local ids. An unrelated struct added before a
    /// state-bearing struct therefore leaves the token recoverable, while a
    /// field insertion, removal, or type change produces a different id and
    /// is rejected at the state boundary instead of trapping later on a bad
    /// path.
    fn native_state_fingerprint(&self, ty: Type) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut visiting = HashSet::new();
        self.mix_native_state_type(&mut hash, ty, &mut visiting);
        if hash == 0 { 1 } else { hash }
    }

    fn mix_native_state_type(&self, hash: &mut u64, ty: Type, visiting: &mut HashSet<(u8, u32)>) {
        match ty {
            Type::Int(spelling) => {
                mix_native_state_bytes(hash, b"int");
                mix_native_state_u64(hash, int_code(spelling));
            }
            Type::Float(spelling) => {
                mix_native_state_bytes(hash, b"float");
                mix_native_state_u64(hash, float_code(spelling));
            }
            Type::Bool => mix_native_state_bytes(hash, b"bool"),
            Type::String => mix_native_state_bytes(hash, b"string"),
            Type::RawPtr | Type::ForeignPtr(_) => mix_native_state_bytes(hash, b"raw-ptr"),
            Type::Struct(id) => {
                let Some(def) = self.structs.get(id) else {
                    mix_native_state_bytes(hash, b"missing-struct");
                    return;
                };
                mix_native_state_bytes(hash, b"struct");
                mix_native_state_bytes(hash, def.name.as_bytes());
                // A function type's representation is named for its signature,
                // and the signature is the whole of its identity: the fields
                // are the captures this compilation happened to find, so
                // walking them would give a library and the application that
                // links it two different ids for one type — and the recovery
                // would be refused for a program that is correct.
                if self.structs.origin(id) == crate::StructOrigin::FunctionType {
                    return;
                }
                if !visiting.insert((5, id.index())) {
                    mix_native_state_bytes(hash, b"recursive");
                    return;
                }
                for field in &def.fields {
                    mix_native_state_bytes(hash, field.name.as_bytes());
                    mix_native_state_u64(hash, u64::from(field.mutable as u8));
                    self.mix_native_state_type(hash, field.ty, visiting);
                }
                visiting.remove(&(5, id.index()));
            }
            Type::Array(id) => {
                mix_native_state_bytes(hash, b"array");
                if let Some(element) = self.arrays.element(id) {
                    self.mix_native_state_type(hash, element, visiting);
                } else {
                    mix_native_state_bytes(hash, b"missing-array");
                }
            }
            Type::Enum(id) => {
                let Some(def) = self.enums.get(id) else {
                    mix_native_state_bytes(hash, b"missing-enum");
                    return;
                };
                mix_native_state_bytes(hash, b"enum");
                mix_native_state_bytes(hash, def.name.as_bytes());
                if !visiting.insert((7, id.index())) {
                    mix_native_state_bytes(hash, b"recursive");
                    return;
                }
                for variant in &def.variants {
                    mix_native_state_bytes(hash, variant.name.as_bytes());
                    match variant.payload {
                        Some(payload) => {
                            mix_native_state_bytes(hash, b"payload");
                            self.mix_native_state_type(hash, payload, visiting);
                        }
                        None => mix_native_state_bytes(hash, b"no-payload"),
                    }
                }
                visiting.remove(&(7, id.index()));
            }
            Type::Cell(id) => {
                mix_native_state_bytes(hash, b"cell");
                if !visiting.insert((9, id.index())) {
                    mix_native_state_bytes(hash, b"recursive");
                    return;
                }
                match self.cells.inner(id) {
                    Some(inner) => self.mix_native_state_type(hash, inner, visiting),
                    None => mix_native_state_bytes(hash, b"missing-cell"),
                }
                visiting.remove(&(9, id.index()));
            }
            // These shapes are refused before this method is called. Keeping a
            // marker here makes the fingerprint total if an error node leaks
            // through a diagnostic-preserving analysis.
            Type::Void
            | Type::Error
            | Type::CString
            | Type::CBlock
            | Type::NativeState(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::Any => mix_native_state_bytes(hash, b"unsupported"),
        }
    }

    /// The canonical spelling of `ty`, for diagnostics.
    ///
    /// Owned rather than borrowed because an array's name is *built* — `[Int]`
    /// is nowhere in the table to point at.
    pub fn type_name(&self, ty: Type) -> String {
        match ty {
            // A width names itself: a mismatch between `U8` and `U32` has to
            // say which two types it means, not report both as "Int".
            Type::Int(spelling) => spelling.name().to_owned(),
            Type::Float(spelling) => spelling.name().to_owned(),
            Type::Bool => "Bool".to_owned(),
            Type::String => "String".to_owned(),
            Type::Any => "Any".to_owned(),
            Type::Void => "Void".to_owned(),
            Type::RawPtr => "RawPtr".to_owned(),
            Type::CString => "CString".to_owned(),
            // Not surface: a reader meeting this name is looking at seam
            // storage the analyzer minted, not at a type they can spell.
            Type::CBlock => "<C storage>".to_owned(),
            Type::NativeState(id) => match self.native_states.target(id) {
                Some(target) => format!("NativeState<{}>", self.type_name(target)),
                None => "<unknown native state>".to_owned(),
            },
            Type::Task(result) => format!("Task<{}>", result.label()),
            Type::MainThreadTask(result) => {
                format!("MainThreadTask<{}>", self.type_name(result.value_type()))
            }
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
            // Named by what it addresses. The typedef's own name is not here —
            // the table holds types, not the spellings that resolved to them —
            // and the target is the fact a reader needs anyway.
            Type::ForeignPtr(id) => match self.foreign_ptr_target(id) {
                Some(target) => format!("pointer to {}", self.type_name(Type::Struct(target))),
                None => "<unknown foreign pointer>".to_owned(),
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
            // A C block always is: it *is* the owned allocation.
            Type::String
            | Type::Array(_)
            | Type::Enum(_)
            | Type::Any
            | Type::Cell(_)
            | Type::CBlock => true,
            // A type with a user `Drop` always owns something a release has to
            // do — the body itself — so it is released even when every field it
            // holds is a scalar.
            Type::Struct(id) => match self.structs.get(id) {
                Some(def) => {
                    def.drop_glue.is_some()
                        || def.owning_c_slots().next().is_some()
                        || def.fields.iter().any(|field| self.owns_heap(field.ty))
                }
                None => false,
            },
            _ => false,
        }
    }

    /// The function running `ty`'s user `Drop` body, when it declares one.
    ///
    /// Only a struct-shaped type can: a trait is claimed by a declaration, and
    /// every declaration that can claim one becomes a struct row.
    pub fn user_drop(&self, ty: Type) -> Option<u32> {
        match ty {
            Type::Struct(id) => self.structs.get(id).and_then(|def| def.drop_glue),
            _ => None,
        }
    }

    /// Whether releasing `ty` runs a user `Drop` body, directly or through
    /// something it holds.
    ///
    /// Follows fields and array elements, because a release does: a struct
    /// holding a `Drop` value releases it, and so runs its body. That is why
    /// this — and not [`TypeTable::user_drop`] — is what decides whether
    /// binding the value moves.
    pub fn runs_user_drop(&self, ty: Type) -> bool {
        self.reaches_user_drop(ty, &mut HashSet::new())
    }

    /// [`TypeTable::runs_user_drop`] with the set of shapes already being
    /// walked.
    ///
    /// A shape can reach itself — a closure's representation struct may capture
    /// a value of its own function type — and unlike
    /// [`TypeTable::owns_heap`], which stops at the first owning member, this
    /// walk has to visit everything before it can answer no. So it remembers
    /// where it has been: a shape already open cannot be what makes itself run
    /// a body.
    fn reaches_user_drop(&self, ty: Type, open: &mut HashSet<Type>) -> bool {
        if !open.insert(ty) {
            return false;
        }
        match ty {
            Type::Struct(id) => self.structs.get(id).is_some_and(|def| {
                def.drop_glue.is_some()
                    || def
                        .fields
                        .iter()
                        .any(|field| self.reaches_user_drop(field.ty, open))
            }),
            Type::Array(id) => self
                .arrays
                .element(id)
                .is_some_and(|element| self.reaches_user_drop(element, open)),
            _ => false,
        }
    }

    /// Whether copying `ty` must replace unique C-block handles in the copy.
    ///
    /// Shared heap objects such as arrays and enums answer false: copying their
    /// handle takes a share, and their copy-on-write path clones contained
    /// values only when it builds independent storage. A C block and a struct
    /// containing one directly answer true because two owners may never carry
    /// the same block handle.
    pub fn owns_unique_c_storage(&self, ty: Type) -> bool {
        match ty {
            Type::CBlock => true,
            Type::Struct(id) => self.structs.get(id).is_some_and(|def| {
                def.owning_c_slots().next().is_some()
                    || def
                        .fields
                        .iter()
                        .any(|field| self.owns_unique_c_storage(field.ty))
            }),
            _ => false,
        }
    }

    /// Whether `ty` can reach a uniquely owned C block.
    ///
    /// Unlike [`TypeTable::owns_unique_c_storage`], this follows arrays. An
    /// array copy shares its item block safely, but a retained foreign call
    /// must still transfer C blocks nested inside those items.
    pub fn contains_c_storage(&self, ty: Type) -> bool {
        match ty {
            Type::CBlock => true,
            Type::Struct(id) => self.structs.get(id).is_some_and(|def| {
                def.owning_c_slots().next().is_some()
                    || def
                        .fields
                        .iter()
                        .any(|field| self.contains_c_storage(field.ty))
            }),
            Type::Array(id) => self
                .arrays
                .element(id)
                .is_some_and(|element| self.contains_c_storage(element)),
            _ => false,
        }
    }

    /// Whether binding a value of `ty` consumes its source.
    ///
    /// [`Type::moves_on_bind`] with the one answer the bare type cannot give:
    /// a C-layout struct with owning seam slots aliases its source exactly as
    /// an array does — its blocks have one owner — so binding one moves.
    pub fn moves_on_bind(&self, ty: Type) -> bool {
        if ty.moves_on_bind() {
            return true;
        }
        // A value that runs a user `Drop` body has exactly one release, so it
        // has exactly one owner: a second binding would be a second body to
        // run for storage that only goes away once.
        self.owns_unique_c_storage(ty) || self.runs_user_drop(ty)
    }
}

fn mix_native_state_bytes(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Keep concatenations unambiguous (`ab` + `c` is not `a` + `bc`).
    *hash ^= 0xff;
    *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
}

fn mix_native_state_u64(hash: &mut u64, value: u64) {
    mix_native_state_bytes(hash, &value.to_le_bytes());
}

// The codes leave a gap where `I64` and `F64` were, so a value that
// outlived them still means what it did.
fn int_code(spelling: IntSpelling) -> u64 {
    match spelling {
        IntSpelling::Plain => 0,
        IntSpelling::I8 => 1,
        IntSpelling::I16 => 2,
        IntSpelling::I32 => 3,
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
                c_layout: false,
                drop_glue: None,
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
                c_layout: false,
                drop_glue: None,
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
                c_layout: false,
                drop_glue: None,
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
                c_layout: false,
                drop_glue: None,
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
                c_layout: false,
                drop_glue: None,
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
