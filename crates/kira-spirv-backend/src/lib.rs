//! Lowers shader IR to SPIR-V.
//!
//! Layer 4 of the Kira package graph.
//!
//! Emits **one module per stage** and, alone among the backends here, emits
//! binary rather than source: SPIR-V is the format Vulkan consumes, so the
//! output is a word stream and not a string a driver would have to parse.
//!
//! # What a stage's module holds
//!
//! A SPIR-V entry point returns nothing and takes nothing. A stage's interface
//! is module-scope `Input` and `Output` variables instead, one per interface
//! member, carrying either a `Location` or a `BuiltIn` decoration. So the entry
//! reads every input variable and composes the struct the KSL body was written
//! against, and takes the returned struct apart into the output variables on the
//! way out — the same marshalling the GLSL backend does with loose `in`/`out`
//! variables, for the same reason.
//!
//! Every local is a function-scope variable, loaded and stored through. KSL
//! rebinds a `let` freely, so the alternative is building phi nodes at every
//! join; a driver's optimizer undoes this in its first pass.
//!
//! # Layout is said out loud
//!
//! A buffer-facing struct carries an explicit `Offset` on every member, a
//! matrix carries `ColMajor` and a `MatrixStride`, and a runtime array carries
//! an `ArrayStride` — all of them this workspace's own layout rather than
//! anything SPIR-V would default to. A driver reads what these say, so a module
//! that left them out would be rejected, and one that got them wrong would read
//! the host's bytes at the wrong offsets and show it as a wrong image.
//!
//! That is also why a struct reaches the module twice when a buffer holds it:
//! `Offset` is illegal on a struct in the function storage class and `Block` is
//! illegal outside a buffer, so the local's form and the buffer's form cannot be
//! one type.

mod builder;
mod entry;
mod lower;
mod resources;
mod spec;
#[cfg(test)]
mod tests;
mod types;

use std::collections::HashMap;

use kira_ksl_semantics::model::{CheckedFunction, CheckedModule, CheckedStmtId, ConstValue};
use kira_shader_ir::ShaderIr;
use kira_shader_model::{Builtin, ScalarType, Stage, Type};

use crate::builder::{Builder, Id};
use crate::spec::{built_in, execution_mode, execution_model};

/// Why a shader could not be emitted as SPIR-V.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SpirvError {
    /// A compute stage declares an output interface.
    #[error(
        "`{shader}`'s compute stage declares the output `{output}`, which SPIR-V cannot express: \
         a `GLCompute` entry point has no output variables, and a compute shader publishes through \
         the buffers it binds instead"
    )]
    ComputeStageWithOutput {
        /// The shader's name.
        shader: String,
        /// The output interface it declared.
        output: String,
    },
}

/// A pointer and what it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) pointer: Id,
    pub(crate) ty: Type,
    pub(crate) storage: u32,
}

/// A module-scope variable a resource binds to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Global {
    /// A uniform block, whose members are the struct's own.
    Uniform { pointer: Id, name: String },
    /// A storage block, whose one member is the runtime array.
    Storage { pointer: Id, element: Type },
    /// A texture or a sampler, which is a handle rather than memory.
    Handle { pointer: Id, ty: Type },
}

/// The module being emitted.
pub(crate) struct Emitter<'a> {
    pub(crate) module: &'a CheckedModule,
    pub(crate) builder: Builder,
    /// Undecorated struct types, by KSL name.
    pub(crate) structs: HashMap<String, Id>,
    /// Buffer-facing struct types, by KSL name.
    pub(crate) laid_out: HashMap<String, Id>,
    /// The stride a runtime array of one element type steps by.
    pub(crate) strides: HashMap<Id, u32>,
    /// Every resource's variable, by the name the shader binds it under.
    pub(crate) globals: HashMap<String, Global>,
    /// The value each compile-time option was declared with.
    pub(crate) options: HashMap<String, ConstValue>,
    /// Every function a body may call, by name.
    pub(crate) callable: HashMap<String, Id>,
    /// The variable each `let` in the function being emitted was given.
    pub(crate) declared: HashMap<CheckedStmtId, Id>,
    /// The locals in scope, innermost last.
    pub(crate) scopes: Vec<HashMap<String, Place>>,
    /// While the entry point is being emitted, the output variable and member
    /// type of each field its returned struct is taken apart into.
    pub(crate) entry_outputs: Option<Vec<(Id, Id)>>,
    /// Whether the block being written has already ended.
    pub(crate) terminated: bool,
}

/// A module rendered as hexadecimal, eight characters per word.
///
/// The shader artifact a `ksl!` macro builds carries every backend's output as
/// a Kira string, because that is what a macro splices into generated source —
/// and this is the one backend whose output is not text. Eight characters per
/// word rather than a byte stream, because `vkCreateShaderModule` takes
/// `uint32_t*`: a host reads the string eight at a time straight into the array
/// it passes, with no byte order to get backwards on the way.
///
/// Empty in, empty out — a stage the shader does not declare carries an empty
/// string like every other backend's absent stage.
#[must_use]
pub fn hex(words: &[u32]) -> String {
    let mut text = String::with_capacity(words.len() * 8);
    for word in words {
        text.push_str(&format!("{word:08x}"));
    }
    text
}

/// Emits the SPIR-V module for one stage of `ir`.
///
/// An empty stream when the shader has no such stage, which is not an error: a
/// shader may declare only a compute stage.
pub fn emit(ir: &ShaderIr, stage: Stage) -> Result<Vec<u32>, SpirvError> {
    let (Some(reflection), Some(shader)) = (&ir.reflection, &ir.module.shader) else {
        return Ok(Vec::new());
    };
    for candidate in &shader.stages {
        if candidate.stage == Stage::Compute
            && let Some(output) = &candidate.output
        {
            return Err(SpirvError::ComputeStageWithOutput {
                shader: shader.name.clone(),
                output: output.clone(),
            });
        }
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
        return Ok(Vec::new());
    };

    let mut emitter = Emitter {
        module: &ir.module,
        builder: Builder::new(),
        structs: HashMap::new(),
        laid_out: HashMap::new(),
        strides: HashMap::new(),
        globals: HashMap::new(),
        options: shader
            .options
            .iter()
            .map(|option| (option.name.clone(), option.value))
            .collect(),
        callable: HashMap::new(),
        declared: HashMap::new(),
        scopes: Vec::new(),
        entry_outputs: None,
        terminated: false,
    };

    emitter.resources(reflection, shader);
    let inputs = emitter.interface(reflected, stage, true);
    let outputs = emitter.interface(reflected, stage, false);

    // Every callee's id is handed out before any body is written, because a
    // body may call a function declared after it.
    let helpers: Vec<&CheckedFunction> =
        ir.module.functions.iter().chain(&checked.helpers).collect();
    for function in &helpers {
        let id = emitter.builder.fresh();
        emitter.builder.name(id, &function.name);
        emitter.callable.insert(function.name.clone(), id);
    }
    for function in &helpers {
        let Some(&id) = emitter.callable.get(&function.name) else {
            continue;
        };
        emitter.function(function, id);
    }

    let entry = emitter.builder.fresh();
    emitter.builder.name(entry, &reflected.entry_name);
    let mut interface: Vec<Id> = inputs.iter().map(|(_, id, _)| *id).collect();
    interface.extend(outputs.iter().map(|(_, id, _)| *id));
    emitter
        .builder
        .entry_point(model(stage), entry, &reflected.entry_name, &interface);
    match stage {
        // A Vulkan framebuffer's origin is the top left, and a fragment shader
        // that does not say so reads `SV_Position`'s y upside down.
        Stage::Fragment => {
            emitter
                .builder
                .execution_mode(entry, execution_mode::ORIGIN_UPPER_LEFT, &[]);
        }
        Stage::Compute => {
            let [x, y, z] = reflected.threads.unwrap_or([1, 1, 1]);
            emitter
                .builder
                .execution_mode(entry, execution_mode::LOCAL_SIZE, &[x, y, z]);
        }
        Stage::Vertex => {}
    }
    emitter.entry(entry, checked, reflected, &inputs, &outputs);
    Ok(emitter.builder.finish())
}

/// The execution model a stage runs under.
fn model(stage: Stage) -> u32 {
    match stage {
        Stage::Vertex => execution_model::VERTEX,
        Stage::Fragment => execution_model::FRAGMENT,
        Stage::Compute => execution_model::GL_COMPUTE,
    }
}

/// One interface member: its KSL type, its variable, and its name.
type Interface = (Type, Id, String);

/// What a resource binds, before its variable exists.
enum Bound {
    /// A uniform block over the named struct.
    Uniform(String),
    /// A storage block over a runtime array of this element.
    Storage(Type),
    /// A texture or a sampler.
    Handle(Type),
}

/// The `BuiltIn` decoration a builtin takes in `stage`.
///
/// The stage is not decoration: `@builtin(position)` is one KSL annotation and
/// two SPIR-V builtins. It is `Position`, the clip-space position a vertex
/// stage *writes*, and `FragCoord`, the window-space coordinate a fragment
/// stage *reads* — and Vulkan rejects a fragment shader that names the first.
/// Metal spells both `[[position]]` and WGSL spells both
/// `@builtin(position)`, which is why the distinction only surfaces here.
fn built_in_of(builtin: Builtin, stage: Stage) -> u32 {
    match builtin {
        Builtin::Position if stage == Stage::Fragment => built_in::FRAG_COORD,
        Builtin::Position => built_in::POSITION,
        Builtin::VertexIndex => built_in::VERTEX_INDEX,
        Builtin::InstanceIndex => built_in::INSTANCE_INDEX,
        Builtin::FrontFacing => built_in::FRONT_FACING,
        Builtin::FragCoord => built_in::FRAG_COORD,
        Builtin::ThreadId => built_in::GLOBAL_INVOCATION_ID,
        Builtin::LocalId => built_in::LOCAL_INVOCATION_ID,
        Builtin::GroupId => built_in::WORKGROUP_ID,
        Builtin::LocalIndex => built_in::LOCAL_INVOCATION_INDEX,
    }
}

/// The scalar a type is made of, when it is made of one.
fn scalar_of(ty: &Type) -> Option<ScalarType> {
    match ty {
        Type::Scalar(scalar) => Some(*scalar),
        Type::Vector(vector) => Some(vector.scalar),
        _ => None,
    }
}
