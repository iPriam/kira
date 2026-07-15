//! Resolved-type representation used across semantic analysis.
//!
//! Ported from kira-zig `kira_semantics_model/src/types.zig`.

/// Kind of a resolved type (Zig `Type`).
///
/// Named `TypeKindTag` here because `TypeKind` is taken by the HIR's
/// class/struct discriminator (Zig has both under different files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TypeKindTag {
    /// Zig `.void`.
    Void,
    /// Zig `.integer`.
    Integer,
    /// Zig `.float`.
    Float,
    /// Zig `.boolean`.
    Boolean,
    /// Zig `.string`.
    String,
    /// Zig `.c_string`.
    CString,
    /// Zig `.raw_ptr`.
    RawPtr,
    /// Zig `.callback`.
    Callback,
    /// Zig `.ffi_struct`.
    FfiStruct,
    /// Zig `.named`.
    Named,
    /// Zig `.enum_instance`.
    EnumInstance,
    /// Zig `.construct_any`.
    ConstructAny,
    /// Zig `.array`.
    Array,
    /// Zig `.native_state`.
    NativeState,
    /// Zig `.native_state_view`.
    NativeStateView,
    /// Zig `.unknown`.
    #[default]
    Unknown,
}

/// How a value crosses a binding/call boundary (Zig `OwnershipMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OwnershipMode {
    /// Zig `.owned`.
    #[default]
    Owned,
    /// Zig `.borrow_read`.
    BorrowRead,
    /// Zig `.borrow_mut`.
    BorrowMut,
    /// Zig `.move`.
    Move,
    /// Zig `.copy`.
    Copy,
}

/// A construct constraint on a type (Zig `ConstructConstraint`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConstructConstraint {
    /// Zig `construct_name: []const u8`.
    pub construct_name: String,
}

/// A fully resolved type (Zig `ResolvedType`).
///
/// Note: the Zig `eql` treats missing names as wildcards for kinds that do
/// not require an exact name; the derived `PartialEq` here is stricter.
/// TODO(port): port the custom `eql` semantics when the analyzer needs them.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ResolvedType {
    /// Zig `kind: Type`.
    pub kind: TypeKindTag,
    /// Zig `name: ?[]const u8`.
    pub name: Option<String>,
    /// Zig `construct_constraint: ?ConstructConstraint`.
    pub construct_constraint: Option<ConstructConstraint>,
}

impl ResolvedType {
    /// A resolved type with only a kind (Zig `ResolvedType.plain`).
    pub fn plain(kind: TypeKindTag) -> ResolvedType {
        ResolvedType {
            kind,
            name: None,
            construct_constraint: None,
        }
    }
}
