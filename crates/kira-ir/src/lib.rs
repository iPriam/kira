//! Kira mid-level and low-level IR: programs, functions, constructs, and
//! ownership modes.
//!
//! Layer 3 of the Kira package graph.
//! Ported from kira-zig `packages/kira_ir`:
//!
//! - [`ir`] — the low IR (register machine) consumed by the bytecode compiler
//!   and the LLVM backend (`ir.zig`).
//! - [`mid_ir`] — the place/value mid IR the ownership checker runs on
//!   (`mid_ir.zig`).
//! - [`program_phases`] — explicit compiler-phase wrapper types; backends
//!   accept only [`program_phases::VerifiedProgram`] (`program_phases.zig`).
//!
//! TODO(port): the lowering passes (`lower_from_hir*.zig`), the mid-IR
//! ownership checker (`mid_ir_check.zig` et al.), the async state-machine
//! transform (`async_state_machine.zig`), and the pipeline drivers
//! (`mid_ir_pipeline.zig`).

pub mod instruction;
pub mod ir;
pub mod mid_ir;
pub mod program_phases;

pub use instruction::Instruction;
pub use ir::{
    Construct, ConstructConstraint, ConstructImplementation, EnumTypeDecl, EnumVariantIr,
    FfiTypeInfo, Field, ForeignFunction, Function, LifecycleHook, MethodMember, OwnershipMode,
    Program, TypeDecl, TypeKind, ValueType, ValueTypeKind,
};
pub use program_phases::{
    BackendCapabilities, ExecutableProgram, VerifiedProgram, VerifyFailure, VerifyFailureKind,
    VerifyResult,
};
