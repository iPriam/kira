//! What a runtime type descriptor holds, and the table that hands them out.
//!
//! # Why a table and not a hash
//!
//! Runtime type equality is exact nominal identity equality, and `value.type`
//! has to answer with a name, a package, a kind, and the arguments an
//! instantiation was minted with. A hash of the identity string would give
//! comparison and nothing else, and would trade exactness for a collision
//! probability in the one place a collision is a wrong answer rather than a
//! slow one. A table gives both: an index compares in one instruction, and the
//! row beside it carries what the descriptor exposes.
//!
//! # Why identities are strings and indexes are not
//!
//! An index is meaningful only inside one compiled program, which is enough for
//! every question asked *during* a run: the bytecode half, the native half, and
//! the hybrid bundle of one program are built from one table. Anything that
//! outlives a build — a hot-reload compatibility check, a serialized schema, an
//! ABI record — compares [`TypeDescriptor::identity`], which is the
//! package-qualified identity key and is stable across builds.
//!
//! # Distincts
//!
//! A distinct keeps its own row even though `kira-ir` rewrites it to its
//! representation before a backend sees a program. The rewrite is about the
//! machine form of the value; the identity is what the language says the value
//! *is*, and the two are deliberately different questions.

use std::collections::HashMap;

use super::identity::NominalKind;
use super::table::TypeTable;
use super::Type;

/// Which family a descriptor belongs to.
///
/// The value is written into the erasure box and into the VM's `Erase`
/// immediate, so these are **append-only**: a new family takes the next free
/// number and the existing ones never move. The runtime reads the family to
/// know how to compare a payload word, which is why floats have one of their
/// own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum DescriptorFamily {
    /// Every integer spelling.
    Int = 0,
    /// Every float spelling.
    Float = 1,
    /// `Bool`.
    Bool = 2,
    /// `String`.
    String = 3,
    /// A pointer word: `RawPtr` and every `@FFI.Pointer`.
    RawPtr = 4,
    /// A declared struct or class.
    Struct = 5,
    /// An array.
    Array = 6,
    /// A declared enum, including a construct family and a trait existential.
    Enum = 7,
    /// A `distinct` type, which keeps its identity though its representation
    /// is what a backend stores.
    Distinct = 8,
    /// A runtime type descriptor: the type of `value.type` itself.
    RuntimeType = 9,
}

impl DescriptorFamily {
    /// The family as the word an id carries in its high half.
    pub const fn as_word(self) -> u64 {
        self as u64
    }
}

/// What a program can ask of a type at run time.
///
/// Deliberately not fields, layout, methods, or source spans: those are
/// compile-time reflection's, and a runtime descriptor that carried them would
/// make every declaration's private shape a public fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDescriptor {
    /// The package-qualified identity key, stable across builds.
    pub identity: String,
    /// The declared name, as a person reads it.
    pub name: String,
    /// The declaring package's identity key, or empty for a builtin type and
    /// for the program's own declarations.
    pub package: String,
    /// The declaring module inside its package, or empty for a builtin.
    pub module: String,
    /// What kind of type this is.
    pub kind: DescriptorKind,
    /// The family its id carries.
    pub family: DescriptorFamily,
    /// The descriptor indexes of the generic arguments this instantiation was
    /// minted with, in declaration order. Empty for everything else.
    pub arguments: Vec<u32>,
    /// The traits this type conforms to, sorted.
    ///
    /// The conformance facts a runtime descriptor exposes. Names rather than
    /// descriptors: a trait is not a type a value can hold, so there is no row
    /// to point at, and what a program does with the answer is compare it.
    pub conformances: Vec<String>,
}

/// The kind a descriptor reports, as the language spells it.
///
/// A superset of [`NominalKind`]: a runtime descriptor answers for builtin
/// types too, and "what kind of type is this" has to have an answer for `Int`
/// as much as for a declared struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DescriptorKind {
    /// An integer type of any spelling.
    Int = 0,
    /// A float type of any spelling.
    Float = 1,
    /// `Bool`.
    Bool = 2,
    /// `String`.
    String = 3,
    /// A pointer word.
    RawPtr = 4,
    /// An array.
    Array = 5,
    /// A declared struct.
    Struct = 6,
    /// A declared class.
    Class = 7,
    /// A declared enum.
    Enum = 8,
    /// A `distinct` type.
    Distinct = 9,
    /// A construct family existential.
    ConstructFamily = 10,
    /// A trait existential.
    TraitExistential = 11,
    /// A function type.
    Function = 12,
    /// A runtime type descriptor.
    RuntimeType = 13,
}

impl DescriptorKind {
    /// The kind as the word a program reads.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Float => "float",
            Self::Bool => "bool",
            Self::String => "string",
            Self::RawPtr => "pointer",
            Self::Array => "array",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Enum => "enum",
            Self::Distinct => "distinct",
            Self::ConstructFamily => "construct family",
            Self::TraitExistential => "trait existential",
            Self::Function => "function type",
            Self::RuntimeType => "type",
        }
    }

    /// The kind as the byte a module and a native descriptor table carry.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// The kind a nominal declaration reports.
    const fn of_nominal(kind: NominalKind) -> Self {
        match kind {
            NominalKind::Struct => Self::Struct,
            NominalKind::Class => Self::Class,
            NominalKind::Enum => Self::Enum,
            NominalKind::Distinct => Self::Distinct,
            NominalKind::ConstructFamily => Self::ConstructFamily,
            NominalKind::TraitExistential => Self::TraitExistential,
            NominalKind::FunctionType => Self::Function,
        }
    }
}

/// What a program may read off a runtime type descriptor.
///
/// The whole surface, and deliberately closed: adding a field here is adding a
/// runtime fact about every declaration in every program, so each one is a
/// decision rather than a convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TypeField {
    /// `t.name`: the declared name, as a person reads it.
    Name = 0,
    /// `t.package`: the declaring package's identity key, empty for a builtin
    /// type and for the program's own declarations.
    Package = 1,
    /// `t.kind`: the kind word, one of [`DescriptorKind::label`].
    Kind = 2,
    /// `t.arguments`: the descriptors of the generic arguments this
    /// instantiation was minted with, in declaration order.
    Arguments = 3,
    /// `t.conformances`: the names of the traits this type keeps, sorted.
    Conformances = 4,
}

impl TypeField {
    /// The property `name` spells, or `None` when a descriptor has no such
    /// member.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "name" => Self::Name,
            "package" => Self::Package,
            "kind" => Self::Kind,
            "arguments" => Self::Arguments,
            "conformances" => Self::Conformances,
            _ => return None,
        })
    }

    /// The property as the byte an instruction carries.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// The property one byte names.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Name,
            1 => Self::Package,
            2 => Self::Kind,
            3 => Self::Arguments,
            4 => Self::Conformances,
            _ => return None,
        })
    }
}

/// Every runtime type one program can name, in the order they were interned.
///
/// Interning is by [`Type`], so two mentions of one type share a row and
/// therefore an id. The order a program interns in is the order it lowers in,
/// which is deterministic, so two builds of one program produce one table.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TypeDescriptorTable {
    rows: Vec<TypeDescriptor>,
    index: HashMap<Type, u32>,
}

impl TypeDescriptorTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every row, in id order.
    pub fn rows(&self) -> &[TypeDescriptor] {
        &self.rows
    }

    /// The row `index` names.
    pub fn get(&self, index: u32) -> Option<&TypeDescriptor> {
        self.rows.get(index as usize)
    }

    /// The row `ty` was interned under, without minting one.
    pub fn id_of(&self, ty: Type) -> Option<u32> {
        self.index.get(&canonical(ty)).copied()
    }

    /// Records what each interned type conforms to.
    ///
    /// Applied after interning rather than during it: a conformance is a fact
    /// about a type, not about the position that mentioned it, and the program
    /// carries one list for every type it declared.
    pub fn record_conformances(&mut self, conformances: &[(Type, String)]) {
        for (ty, name) in conformances {
            let Some(&index) = self.index.get(&canonical(*ty)) else {
                continue;
            };
            let row = &mut self.rows[index as usize];
            if !row.conformances.iter().any(|known| known == name) {
                row.conformances.push(name.clone());
            }
        }
    }

    /// Every type this table interned, with its id, in id order.
    ///
    /// A backend enumerating the erased types a program can hold reads this
    /// rather than walking the type table: a type nothing erases has no
    /// identity to switch on, and one that erases has exactly one. Empty for a
    /// table rebuilt from serialized rows, which carry names rather than the
    /// [`Type`]s that minted them.
    pub fn interned(&self) -> Vec<(Type, u32)> {
        let mut pairs: Vec<(Type, u32)> = self.index.iter().map(|(&ty, &id)| (ty, id)).collect();
        pairs.sort_by_key(|&(_, id)| id);
        pairs
    }

    /// Rebuilds a table from rows a module carried.
    pub fn from_rows(rows: Vec<TypeDescriptor>) -> Self {
        Self {
            rows,
            index: HashMap::new(),
        }
    }

    /// The descriptor id of `ty`, minting the row on first mention.
    ///
    /// `None` for a type that names no value: `Void`, the error type, and the
    /// internal seam types a program cannot hold.
    pub fn intern(&mut self, types: &TypeTable, ty: Type) -> Option<u32> {
        let ty = canonical(ty);
        if let Some(&existing) = self.index.get(&ty) {
            return Some(existing);
        }
        let family = family_of(ty)?;
        // Reserved before the arguments are interned, so a template whose
        // argument is itself gets one row rather than recursing forever.
        let id = u32::try_from(self.rows.len()).ok()?;
        self.index.insert(ty, id);
        self.rows.push(TypeDescriptor {
            identity: types.identity_key(ty),
            name: descriptor_name(types, ty),
            package: String::new(),
            module: String::new(),
            kind: kind_of(types, ty),
            family,
            arguments: Vec::new(),
            conformances: Vec::new(),
        });
        let identity = types.identity(ty);
        let arguments: Vec<u32> = match &identity {
            Some(identity) => identity
                .arguments
                .iter()
                .filter_map(|&argument| self.intern(types, argument))
                .collect(),
            None => match types.element_of(ty) {
                // An array's element is its one argument, which is what makes
                // `[Int]` and `[Bool]` different types at run time as well as
                // in the checker.
                Some(element) if matches!(ty, Type::Array(_)) => {
                    self.intern(types, element).into_iter().collect()
                }
                _ => Vec::new(),
            },
        };
        let row = &mut self.rows[id as usize];
        row.arguments = arguments;
        if let Some(identity) = identity {
            row.module = identity.module;
            if let Some(package) = identity.package {
                row.package = package.key();
            }
        }
        Some(id)
    }
}

/// The type whose descriptor `ty` shares.
///
/// Integer spellings collapse to `Int` and float spellings to `Float`, which is
/// the rule erasure already follows: a wildcard `Int` is assignable to and from
/// every sized spelling, so an erased `I32` and an erased `Int` holding the same
/// bits are one value by every other measure the language offers, and
/// `erased is Int` has to agree with that. A typed foreign pointer collapses to
/// `RawPtr` for the same reason it crosses the seam as one: what it addresses is
/// a compile-time fact.
fn canonical(ty: Type) -> Type {
    match ty {
        Type::Int(_) => Type::INT,
        Type::Float(_) => Type::FLOAT,
        Type::ForeignPtr(_) => Type::RawPtr,
        other => other,
    }
}

/// The family `ty` belongs to, or `None` for a type that names no value.
pub(super) fn family_of(ty: Type) -> Option<DescriptorFamily> {
    Some(match ty {
        Type::Int(_) => DescriptorFamily::Int,
        Type::Float(_) => DescriptorFamily::Float,
        Type::Bool => DescriptorFamily::Bool,
        Type::String => DescriptorFamily::String,
        Type::RawPtr | Type::ForeignPtr(_) => DescriptorFamily::RawPtr,
        Type::Struct(_) => DescriptorFamily::Struct,
        Type::Array(_) => DescriptorFamily::Array,
        Type::Enum(_) => DescriptorFamily::Enum,
        Type::Distinct(_) => DescriptorFamily::Distinct,
        Type::RuntimeType => DescriptorFamily::RuntimeType,
        Type::Void
        | Type::Error
        | Type::Any
        | Type::CString
        | Type::CBlock
        | Type::Cell(_)
        | Type::Task(_)
        | Type::MainThreadTask(_)
        | Type::NativeState(_) => return None,
    })
}

/// The kind `ty` reports.
fn kind_of(types: &TypeTable, ty: Type) -> DescriptorKind {
    if let Some(identity) = types.identity(ty) {
        return DescriptorKind::of_nominal(identity.kind);
    }
    match ty {
        Type::Int(_) => DescriptorKind::Int,
        Type::Float(_) => DescriptorKind::Float,
        Type::Bool => DescriptorKind::Bool,
        Type::String => DescriptorKind::String,
        Type::Array(_) => DescriptorKind::Array,
        Type::RuntimeType => DescriptorKind::RuntimeType,
        _ => DescriptorKind::RawPtr,
    }
}

/// The name a descriptor reports: the declared name for a nominal type, the
/// written spelling for everything else.
fn descriptor_name(types: &TypeTable, ty: Type) -> String {
    match types.identity(ty) {
        Some(identity) => identity.name,
        None => types.type_name(ty),
    }
}
