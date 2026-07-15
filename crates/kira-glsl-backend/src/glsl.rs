//! GLSL 330 emitter: lowers a `kira_shader_ir::ShaderDecl` to per-stage GLSL
//! sources (vertex/fragment for graphics shaders, compute for compute
//! shaders), flattening resource groups to GLSL uniform names and mapping
//! builtins/intrinsics to their GLSL spellings.
//!
//! Ported from kira-zig `packages/kira_glsl_backend/src/glsl.zig`.

use kira_shader_ir as shader_ir;

/// Per-stage GLSL sources for one lowered shader. Zig: `LoweredShader`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoweredShader {
    pub shader_name: String,
    pub vertex_source: Option<String>,
    pub fragment_source: Option<String>,
    pub compute_source: Option<String>,
}

/// Emitter state for lowering one shader declaration.
///
/// Zig: the private `Lowerer` struct in `glsl.zig` (program + shader refs,
/// output buffer, stage context). `lower_shader` logic lands with the port.
#[derive(Debug, Default)]
pub struct GlslLowerer {
    /// Accumulated source text for the stage being emitted.
    pub output: String,
    /// Shaders lowered so far in this run (name-keyed diagnostics context).
    pub lowered: Vec<LoweredShader>,
    /// IR of the shader currently being lowered, if any.
    pub current_shader: Option<shader_ir::ShaderDecl>,
}
