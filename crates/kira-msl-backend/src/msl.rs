//! MSL emitter: lowers a `kira_shader_ir::ShaderDecl` to Metal Shading
//! Language sources (vertex/fragment/compute), mapping resource groups to
//! Metal argument buffer/binding indices and builtins to `[[attribute]]`
//! spellings.
//!
//! Ported from kira-zig `packages/kira_msl_backend/src/msl.zig`.

use kira_shader_ir as shader_ir;

/// Per-stage MSL sources for one lowered shader. Zig: `LoweredShader`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoweredShader {
    pub shader_name: String,
    pub vertex_source: Option<String>,
    pub fragment_source: Option<String>,
    pub compute_source: Option<String>,
}

/// Emitter state for lowering one shader declaration.
///
/// Zig: the private `Lowerer` struct in `msl.zig`. `lower_shader` logic lands
/// with the port.
#[derive(Debug, Default)]
pub struct MslLowerer {
    /// Accumulated source text for the stage being emitted.
    pub output: String,
    /// IR of the shader currently being lowered, if any.
    pub current_shader: Option<shader_ir::ShaderDecl>,
}
