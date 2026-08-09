//! Reflection metadata emitted alongside lowered shaders: backend targets,
//! binding assignments, layouts, and stage/resource summaries the graphics
//! host consumes at pipeline-creation time.

use crate::module;
use crate::types;

/// Shader-language backend a shader was lowered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendTarget {
    Glsl430,
    Wgsl,
    Hlsl,
    Msl,
    Spirv,
}

impl BackendTarget {
    /// Every backend, in the order a shader is compiled for them.
    ///
    /// The one place the set is written down, so a backend cannot be added to
    /// the enum and forgotten by something that iterates them.
    pub const ALL: [BackendTarget; 5] = [
        BackendTarget::Msl,
        BackendTarget::Wgsl,
        BackendTarget::Glsl430,
        BackendTarget::Hlsl,
        BackendTarget::Spirv,
    ];

    /// The backend a canonical label names, exactly.
    ///
    /// Stricter than [`Self::parse`] on purpose: a *case* a macro body writes is
    /// spelled one way, and accepting aliases there would let two spellings of
    /// one target drift apart.
    pub fn from_label(label: &str) -> Option<BackendTarget> {
        Self::ALL.into_iter().find(|target| target.label() == label)
    }

    /// Parse a user-facing backend name (accepts common aliases).
    pub fn parse(value: &str) -> Option<BackendTarget> {
        match value {
            "glsl" | "glsl_430" | "glsl430" => Some(BackendTarget::Glsl430),
            "wgsl" => Some(BackendTarget::Wgsl),
            "hlsl" => Some(BackendTarget::Hlsl),
            "msl" | "metal" | "mlsl" => Some(BackendTarget::Msl),
            "spirv" | "spir-v" | "spv" => Some(BackendTarget::Spirv),
            _ => None,
        }
    }

    /// The name a Kira program writes this backend's case as.
    ///
    /// Deliberately without the version [`Self::label`] carries: `Glsl` is the
    /// backend, and which GLSL version it emits is this compiler's business. A
    /// bump from 330 to 430 changed the label and must not change what a
    /// program wrote.
    pub fn case_name(self) -> &'static str {
        match self {
            BackendTarget::Glsl430 => "Glsl",
            BackendTarget::Wgsl => "Wgsl",
            BackendTarget::Hlsl => "Hlsl",
            BackendTarget::Msl => "Msl",
            BackendTarget::Spirv => "Spirv",
        }
    }

    /// Canonical label, version included, for a diagnostic or a file name.
    pub fn label(self) -> &'static str {
        match self {
            BackendTarget::Glsl430 => "glsl_430",
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
    /// Where a target that cannot ask the GPU for an array's length reads it
    /// from instead. Metal is the only one of the five: WGSL has
    /// `arrayLength`, GLSL has `.length()`, HLSL has `GetDimensions`, and
    /// SPIR-V has `OpArrayLength`, but MSL has nothing, so a host binds the
    /// count as its own small buffer.
    pub length_bindings: Vec<(BackendTarget, u32)>,
    /// For a texture, the sampler a stage body actually reads it with.
    ///
    /// KSL keeps a texture and a sampler apart, and so do Metal, WebGPU, HLSL
    /// and SPIR-V. GLSL does not: the two collapse into one `sampler2D`, so a GL
    /// host has to know which sampler object to attach to a texture unit, and
    /// nothing in the declarations says — only the `sample` call does. Measured
    /// from the bodies rather than assumed from declaration order, because
    /// adjacency is a convention a shader is free not to follow.
    ///
    /// `None` on every resource that is not a texture, and on a texture no stage
    /// samples.
    pub paired_sampler: Option<String>,
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
