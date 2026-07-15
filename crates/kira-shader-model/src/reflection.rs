//! Reflection metadata emitted alongside lowered shaders: backend targets,
//! binding assignments, layouts, and stage/resource summaries the graphics
//! host consumes at pipeline-creation time.

use crate::module;
use crate::types;

/// Shader-language backend a shader was lowered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendTarget {
    Glsl330,
    Wgsl,
    Hlsl,
    Msl,
    Spirv,
}

impl BackendTarget {
    /// Parse a user-facing backend name (accepts common aliases).
    pub fn parse(value: &str) -> Option<BackendTarget> {
        match value {
            "glsl" | "glsl_330" | "glsl330" => Some(BackendTarget::Glsl330),
            "wgsl" => Some(BackendTarget::Wgsl),
            "hlsl" => Some(BackendTarget::Hlsl),
            "msl" | "metal" | "mlsl" => Some(BackendTarget::Msl),
            "spirv" | "spir-v" | "spv" => Some(BackendTarget::Spirv),
            _ => None,
        }
    }

    /// Canonical label.
    pub fn label(self) -> &'static str {
        match self {
            BackendTarget::Glsl330 => "glsl_330",
            BackendTarget::Wgsl => "wgsl",
            BackendTarget::Hlsl => "hlsl",
            BackendTarget::Msl => "msl",
            BackendTarget::Spirv => "spirv",
        }
    }
}

/// Per-backend binding assignment for a resource.
#[derive(Debug, Clone, PartialEq)]
pub struct BackendBinding {
    pub target: BackendTarget,
    pub group_index: u32,
    pub binding_index: u32,
    pub glsl_name: Option<String>,
}

/// Reflected compile-time option.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedOption {
    pub name: String,
    pub type_name: String,
    pub default_value: String,
}

/// Reflected interface field.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedField {
    pub name: String,
    pub type_name: String,
    pub builtin: Option<types::Builtin>,
    pub interpolation: Option<types::Interpolation>,
    pub location: Option<u32>,
}

/// One field of a reflected uniform/storage layout.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedLayoutField {
    pub name: String,
    pub offset: u32,
    pub alignment: u32,
    pub size: u32,
    pub stride: u32,
}

/// A reflected memory layout for a struct class.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedLayout {
    pub class: String,
    pub alignment: u32,
    pub size: u32,
    pub fields: Vec<ReflectedLayoutField>,
}

/// A reflected user struct with optional layouts.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedType {
    pub name: String,
    pub fields: Vec<ReflectedField>,
    pub uniform_layout: Option<ReflectedLayout>,
    pub storage_layout: Option<ReflectedLayout>,
}

/// A reflected stage entry.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedStage {
    pub stage: types::Stage,
    pub entry_name: String,
    pub input_type: Option<String>,
    pub output_type: Option<String>,
    pub threads: Option<[u32; 3]>,
    pub inputs: Vec<ReflectedField>,
    pub outputs: Vec<ReflectedField>,
}

/// A reflected resource binding.
#[derive(Debug, Clone, PartialEq)]
pub struct ReflectedResource {
    pub group_name: String,
    pub group_class: module::GroupClass,
    pub group_index: u32,
    pub resource_name: String,
    pub resource_kind: module::ResourceKind,
    pub type_name: String,
    pub visibility: Vec<types::Stage>,
    pub access: Option<types::AccessMode>,
    pub backend_bindings: Vec<BackendBinding>,
}

/// The complete reflection blob for one lowered shader.
#[derive(Debug, Clone, PartialEq)]
pub struct Reflection {
    pub shader_name: String,
    pub shader_kind: module::ShaderKind,
    pub backend: BackendTarget,
    pub options: Vec<ReflectedOption>,
    pub stages: Vec<ReflectedStage>,
    pub types: Vec<ReflectedType>,
    pub resources: Vec<ReflectedResource>,
}
