//! Lowers shader IR to WGSL.
//!
//! Layer 4 of the Kira package graph.
//!
//! Emits **one module per stage**, because that is what a WebGPU pipeline
//! takes: a vertex module and a fragment module are created separately and
//! named separately, which is why the artifact carries `vertexWgsl` and
//! `fragmentWgsl` rather than one combined source.
//!
//! Each module holds only what its stage reaches — its interface structs, the
//! resources, and the functions — so nothing unreferenced makes it into a
//! module WebGPU would then have to validate.

mod emit;
#[cfg(test)]
mod tests;

use kira_shader_ir::ShaderIr;
use kira_shader_model::{Reflection, Stage};

pub use emit::type_name;

/// Emits the WGSL module for one stage of `ir`.
///
/// Returns an empty string when the shader has no such stage.
#[must_use]
pub fn emit(ir: &ShaderIr, stage: Stage) -> String {
    let Some(reflection) = &ir.reflection else {
        return String::new();
    };
    let Some(shader) = &ir.module.shader else {
        return String::new();
    };
    let Some(checked) = shader
        .stages
        .iter()
        .find(|candidate| candidate.stage == stage)
    else {
        return String::new();
    };
    let Some(reflected) = reflection
        .stages
        .iter()
        .find(|candidate| candidate.stage == stage)
    else {
        return String::new();
    };

    let mut emitter = emit::Emitter {
        module: &ir.module,
        reflection,
        out: String::new(),
        atomics: emit::atomic_resources(&ir.module),
    };

    for option in &shader.options {
        let value = match option.value {
            kira_ksl_semantics::model::ConstValue::Bool(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Int(value) => format!("{value}i"),
            kira_ksl_semantics::model::ConstValue::Uint(value) => format!("{value}u"),
            kira_ksl_semantics::model::ConstValue::Float(value) => format!("{value:?}"),
        };
        let line = format!(
            "const {}: {} = {value};",
            option.name,
            type_name(&option.ty)
        );
        emitter.line(0, &line);
    }
    if !shader.options.is_empty() {
        emitter.out.push('\n');
    }

    emit_structs(&mut emitter, reflection, stage);
    emitter.resources();

    for function in &ir.module.functions {
        emitter.function(function);
    }
    for helper in &checked.helpers {
        emitter.function(helper);
    }

    // A compute entry takes its builtins as parameters rather than a struct,
    // because WGSL has no stage-input struct for a compute shader that a
    // workgroup builtin can sit in alongside user data.
    let params = if stage == Stage::Compute {
        reflected
            .inputs
            .iter()
            .filter_map(|field| {
                let builtin = field.builtin?;
                Some(format!(
                    "{} {}: {}",
                    emit::builtin_attribute(builtin),
                    field.name,
                    emit::wgsl_name(&field.type_name)
                ))
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        match (&reflected.input_type, checked.entry.params.first()) {
            (Some(name), Some(param)) => format!("{}: {name}", param.name),
            _ => String::new(),
        }
    };
    let result = reflected
        .output_type
        .as_ref()
        .map_or_else(String::new, |name| format!(" -> {name}"));
    let signature = format!(
        "{} fn {}({params}){result} {{",
        emit::stage_attribute(stage, reflected.threads),
        reflected.entry_name
    );
    emitter.line(0, &signature);
    if stage == Stage::Compute
        && let (Some(param), Some(name)) = (checked.entry.params.first(), &reflected.input_type)
    {
        let fields = reflected
            .inputs
            .iter()
            .filter(|field| field.builtin.is_some())
            .map(|field| field.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let rebuilt = format!("var {}: {name} = {name}({fields});", param.name);
        emitter.line(1, &rebuilt);
    }
    emitter.body(&checked.entry.body, 1);
    emitter.line(0, "}");
    emitter.out
}

/// Emits every struct the stage needs, interfaces decorated.
fn emit_structs(emitter: &mut emit::Emitter<'_>, reflection: &Reflection, stage: Stage) {
    let Some(reflected) = reflection
        .stages
        .iter()
        .find(|candidate| candidate.stage == stage)
    else {
        return;
    };
    // A compute input is an ordinary struct: its builtins arrive as entry
    // parameters, so decorating the struct would attach them twice.
    let decorated: Vec<(&String, &Vec<kira_shader_model::ReflectedField>)> = match stage {
        Stage::Compute => reflected
            .output_type
            .iter()
            .map(|name| (name, &reflected.outputs))
            .collect(),
        _ => reflected
            .input_type
            .iter()
            .map(|name| (name, &reflected.inputs))
            .chain(
                reflected
                    .output_type
                    .iter()
                    .map(|name| (name, &reflected.outputs)),
            )
            .collect(),
    };

    for declared in &emitter.module.structs.clone() {
        if let Some((_, fields)) = decorated.iter().find(|(name, _)| **name == declared.name) {
            let opened = format!("struct {} {{", declared.name);
            emitter.line(0, &opened);
            for field in fields.iter() {
                let attribute = match field.builtin {
                    Some(builtin) => emit::builtin_attribute(builtin).to_owned(),
                    None => format!("@location({})", field.location.unwrap_or(0)),
                };
                let flat = if field.interpolation == Some(kira_shader_model::Interpolation::Flat) {
                    "@interpolate(flat) "
                } else {
                    ""
                };
                let line = format!(
                    "{attribute} {flat}{}: {},",
                    field.name,
                    emit::wgsl_name(&field.type_name)
                );
                emitter.line(1, &line);
            }
            emitter.line(0, "}");
            emitter.out.push('\n');
            continue;
        }
        let opened = format!("struct {} {{", declared.name);
        emitter.line(0, &opened);
        for field in &declared.fields {
            let line = format!("{}: {},", field.name, type_name(&field.ty));
            emitter.line(1, &line);
        }
        emitter.line(0, "}");
        emitter.out.push('\n');
    }
}
