//! Lowers shader IR to GLSL 430.
//!
//! Layer 4 of the Kira package graph.
//!
//! Emits **one module per stage**, like WGSL and unlike Metal, because OpenGL
//! compiles and attaches a vertex and a fragment shader separately.
//!
//! 430 rather than 330 because that is the version with the features KSL
//! actually uses: compute stages and shader storage buffers both arrived in it,
//! and the corpus builds its GPU simulation steps on storage buffers. At 330
//! this was the one target that could not express every shader, and a shader it
//! refused reached the driver as an empty string.
//!
//! Two dialect facts shape the rest. GLSL has no standalone sampler object,
//! so a texture and the sampler that reads it collapse into one `sampler2D`
//! uniform and the sampler argument disappears at the call. And a stage's
//! interface is loose `in`/`out` variables rather than a struct, so the entry
//! point builds its input struct from them on the way in and copies its output
//! struct back out on the way out.

mod emit;
#[cfg(test)]
mod tests;

use kira_ksl_semantics::model::CheckedStage;
use kira_shader_ir::ShaderIr;
use kira_shader_model::Stage;

pub use emit::type_name;

/// Emits the GLSL module for one stage of `ir`.
///
/// `""` when the shader has no such stage, which is not an error: a shader may
/// declare only a vertex stage.
pub fn emit(ir: &ShaderIr, stage: Stage) -> String {
    let (Some(reflection), Some(shader)) = (&ir.reflection, &ir.module.shader) else {
        return String::new();
    };
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
        return String::new();
    };

    let mut emitter = emit::Emitter {
        module: &ir.module,
        reflection,
        out: String::new(),
        stage,
        samplers: Vec::new(),
        images: Vec::new(),
        entry_outputs: None,
    };
    emitter.line(0, "#version 430 core");
    emitter.out.push('\n');

    // GL measures a fragment's window position from the framebuffer's LOWER
    // left; every other target KSL emits for measures it from the upper left.
    // A shader that reads `@builtin(position)` in its fragment stage would
    // therefore mean two different pixels in one source file, which is what
    // `layout(origin_upper_left)` settles — the redeclaration is core GLSL
    // since 1.50 and costs nothing where the builtin is never read.
    if stage == Stage::Fragment {
        emitter.line(0, "layout(origin_upper_left) in vec4 gl_FragCoord;");
        emitter.out.push('\n');
    }

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

    emit_structs(&mut emitter);
    emitter.resources();
    // A compute stage declares its workgroup size instead of an interface: its
    // inputs are builtins, which are already in scope.
    if stage == Stage::Compute {
        let [x, y, z] = reflected.threads.unwrap_or([1, 1, 1]);
        let line =
            format!("layout(local_size_x = {x}, local_size_y = {y}, local_size_z = {z}) in;");
        emitter.line(0, &line);
        emitter.out.push('\n');
    } else {
        emit_interface(&mut emitter, reflected, stage);
    }

    for function in &ir.module.functions {
        emitter.function(function);
    }
    for helper in &checked.helpers {
        emitter.function(helper);
    }
    emit_entry(&mut emitter, checked, reflected, stage);
    emitter.out
}

/// Emits every struct, interfaces and uniform types included — GLSL keeps them
/// as ordinary types because the stage's own interface is loose variables
/// instead, and a uniform is a struct-typed uniform rather than a block.
///
/// A **uniform** struct's unsigned members are emitted signed, which is the one
/// place this backend narrows a type. GL loads a uniform by name through one
/// call per type, and the host this emits for reaches an integral uniform
/// through `glUniform*iv` — handing an `int` array to a `uint` uniform is a
/// type mismatch GL refuses outright, so an unsigned member declared as `uint`
/// is a uniform nothing can ever write. The bits round-trip exactly, because
/// GLSL's implicit `int` to `uint` conversion is a reinterpretation and every
/// read of the member is in unsigned context; only reading one **above** 2^31
/// as a float differs, and that is a count no extent or index reaches.
fn emit_structs(emitter: &mut emit::Emitter<'_>) {
    let uniform_types: Vec<String> = emitter
        .reflection
        .resources
        .iter()
        .filter(|resource| resource.resource_kind == kira_shader_model::ResourceKind::Uniform)
        .map(|resource| resource.type_name.clone())
        .collect();
    for declared in &emitter.module.structs.clone() {
        let is_uniform = uniform_types.contains(&declared.name);
        let opened = format!("struct {} {{", declared.name);
        emitter.line(0, &opened);
        for field in &declared.fields {
            let ty = if is_uniform {
                type_name(&signed_for_uniform(&field.ty))
            } else {
                type_name(&field.ty)
            };
            let line = format!("{ty} {};", field.name);
            emitter.line(1, &line);
        }
        emitter.line(0, "};");
        emitter.out.push('\n');
    }
}

/// `ty` with every unsigned scalar made signed, for a uniform member.
fn signed_for_uniform(ty: &kira_shader_model::Type) -> kira_shader_model::Type {
    use kira_shader_model::{ScalarType, Type};
    match ty {
        Type::Scalar(ScalarType::Uint) => Type::Scalar(ScalarType::Int),
        Type::Vector(vector) if vector.scalar == ScalarType::Uint => {
            Type::Vector(kira_shader_model::VectorType {
                scalar: ScalarType::Int,
                width: vector.width,
            })
        }
        other => other.clone(),
    }
}

/// Emits the stage's loose `in` and `out` variables.
///
/// A varying is named `v_<field>` in both stages so the vertex output and the
/// fragment input link by name, which is how GLSL matches them.
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
        let param_name = emit::safe_name(&param.name);
        let declared = format!("{name} {param_name};");
        emitter.line(1, &declared);
        for field in &reflected.inputs {
            let source = match (field.builtin, stage) {
                (Some(builtin), _) => emit::builtin_name(builtin, stage).to_owned(),
                (None, Stage::Vertex) => field.name.clone(),
                (None, _) => format!("v_{}", field.name),
            };
            let line = format!("{param_name}.{} = {source};", field.name);
            emitter.line(1, &line);
        }
    }

    // The body, with every `return value` becoming the copy-out: `main` returns
    // nothing and the outputs are variables rather than a value. Handled by the
    // statement emitter rather than here, so a `return` inside an `if` is
    // rewritten too.
    emitter.entry_outputs = Some(emit::EntryOutputs {
        fields: reflected.outputs.clone(),
        stage,
    });
    for &id in &checked.entry.body {
        emitter.stmt(id, 1);
    }
    emitter.entry_outputs = None;
    emitter.line(0, "}");
}
