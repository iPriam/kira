//! SPIR-V emitter: lowers a `kira_shader_ir::ShaderDecl` to graphics-stage
//! SPIR-V modules for render-pipeline validation. Compute shaders are
//! rejected with a diagnostic (KSL125) until a compute-capable lowering path
//! is added — mirror of the Zig behavior.
//!
//! Ported from kira-zig `packages/kira_spirv_backend/src/spirv.zig`.

use kira_shader_ir as shader_ir;

/// Per-stage SPIR-V outputs for one lowered shader. Zig: `LoweredShader`
/// (vertex/fragment only; no compute today).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoweredShader {
    pub shader_name: String,
    pub vertex_source: Option<String>,
    pub fragment_source: Option<String>,
}

/// Emitter state for lowering one shader declaration.
///
/// Zig: the private `Lowerer` struct in `spirv.zig` (id allocator, type/const
/// dedup tables, instruction buffers). `lower_shader` logic lands with the
/// port.
#[derive(Debug, Default)]
pub struct SpirvLowerer {
    /// Next SPIR-V result id to allocate. Zig: id counter in `Lowerer`.
    pub next_id: u32,
    /// Encoded instruction words for the module being emitted.
    pub words: Vec<u32>,
    /// IR of the shader currently being lowered, if any.
    pub current_shader: Option<shader_ir::ShaderDecl>,
}
