//! One memory layout, used by every backend.
//!
//! A uniform buffer is packed by the host and read by the GPU, so the two have
//! to agree byte for byte. The dialects do not agree on their own — Metal packs
//! a `float3` into 12 bytes where GLSL's `std140` pads it to 16 — so this
//! module defines a single layout and every backend emits types that match it
//! rather than each following its dialect's default.
//!
//! The rules are `std140`'s, because it is the strictest of the ones in play:
//! anything laid out this way is also legal under Metal's and HLSL's packing,
//! whereas the reverse does not hold.
//!
//! - a scalar is 4 bytes, aligned to 4
//! - a 2-wide vector is 8 bytes, aligned to 8
//! - a 3- or 4-wide vector is 16 bytes, aligned to 16 (a 3-wide one is padded)
//! - a matrix is its column count of 4-wide vectors
//! - a struct is aligned to 16, and its size is rounded up to that

use kira_ksl_semantics::model::{CheckedField, CheckedModule};
use kira_shader_model::{ReflectedLayout, ReflectedLayoutField, Type};

/// How many bytes a value of `ty` occupies, and what it aligns to.
#[must_use]
pub fn size_and_alignment(ty: &Type) -> (u32, u32) {
    match ty {
        Type::Scalar(_) => (4, 4),
        Type::Vector(vector) => match vector.width {
            2 => (8, 8),
            3 => (16, 16),
            _ => (16, 16),
        },
        Type::Matrix(matrix) => (16 * u32::from(matrix.columns), 16),
        // A runtime array's length is the binding's, so only its stride is
        // knowable here.
        Type::RuntimeArray(element) => {
            let (size, alignment) = size_and_alignment(element);
            (0, alignment.max(size))
        }
        // A struct's size needs its fields, which `layout_of` has and this
        // does not; a nested one is measured there.
        Type::StructRef(_) => (0, 16),
        Type::Void | Type::Texture(_) | Type::Sampler(_) => (0, 1),
    }
}

/// The stride between consecutive elements of `ty` in an array.
#[must_use]
pub fn stride_of(module: &CheckedModule, ty: &Type) -> u32 {
    match ty {
        Type::StructRef(name) => module
            .struct_named(name)
            .map_or(0, |declared| layout_of(module, &declared.fields).size),
        other => {
            let (size, alignment) = size_and_alignment(other);
            round_up(size, alignment)
        }
    }
}

/// Lays out `fields` in declaration order.
#[must_use]
pub fn layout_of(module: &CheckedModule, fields: &[CheckedField]) -> ReflectedLayout {
    let mut offset = 0u32;
    let mut alignment = 16u32;
    let mut laid_out = Vec::with_capacity(fields.len());
    for field in fields {
        let (size, field_alignment) = match &field.ty {
            Type::StructRef(name) => match module.struct_named(name) {
                Some(declared) => {
                    let nested = layout_of(module, &declared.fields);
                    (nested.size, nested.alignment)
                }
                None => (0, 16),
            },
            other => size_and_alignment(other),
        };
        offset = round_up(offset, field_alignment.max(1));
        alignment = alignment.max(field_alignment);
        laid_out.push(ReflectedLayoutField {
            name: field.name.clone(),
            offset,
            alignment: field_alignment,
            size,
            stride: stride_of(module, &field.ty),
        });
        offset += size;
    }
    ReflectedLayout {
        class: "uniform".to_owned(),
        alignment,
        size: round_up(offset, alignment),
        fields: laid_out,
    }
}

/// `value` rounded up to the next multiple of `alignment`.
fn round_up(value: u32, alignment: u32) -> u32 {
    if alignment == 0 {
        return value;
    }
    value.div_ceil(alignment) * alignment
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_ksl_semantics::model::CheckedStruct;
    use kira_shader_model::{MatrixType, ScalarType, VectorType};

    fn field(name: &str, ty: Type) -> CheckedField {
        CheckedField {
            name: name.to_owned(),
            ty,
            builtin: None,
            interpolation: None,
        }
    }

    #[test]
    fn a_three_wide_vector_is_padded_to_four() {
        // The whole reason this module exists: Metal would pack this into 12.
        assert_eq!(
            size_and_alignment(&Type::Vector(VectorType {
                scalar: ScalarType::Float,
                width: 3
            })),
            (16, 16)
        );
    }

    #[test]
    fn a_four_by_four_matrix_is_four_columns_of_sixteen() {
        assert_eq!(
            size_and_alignment(&Type::Matrix(MatrixType {
                columns: 4,
                rows: 4
            })),
            (64, 16)
        );
    }

    #[test]
    fn a_scalar_after_a_three_wide_vector_lands_in_the_padding_lane() {
        let module = CheckedModule::default();
        let layout = layout_of(
            &module,
            &[
                field(
                    "albedo",
                    Type::Vector(VectorType {
                        scalar: ScalarType::Float,
                        width: 3,
                    }),
                ),
                field("alpha", Type::Scalar(ScalarType::Float)),
            ],
        );
        assert_eq!(layout.fields[0].offset, 0);
        // `std140` pads the vector to 16, so the scalar starts there rather
        // than at 12 — which is exactly the disagreement this pins down.
        assert_eq!(layout.fields[1].offset, 16);
        assert_eq!(layout.size, 32);
    }

    #[test]
    fn a_struct_is_rounded_up_to_its_own_alignment() {
        let module = CheckedModule::default();
        let layout = layout_of(&module, &[field("count", Type::Scalar(ScalarType::Uint))]);
        assert_eq!(layout.fields[0].size, 4);
        assert_eq!(layout.size, 16, "one `UInt` still occupies a whole slot");
    }

    #[test]
    fn a_nested_struct_is_measured_rather_than_guessed() {
        let module = CheckedModule {
            structs: vec![CheckedStruct {
                name: "Inner".to_owned(),
                fields: vec![
                    field("a", Type::Scalar(ScalarType::Float)),
                    field("b", Type::Scalar(ScalarType::Float)),
                ],
            }],
            ..CheckedModule::default()
        };
        let layout = layout_of(
            &module,
            &[
                field("first", Type::StructRef("Inner".to_owned())),
                field("after", Type::Scalar(ScalarType::Float)),
            ],
        );
        assert_eq!(layout.fields[0].size, 16);
        assert_eq!(layout.fields[1].offset, 16);
    }

    #[test]
    fn an_array_of_structs_strides_by_the_structs_whole_size() {
        let module = CheckedModule {
            structs: vec![CheckedStruct {
                name: "Particle".to_owned(),
                fields: vec![
                    field("px", Type::Scalar(ScalarType::Float)),
                    field("vy", Type::Scalar(ScalarType::Float)),
                ],
            }],
            ..CheckedModule::default()
        };
        let stride = stride_of(&module, &Type::StructRef("Particle".to_owned()));
        assert_eq!(stride, 16);
    }
}
