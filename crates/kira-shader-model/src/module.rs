//! Shader module declarations: shader kind, resource groups, interfaces,
//! options, and entry points.
//!
//! Ported from kira-zig `packages/kira_shader_model/src/module.zig`.

use crate::types;

/// Graphics vs compute shader. Zig: `ShaderKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderKind {
    Graphics,
    Compute,
}

/// Canonical resource group class, derived from the group name.
/// Zig: `GroupClass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupClass {
    Frame,
    Pass,
    Material,
    Object,
    Draw,
    Dispatch,
    Custom,
}

/// Kind of a bound shader resource. Zig: `ResourceKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Uniform,
    Storage,
    Texture,
    Sampler,
}

/// A compile-time shader option declaration. Zig: `OptionDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionDecl {
    pub name: String,
    pub ty: types::Type,
}

/// One field of a stage interface block. Zig: `InterfaceField`.
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceField {
    pub name: String,
    pub ty: types::Type,
    pub builtin: Option<types::Builtin>,
    pub interpolation: Option<types::Interpolation>,
}

/// A stage input/output interface block. Zig: `Interface`.
#[derive(Debug, Clone, PartialEq)]
pub struct Interface {
    pub name: String,
    pub direction: types::InterfaceDirection,
    pub fields: Vec<InterfaceField>,
}

/// One bound resource inside a group. Zig: `Resource`.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    pub name: String,
    pub kind: ResourceKind,
    pub ty: types::Type,
    pub access: Option<types::AccessMode>,
}

/// A named group of resources bound together. Zig: `ResourceGroup`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceGroup {
    pub name: String,
    pub class: GroupClass,
    pub resources: Vec<Resource>,
}

/// A stage entry point declaration. Zig: `EntryPoint`.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryPoint {
    pub stage: types::Stage,
    pub input_name: String,
    pub output_name: Option<String>,
}

/// A complete shader declaration. Zig: `ShaderDecl`.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderDecl {
    pub name: String,
    pub kind: ShaderKind,
    pub options: Vec<OptionDecl>,
    pub groups: Vec<ResourceGroup>,
    pub entries: Vec<EntryPoint>,
}

/// Classify a resource group by its canonical name (case-insensitive).
/// Zig: `classifyGroupName`.
pub fn classify_group_name(name: &str) -> GroupClass {
    if name.eq_ignore_ascii_case("Frame") {
        GroupClass::Frame
    } else if name.eq_ignore_ascii_case("Pass") {
        GroupClass::Pass
    } else if name.eq_ignore_ascii_case("Material") {
        GroupClass::Material
    } else if name.eq_ignore_ascii_case("Object") {
        GroupClass::Object
    } else if name.eq_ignore_ascii_case("Draw") {
        GroupClass::Draw
    } else if name.eq_ignore_ascii_case("Dispatch") {
        GroupClass::Dispatch
    } else {
        GroupClass::Custom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_classification_follows_canonical_names() {
        assert_eq!(GroupClass::Frame, classify_group_name("Frame"));
        assert_eq!(GroupClass::Dispatch, classify_group_name("dispatch"));
        assert_eq!(GroupClass::Custom, classify_group_name("Lighting"));
    }
}
