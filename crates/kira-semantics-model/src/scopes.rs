//! Lexical scopes and the per-binding move/borrow state.
//!
//! Ported from kira-zig `kira_semantics_model/src/scopes.zig`.

use std::collections::HashMap;

use crate::hir::FieldStorage;
use crate::span::Span;
use crate::types::{OwnershipMode, ResolvedType};

/// Move/borrow state of one local binding (Zig `LocalBinding`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocalBinding {
    /// Zig `id: u32`.
    pub id: u32,
    /// Zig `ty: ResolvedType`.
    pub ty: ResolvedType,
    /// Zig `storage: FieldStorage`.
    pub storage: FieldStorage,
    /// Zig `ownership: OwnershipMode = .owned`.
    pub ownership: OwnershipMode,
    /// Zig `initialized: bool = true`.
    pub initialized: bool,
    /// Zig `moved: bool = false`.
    pub moved: bool,
    /// Zig `move_span: ?Span`.
    pub move_span: Option<Span>,
    /// Zig `is_task_handle: bool` — the binding holds a `Task { ... }` handle,
    /// which may only be joined, cancelled, or detached (KSEM158 guard).
    pub is_task_handle: bool,
    /// Zig `decl_span: Span`.
    pub decl_span: Span,
    /// Zig `moved_fields` — top-level fields moved out of this binding
    /// (`let x = obj.field` on an aliasing aggregate; Rust partial-move rules).
    pub moved_fields: Vec<String>,
}

impl LocalBinding {
    /// True when `name` has been moved out of this binding (Zig `fieldMoved`).
    pub fn field_moved(&self, name: &str) -> bool {
        self.moved_fields.iter().any(|f| f == name)
    }

    /// True when any field has been moved out (Zig `hasMovedFields`).
    pub fn has_moved_fields(&self) -> bool {
        !self.moved_fields.is_empty()
    }
}

// TODO(port): the remaining LocalBinding move-state helpers
// (markFieldMoved / clearFieldMoved / replaceMovedFields / clearMoveState).

/// One lexical scope: name -> binding (Zig `Scope`).
#[derive(Debug, Clone, Default)]
pub struct Scope {
    /// Zig `entries: std.StringHashMapUnmanaged(LocalBinding)`.
    pub entries: HashMap<String, LocalBinding>,
}
