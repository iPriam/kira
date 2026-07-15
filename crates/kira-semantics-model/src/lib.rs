//! Semantic model (HIR): programs, constructs, symbols, types, scopes, and FFI declarations.
//!
//! Layer 2 of the Kira package graph.
//! Ported from kira-zig `packages/kira_semantics_model`.
//!
//! Port shape: the Zig HIR threads `*Expr` pointers through an arena
//! allocator; the Rust port stores every expression in a per-program
//! [`hir_expr::ExprArena`] and references them by [`hir_expr::ExprId`]
//! (la-arena index — no lifetimes, no `Rc`).

pub mod ffi;
pub mod hir;
pub mod hir_expr;
pub mod scopes;
pub mod span;
pub mod symbols;
pub mod types;

pub use ffi::{
    AliasInfo, ArrayInfo, CallbackInfo, ForeignFunction, NamedTypeInfo, Ownership, PointerInfo,
    StructInfo,
};
pub use hir::{
    Annotation, AnnotationArgument, AnnotationDecl, AnnotationParameterDecl, AnnotationRule,
    AnnotationTarget, AnnotationValue, CapabilityDecl, Construct, ConstructDefaultMember,
    ConstructForm, ConstructParent, ContentChannel, ContentProjection, EnumDecl, EnumVariantHir,
    Field, FieldStorage, Function, GeneratedFunction, Import, LifecycleHook, MatchArm,
    MatchPattern, MethodMember, Parameter, Program, PropertySchema, RequiredField,
    RequiredFunction, SectionFunction, Statement, TestCase, TypeDecl, TypeKind,
};
pub use hir_expr::{
    BinaryOp, BuilderBlock, BuilderItem, Capture, Expr, ExprArena, ExprId, UnaryOp,
};
pub use scopes::{LocalBinding, Scope};
pub use span::Span;
pub use symbols::LocalSymbol;
pub use types::{ConstructConstraint, OwnershipMode, ResolvedType, TypeKindTag};
