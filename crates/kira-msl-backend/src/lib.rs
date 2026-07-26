//! Lowers shader IR to MSL (Metal Shading Language).
//!
//! Layer 4 of the Kira package graph.
//!
//! Emits **one module** holding every stage, because that is what Metal
//! compiles: a `.metallib` is built from one source and the pipeline names its
//! vertex and fragment functions out of it. The other dialects split per stage;
//! this one does not, which is why the artifact carries a `combinedMsl` rather
//! than a pair.
//!
//! Structs are emitted once and shared by the stages that use them. An
//! interface struct is emitted twice under different names when it is both a
//! vertex output and a fragment input, because Metal spells the two
//! differently: an output names its locations with `[[user(…)]]` and an input
//! takes them with `[[stage_in]]`, and the same declaration cannot do both.

mod emit;
#[cfg(test)]
mod tests;

use kira_shader_ir::ShaderIr;
use kira_shader_model::{Reflection, Stage};

pub use emit::type_name;

/// Emits the whole MSL module for `ir`.
///
/// Returns an empty string when the module declares no shader, which is what a
/// KSL file holding only shared types and functions is.
#[must_use]
pub fn emit(ir: &ShaderIr) -> String {
    let Some(reflection) = &ir.reflection else {
        return String::new();
    };
    let Some(shader) = &ir.module.shader else {
        return String::new();
    };
    let mut emitter = emit::Emitter {
        module: &ir.module,
        reflection,
        out: String::new(),
        renames: std::collections::HashMap::new(),
    };
    emitter.line(0, "#include <metal_stdlib>");
    emitter.line(0, "using namespace metal;");
    emitter.out.push('\n');

    for option in &shader.options {
        let value = match option.value {
            kira_ksl_semantics::model::ConstValue::Bool(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Int(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Uint(value) => format!("{value}u"),
            kira_ksl_semantics::model::ConstValue::Float(value) => format!("{value:?}"),
        };
        let line = format!(
            "constant {} {} = {value};",
            type_name(&option.ty),
            option.name
        );
        emitter.line(0, &line);
    }
    if !shader.options.is_empty() {
        emitter.out.push('\n');
    }

    emit_structs(&mut emitter, reflection);
    for function in &ir.module.functions {
        emitter.function(function);
    }
    for stage in &shader.stages {
        for helper in &stage.helpers {
            emitter.function(helper);
        }
    }
    for stage in &shader.stages {
        emit_entry(&mut emitter, stage, reflection);
    }
    emitter.out
}

/// Emits every struct the module declares, in declaration order.
///
/// Order is declaration order rather than use order because KSL already
/// requires a type to be declared before the struct that holds it, and Metal
/// needs the same.
fn emit_structs(emitter: &mut emit::Emitter<'_>, reflection: &Reflection) {
    let interfaces = interface_names(reflection);
    for declared in &emitter.module.structs.clone() {
        // An interface struct is emitted per stage instead, with that stage's
        // attributes on its fields.
        if interfaces.contains(&declared.name) {
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

/// Every struct name a stage takes through Metal's interface machinery.
///
/// A compute stage's input is not one of them: a kernel takes its builtins as
/// loose parameters, so that struct stays an ordinary one the body builds.
fn interface_names(reflection: &Reflection) -> Vec<String> {
    let mut names = Vec::new();
    for stage in &reflection.stages {
        if stage.stage != Stage::Compute {
            names.extend(stage.input_type.clone());
        }
        names.extend(stage.output_type.clone());
    }
    names.sort();
    names.dedup();
    names
}

/// Emits one stage: its interface structs, then its entry point.
fn emit_entry(
    emitter: &mut emit::Emitter<'_>,
    stage: &kira_ksl_semantics::model::CheckedStage,
    reflection: &Reflection,
) {
    let Some(reflected) = reflection
        .stages
        .iter()
        .find(|candidate| candidate.stage == stage.stage)
    else {
        return;
    };
    let prefix = stage_prefix(stage.stage);
    let input_name = reflected
        .input_type
        .as_ref()
        .map(|name| format!("{prefix}_{name}_in"));
    let output_name = reflected
        .output_type
        .as_ref()
        .map(|name| format!("{prefix}_{name}_out"));
    let _ = &output_name;

    emitter.renames.clear();
    if let (Some(spelled), Some(source)) = (&input_name, &reflected.input_type)
        && stage.stage != Stage::Compute
    {
        emit_interface(
            emitter,
            spelled,
            source,
            &reflected.inputs,
            stage.stage,
            true,
        );
        emitter.renames.insert(source.clone(), spelled.clone());
    }
    if let (Some(spelled), Some(source)) = (&output_name, &reflected.output_type) {
        emit_interface(
            emitter,
            spelled,
            source,
            &reflected.outputs,
            stage.stage,
            false,
        );
        emitter.renames.insert(source.clone(), spelled.clone());
    }

    let mut params: Vec<String> = Vec::new();
    match (&input_name, stage.stage) {
        // A kernel takes its builtins as loose parameters; there is no
        // `[[stage_in]]` for a compute function.
        (Some(_), Stage::Compute) => {
            for field in &reflected.inputs {
                if let Some(builtin) = field.builtin {
                    params.push(format!(
                        "{} {} {}",
                        type_name(&builtin_type(&field.type_name)),
                        field.name,
                        emit::builtin_attribute(builtin, stage.stage, true)
                    ));
                }
            }
        }
        (Some(spelled), _) => {
            let parameter = stage
                .entry
                .params
                .first()
                .map_or_else(|| "in".to_owned(), |param| param.name.clone());
            params.push(format!("{spelled} {parameter} [[stage_in]]"));
        }
        (None, _) => {}
    }
    params.extend(emitter.resource_params(stage.stage));

    let result = output_name.clone().unwrap_or_else(|| "void".to_owned());
    let signature = format!(
        "{} {} {}({}) {{",
        prefix,
        result,
        reflected.entry_name,
        params.join(", ")
    );
    emitter.line(0, &signature);

    // A kernel's parameters are the builtins themselves, so the body's
    // reference to the input struct is rebuilt from them.
    if stage.stage == Stage::Compute
        && let (Some(param), Some(source)) = (stage.entry.params.first(), &reflected.input_type)
    {
        let fields = reflected
            .inputs
            .iter()
            .filter(|field| field.builtin.is_some())
            .map(|field| field.name.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let rebuilt = format!("{source} {} = {{ {fields} }};", param.name);
        emitter.line(1, &rebuilt);
    }
    emitter.body(&stage.entry.body, 1);
    emitter.line(0, "}");
    emitter.out.push('\n');
    emitter.renames.clear();
}

/// Emits one stage's view of an interface struct.
fn emit_interface(
    emitter: &mut emit::Emitter<'_>,
    spelled: &str,
    source: &str,
    fields: &[kira_shader_model::ReflectedField],
    stage: Stage,
    is_input: bool,
) {
    let opened = format!("struct {spelled} {{");
    emitter.line(0, &opened);
    for field in fields {
        let attribute = match (field.builtin, is_input, stage) {
            (Some(builtin), _, _) => emit::builtin_attribute(builtin, stage, is_input).to_owned(),
            // A vertex input's locations are vertex-descriptor attributes; a
            // fragment output's are colour attachments; everything between is
            // a user-named varying.
            (None, true, Stage::Vertex) => {
                format!("[[attribute({})]]", field.location.unwrap_or(0))
            }
            (None, false, Stage::Fragment) => {
                format!("[[color({})]]", field.location.unwrap_or(0))
            }
            (None, _, _) => format!("[[user(loc{})]]", field.location.unwrap_or(0)),
        };
        let flat = if field.interpolation == Some(kira_shader_model::Interpolation::Flat) {
            " [[flat]]"
        } else {
            ""
        };
        let line = format!(
            "{} {} {attribute}{flat};",
            type_name(&builtin_type(&field.type_name)),
            field.name
        );
        emitter.line(1, &line);
    }
    emitter.line(0, "};");
    emitter.out.push('\n');
    let _ = source;
}

/// The type a reflected type name spells, for the interface path where only
/// the name survived.
fn builtin_type(name: &str) -> kira_shader_model::Type {
    kira_ksl_semantics::builtins::builtin_type(name)
        .unwrap_or_else(|| kira_shader_model::Type::StructRef(name.to_owned()))
}

/// The Metal qualifier a stage's entry point carries.
fn stage_prefix(stage: Stage) -> &'static str {
    match stage {
        Stage::Vertex => "vertex",
        Stage::Fragment => "fragment",
        Stage::Compute => "kernel",
    }
}
