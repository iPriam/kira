//! KBC bytecode module format: constructs, lifecycle hooks, and encoding.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_bytecode`.

pub mod instruction;
pub mod module;
pub mod opcode;
pub mod ownership_mode;
pub mod serialization;

pub use instruction::{
    ArithKind, BitOp, CompareOp, ConstructConstraint, FunctionConstRepresentation, Instruction,
    StringFromScalarSource, TypeRef, TypeRefKind, UnaryOp,
};
pub use module::{
    Construct, ConstructImplementation, EnumTypeDecl, EnumVariantDecl, Field, ForeignFunction,
    Function, LifecycleHook, MethodMember, Module, SourceLoc, TypeDecl, TypeKind,
};
pub use opcode::{OpCode, is_fused};
pub use ownership_mode::OwnershipMode;
