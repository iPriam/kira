//! Lowering a checked module to the IR the backends emit from.
//!
//! Two things happen here that neither semantics nor a backend should do.
//! Bindings get assigned — each dialect numbers its resources its own way, and
//! doing it once means the reflection a host reads and the source a backend
//! emits cannot drift apart. And interface locations get assigned, in
//! declaration order, so a vertex stage's outputs and a fragment stage's inputs
//! agree without either backend inventing an order.

use kira_ksl_semantics::model::{
    CheckedExprId, CheckedExprKind, CheckedModule, CheckedResource, CheckedShader, CheckedStage,
    CheckedStmt, CheckedStmtId, ConstValue,
};
use kira_shader_model::{
    BackendBinding, BackendTarget, ReflectedField, ReflectedOption, ReflectedResource,
    ReflectedStage, ReflectedType, Reflection, ResourceKind, ShaderKind, Stage, Type,
};

use crate::ShaderIr;
use crate::glsl_names::glsl_safe_name;
use crate::layout;

/// Every target a shader is reflected for.
const TARGETS: [BackendTarget; 5] = [
    BackendTarget::Msl,
    BackendTarget::Wgsl,
    BackendTarget::Glsl430,
    BackendTarget::Hlsl,
    BackendTarget::Spirv,
];

/// Lowers `module` for `target`.
#[must_use]
pub fn lower(module: CheckedModule, target: BackendTarget) -> ShaderIr {
    let reflection = module
        .shader
        .as_ref()
        .map(|shader| reflect(&module, shader, target));
    ShaderIr { module, reflection }
}

/// Builds the reflection for one shader.
fn reflect(module: &CheckedModule, shader: &CheckedShader, target: BackendTarget) -> Reflection {
    let kind = if shader
        .stages
        .iter()
        .all(|stage| stage.stage == Stage::Compute)
    {
        ShaderKind::Compute
    } else {
        ShaderKind::Graphics
    };
    Reflection {
        shader_name: shader.name.clone(),
        shader_kind: kind,
        backend: target,
        options: shader
            .options
            .iter()
            .map(|option| ReflectedOption {
                name: option.name.clone(),
                type_name: type_name(&option.ty),
                default_value: constant_text(option.value),
            })
            .collect(),
        stages: shader
            .stages
            .iter()
            .map(|stage| reflect_stage(module, stage))
            .collect(),
        types: reflect_types(module, shader),
        resources: reflect_resources(module, shader),
    }
}

/// Reflects one stage and its interface.
fn reflect_stage(module: &CheckedModule, stage: &CheckedStage) -> ReflectedStage {
    ReflectedStage {
        stage: stage.stage,
        entry_name: entry_name(stage.stage),
        input_type: stage.input.clone(),
        output_type: stage.output.clone(),
        threads: stage.threads,
        inputs: interface(module, stage.input.as_deref()),
        outputs: interface(module, stage.output.as_deref()),
    }
}

/// The fields of an interface struct, with locations assigned in order.
///
/// A field carrying a builtin takes no location: the dialect names it, and
/// giving it one too would consume a slot the hardware does not have.
fn interface(module: &CheckedModule, name: Option<&str>) -> Vec<ReflectedField> {
    let Some(declared) = name.and_then(|name| module.struct_named(name)) else {
        return Vec::new();
    };
    let mut location = 0u32;
    declared
        .fields
        .iter()
        .map(|field| {
            let at = if field.builtin.is_some() {
                None
            } else {
                let slot = location;
                location += 1;
                Some(slot)
            };
            ReflectedField {
                name: field.name.clone(),
                type_name: type_name(&field.ty),
                builtin: field.builtin,
                interpolation: field.interpolation,
                location: at,
            }
        })
        .collect()
}

/// Reflects every struct a resource or interface names, with its layout.
fn reflect_types(module: &CheckedModule, shader: &CheckedShader) -> Vec<ReflectedType> {
    let mut named: Vec<String> = Vec::new();
    for group in &shader.groups {
        for resource in &group.resources {
            match &resource.ty {
                Type::StructRef(name) => named.push(name.clone()),
                Type::RuntimeArray(element) => {
                    if let Type::StructRef(name) = element.as_ref() {
                        named.push(name.clone());
                    }
                }
                _ => {}
            }
        }
    }
    for stage in &shader.stages {
        named.extend(stage.input.clone());
        named.extend(stage.output.clone());
    }
    named.sort();
    named.dedup();
    named
        .into_iter()
        .filter_map(|name| {
            let declared = module.struct_named(&name)?;
            Some(ReflectedType {
                name,
                fields: declared
                    .fields
                    .iter()
                    .map(|field| ReflectedField {
                        name: field.name.clone(),
                        type_name: type_name(&field.ty),
                        builtin: field.builtin,
                        interpolation: field.interpolation,
                        location: None,
                    })
                    .collect(),
                uniform_layout: Some(layout::layout_of(module, &declared.fields)),
                storage_layout: None,
            })
        })
        .collect()
}

/// Reflects every resource, with one binding per target.
fn reflect_resources(module: &CheckedModule, shader: &CheckedShader) -> Vec<ReflectedResource> {
    let mut counters = Counters::default();
    let mut reflected = Vec::new();
    for (group_index, group) in shader.groups.iter().enumerate() {
        let group_index = u32::try_from(group_index).unwrap_or(u32::MAX);
        for (position, resource) in group.resources.iter().enumerate() {
            // A written `@binding(n)` decides the slot outright; position is
            // only the default. The two mix freely within a group — the checker
            // is what refuses two resources that would land on one slot, so by
            // here every slot in the group is distinct.
            let within = resource
                .binding
                .unwrap_or_else(|| u32::try_from(position).unwrap_or(u32::MAX));
            reflected.push(ReflectedResource {
                group_name: group.name.clone(),
                group_class: group.class,
                group_index,
                resource_name: resource.name.clone(),
                resource_kind: resource.kind,
                type_name: resource_type_name(module, resource),
                visibility: visible_stages(module, shader, &resource.name),
                access: resource.access,
                backend_bindings: TARGETS
                    .iter()
                    .map(|&target| counters.assign(target, resource, group_index, within))
                    .collect(),
                length_bindings: Vec::new(),
                paired_sampler: match resource.kind {
                    ResourceKind::Texture => paired_sampler(module, shader, &resource.name)
                        .or_else(|| declared_sampler(group, position)),
                    ResourceKind::Uniform | ResourceKind::Storage | ResourceKind::Sampler => None,
                },
            });
            if matches!(resource.ty, Type::RuntimeArray(_)) {
                counters.msl_length.push(resource.name.clone());
            }
        }
    }
    // Metal reads an array's length from a buffer of its own, and those come
    // after every resource buffer so adding one never renumbers the others.
    for name in &counters.msl_length {
        counters.msl_buffer += 1;
        if let Some(resource) = reflected
            .iter_mut()
            .find(|resource| resource.resource_name == *name)
        {
            resource
                .length_bindings
                .push((BackendTarget::Msl, counters.msl_buffer));
        }
    }
    reflected
}

/// The sampler declared nearest after `position` in the same group.
///
/// The fallback for a texture no stage samples. A shader may declare a resource
/// its bodies do not read — a group is an interface, and a host binds every slot
/// in it — and GLSL still needs a sampler paired with the texture unit, because
/// the pair is how a `sampler2D` uniform is named at all. Declaration order is
/// the only signal left once no `sample` call names one, and a KSL group writes a
/// texture immediately before the sampler that belongs to it.
fn declared_sampler(
    group: &kira_ksl_semantics::model::CheckedGroup,
    position: usize,
) -> Option<String> {
    group
        .resources
        .iter()
        .skip(position + 1)
        .find(|candidate| candidate.kind == ResourceKind::Sampler)
        .or_else(|| {
            group
                .resources
                .iter()
                .find(|candidate| candidate.kind == ResourceKind::Sampler)
        })
        .map(|candidate| candidate.name.clone())
}

/// The sampler `texture` is sampled with, when a body samples it.
///
/// GLSL collapses a texture and its sampler into one `sampler2D`, so a GL host
/// needs to know which sampler object belongs on the texture's unit — and the
/// declarations do not say. The `sample` call does, so that is what is read.
/// Declaration adjacency would usually give the same answer and is a convention
/// rather than a rule.
fn paired_sampler(module: &CheckedModule, shader: &CheckedShader, texture: &str) -> Option<String> {
    shader.stages.iter().find_map(|stage| {
        std::iter::once(&stage.entry)
            .chain(&stage.helpers)
            .find_map(|function| sampler_in_body(module, &function.body, texture))
    })
}

/// The sampler `texture` is sampled with anywhere in `body`.
fn sampler_in_body(
    module: &CheckedModule,
    body: &[CheckedStmtId],
    texture: &str,
) -> Option<String> {
    body.iter().find_map(|&id| match module.stmt(id) {
        CheckedStmt::Let { init, .. } => {
            init.and_then(|value| sampler_in_expr(module, value, texture))
        }
        CheckedStmt::Assign { target, value } => sampler_in_expr(module, *target, texture)
            .or_else(|| sampler_in_expr(module, *value, texture)),
        CheckedStmt::If {
            cond,
            then,
            otherwise,
        } => sampler_in_expr(module, *cond, texture)
            .or_else(|| sampler_in_body(module, then, texture))
            .or_else(|| {
                otherwise
                    .as_ref()
                    .and_then(|body| sampler_in_body(module, body, texture))
            }),
        CheckedStmt::While { cond, body } => sampler_in_expr(module, *cond, texture)
            .or_else(|| sampler_in_body(module, body, texture)),
        CheckedStmt::Return(value) => {
            value.and_then(|value| sampler_in_expr(module, value, texture))
        }
        CheckedStmt::Expr(value) => sampler_in_expr(module, *value, texture),
    })
}

/// The sampler `texture` is sampled with at `id` or anywhere under it.
fn sampler_in_expr(module: &CheckedModule, id: CheckedExprId, texture: &str) -> Option<String> {
    let node = module.expr(id);
    if let CheckedExprKind::Builtin {
        which: kira_ksl_semantics::model::BuiltinFn::Sample,
        args,
    } = &node.kind
        && let [image, sampler, ..] = args.as_slice()
        && matches!(&module.expr(*image).kind, CheckedExprKind::Resource(name) if name == texture)
        && let CheckedExprKind::Resource(name) = &module.expr(*sampler).kind
    {
        return Some(name.clone());
    }
    match &node.kind {
        CheckedExprKind::Field { base, .. }
        | CheckedExprKind::Swizzle { base, .. }
        | CheckedExprKind::ArrayLength { base } => sampler_in_expr(module, *base, texture),
        CheckedExprKind::Index { base, index } => sampler_in_expr(module, *base, texture)
            .or_else(|| sampler_in_expr(module, *index, texture)),
        CheckedExprKind::Construct { args }
        | CheckedExprKind::Call { args, .. }
        | CheckedExprKind::Builtin { args, .. } => args
            .iter()
            .find_map(|&arg| sampler_in_expr(module, arg, texture)),
        CheckedExprKind::Cast { value } | CheckedExprKind::Unary { operand: value, .. } => {
            sampler_in_expr(module, *value, texture)
        }
        CheckedExprKind::Binary { lhs, rhs, .. } => sampler_in_expr(module, *lhs, texture)
            .or_else(|| sampler_in_expr(module, *rhs, texture)),
        CheckedExprKind::Const(_)
        | CheckedExprKind::Local(_)
        | CheckedExprKind::Option(_)
        | CheckedExprKind::Resource(_)
        | CheckedExprKind::Invalid => None,
    }
}

/// The stages whose bodies actually read `name`.
///
/// Measured rather than assumed. A host binds a uniform block to every stage
/// this lists, and a stage has only so many block slots — so claiming a
/// resource is visible everywhere spends slots on stages that never touch it.
fn visible_stages(module: &CheckedModule, shader: &CheckedShader, name: &str) -> Vec<Stage> {
    shader
        .stages
        .iter()
        .filter(|stage| {
            std::iter::once(&stage.entry)
                .chain(&stage.helpers)
                .any(|function| reads_resource(module, &function.body, name))
        })
        .map(|stage| stage.stage)
        .collect()
}

/// Whether any statement in `body` reads the resource `name`.
fn reads_resource(module: &CheckedModule, body: &[CheckedStmtId], name: &str) -> bool {
    body.iter().any(|&id| match module.stmt(id) {
        CheckedStmt::Let { init, .. } => init.is_some_and(|value| expr_reads(module, value, name)),
        CheckedStmt::Assign { target, value } => {
            expr_reads(module, *target, name) || expr_reads(module, *value, name)
        }
        CheckedStmt::If {
            cond,
            then,
            otherwise,
        } => {
            expr_reads(module, *cond, name)
                || reads_resource(module, then, name)
                || otherwise
                    .as_ref()
                    .is_some_and(|body| reads_resource(module, body, name))
        }
        CheckedStmt::While { cond, body } => {
            expr_reads(module, *cond, name) || reads_resource(module, body, name)
        }
        CheckedStmt::Return(value) => value.is_some_and(|value| expr_reads(module, value, name)),
        CheckedStmt::Expr(value) => expr_reads(module, *value, name),
    })
}

/// Whether `id` or anything under it reads the resource `name`.
fn expr_reads(module: &CheckedModule, id: CheckedExprId, name: &str) -> bool {
    let node = module.expr(id);
    match &node.kind {
        CheckedExprKind::Resource(read) => read == name,
        CheckedExprKind::Field { base, .. }
        | CheckedExprKind::Swizzle { base, .. }
        | CheckedExprKind::ArrayLength { base } => expr_reads(module, *base, name),
        CheckedExprKind::Index { base, index } => {
            expr_reads(module, *base, name) || expr_reads(module, *index, name)
        }
        CheckedExprKind::Construct { args }
        | CheckedExprKind::Call { args, .. }
        | CheckedExprKind::Builtin { args, .. } => {
            args.iter().any(|&arg| expr_reads(module, arg, name))
        }
        CheckedExprKind::Cast { value } | CheckedExprKind::Unary { operand: value, .. } => {
            expr_reads(module, *value, name)
        }
        CheckedExprKind::Binary { lhs, rhs, .. } => {
            expr_reads(module, *lhs, name) || expr_reads(module, *rhs, name)
        }
        CheckedExprKind::Const(_)
        | CheckedExprKind::Local(_)
        | CheckedExprKind::Option(_)
        | CheckedExprKind::Invalid => false,
    }
}

/// The per-target binding counters, which run over the whole shader.
#[derive(Default)]
struct Counters {
    /// Metal's array-length buffers, assigned after every other buffer so a
    /// shader with no runtime array numbers exactly as it would without them.
    msl_length: Vec<String>,
    /// Metal numbers buffers, textures, and samplers in three spaces.
    msl_buffer: u32,
    msl_texture: u32,
    msl_sampler: u32,
    /// HLSL numbers `b`, `t`, and `s` registers separately, and a storage
    /// buffer shares the `t` space with textures.
    hlsl_buffer: u32,
    hlsl_texture: u32,
    hlsl_sampler: u32,
    /// GLSL binds uniform blocks and texture units in two spaces.
    glsl_block: u32,
    glsl_texture: u32,
}

impl Counters {
    /// The binding `resource` takes on `target`.
    fn assign(
        &mut self,
        target: BackendTarget,
        resource: &CheckedResource,
        group_index: u32,
        within: u32,
    ) -> BackendBinding {
        // A written slot is written for one reason: to land where the host
        // already binds. Honor it on every target rather than only where the
        // default happens to be positional, so a shader cannot be right on
        // Metal and silently wrong on WebGPU.
        if let Some(slot) = resource.binding {
            let glsl_name = match target {
                BackendTarget::Glsl430 => Some(glsl_safe_name(&resource.name)),
                _ => None,
            };
            return BackendBinding {
                target,
                group_index,
                binding_index: slot,
                glsl_name,
            };
        }
        let (group, binding, glsl_name) = match target {
            // Metal's vertex buffer 0 is the vertex attribute stream, so the
            // resource buffers start at 1.
            BackendTarget::Msl => match resource.kind {
                ResourceKind::Uniform | ResourceKind::Storage => {
                    self.msl_buffer += 1;
                    (0, self.msl_buffer, None)
                }
                ResourceKind::Texture => {
                    let at = self.msl_texture;
                    self.msl_texture += 1;
                    (0, at, None)
                }
                ResourceKind::Sampler => {
                    let at = self.msl_sampler;
                    self.msl_sampler += 1;
                    (0, at, None)
                }
            },
            // WGSL and SPIR-V both address a resource as (set, binding), and
            // the group a shader wrote is exactly that set.
            BackendTarget::Wgsl | BackendTarget::Spirv => (group_index, within, None),
            BackendTarget::Hlsl => match resource.kind {
                ResourceKind::Uniform => {
                    let at = self.hlsl_buffer;
                    self.hlsl_buffer += 1;
                    (0, at, None)
                }
                ResourceKind::Storage | ResourceKind::Texture => {
                    let at = self.hlsl_texture;
                    self.hlsl_texture += 1;
                    (0, at, None)
                }
                ResourceKind::Sampler => {
                    let at = self.hlsl_sampler;
                    self.hlsl_sampler += 1;
                    (0, at, None)
                }
            },
            // GLSL 330 has no separate sampler object: a texture and the
            // sampler that reads it collapse into one `sampler2D` uniform, so
            // a sampler takes no unit of its own and the name is what the host
            // looks the binding up by.
            BackendTarget::Glsl430 => match resource.kind {
                ResourceKind::Uniform | ResourceKind::Storage => {
                    let at = self.glsl_block;
                    self.glsl_block += 1;
                    (0, at, Some(glsl_safe_name(&resource.name)))
                }
                ResourceKind::Texture => {
                    let at = self.glsl_texture;
                    self.glsl_texture += 1;
                    (0, at, Some(glsl_safe_name(&resource.name)))
                }
                ResourceKind::Sampler => (0, 0, Some(glsl_safe_name(&resource.name))),
            },
        };
        BackendBinding {
            target,
            group_index: group,
            binding_index: binding,
            glsl_name,
        }
    }
}

/// The entry point's emitted name for a stage.
#[must_use]
pub fn entry_name(stage: Stage) -> String {
    match stage {
        Stage::Vertex => "vertex_main",
        Stage::Fragment => "fragment_main",
        Stage::Compute => "compute_main",
    }
    .to_owned()
}

/// The type name a resource reflects as.
fn resource_type_name(module: &CheckedModule, resource: &CheckedResource) -> String {
    let _ = module;
    type_name(&resource.ty)
}

/// How a type is written in reflection, which is how KSL writes it.
#[must_use]
pub fn type_name(ty: &Type) -> String {
    use kira_shader_model::ScalarType;
    let scalar = |scalar| match scalar {
        ScalarType::Bool => "Bool",
        ScalarType::Int => "Int",
        ScalarType::Uint => "UInt",
        ScalarType::Float => "Float",
    };
    match ty {
        Type::Void => "Void".to_owned(),
        Type::Scalar(value) => scalar(*value).to_owned(),
        Type::Vector(vector) => format!("{}{}", scalar(vector.scalar), vector.width),
        Type::Matrix(matrix) => format!("Float{}x{}", matrix.columns, matrix.rows),
        Type::StructRef(name) => name.clone(),
        Type::Texture(dimension) => match dimension {
            kira_shader_model::TextureDimension::Texture2d => "Texture2d",
            kira_shader_model::TextureDimension::TextureCube => "TextureCube",
            kira_shader_model::TextureDimension::Depth2d => "Depth2d",
            kira_shader_model::TextureDimension::Texture2dUint => "Texture2dUint",
        }
        .to_owned(),
        Type::Sampler(kind) => match kind {
            kira_shader_model::SamplerKind::Filtering => "Sampler",
            kira_shader_model::SamplerKind::Comparison => "SamplerComparison",
        }
        .to_owned(),
        // A `-` would collide with the absent-field marker, so an array is
        // written with its element in brackets and no spaces.
        Type::RuntimeArray(element) => format!("[{}]", type_name(element)),
    }
}

/// How a constant is written in reflection.
fn constant_text(value: ConstValue) -> String {
    match value {
        ConstValue::Bool(value) => value.to_string(),
        ConstValue::Int(value) => value.to_string(),
        ConstValue::Uint(value) => value.to_string(),
        ConstValue::Float(value) => format!("{value:?}"),
    }
}
