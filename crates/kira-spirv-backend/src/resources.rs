//! Emitting the module-scope variables a stage binds and links against.
//!
//! Two kinds, and they are not alike. A resource is memory or a handle the host
//! binds, addressed by a descriptor set and a binding; an interface member is a
//! variable the pipeline links stage to stage, addressed by a location or named
//! by a builtin. Both are module-scope variables in SPIR-V, and everything else
//! about them differs.

use kira_shader_model::{
    AccessMode, BackendTarget, Interpolation, ReflectedStage, Reflection, ResourceKind, ScalarType,
    Stage, Type,
};

use crate::builder::{Id, TypeKey};
use crate::spec::{decoration, op, storage_class};
use crate::{Bound, Emitter, Global, Interface, built_in_of, scalar_of};

impl Emitter<'_> {
    /// Emits every resource as a module-scope variable with its binding.
    pub(crate) fn resources(
        &mut self,
        reflection: &Reflection,
        shader: &kira_ksl_semantics::model::CheckedShader,
    ) {
        for reflected in &reflection.resources {
            let Some(binding) = reflected
                .backend_bindings
                .iter()
                .find(|binding| binding.target == BackendTarget::Spirv)
            else {
                continue;
            };
            let Some(declared) = shader
                .groups
                .iter()
                .flat_map(|group| &group.resources)
                .find(|candidate| candidate.name == reflected.resource_name)
            else {
                continue;
            };
            let name = declared.name.clone();
            let (storage, pointee, bound) = match reflected.resource_kind {
                ResourceKind::Uniform => {
                    let Type::StructRef(struct_name) = &declared.ty else {
                        continue;
                    };
                    let block = self.laid_out_struct(struct_name);
                    self.builder.decorate(block, decoration::BLOCK, &[]);
                    (
                        storage_class::UNIFORM,
                        block,
                        Bound::Uniform(struct_name.clone()),
                    )
                }
                ResourceKind::Storage => {
                    let Type::RuntimeArray(element) = &declared.ty else {
                        continue;
                    };
                    let element = element.as_ref().clone();
                    let element_id = match &element {
                        Type::StructRef(nested) => self.laid_out_struct(nested),
                        other => self.ty(other),
                    };
                    let stride = kira_shader_ir::layout::stride_of(self.module, &element);
                    self.remember_stride(element_id, stride);
                    let array = self.runtime_array(element_id);
                    let block = self.buffer_block(&name, array);
                    (
                        storage_class::STORAGE_BUFFER,
                        block,
                        Bound::Storage(element),
                    )
                }
                ResourceKind::Texture | ResourceKind::Sampler => {
                    let handle = self.ty(&declared.ty);
                    (
                        storage_class::UNIFORM_CONSTANT,
                        handle,
                        Bound::Handle(declared.ty.clone()),
                    )
                }
            };
            let pointer_ty = self.pointer(storage, pointee);
            let variable = self.builder.fresh();
            self.builder
                .global(op::VARIABLE, &[pointer_ty.word(), variable.word(), storage]);
            self.builder.name(variable, &name);
            self.builder
                .decorate(variable, decoration::DESCRIPTOR_SET, &[binding.group_index]);
            self.builder
                .decorate(variable, decoration::BINDING, &[binding.binding_index]);
            if reflected.resource_kind == ResourceKind::Storage
                && reflected.access != Some(AccessMode::ReadWrite)
            {
                self.builder
                    .decorate(variable, decoration::NON_WRITABLE, &[]);
            }
            let global = match bound {
                Bound::Uniform(struct_name) => Global::Uniform {
                    pointer: variable,
                    name: struct_name,
                },
                Bound::Storage(element) => Global::Storage {
                    pointer: variable,
                    element,
                },
                Bound::Handle(ty) => Global::Handle {
                    pointer: variable,
                    ty,
                },
            };
            self.globals.insert(name, global);
        }
    }

    /// The one-member `Block` struct a storage buffer's array sits in.
    fn buffer_block(&mut self, name: &str, array: Id) -> Id {
        let key = TypeKey::Struct(format!("{name}#buffer"));
        let block = self.builder.pooled(key, |builder, id| {
            builder.global(op::TYPE_STRUCT, &[id.word(), array.word()]);
        });
        self.builder.decorate(block, decoration::BLOCK, &[]);
        self.builder
            .member_decorate(block, 0, decoration::OFFSET, &[0]);
        block
    }

    /// Emits the `Input` or `Output` variable for every interface member.
    ///
    /// A compute stage has only builtins on the way in and nothing on the way
    /// out: its inputs are the invocation's own coordinates, and it publishes
    /// through the buffers it binds.
    pub(crate) fn interface(
        &mut self,
        reflected: &ReflectedStage,
        stage: Stage,
        is_input: bool,
    ) -> Vec<Interface> {
        let (name, fields) = if is_input {
            (&reflected.input_type, &reflected.inputs)
        } else {
            (&reflected.output_type, &reflected.outputs)
        };
        let Some(struct_name) = name else {
            return Vec::new();
        };
        if stage == Stage::Compute && !is_input {
            return Vec::new();
        }
        let Some(declared) = self.module.struct_named(struct_name).cloned() else {
            return Vec::new();
        };
        let storage = if is_input {
            storage_class::INPUT
        } else {
            storage_class::OUTPUT
        };
        let mut built = Vec::new();
        for (at, field) in declared.fields.iter().enumerate() {
            let reflected_field = fields.get(at);
            let builtin = reflected_field.and_then(|found| found.builtin);
            if stage == Stage::Compute && builtin.is_none() {
                continue;
            }
            let pointee = self.ty(&field.ty);
            let pointer_ty = self.pointer(storage, pointee);
            let variable = self.builder.fresh();
            self.builder
                .global(op::VARIABLE, &[pointer_ty.word(), variable.word(), storage]);
            self.builder.name(variable, &field.name);
            match builtin {
                Some(builtin) => {
                    self.builder.decorate(
                        variable,
                        decoration::BUILT_IN,
                        &[built_in_of(builtin, stage)],
                    );
                }
                None => {
                    let location = reflected_field
                        .and_then(|found| found.location)
                        .unwrap_or(0);
                    self.builder
                        .decorate(variable, decoration::LOCATION, &[location]);
                    self.interpolate(variable, field, reflected_field, stage, is_input);
                }
            }
            built.push((field.ty.clone(), variable, field.name.clone()));
        }
        built
    }

    /// Decorates a varying with how it is interpolated.
    ///
    /// An integer varying is the one that must be said: SPIR-V requires `Flat`
    /// on it, because there is no meaningful way to interpolate an integer and
    /// a module that asks for one is rejected.
    fn interpolate(
        &mut self,
        variable: Id,
        field: &kira_ksl_semantics::model::CheckedField,
        reflected: Option<&kira_shader_model::ReflectedField>,
        stage: Stage,
        is_input: bool,
    ) {
        let interpolated = matches!(
            (stage, is_input),
            (Stage::Vertex, false) | (Stage::Fragment, true)
        );
        if !interpolated {
            return;
        }
        let integral = matches!(
            scalar_of(&field.ty),
            Some(ScalarType::Int | ScalarType::Uint | ScalarType::Bool)
        );
        let declared = reflected.and_then(|found| found.interpolation);
        if integral || declared == Some(Interpolation::Flat) {
            self.builder.decorate(variable, decoration::FLAT, &[]);
        } else if declared == Some(Interpolation::Linear) {
            self.builder
                .decorate(variable, decoration::NO_PERSPECTIVE, &[]);
        }
    }
}
