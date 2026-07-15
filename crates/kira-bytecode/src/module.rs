//! The KBC module data model: functions, type declarations, and constructs.
//!
//! Ported from kira-zig `kira_bytecode/src/bytecode.zig`.

use kira_runtime_abi::CallingConvention;

use crate::instruction::{Instruction, TypeRef};
use crate::ownership_mode::OwnershipMode;

/// Compact per-instruction source location carried in the optional debug
/// section (Zig `SourceLoc`). `file_id` indexes [`Module::source_files`];
/// `start`/`end` are byte offsets into that source. A `{0,0}` span means
/// "no known location".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceLoc {
    /// Zig `file_id: u32`.
    pub file_id: u32,
    /// Zig `start: u32`.
    pub start: u32,
    /// Zig `end: u32`.
    pub end: u32,
}

/// A complete bytecode module (Zig `Module`).
#[derive(Debug, Clone, Default)]
pub struct Module {
    /// Zig `constructs: []Construct`.
    pub constructs: Vec<Construct>,
    /// Zig `construct_implementations: []ConstructImplementation`.
    pub construct_implementations: Vec<ConstructImplementation>,
    /// Zig `types: []TypeDecl`.
    pub types: Vec<TypeDecl>,
    /// Zig `enums: []EnumTypeDecl`.
    pub enums: Vec<EnumTypeDecl>,
    /// Zig `functions: []Function`.
    pub functions: Vec<Function>,
    /// Zig `entry_function_id: ?u32`.
    pub entry_function_id: Option<u32>,
    /// Zig `source_files: []const []const u8` — dedup source-file string table
    /// referenced by [`SourceLoc::file_id`]; empty without debug info (KBCD).
    pub source_files: Vec<String>,
}

impl Module {
    /// Finds a function by id (Zig `findFunctionById`).
    pub fn find_function_by_id(&self, function_id: u32) -> Option<&Function> {
        self.functions.iter().find(|f| f.id == function_id)
    }
}

// TODO(port): `Module.writeToFile` / `Module.readFromFile` file IO wrappers
// around `serialize`/`deserialize` (see crate::serialization).

/// Foreign (FFI) binding for an `@FFI.Extern` function (Zig `ForeignFunction`).
/// Present only on `is_extern` functions; lets the VM dispatch through LibFFI.
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignFunction {
    /// Zig `library_name: []const u8`.
    pub library_name: String,
    /// Zig `symbol_name: []const u8`.
    pub symbol_name: String,
    /// Zig `calling_convention: runtime_abi.CallingConvention = .c`.
    pub calling_convention: CallingConvention,
}

/// A bytecode function (Zig `Function`).
#[derive(Debug, Clone, Default)]
pub struct Function {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `is_async: bool = false`.
    pub is_async: bool,
    /// Zig `param_count: u32 = 0`.
    pub param_count: u32,
    /// Zig `param_ownership: []const OwnershipMode`.
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `param_types: []const TypeRef` — declared parameter types; carries
    /// the precise FFI primitive name in `TypeRef.name` for LibFFI mapping.
    pub param_types: Vec<TypeRef>,
    /// Zig `return_type: TypeRef = .{ .kind = .void }`.
    pub return_type: TypeRef,
    /// Zig `return_ownership: OwnershipMode = .owned`.
    pub return_ownership: OwnershipMode,
    /// Zig `is_extern: bool = false`.
    pub is_extern: bool,
    /// Zig `foreign: ?ForeignFunction`.
    pub foreign: Option<ForeignFunction>,
    /// Zig `register_count: u32`.
    pub register_count: u32,
    /// Zig `local_count: u32`.
    pub local_count: u32,
    /// Zig `local_types: []TypeRef`.
    pub local_types: Vec<TypeRef>,
    /// Zig `instructions: []Instruction`.
    pub instructions: Vec<Instruction>,
    /// Zig `debug_locations: []const SourceLoc` — optional compact PC->source
    /// line table, index-aligned with `instructions` when populated (KBCD).
    pub debug_locations: Vec<SourceLoc>,
    /// Zig `local_names: []const []const u8` — optional positional local-slot
    /// names (index i names local slot i).
    pub local_names: Vec<String>,
}

/// A construct declaration (Zig `Construct`).
#[derive(Debug, Clone, PartialEq)]
pub struct Construct {
    /// Zig `name: []const u8`.
    pub name: String,
}

/// A construct implementation for a concrete type (Zig `ConstructImplementation`).
#[derive(Debug, Clone)]
pub struct ConstructImplementation {
    /// Zig `type_name: []const u8`.
    pub type_name: String,
    /// Zig `construct_constraint: TypeRef.ConstructConstraint`.
    pub construct_constraint: crate::instruction::ConstructConstraint,
    /// Zig `families: []const []const u8` (KBC6+).
    pub families: Vec<String>,
    /// Zig `fields: []Field`.
    pub fields: Vec<Field>,
    /// Zig `has_content: bool`.
    pub has_content: bool,
    /// Zig `lifecycle_hooks: []LifecycleHook`.
    pub lifecycle_hooks: Vec<LifecycleHook>,
}

/// A lifecycle hook name (Zig `LifecycleHook`).
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleHook {
    /// Zig `name: []const u8`.
    pub name: String,
}

/// Kind of a type declaration (Zig `TypeKind`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TypeKind {
    /// Zig `.class = 0`.
    Class = 0,
    /// Zig `.struct_decl = 1`.
    #[default]
    StructDecl = 1,
}

/// A struct/class declaration (Zig `TypeDecl`).
#[derive(Debug, Clone)]
pub struct TypeDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `kind: TypeKind = .struct_decl`.
    pub kind: TypeKind,
    /// Zig `fields: []Field`.
    pub fields: Vec<Field>,
    /// Zig `methods: []MethodMember`.
    pub methods: Vec<MethodMember>,
}

/// A method table entry (Zig `MethodMember`).
#[derive(Debug, Clone, PartialEq)]
pub struct MethodMember {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `function_id: u32`.
    pub function_id: u32,
    /// Zig `receiver_offset: u32`.
    pub receiver_offset: u32,
}

/// An enum declaration (Zig `EnumTypeDecl`).
#[derive(Debug, Clone)]
pub struct EnumTypeDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `variants: []EnumVariantDecl`.
    pub variants: Vec<EnumVariantDecl>,
}

/// An enum variant declaration (Zig `EnumVariantDecl`).
#[derive(Debug, Clone)]
pub struct EnumVariantDecl {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `discriminant: u32`.
    pub discriminant: u32,
    /// Zig `payload_ty: ?TypeRef`.
    pub payload_ty: Option<TypeRef>,
}

/// A named, typed field (Zig `Field`).
#[derive(Debug, Clone)]
pub struct Field {
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: TypeRef`.
    pub ty: TypeRef,
}
