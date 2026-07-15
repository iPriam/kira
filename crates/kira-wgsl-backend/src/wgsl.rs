//! WGSL emitter: lowers a `kira_shader_ir::ShaderDecl` to WGSL sources
//! (vertex/fragment; compute stages are emitted into the same module),
//! mapping resource groups to `@group/@binding` pairs and builtins to
//! `@builtin(...)` attributes.
//!
//! Ported from kira-zig `packages/kira_wgsl_backend/src/wgsl.zig`.

use kira_shader_ir as shader_ir;

/// Per-stage WGSL sources for one lowered shader. Zig: `LoweredShader`
/// (vertex/fragment only — WGSL compute entry points share the module text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoweredShader {
    pub shader_name: String,
    pub vertex_source: Option<String>,
    pub fragment_source: Option<String>,
}

/// Emitter state for lowering one shader declaration.
///
/// Zig: the private `Lowerer` struct in `wgsl.zig`. `lower_shader` logic lands
/// with the port.
#[derive(Debug, Default)]
pub struct WgslLowerer {
    /// Accumulated source text for the stage being emitted.
    pub output: String,
    /// IR of the shader currently being lowered, if any.
    pub current_shader: Option<shader_ir::ShaderDecl>,
}
