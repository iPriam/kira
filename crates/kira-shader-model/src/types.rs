//! Core shader type system: stages, scalar/vector/matrix types, resource
//! type descriptors, and builtin legality rules.
//!
//! Ported from kira-zig `packages/kira_shader_model/src/types.zig`.

/// Pipeline stage a shader function runs in. Zig: `Stage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

/// Direction of a stage interface block. Zig: `InterfaceDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterfaceDirection {
    Input,
    Output,
}

/// Scalar element type. Zig: `ScalarType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarType {
    Bool,
    Int,
    Uint,
    Float,
}

/// Vector of scalars. Zig: `VectorType` (`scalar`, `width`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VectorType {
    pub scalar: ScalarType,
    pub width: u8,
}

/// Column-major float matrix. Zig: `MatrixType` (`columns`, `rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatrixType {
    pub columns: u8,
    pub rows: u8,
}

/// Texture resource dimensionality. Zig: `TextureDimension`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureDimension {
    Texture2d,
    TextureCube,
    Depth2d,
    /// 2D texture of unsigned-integer texels (e.g. R32Uint visibility buffer).
    /// Read via `load` (texelFetch), never sampled/filtered.
    Texture2dUint,
}

/// Sampler resource flavor. Zig: `SamplerKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerKind {
    Filtering,
    Comparison,
}

/// Storage-resource access mode. Zig: `AccessMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    Read,
    ReadWrite,
}

/// Varying interpolation qualifier. Zig: `Interpolation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interpolation {
    Perspective,
    Linear,
    Flat,
}

/// Stage builtin values. Zig: `Builtin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Builtin {
    Position,
    VertexIndex,
    InstanceIndex,
    FrontFacing,
    FragCoord,
    ThreadId,
    LocalId,
    GroupId,
    LocalIndex,
}

/// A KSL-visible shader type. Zig: `Type` (tagged union).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Void,
    Scalar(ScalarType),
    Vector(VectorType),
    Matrix(MatrixType),
    /// Reference to a user-declared struct by name. Zig: `struct_ref: []const u8`.
    StructRef(String),
    Texture(TextureDimension),
    Sampler(SamplerKind),
    /// Unsized array element type. Zig: `runtime_array: *const Type`.
    RuntimeArray(Box<Type>),
}

/// Whether `builtin` is legal for `stage`/`direction`.
/// Zig: `builtinAllowed`.
pub fn builtin_allowed(builtin: Builtin, stage: Stage, direction: InterfaceDirection) -> bool {
    match builtin {
        Builtin::Position => {
            (stage == Stage::Vertex && direction == InterfaceDirection::Output)
                || (stage == Stage::Fragment && direction == InterfaceDirection::Input)
        }
        Builtin::VertexIndex | Builtin::InstanceIndex => {
            stage == Stage::Vertex && direction == InterfaceDirection::Input
        }
        Builtin::FrontFacing | Builtin::FragCoord => {
            stage == Stage::Fragment && direction == InterfaceDirection::Input
        }
        Builtin::ThreadId | Builtin::LocalId | Builtin::GroupId | Builtin::LocalIndex => {
            stage == Stage::Compute && direction == InterfaceDirection::Input
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_legality_follows_stage_direction_rules() {
        assert!(builtin_allowed(
            Builtin::Position,
            Stage::Vertex,
            InterfaceDirection::Output
        ));
        assert!(!builtin_allowed(
            Builtin::Position,
            Stage::Vertex,
            InterfaceDirection::Input
        ));
        assert!(builtin_allowed(
            Builtin::Position,
            Stage::Fragment,
            InterfaceDirection::Input
        ));
        assert!(builtin_allowed(
            Builtin::ThreadId,
            Stage::Compute,
            InterfaceDirection::Input
        ));
        assert!(!builtin_allowed(
            Builtin::ThreadId,
            Stage::Fragment,
            InterfaceDirection::Input
        ));
    }
}
