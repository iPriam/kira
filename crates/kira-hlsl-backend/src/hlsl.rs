//! HLSL emitter: lowers a `kira_shader_ir::ShaderDecl` to HLSL sources
//! (vertex/fragment/compute), mapping resource groups to register spaces and
//! builtins to `SV_*` semantics.
//!
//! Ported from kira-zig `packages/kira_hlsl_backend/src/hlsl.zig`.

use kira_shader_ir as shader_ir;

/// Per-stage HLSL sources for one lowered shader. Zig: `LoweredShader`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoweredShader {
    pub shader_name: String,
    pub vertex_source: Option<String>,
    pub fragment_source: Option<String>,
    pub compute_source: Option<String>,
}

/// Emitter state for lowering one shader declaration.
///
/// Zig: the private `Lowerer` struct in `hlsl.zig`. `lower_shader` logic lands
/// with the port.
#[derive(Debug, Default)]
pub struct HlslLowerer {
    /// Accumulated source text for the stage being emitted.
    pub output: String,
    /// IR of the shader currently being lowered, if any.
    pub current_shader: Option<shader_ir::ShaderDecl>,
}
