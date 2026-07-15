//! Shader type, module, and reflection model shared by KSL and the shader-language backends.
//!
//! Layer 2 of the Kira package graph.

pub mod module;
pub mod reflection;
pub mod types;

pub use module::{
    EntryPoint, GroupClass, Interface, InterfaceField, OptionDecl, Resource, ResourceGroup,
    ResourceKind, ShaderDecl, ShaderKind, classify_group_name,
};
pub use reflection::{
    BackendBinding, BackendTarget, ReflectedField, ReflectedLayout, ReflectedLayoutField,
    ReflectedOption, ReflectedResource, ReflectedStage, ReflectedType, Reflection,
};
pub use types::{
    AccessMode, Builtin, InterfaceDirection, Interpolation, MatrixType, SamplerKind, ScalarType,
    Stage, TextureDimension, Type, VectorType, builtin_allowed,
};
