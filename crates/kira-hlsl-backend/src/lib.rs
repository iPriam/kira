//! Lowers shader IR to HLSL.
//!
//! Layer 4 of the Kira package graph.
//!
//! Emits **one module per stage**, like WGSL and GLSL and unlike Metal: D3D
//! compiles a shader by naming one entry point, and a vertex and a pixel shader
//! are two compilations. That is why the artifact carries `vertexHlsl` and
//! `fragmentHlsl` rather than one combined source.
//!
//! Three dialect facts shape everything below.
//!
//! A stage's interface is a struct whose members carry **semantics** —
//! `TEXCOORDn` for a varying, `SV_Position` and friends for a builtin,
//! `SV_Targetn` for a colour output. So an interface struct is emitted a second
//! time under this stage's name with this stage's semantics on it, because the
//! same KSL struct is a vertex output in one module and a fragment input in
//! another and the two spellings differ.
//!
//! HLSL stores a matrix by rows and this workspace lays one out by columns, so
//! every matrix declaration carries `column_major`. Without it the shader reads
//! the host's bytes transposed — a wrong image, never a compile error, which is
//! the worst way for a disagreement like that to show up.
//!
//! And two things this IR says as expressions are statements in HLSL: an atomic
//! answers through an `out` parameter (`InterlockedAdd`) and so does a buffer's
//! length (`GetDimensions`). Both are hoisted into a temporary declared just
//! before the statement that wanted them. A loop condition is the one place
//! that cannot work, because the condition is re-evaluated every iteration and
//! a hoisted temporary would not be — so a shader that writes one there is
//! refused by name rather than emitted as a loop reading a stale value.

mod emit;
#[cfg(test)]
mod tests;

use kira_ksl_semantics::model::{
    BuiltinFn, CheckedExprId, CheckedExprKind, CheckedModule, CheckedShader, CheckedStage,
    CheckedStmt, CheckedStmtId,
};
use kira_shader_ir::ShaderIr;
use kira_shader_model::{ReflectedField, ReflectedStage, Stage};

pub use emit::{hlsl_name, type_name};

/// Why a shader could not be emitted as HLSL.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HlslError {
    /// A loop condition reads something HLSL can only say as a statement.
    #[error(
        "`{shader}` calls `{call}` in a loop condition, which HLSL cannot express: it answers \
         through an `out` parameter, so it has to run as a statement before the condition — and a \
         condition is re-evaluated every iteration while a statement before the loop is not"
    )]
    StatementOnlyInLoopCondition {
        /// The shader's name.
        shader: String,
        /// What was called there, in KSL's spelling.
        call: String,
    },
}

/// Emits the HLSL module for one stage of `ir`.
///
/// `Ok("")` when the shader has no such stage, which is not an error: a shader
/// may declare only a compute stage.
pub fn emit(ir: &ShaderIr, stage: Stage) -> Result<String, HlslError> {
    let (Some(reflection), Some(shader)) = (&ir.reflection, &ir.module.shader) else {
        return Ok(String::new());
    };
    if let Some(refusal) = refuse(&ir.module, shader) {
        return Err(refusal);
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

    let mut emitter = emit::Emitter::new(&ir.module, reflection);

    for option in &shader.options {
        let value = match option.value {
            kira_ksl_semantics::model::ConstValue::Bool(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Int(value) => value.to_string(),
            kira_ksl_semantics::model::ConstValue::Uint(value) => format!("{value}u"),
            kira_ksl_semantics::model::ConstValue::Float(value) => format!("{value:?}"),
        };
        let line = format!(
            "static const {} {} = {value};",
            type_name(&option.ty),
            option.name
        );
        emitter.line(0, &line);
    }
    if !shader.options.is_empty() {
        emitter.out.push('\n');
    }

    emit_structs(&mut emitter);
    emitter.resources();
    emit_interfaces(&mut emitter, reflected, stage);

    for function in &ir.module.functions {
        emitter.function(function);
    }
    for helper in &checked.helpers {
        emitter.function(helper);
    }
    emit_entry(&mut emitter, checked, reflected, stage);
    Ok(emitter.out)
}

/// The shader's refusal, when it writes a statement-only call where HLSL cannot
/// hoist it.
fn refuse(module: &CheckedModule, shader: &CheckedShader) -> Option<HlslError> {
    let bodies = shader
        .stages
        .iter()
        .flat_map(|stage| std::iter::once(&stage.entry).chain(&stage.helpers))
        .chain(&module.functions);
    for function in bodies {
        if let Some(call) = statement_only_in_loop(module, &function.body) {
            return Some(HlslError::StatementOnlyInLoopCondition {
                shader: shader.name.clone(),
                call,
            });
        }
    }
    None
}

/// The statement-only call a loop condition inside `body` reads, if one does.
fn statement_only_in_loop(module: &CheckedModule, body: &[CheckedStmtId]) -> Option<String> {
    body.iter().find_map(|&id| match module.stmt(id) {
        CheckedStmt::While { cond, body } => {
            statement_only(module, *cond).or_else(|| statement_only_in_loop(module, body))
        }
        CheckedStmt::If {
            then, otherwise, ..
        } => statement_only_in_loop(module, then).or_else(|| {
            otherwise
                .as_ref()
                .and_then(|body| statement_only_in_loop(module, body))
        }),
        _ => None,
    })
}

/// The statement-only call `id` or anything under it makes.
fn statement_only(module: &CheckedModule, id: CheckedExprId) -> Option<String> {
    let node = module.expr(id);
    match &node.kind {
        CheckedExprKind::ArrayLength { .. } => Some("length".to_owned()),
        CheckedExprKind::Builtin {
            which: BuiltinFn::AtomicAdd,
            ..
        } => Some("atomicAdd".to_owned()),
        CheckedExprKind::Field { base, .. } | CheckedExprKind::Swizzle { base, .. } => {
            statement_only(module, *base)
        }
        CheckedExprKind::Cast { value } | CheckedExprKind::Unary { operand: value, .. } => {
            statement_only(module, *value)
        }
        CheckedExprKind::Index { base, index } => {
            statement_only(module, *base).or_else(|| statement_only(module, *index))
        }
        CheckedExprKind::Binary { lhs, rhs, .. } => {
            statement_only(module, *lhs).or_else(|| statement_only(module, *rhs))
        }
        CheckedExprKind::Construct { args }
        | CheckedExprKind::Call { args, .. }
        | CheckedExprKind::Builtin { args, .. } => {
            args.iter().find_map(|&arg| statement_only(module, arg))
        }
        CheckedExprKind::Const(_)
        | CheckedExprKind::Local(_)
        | CheckedExprKind::Option(_)
        | CheckedExprKind::Resource(_)
        | CheckedExprKind::Invalid => None,
    }
}

/// Emits every struct the module declares, in declaration order.
///
/// Interfaces included: the decorated copies are emitted beside these under
/// their own names, never instead of them, because a body that builds a value
/// of one still needs the plain type.
fn emit_structs(emitter: &mut emit::Emitter<'_>) {
    for declared in &emitter.module.structs.clone() {
        let opened = format!("struct {} {{", declared.name);
        emitter.line(0, &opened);
        for field in &declared.fields {
            let line = format!(
                "{}{} {};",
                emit::matrix_storage(&field.ty),
                type_name(&field.ty),
                field.name
            );
            emitter.line(1, &line);
        }
        emitter.line(0, "};");
        emitter.out.push('\n');
    }
}

/// Emits this stage's decorated copies of its interface structs.
///
/// A compute stage takes its builtins as loose parameters, so its input needs
/// no decorated copy — decorating it would name the same builtin twice.
fn emit_interfaces(emitter: &mut emit::Emitter<'_>, reflected: &ReflectedStage, stage: Stage) {
    emitter.renames.clear();
    if let Some(name) = &reflected.input_type
        && stage != Stage::Compute
    {
        let spelled = decorated_name(stage, name, true);
        emit_interface(emitter, &spelled, &reflected.inputs, stage, true);
        emitter.renames.insert(name.clone(), spelled);
    }
    if let Some(name) = &reflected.output_type {
        let spelled = decorated_name(stage, name, false);
        emit_interface(emitter, &spelled, &reflected.outputs, stage, false);
        emitter.renames.insert(name.clone(), spelled);
    }
}

/// The name this stage's copy of an interface struct takes.
fn decorated_name(stage: Stage, name: &str, is_input: bool) -> String {
    let prefix = match stage {
        Stage::Vertex => "vs",
        Stage::Fragment => "ps",
        Stage::Compute => "cs",
    };
    let role = if is_input { "in" } else { "out" };
    format!("{prefix}_{name}_{role}")
}

/// Emits one decorated copy, every member carrying its semantic.
fn emit_interface(
    emitter: &mut emit::Emitter<'_>,
    spelled: &str,
    fields: &[ReflectedField],
    stage: Stage,
    is_input: bool,
) {
    let opened = format!("struct {spelled} {{");
    emitter.line(0, &opened);
    for field in fields {
        let semantic = match (field.builtin, is_input, stage) {
            (Some(builtin), _, _) => emit::semantic(builtin).to_owned(),
            // A fragment output is a colour attachment; everything else that
            // carries a location is a varying, and `TEXCOORDn` is the semantic
            // a host's input layout and the linker both match on.
            (None, false, Stage::Fragment) => format!("SV_Target{}", field.location.unwrap_or(0)),
            (None, _, _) => format!("TEXCOORD{}", field.location.unwrap_or(0)),
        };
        let line = format!(
            "{}{} {} : {semantic};",
            emit::interpolation_modifier(field.interpolation),
            hlsl_name(&field.type_name),
            field.name
        );
        emitter.line(1, &line);
    }
    emitter.line(0, "};");
    emitter.out.push('\n');
}

/// Emits the stage's entry point.
///
/// A graphics stage takes its whole interface struct, because the semantics
/// live on the struct's members and HLSL passes it as one value. A compute
/// stage takes its builtins loose and rebuilds the struct the body reads, the
/// way every other backend here does — HLSL has no interface struct for a
/// kernel that a workgroup builtin can sit in.
fn emit_entry(
    emitter: &mut emit::Emitter<'_>,
    checked: &CheckedStage,
    reflected: &ReflectedStage,
    stage: Stage,
) {
    let params = match (stage, checked.entry.params.first(), &reflected.input_type) {
        (Stage::Compute, _, _) => reflected
            .inputs
            .iter()
            .filter_map(|field| {
                let builtin = field.builtin?;
                Some(format!(
                    "{} {} : {}",
                    hlsl_name(&field.type_name),
                    field.name,
                    emit::semantic(builtin)
                ))
            })
            .collect::<Vec<_>>()
            .join(", "),
        (_, Some(param), Some(name)) => {
            format!("{} {}", decorated_name(stage, name, true), param.name)
        }
        _ => String::new(),
    };
    let result = reflected.output_type.as_ref().map_or_else(
        || "void".to_owned(),
        |name| decorated_name(stage, name, false),
    );

    emitter
        .out
        .push_str(&emit::threads_attribute(stage, reflected.threads));
    let signature = format!("{result} {}({params}) {{", reflected.entry_name);
    emitter.line(0, &signature);

    // A kernel's input struct is not a parameter, so the entry rebuilds it from
    // the loose builtins before the body reads it.
    if stage == Stage::Compute
        && let (Some(param), Some(name)) = (checked.entry.params.first(), &reflected.input_type)
    {
        let declared = format!("{name} {} = ({name})0;", param.name);
        emitter.line(1, &declared);
        for field in &reflected.inputs {
            if field.builtin.is_none() {
                continue;
            }
            let line = format!("{}.{} = {};", param.name, field.name, field.name);
            emitter.line(1, &line);
        }
    }
    emitter.body(&checked.entry.body, 1);
    emitter.line(0, "}");
}
