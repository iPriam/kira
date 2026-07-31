//! Lowers shader IR to GLSL 330.
//!
//! Layer 4 of the Kira package graph.
//!
//! Emits **one module per stage**, like WGSL and unlike Metal, because OpenGL
//! compiles and attaches a vertex and a fragment shader separately.
//!
//! GLSL 330 is the one target that cannot express every KSL shader. It has no
//! compute stage and no shader storage buffers — both arrived in 430 — so a
//! shader using either is refused by name rather than emitted as something that
//! would not link. Refusing is the honest answer: the corpus builds its GPU
//! simulation steps on storage buffers, and silently dropping them would leave
//! a shader that compiles and computes nothing.
//!
//! Two dialect facts shape the rest. GLSL 330 has no standalone sampler object,
//! so a texture and the sampler that reads it collapse into one `sampler2D`
//! uniform and the sampler argument disappears at the call. And a stage's
//! interface is loose `in`/`out` variables rather than a struct, so the entry
//! point builds its input struct from them on the way in and copies its output
//! struct back out on the way out.

mod emit;
#[cfg(test)]
mod tests;

use kira_ksl_semantics::model::{CheckedStage, CheckedStmt};
use kira_shader_ir::ShaderIr;
use kira_shader_model::{Reflection, ResourceKind, Stage};

pub use emit::type_name;

/// Why a shader could not be emitted as GLSL 330.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GlslError {
    /// The shader declares a compute stage, which 330 does not have.
    #[error(
        "`{shader}` has a compute stage, which GLSL 330 does not have — compute arrived in 430"
    )]
    ComputeStage {
        /// The shader's name.
        shader: String,
    },
    /// The shader binds storage, which 330 does not have.
    #[error(
        "`{shader}` binds `{resource}` as storage, which GLSL 330 does not have — shader storage \
         buffers arrived in 430"
    )]
    StorageBuffer {
        /// The shader's name.
        shader: String,
        /// The resource that cannot be bound.
        resource: String,
    },
}

/// Emits the GLSL module for one stage of `ir`.
///
/// `Ok("")` when the shader has no such stage, which is not an error: a shader
/// may declare only a vertex stage.
pub fn emit(ir: &ShaderIr, stage: Stage) -> Result<String, GlslError> {
    let (Some(reflection), Some(shader)) = (&ir.reflection, &ir.module.shader) else {
        return Ok(String::new());
    };
    if shader
        .stages
        .iter()
        .any(|candidate| candidate.stage == Stage::Compute)
    {
        return Err(GlslError::ComputeStage {
            shader: shader.name.clone(),
        });
    }
    if let Some(storage) = reflection
        .resources
        .iter()
        .find(|resource| resource.resource_kind == ResourceKind::Storage)
    {
        return Err(GlslError::StorageBuffer {
            shader: shader.name.clone(),
            resource: storage.resource_name.clone(),
        });
    }
    let (Some(checked), Some(reflected)) = (
        shader
            .stages
            .iter()
            .find(|candidate| candidate.stage == stage),
        reflection
            .stages
            .iter()
            .find(|candidate| candidate.stage == stage),
    ) else {
        return Ok(String::new());
    };

    let mut emitter = emit::Emitter {
        module: &ir.module,
        reflection,
        out: String::new(),
        samplers: Vec::new(),
    };
    emitter.line(0, "#version 330 core");
    emitter.out.push('\n');

    for option in &shader.options {
        let value = match option.value {
            kira_ksl_semantics::model::ConstValue::Bool(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Int(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Uint(value) => format!("{value}u"),
            kira_ksl_semantics::model::ConstValue::Float(value) => format!("{value:?}"),
        };
        let line = format!("const {} {} = {value};", type_name(&option.ty), option.name);
        emitter.line(0, &line);
    }
    if !shader.options.is_empty() {
        emitter.out.push('\n');
    }

    emit_structs(&mut emitter, reflection);
    emitter.resources();
    emit_interface(&mut emitter, reflected, stage);

    for function in &ir.module.functions {
        emitter.function(function);
    }
    for helper in &checked.helpers {
        emitter.function(helper);
    }
    emit_entry(&mut emitter, checked, reflected, stage);
    Ok(emitter.out)
}

/// Emits every struct, interfaces included — GLSL keeps them as ordinary types
/// because the stage's own interface is loose variables instead.
fn emit_structs(emitter: &mut emit::Emitter<'_>, reflection: &Reflection) {
    let uniforms: Vec<String> = reflection
        .resources
        .iter()
        .filter(|resource| resource.resource_kind == ResourceKind::Uniform)
        .map(|resource| resource.type_name.clone())
        .collect();
    for declared in &emitter.module.structs.clone() {
        // A uniform's struct is emitted as the block itself, not beside it.
        if uniforms.contains(&declared.name) {
            continue;
        }
        let opened = format!("struct {} {{", declared.name);
        emitter.line(0, &opened);
        for field in &declared.fields {
            let line = format!("{} {};", type_name(&field.ty), field.name);
            emitter.line(1, &line);
        }
        emitter.line(0, "};");
        emitter.out.push('\n');
    }
}

/// Emits the stage's loose `in` and `out` variables.
///
/// A varying is named `v_<field>` in both stages so the vertex output and the
/// fragment input link by name, which is how GLSL 330 matches them.
fn emit_interface(
    emitter: &mut emit::Emitter<'_>,
    reflected: &kira_shader_model::ReflectedStage,
    stage: Stage,
) {
    let mut wrote = false;
    for field in &reflected.inputs {
        if field.builtin.is_some() {
            continue;
        }
        let line = match stage {
            Stage::Vertex => format!(
                "layout(location = {}) in {} {};",
                field.location.unwrap_or(0),
                emit::glsl_name(&field.type_name),
                field.name
            ),
            _ => format!("in {} v_{};", emit::glsl_name(&field.type_name), field.name),
        };
        emitter.line(0, &line);
        wrote = true;
    }
    for field in &reflected.outputs {
        if field.builtin.is_some() {
            continue;
        }
        let line = match stage {
            Stage::Fragment => format!(
                "layout(location = {}) out {} {};",
                field.location.unwrap_or(0),
                emit::glsl_name(&field.type_name),
                field.name
            ),
            _ => format!(
                "out {} v_{};",
                emit::glsl_name(&field.type_name),
                field.name
            ),
        };
        emitter.line(0, &line);
        wrote = true;
    }
    if wrote {
        emitter.out.push('\n');
    }
}

/// Emits the entry point as `main`, marshalling the interface at both ends.
fn emit_entry(
    emitter: &mut emit::Emitter<'_>,
    checked: &CheckedStage,
    reflected: &kira_shader_model::ReflectedStage,
    stage: Stage,
) {
    emitter.line(0, "void main() {");

    // On the way in: rebuild the input struct from the loose variables.
    if let (Some(param), Some(name)) = (checked.entry.params.first(), &reflected.input_type) {
        let declared = format!("{name} {};", param.name);
        emitter.line(1, &declared);
        for field in &reflected.inputs {
            let source = match (field.builtin, stage) {
                (Some(builtin), _) => emit::builtin_name(builtin, stage).to_owned(),
                (None, Stage::Vertex) => field.name.clone(),
                (None, _) => format!("v_{}", field.name),
            };
            let line = format!("{}.{} = {source};", param.name, field.name);
            emitter.line(1, &line);
        }
    }

    // The body, with its `return` replaced by the copy-out, because `main`
    // returns nothing and the outputs are variables rather than a value.
    for &id in &checked.entry.body {
        match emitter.module.stmt(id).clone() {
            CheckedStmt::Return(Some(value)) => {
                let returned = emitter.expr(value);
                for field in &reflected.outputs {
                    let target = match (field.builtin, stage) {
                        (Some(builtin), _) => emit::builtin_name(builtin, stage).to_owned(),
                        (None, Stage::Fragment) => field.name.clone(),
                        (None, _) => format!("v_{}", field.name),
                    };
                    let line = format!("{target} = {returned}.{};", field.name);
                    emitter.line(1, &line);
                }
                emitter.line(1, "return;");
            }
            _ => emitter.stmt(id, 1),
        }
    }
    emitter.line(0, "}");
}
