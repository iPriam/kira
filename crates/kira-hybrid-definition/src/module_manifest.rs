//! The hybrid module manifest (KHM container).
//!
//! Ported from kira-zig `kira_hybrid_definition/src/module_manifest.zig`.
//!
//! # Container version lineage
//!
//! - `KHM1` — original manifest; param/return ownership absent (readers
//!   default params to owned).
//! - `KHM2` — adds per-parameter ownership and return ownership. The writer
//!   emits KHM2.

use kira_runtime_abi::FunctionExecution;

/// KHM1 magic (read-only legacy).
pub const MAGIC_KHM1: &[u8; 4] = b"KHM1";
/// KHM2 magic (current; adds ownership).
pub const MAGIC_KHM2: &[u8; 4] = b"KHM2";

/// Kind of a manifest [`TypeRef`] (Zig `TypeRef.Kind`, `enum(u8)`).
///
/// This is the hybrid definition's own copy (kept dependency-light in Zig,
/// mirrored here) — it matches the bytecode `TypeRefKind` byte-for-byte.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TypeRefKind {
    /// Zig `.void = 0`.
    #[default]
    Void = 0,
    /// Zig `.integer = 1`.
    Integer = 1,
    /// Zig `.float = 2`.
    Float = 2,
    /// Zig `.string = 3`.
    String = 3,
    /// Zig `.boolean = 4`.
    Boolean = 4,
    /// Zig `.construct_any = 5`.
    ConstructAny = 5,
    /// Zig `.array = 6`.
    Array = 6,
    /// Zig `.raw_ptr = 7`.
    RawPtr = 7,
    /// Zig `.ffi_struct = 8`.
    FfiStruct = 8,
    /// Zig `.enum_instance = 9`.
    EnumInstance = 9,
}

/// Construct constraint on a manifest type ref (Zig `TypeRef.ConstructConstraint`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructConstraint {
    /// Zig `construct_name: []const u8`.
    pub construct_name: String,
}

/// A serialized type reference (Zig `TypeRef`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TypeRef {
    /// Zig `kind: Kind`.
    pub kind: TypeRefKind,
    /// Zig `name: ?[]const u8`.
    pub name: Option<String>,
    /// Zig `construct_constraint: ?ConstructConstraint`.
    pub construct_constraint: Option<ConstructConstraint>,
}

/// Ownership mode as serialized in the manifest (Zig `OwnershipMode`,
/// `enum(u8)` — the hybrid definition's own copy, matching bytecode).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OwnershipMode {
    /// Zig `.owned = 0`.
    #[default]
    Owned = 0,
    /// Zig `.borrow_read = 1`.
    BorrowRead = 1,
    /// Zig `.borrow_mut = 2`.
    BorrowMut = 2,
    /// Zig `.move = 3`.
    Move = 3,
    /// Zig `.copy = 4`.
    Copy = 4,
}

/// Per-function manifest entry (Zig `FunctionManifest`).
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionManifest {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `execution: FunctionExecution`.
    pub execution: FunctionExecution,
    /// Zig `param_types: []const TypeRef`.
    pub param_types: Vec<TypeRef>,
    /// Zig `param_ownership: []const OwnershipMode` (KHM2).
    pub param_ownership: Vec<OwnershipMode>,
    /// Zig `return_type: TypeRef = .{ .kind = .void }`.
    pub return_type: TypeRef,
    /// Zig `return_ownership: OwnershipMode = .owned` (KHM2).
    pub return_ownership: OwnershipMode,
    /// Zig `exported_name: ?[]const u8` (serialized as "" when absent).
    pub exported_name: Option<String>,
}

/// A hybrid module manifest (Zig `HybridModuleManifest`).
#[derive(Debug, Clone, PartialEq)]
pub struct HybridModuleManifest {
    /// Zig `module_name: []const u8`.
    pub module_name: String,
    /// Zig `bytecode_path: []const u8`.
    pub bytecode_path: String,
    /// Zig `native_library_path: []const u8`.
    pub native_library_path: String,
    /// Zig `entry_function_id: u32`.
    pub entry_function_id: u32,
    /// Zig `entry_execution: FunctionExecution`.
    pub entry_execution: FunctionExecution,
    /// Zig `functions: []const FunctionManifest`.
    pub functions: Vec<FunctionManifest>,
}

// TODO(port): `HybridModuleManifest.writeToFile` (emits KHM2) and
// `readFromFile` (accepts KHM1 with owned-defaulted ownership, KHM2 with the
// serialized ownership bytes) plus the string/type-ref codec helpers.
