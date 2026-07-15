//! Explicit compiler-phase types for the Kira pipeline.
//!
//! Ported from kira-zig `kira_ir/src/program_phases.zig`. The pipeline
//! distinguishes phases by *type* so a backend can never consume a program
//! that is merely typechecked or only half-lowered:
//!
//! - [`ExecutableProgram`] — executable lowering completed; produced by IR lowering.
//! - [`VerifiedProgram`] — every executable obligation discharged for the
//!   target backend; the ONLY constructor is `verify`, and VM/LLVM/hybrid
//!   emission accepts solely this type.

use crate::ir::Program;

/// A program that has finished executable lowering (Zig `ExecutableProgram`).
/// The sole input accepted by `verify`.
#[derive(Debug, Clone, Default)]
pub struct ExecutableProgram {
    /// Zig `program: ir.Program`.
    pub program: Program,
}

/// What the target backend requires of an executable program (Zig `BackendCapabilities`).
#[derive(Debug, Clone, Copy, Default)]
pub struct BackendCapabilities {
    /// Zig `requires_native_layout` — native backends (LLVM, hybrid) require
    /// every referenced aggregate to have a known native layout.
    pub requires_native_layout: bool,
}

/// Kind of the first unmet obligation (Zig `VerifyFailureKind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyFailureKind {
    /// Zig `.invalid_entry_point`.
    InvalidEntryPoint,
    /// Zig `.unresolved_call_target`.
    UnresolvedCallTarget,
    /// Zig `.unknown_struct_type`.
    UnknownStructType,
    /// Zig `.unknown_enum_type`.
    UnknownEnumType,
}

impl VerifyFailureKind {
    /// One-line human summary (Zig `summary`).
    pub fn summary(self) -> &'static str {
        match self {
            VerifyFailureKind::InvalidEntryPoint => "program entry point is out of range",
            VerifyFailureKind::UnresolvedCallTarget => {
                "call targets a function that was never lowered"
            }
            VerifyFailureKind::UnknownStructType => {
                "struct type has no declaration (layout unknown)"
            }
            VerifyFailureKind::UnknownEnumType => "enum type has no declaration (layout unknown)",
        }
    }
}

/// A precise description of the first unmet obligation (Zig `VerifyFailure`).
#[derive(Debug, Clone)]
pub struct VerifyFailure {
    /// Zig `kind: VerifyFailureKind`.
    pub kind: VerifyFailureKind,
    /// Zig `function_name` — function the obligation was checked in (empty
    /// for program-level failures).
    pub function_name: String,
    /// Zig `detail` — the offending symbol/type name.
    pub detail: String,
}

/// A program proven to satisfy every executable obligation (Zig `VerifiedProgram`).
/// Backends accept only this type; the only way to obtain one is `verify`.
#[derive(Debug, Clone, Default)]
pub struct VerifiedProgram {
    program: Program,
}

impl VerifiedProgram {
    /// Read access to the wrapped program (Zig `programPtr`).
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Explicit, loud escape hatch (Zig `assumeVerified`): wrap a program as
    /// verified WITHOUT running the obligation checks. Only for trusted
    /// inputs (hand-authored test IR); intentionally grep-able.
    pub fn assume_verified(program: Program) -> VerifiedProgram {
        VerifiedProgram { program }
    }
}

/// Outcome of `verify` (Zig `VerifyResult`).
#[derive(Debug, Clone)]
pub enum VerifyResult {
    /// Zig `.verified`.
    Verified(VerifiedProgram),
    /// Zig `.failure`.
    Failure(VerifyFailure),
}

// TODO(port): `verify(executable, caps) -> VerifyResult` — the obligation
// checker (entry point in range, resolved call targets, known struct/enum
// layouts when `requires_native_layout`).
