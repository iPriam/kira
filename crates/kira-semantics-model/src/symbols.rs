//! Local symbol table entries.
//!
//! Ported from kira-zig `kira_semantics_model/src/symbols.zig`.

use crate::span::Span;
use crate::types::{OwnershipMode, ResolvedType};

/// A function-local symbol (Zig `LocalSymbol`).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalSymbol {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `ownership: OwnershipMode = .owned`.
    pub ownership: OwnershipMode,
    /// Zig `is_param: bool = false`.
    pub is_param: bool,
    /// Zig `is_capture: bool = false`.
    pub is_capture: bool,
    /// Zig `span: Span`.
    pub span: Span,
}
