//! FFI declaration metadata attached to HIR items.
//!
//! Ported from kira-zig `kira_semantics_model/src/ffi.zig`.

use kira_runtime_abi::CallingConvention;

use crate::span::Span;
use crate::types::ResolvedType;

/// Ownership of an FFI pointer target (Zig `Ownership`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Ownership {
    /// Zig `.borrowed`.
    #[default]
    Borrowed,
    /// Zig `.owned`.
    Owned,
    /// Zig `.opaque`.
    Opaque,
}

/// Foreign binding of an `@FFI.Extern` function (Zig `ForeignFunction`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForeignFunction {
    /// Zig `library_name: []const u8`.
    pub library_name: String,
    /// Zig `symbol_name: []const u8`.
    pub symbol_name: String,
    /// Zig `calling_convention: CallingConvention = .c`.
    pub calling_convention: CallingConvention,
    /// Zig `span: Span`.
    pub span: Span,
}

/// C-layout struct info (Zig `StructInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct StructInfo {
    /// Zig `layout: []const u8 = "c"`.
    pub layout: String,
    /// Zig `span: Span`.
    pub span: Span,
}

/// FFI pointer type info (Zig `PointerInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct PointerInfo {
    /// Zig `target_name: []const u8`.
    pub target_name: String,
    /// Zig `ownership: Ownership = .borrowed`.
    pub ownership: Ownership,
    /// Zig `span: Span`.
    pub span: Span,
}

/// FFI type alias info (Zig `AliasInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct AliasInfo {
    /// Zig `target: ResolvedType`.
    pub target: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// FFI fixed-size array info (Zig `ArrayInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct ArrayInfo {
    /// Zig `element: ResolvedType`.
    pub element: ResolvedType,
    /// Zig `count: usize`.
    pub count: usize,
    /// Zig `span: Span`.
    pub span: Span,
}

/// FFI callback signature info (Zig `CallbackInfo`).
#[derive(Debug, Clone, PartialEq)]
pub struct CallbackInfo {
    /// Zig `calling_convention: CallingConvention = .c`.
    pub calling_convention: CallingConvention,
    /// Zig `params: []const ResolvedType`.
    pub params: Vec<ResolvedType>,
    /// Zig `result: ResolvedType`.
    pub result: ResolvedType,
    /// Zig `span: Span`.
    pub span: Span,
}

/// FFI metadata on a named type declaration (Zig `NamedTypeInfo`).
#[derive(Debug, Clone, PartialEq)]
pub enum NamedTypeInfo {
    /// Zig `.ffi_struct`.
    FfiStruct(StructInfo),
    /// Zig `.pointer`.
    Pointer(PointerInfo),
    /// Zig `.alias`.
    Alias(AliasInfo),
    /// Zig `.array`.
    Array(ArrayInfo),
    /// Zig `.callback`.
    Callback(CallbackInfo),
}
