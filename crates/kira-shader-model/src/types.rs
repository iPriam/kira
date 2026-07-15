//! Core shader type system: stages, scalar/vector/matrix types, resource
//! type descriptors, and builtin legality rules.

/// Pipeline stage a shader function runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

/// Direction of a stage interface block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InterfaceDirection {
    Input,
    Output,
}

/// Scalar element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarType {
    Bool,
    Int,
    Uint,
    Float,
}

/// Vector of scalars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VectorType {
    pub scalar: ScalarType,
    pub width: u8,
}

/// Column-major float matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatrixType {
    pub columns: u8,
    pub rows: u8,
}

/// Texture resource dimensionality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TextureDimension {
    Texture2d,
    TextureCube,
    Depth2d,
    /// 2D texture of unsigned-integer texels (e.g. R32Uint visibility buffer).
    /// Read via `load` (texelFetch), never sampled/filtered.
    Texture2dUint,
}

/// Sampler resource flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SamplerKind {
    Filtering,
    Comparison,
}

/// Storage-resource access mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    Read,
    ReadWrite,
}

/// Varying interpolation qualifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interpolation {
    Perspective,
    Linear,
    Flat,
}

/// Stage builtin values.
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

/// A KSL-visible shader type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Void,
    Scalar(ScalarType),
    Vector(VectorType),
    Matrix(MatrixType),
    /// Reference to a user-declared struct by name.
    StructRef(String),
    Texture(TextureDimension),
    Sampler(SamplerKind),
    /// Unsized array element type.
    RuntimeArray(Box<Type>),
}

/// Whether `builtin` is legal for `stage`/`direction`.
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
