//! FFI call-plan model: ABIs, ownership annotations, the FFI type lattice,
//! and full call signatures, validated before a call plan is built.
//!
//! Ported from kira-zig `packages/kira_dynamic_ffi/src/signature.zig`.
//! Types only; `validateSignature`/`validateType` logic lands with the port.

/// Calling convention for a foreign call. Zig: `Abi`
/// (`platformDefault`: win64 on Windows, aarch64 on Apple, system elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Abi {
    #[default]
    C,
    System,
    Win64,
    Sysv,
    Unix64,
    Aarch64,
}

/// Pointer ownership annotation. Zig: `Ownership`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Ownership {
    #[default]
    Borrowed,
    OwnedByCaller,
    OwnedByCallee,
    Retained,
}

/// The FFI type lattice. Zig: `Type` (tagged union).
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Void,
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    F32,
    F64,
    Pointer(PointerType),
    Handle(HandleType),
    Enumeration(EnumType),
    Bitflags(BitflagsType),
    Structure(StructType),
    Union(UnionType),
    Array(ArrayType),
    Callback(Callback),
}

/// Pointer type. Zig: `Pointer` (`child`, `mutable`, `ownership`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PointerType {
    pub child: Option<Box<Type>>,
    pub mutable: bool,
    pub ownership: Ownership,
}

/// Opaque handle type (e.g. `VkInstance`). Zig: `Handle`.
#[derive(Debug, Clone, PartialEq)]
pub struct HandleType {
    pub name: String,
    pub is_opaque: bool,
}

/// C enum with an integer backing. Zig: `Enum`.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumType {
    pub name: String,
    pub backing: IntBacking,
}

/// C bitflags with an integer backing. Zig: `Bitflags`.
#[derive(Debug, Clone, PartialEq)]
pub struct BitflagsType {
    pub name: String,
    pub backing: IntBacking,
}

/// Integer backing width for enums/bitflags. Zig: `IntBacking`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntBacking {
    I32,
    U32,
    I64,
    U64,
}

/// One aggregate field with an explicit offset. Zig: `Field`.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub offset: Option<usize>,
}

/// C struct layout. Zig: `Struct` (fields/size/alignment all required for a
/// valid call plan).
#[derive(Debug, Clone, PartialEq)]
pub struct StructType {
    pub name: String,
    pub fields: Vec<Field>,
    pub size: usize,
    pub alignment: usize,
}

/// C union layout. Zig: `Union`.
#[derive(Debug, Clone, PartialEq)]
pub struct UnionType {
    pub name: String,
    pub fields: Vec<Field>,
    pub size: usize,
    pub alignment: usize,
    pub tagged: bool,
}

/// Fixed-length C array. Zig: `Array` (`len` must be non-zero).
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayType {
    pub element: Box<Type>,
    pub len: usize,
}

/// C function-pointer callback. Zig: `Callback`.
#[derive(Debug, Clone, PartialEq)]
pub struct Callback {
    pub parameters: Vec<Type>,
    pub result: Box<Type>,
    pub abi: Abi,
}

/// One named parameter. Zig: `Parameter`.
#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub ty: Type,
}

/// A complete foreign-call signature (the call plan input).
/// Zig: `Signature`.
#[derive(Debug, Clone, PartialEq)]
pub struct Signature {
    pub symbol: String,
    pub abi: Abi,
    pub parameters: Vec<Parameter>,
    pub result: Type,
}

/// Validation failure classes. Zig: `DiagnosticCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagnosticCode {
    EmptySymbol,
    InvalidVoidParameter,
    UnsupportedReturnType,
    UnsupportedLayout,
    UnsupportedCallbackResult,
    UnsafeOwnership,
}

/// A signature validation diagnostic. Zig: `Diagnostic`.
///
/// TODO(port): `validate_signature` (rejects empty symbols, void parameters,
/// non-returnable results, incomplete aggregate layouts, mutable
/// callee-owned pointers) lands with the migration.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub message: String,
}
