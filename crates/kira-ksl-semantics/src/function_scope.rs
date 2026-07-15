//! Per-function scope for body analysis: local/param symbol tables, name
//! resolution across locals/params/options/resources/functions, expression
//! type inference, and intrinsic call checking.
//!
//! Ported from kira-zig `packages/kira_ksl_semantics/src/function_scope.zig`.

/// Scope state for analyzing one function body.
///
/// Zig: `FunctionScope` (symbol tables, current shader context, diagnostics).
/// Behavior lands with the port; container scaffold only.
#[derive(Debug, Default)]
pub struct FunctionScope {
    /// Locals declared so far, innermost block last. Zig: locals list.
    pub locals: Vec<LocalBinding>,
}

/// One local binding in scope. Zig: local entry in `FunctionScope`.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalBinding {
    pub name: String,
    pub ty: kira_shader_model::Type,
}
