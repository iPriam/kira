//! Mapping KSL types onto SPIR-V types, and laying the buffer-facing ones out.
//!
//! A struct reaches SPIR-V twice when a buffer holds it. The plain form carries
//! no decorations and is what a local, a parameter, and an interface value have;
//! the laid-out form carries an `Offset` on every member and is the only form a
//! `Block` may be built from. They cannot be one type: `Offset` is illegal on a
//! struct in the function storage class, and `Block` is illegal anywhere but a
//! buffer — so a shader that put one struct in both places would be rejected
//! whichever single form it picked.

use kira_shader_model::{MatrixType, ScalarType, TextureDimension, Type, VectorType};

use crate::Emitter;
use crate::builder::{Id, TypeKey};
use crate::spec::{decoration, dim, op};

impl Emitter<'_> {
    /// The plain SPIR-V type for `ty`.
    pub(crate) fn ty(&mut self, ty: &Type) -> Id {
        match ty {
            Type::Void => self.void(),
            Type::Scalar(scalar) => self.scalar(*scalar),
            Type::Vector(vector) => self.vector(*vector),
            Type::Matrix(matrix) => self.matrix(*matrix),
            Type::StructRef(name) => self.plain_struct(name),
            Type::Texture(dimension) => self.image(*dimension),
            Type::Sampler(_) => self.sampler(),
            Type::RuntimeArray(element) => {
                let element = self.ty(element);
                self.runtime_array(element)
            }
        }
    }

    /// `OpTypeVoid`.
    pub(crate) fn void(&mut self) -> Id {
        self.builder.pooled(TypeKey::Void, |builder, id| {
            builder.global(op::TYPE_VOID, &[id.word()]);
        })
    }

    /// `OpTypeBool`.
    pub(crate) fn bool(&mut self) -> Id {
        self.builder.pooled(TypeKey::Bool, |builder, id| {
            builder.global(op::TYPE_BOOL, &[id.word()]);
        })
    }

    /// A 32-bit integer type, signed or not.
    pub(crate) fn int(&mut self, signed: bool) -> Id {
        self.builder.pooled(TypeKey::Int { signed }, |builder, id| {
            builder.global(op::TYPE_INT, &[id.word(), 32, u32::from(signed)]);
        })
    }

    /// The 32-bit float type.
    pub(crate) fn float(&mut self) -> Id {
        self.builder.pooled(TypeKey::Float, |builder, id| {
            builder.global(op::TYPE_FLOAT, &[id.word(), 32]);
        })
    }

    /// One scalar's type.
    pub(crate) fn scalar(&mut self, scalar: ScalarType) -> Id {
        match scalar {
            ScalarType::Bool => self.bool(),
            ScalarType::Int => self.int(true),
            ScalarType::Uint => self.int(false),
            ScalarType::Float => self.float(),
        }
    }

    /// One vector's type.
    pub(crate) fn vector(&mut self, vector: VectorType) -> Id {
        let element = self.scalar(vector.scalar);
        let width = u32::from(vector.width);
        self.builder
            .pooled(TypeKey::Vector(element, width), |builder, id| {
                builder.global(op::TYPE_VECTOR, &[id.word(), element.word(), width]);
            })
    }

    /// One matrix's type, as its column count of column vectors.
    pub(crate) fn matrix(&mut self, matrix: MatrixType) -> Id {
        let column = self.vector(VectorType {
            scalar: ScalarType::Float,
            width: matrix.rows,
        });
        let columns = u32::from(matrix.columns);
        self.builder
            .pooled(TypeKey::Matrix(column, columns), |builder, id| {
                builder.global(op::TYPE_MATRIX, &[id.word(), column.word(), columns]);
            })
    }

    /// A runtime array of `element`, with the stride the host packs to.
    pub(crate) fn runtime_array(&mut self, element: Id) -> Id {
        let stride = self.strides.get(&element).copied().unwrap_or(0);
        self.builder
            .pooled(TypeKey::RuntimeArray(element), |builder, id| {
                builder.global(op::TYPE_RUNTIME_ARRAY, &[id.word(), element.word()]);
                // Without this a driver has no way to step the array, and the
                // module is rejected rather than reading the wrong element.
                builder.decorate(id, decoration::ARRAY_STRIDE, &[stride]);
            })
    }

    /// A pointer to `pointee` in `storage`.
    pub(crate) fn pointer(&mut self, storage: u32, pointee: Id) -> Id {
        self.builder
            .pooled(TypeKey::Pointer(storage, pointee), |builder, id| {
                builder.global(op::TYPE_POINTER, &[id.word(), storage, pointee.word()]);
            })
    }

    /// A function type returning `result` and taking `params`.
    pub(crate) fn function_type(&mut self, result: Id, params: &[Id]) -> Id {
        let key = TypeKey::Function(result, params.to_vec());
        let words: Vec<u32> = params.iter().map(|id| id.word()).collect();
        self.builder.pooled(key, |builder, id| {
            let mut operands = vec![id.word(), result.word()];
            operands.extend(words);
            builder.global(op::TYPE_FUNCTION, &operands);
        })
    }

    /// A texture's image type.
    pub(crate) fn image(&mut self, dimension: TextureDimension) -> Id {
        let (sampled_type, shape, depth) = match dimension {
            TextureDimension::Texture2d => (self.float(), dim::TWO_D, 0),
            TextureDimension::TextureCube => (self.float(), dim::CUBE, 0),
            TextureDimension::Depth2d => (self.float(), dim::TWO_D, 1),
            TextureDimension::Texture2dUint => (self.int(false), dim::TWO_D, 0),
        };
        self.builder.pooled(
            TypeKey::Image {
                dim: shape,
                sampled_type,
                depth,
            },
            |builder, id| {
                // Arrayed 0, multisampled 0, sampled 1 (read through a sampler
                // or fetched, never written), format Unknown.
                builder.global(
                    op::TYPE_IMAGE,
                    &[id.word(), sampled_type.word(), shape, depth, 0, 0, 1, 0],
                );
            },
        )
    }

    /// `OpTypeSampler`.
    pub(crate) fn sampler(&mut self) -> Id {
        self.builder.pooled(TypeKey::Sampler, |builder, id| {
            builder.global(op::TYPE_SAMPLER, &[id.word()]);
        })
    }

    /// The type an image and a sampler combine into for one sample.
    pub(crate) fn sampled_image(&mut self, image: Id) -> Id {
        self.builder
            .pooled(TypeKey::SampledImage(image), |builder, id| {
                builder.global(op::TYPE_SAMPLED_IMAGE, &[id.word(), image.word()]);
            })
    }

    /// The undecorated struct type `name`, for locals and interface values.
    pub(crate) fn plain_struct(&mut self, name: &str) -> Id {
        if let Some(&found) = self.structs.get(name) {
            return found;
        }
        let fields = self
            .module
            .struct_named(name)
            .map(|declared| declared.fields.clone())
            .unwrap_or_default();
        let members: Vec<(String, Id)> = fields
            .iter()
            .map(|field| (field.name.clone(), self.ty(&field.ty)))
            .collect();
        let id = self.builder.fresh();
        self.structs.insert(name.to_owned(), id);
        let words: Vec<u32> = std::iter::once(id.word())
            .chain(members.iter().map(|(_, member)| member.word()))
            .collect();
        self.builder.global(op::TYPE_STRUCT, &words);
        self.builder.name(id, name);
        for (at, (member, _)) in members.iter().enumerate() {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            self.builder.member_name(id, at, member);
        }
        id
    }

    /// The struct type `name` with every member's offset on it.
    ///
    /// Built from this workspace's one layout rather than from SPIR-V's
    /// defaults, because the host packs to that layout and a driver reads what
    /// these offsets say. A nested struct member is laid out too — its own
    /// offsets are as load-bearing as the outer ones.
    pub(crate) fn laid_out_struct(&mut self, name: &str) -> Id {
        if let Some(&found) = self.laid_out.get(name) {
            return found;
        }
        let Some(declared) = self.module.struct_named(name).cloned() else {
            // A struct the module never declared has no members to lay out; the
            // empty struct keeps emission total rather than making this the one
            // path that can fail.
            let id = self.builder.fresh();
            self.laid_out.insert(name.to_owned(), id);
            self.builder.global(op::TYPE_STRUCT, &[id.word()]);
            return id;
        };
        let layout = kira_shader_ir::layout::layout_of(self.module, &declared.fields);
        let members: Vec<Id> = declared
            .fields
            .iter()
            .map(|field| match &field.ty {
                Type::StructRef(nested) => self.laid_out_struct(nested),
                other => self.ty(other),
            })
            .collect();
        let id = self.builder.fresh();
        self.laid_out.insert(name.to_owned(), id);
        let words: Vec<u32> = std::iter::once(id.word())
            .chain(members.iter().map(|member| member.word()))
            .collect();
        self.builder.global(op::TYPE_STRUCT, &words);
        self.builder.name(id, &format!("{name}_layout"));
        for (at, field) in declared.fields.iter().enumerate() {
            let index = u32::try_from(at).unwrap_or(u32::MAX);
            let offset = layout.fields.get(at).map_or(0, |laid| laid.offset);
            self.builder.member_name(id, index, &field.name);
            self.builder
                .member_decorate(id, index, decoration::OFFSET, &[offset]);
            // A matrix needs both halves said: which way it is stored, and how
            // far apart its columns are. This workspace stores columns, 16
            // bytes apart, which is what `layout.rs` packs and the host writes.
            if let Type::Matrix(_) = field.ty {
                self.builder
                    .member_decorate(id, index, decoration::COL_MAJOR, &[]);
                self.builder
                    .member_decorate(id, index, decoration::MATRIX_STRIDE, &[16]);
            }
        }
        id
    }

    /// Records the stride a runtime array of `element` steps by.
    ///
    /// Kept beside the type rather than derived from it: the stride is the
    /// host's packing of the KSL element type, which the SPIR-V type alone no
    /// longer names.
    pub(crate) fn remember_stride(&mut self, element: Id, stride: u32) {
        self.strides.insert(element, stride);
    }

    /// An unsigned constant.
    pub(crate) fn uint(&mut self, value: u32) -> Id {
        let ty = self.int(false);
        self.builder.constant(ty, value)
    }

    /// A signed constant.
    ///
    /// The word is the two's-complement bit pattern, which is what SPIR-V's
    /// literal is — a reinterpretation rather than a conversion.
    pub(crate) fn sint(&mut self, value: i32) -> Id {
        let ty = self.int(true);
        self.builder
            .constant(ty, u32::from_ne_bytes(value.to_ne_bytes()))
    }
}
