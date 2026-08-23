//! The bytecode compiler: lowers a verified [`IrProgram`] to a [`Module`].
//!
//! Compilation is a direct, stack-oriented translation. Control flow uses
//! absolute jump targets patched after the branch bodies are emitted, and
//! `&&`/`||` compile to short-circuiting jumps. String constants are pooled and
//! deduplicated into the module's string table.
//!
//! Compilation is fallible only for malformed lowered input and internal
//! invariant breaks. Bytecode-owned indexes and counts use the wide format
//! representation, so frontend-valid sizes are not rejected at this layer.

use std::collections::HashMap;

use kira_ir::{IrBinOp, IrProgram, IrUnOp};

mod error;
mod expression;
mod function;
mod widen;

pub use error::CompileError;

use crate::exports::build_export_table;
use crate::module::{FrameRelease, FuncProto, Module};
use crate::op::Instruction;
use kira_runtime_abi::Execution;

/// How the VM lends a borrowed parameter, told to `kira_ir::mid` so it plans
/// releases for the engine that will run this module.
///
/// By value, both kinds, because the VM has no other option: its values live in
/// a frame's slot vector, not at addresses a callee could hold. A `borrow mut`
/// therefore reaches a callee as a copy the callee owns and returns to the
/// caller by writeback, which leaves the slot `Void` — so a plan that releases
/// it frees the copy on the paths with no writeback and does nothing on the
/// paths with one. The native backend answers the same question differently
/// because its calling convention is different, not because it decided
/// separately.
const VM_LENDING: kira_ir::mid::Lending = kira_ir::mid::Lending::BY_VALUE;

/// Compiles a lowered program into a runnable module for the VM.
///
/// Every function is compiled to bytecode, whatever it was annotated with: a
/// VM-only build has no native half, so `@Native` marks a boundary that does
/// not exist in this build. That is what keeps `--backend vm` and
/// `--backend llvm` agreeing on any program, annotated or not.
pub fn compile(program: &IrProgram) -> Result<Module, CompileError> {
    let engines = vec![Execution::Runtime; program.functions.len()];
    compile_with(program, &engines)
}

/// Compiles the bytecode half of a hybrid program.
///
/// `@Native` functions keep their slot — one id has to index both halves — and
/// keep their signature, so a caller knows the arity to marshal, but carry no
/// body: theirs lives in the shared library. Calls to them become `CallNative`,
/// which the VM routes through the embedder.
///
/// A function with no annotation runs on the VM. The VM is the default engine
/// and native is the opt-in, matching what `@Native` means: a boundary you ask
/// for, not one you get by accident.
pub fn compile_hybrid(program: &IrProgram) -> Result<Module, CompileError> {
    let engines: Vec<Execution> = program
        .functions
        .iter()
        .map(|function| function.execution.resolve(Execution::Runtime))
        .collect();
    compile_with(program, &engines)
}

/// Compiles `program` with each function assigned to `engines[index]`.
fn compile_with(program: &IrProgram, engines: &[Execution]) -> Result<Module, CompileError> {
    let mut strings = StringPool::default();
    let mut functions = Vec::with_capacity(program.functions.len());
    let function_count = program.functions.len() as u64;
    let mut widens = widen::WidenHelpers::new(function_count);
    let plans = kira_ir::mid::plan(program, VM_LENDING)?;
    for (index, function) in program.functions.iter().enumerate() {
        let execution = engines.get(index).copied().unwrap_or(Execution::Runtime);
        let param_count = u64::from(function.param_count);
        let local_count = u64::from(function.local_count());
        // A native function's body is not ours to emit; it is compiled into the
        // shared library instead.
        let code = if execution == Execution::Native {
            Vec::new()
        } else {
            let mut compiler = FnCompiler {
                program,
                function,
                function_name: &function.name,
                strings: &mut strings,
                engines,
                code: Vec::new(),
                widens: &mut widens,
                loops: Vec::new(),
            };
            compiler.compile_body(&function.body)?;
            // Safety net: a function that falls off its end returns unit. (The
            // analyzer's definite-return check guarantees non-Void functions
            // never reach this instruction.)
            compiler.code.push(Instruction::ReturnVoid);
            compiler.code
        };
        let releases = match plans.get(index) {
            Some(plan) => frame_release(plan, &function.name)?,
            None => FrameRelease::EveryLocal,
        };
        functions.push(FuncProto {
            name: function.name.clone(),
            param_count,
            local_count,
            execution,
            code,
            releases,
        });
    }
    // The helpers go last, keeping every index a call site was compiled with.
    // Emitting one may register another, so this drains a worklist rather than
    // walking a fixed set.
    widens.emit_pending(program)?;
    for (index, code) in widens.into_protos() {
        debug_assert_eq!(index as usize, functions.len());
        functions.push(FuncProto {
            name: widen::HELPER_NAME.to_owned(),
            // One parameter — the value being carried — and no other local.
            param_count: 1,
            local_count: 1,
            // A helper is bytecode wherever it is called from: the native half
            // of a hybrid build has its own leaf and never calls this one.
            execution: Execution::Runtime,
            code,
            // A helper is synthesized here and has no IR function behind it,
            // so there is nothing for the mid stage to plan from. It gets the
            // frame discipline every module had before plans existed, which is
            // also the one its emitter was written against.
            releases: FrameRelease::EveryLocal,
        });
    }

    let module = Module {
        functions,
        main: program.main,
        strings: strings.into_vec(),
        exports: build_export_table(program)?,
        foreign_imports: program
            .foreign_imports
            .iter()
            .map(|foreign| foreign.import.clone())
            .collect(),
        foreign_aggregates: program.foreign_aggregates.clone(),
        foreign_callbacks: program.foreign_callbacks.clone(),
    };
    // The compiler checks its own output against the rules every loader checks
    // it against. Without this the rules guard the VM's front door and nothing
    // else: a malformed function is written to a `.kbc`, described in a hybrid
    // manifest, and reported — if at all — by whichever engine loads it first,
    // in that engine's vocabulary. `validate` names the function and the rule.
    module.validate()?;
    Ok(module)
}

fn frame_release(
    plan: &kira_ir::mid::ReleasePlan,
    _function: &str,
) -> Result<FrameRelease, CompileError> {
    let mut slots = Vec::with_capacity(plan.len());
    for &slot in plan.slots() {
        slots.push(u64::from(slot));
    }
    Ok(FrameRelease::Planned(slots))
}

/// Deduplicating string constant pool.
#[derive(Default)]
struct StringPool {
    index: HashMap<String, u64>,
    strings: Vec<String>,
}

impl StringPool {
    fn intern(&mut self, value: &str) -> u64 {
        if let Some(&id) = self.index.get(value) {
            return id;
        }
        let id = self.strings.len() as u64;
        self.strings.push(value.to_owned());
        self.index.insert(value.to_owned(), id);
        id
    }

    fn into_vec(self) -> Vec<String> {
        self.strings
    }
}

struct FnCompiler<'a> {
    program: &'a IrProgram,
    /// The function being compiled, for the questions a slot's *type* answers.
    function: &'a kira_ir::IrFunction,
    function_name: &'a str,
    strings: &'a mut StringPool,
    /// Which engine owns each function, so a call site knows which of the two
    /// call instructions it is emitting.
    engines: &'a [Execution],
    code: Vec<Instruction>,
    /// The synthesized widen helpers, shared by every function being compiled.
    ///
    /// Shared rather than per-function because a helper is a module-level
    /// object: two functions widening the same pair call one helper, and its
    /// index has to mean the same thing in both.
    widens: &'a mut widen::WidenHelpers,
    /// The loops enclosing the statement being compiled, innermost last.
    ///
    /// A `break`/`continue` acts on the innermost, so it reads the top of this
    /// stack. Analysis rejects one outside a loop, which is what makes an empty
    /// stack a compiler bug rather than a user error.
    loops: Vec<LoopFrame>,
}

/// Where a `break`/`continue` inside one loop jumps to.
struct LoopFrame {
    /// The address of the loop's condition test — a `continue` jumps here.
    ///
    /// Known when the frame is pushed, so a `continue` needs no patching.
    continue_target: u64,
    /// Placeholder `Jump`s emitted by `break`, patched to the loop's exit once
    /// the body is compiled and that address is known.
    break_jumps: Vec<usize>,
}

fn unary_instruction(op: IrUnOp) -> Instruction {
    match op {
        IrUnOp::NegInt => Instruction::NegInt,
        IrUnOp::NegFloat => Instruction::NegFloat,
        IrUnOp::Not => Instruction::Not,
        IrUnOp::BitNot => Instruction::BitNot,
    }
}

fn binary_instruction(op: IrBinOp) -> Result<Instruction, CompileError> {
    let instruction = match op {
        IrBinOp::AddInt => Instruction::AddInt,
        IrBinOp::SubInt => Instruction::SubInt,
        IrBinOp::MulInt => Instruction::MulInt,
        IrBinOp::DivInt => Instruction::DivInt,
        IrBinOp::RemInt => Instruction::RemInt,
        IrBinOp::DivUInt => Instruction::DivUInt,
        IrBinOp::RemUInt => Instruction::RemUInt,
        IrBinOp::AddFloat => Instruction::AddFloat,
        IrBinOp::SubFloat => Instruction::SubFloat,
        IrBinOp::MulFloat => Instruction::MulFloat,
        IrBinOp::DivFloat => Instruction::DivFloat,
        IrBinOp::RemFloat => Instruction::RemFloat,
        IrBinOp::ConcatStr => Instruction::ConcatStr,
        IrBinOp::EqInt => Instruction::EqInt,
        IrBinOp::NeInt => Instruction::NeInt,
        IrBinOp::LtInt => Instruction::LtInt,
        IrBinOp::LeInt => Instruction::LeInt,
        IrBinOp::GtInt => Instruction::GtInt,
        IrBinOp::GeInt => Instruction::GeInt,
        IrBinOp::LtUInt => Instruction::LtUInt,
        IrBinOp::LeUInt => Instruction::LeUInt,
        IrBinOp::GtUInt => Instruction::GtUInt,
        IrBinOp::GeUInt => Instruction::GeUInt,
        IrBinOp::EqFloat => Instruction::EqFloat,
        IrBinOp::NeFloat => Instruction::NeFloat,
        IrBinOp::LtFloat => Instruction::LtFloat,
        IrBinOp::LeFloat => Instruction::LeFloat,
        IrBinOp::GtFloat => Instruction::GtFloat,
        IrBinOp::GeFloat => Instruction::GeFloat,
        IrBinOp::EqBool => Instruction::EqBool,
        IrBinOp::NeBool => Instruction::NeBool,
        IrBinOp::EqStr => Instruction::EqStr,
        IrBinOp::NeStr => Instruction::NeStr,
        IrBinOp::EqAny => Instruction::EqAny,
        IrBinOp::NeAny => Instruction::NeAny,
        IrBinOp::BitAnd => Instruction::BitAnd,
        IrBinOp::BitOr => Instruction::BitOr,
        IrBinOp::BitXor => Instruction::BitXor,
        IrBinOp::Shl => Instruction::Shl,
        IrBinOp::ShrInt => Instruction::ShrInt,
        IrBinOp::ShrUInt => Instruction::ShrUInt,
        // Short-circuit operators are compiled as control flow, never as a
        // single opcode; reaching here is a compiler bug surfaced typed.
        IrBinOp::And | IrBinOp::Or => return Err(CompileError::ShortCircuitOpcode),
    };
    Ok(instruction)
}
