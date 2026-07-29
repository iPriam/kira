//! The bytecode compiler: lowers a verified [`IrProgram`] to a [`Module`].
//!
//! Compilation is a direct, stack-oriented translation. Control flow uses
//! absolute jump targets patched after the branch bodies are emitted, and
//! `&&`/`||` compile to short-circuiting jumps. String constants are pooled and
//! deduplicated into the module's string table.
//!
//! Compilation is fallible: counts that exceed the bytecode format's operand
//! widths (u16 local slots, u32 string pool) and internal invariant breaks
//! surface as a typed [`CompileError`] — never a panic.

use std::collections::HashMap;

use kira_ir::{IrBinOp, IrProgram, IrUnOp};

mod error;
mod expression;
mod function;
mod widen;

pub use error::CompileError;

use crate::exports::build_export_table;
use crate::module::{FuncProto, Module};
use crate::op::Instruction;
use kira_runtime_abi::Execution;

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
    let function_count =
        u32::try_from(program.functions.len()).map_err(|_| CompileError::TooManyFunctions {
            count: program.functions.len(),
        })?;
    let mut widens = widen::WidenHelpers::new(function_count);
    for (index, function) in program.functions.iter().enumerate() {
        let execution = engines.get(index).copied().unwrap_or(Execution::Runtime);
        let param_count =
            u16::try_from(function.param_count).map_err(|_| CompileError::TooManyLocals {
                function: function.name.clone(),
                count: function.param_count,
            })?;
        let local_count =
            u16::try_from(function.local_count()).map_err(|_| CompileError::TooManyLocals {
                function: function.name.clone(),
                count: function.local_count(),
            })?;
        // A native function's body is not ours to emit; it is compiled into the
        // shared library instead.
        let code = if execution == Execution::Native {
            Vec::new()
        } else {
            let mut compiler = FnCompiler {
                program,
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
        functions.push(FuncProto {
            name: function.name.clone(),
            param_count,
            local_count,
            execution,
            code,
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
        });
    }

    Ok(Module {
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
    })
}

/// Deduplicating string constant pool.
#[derive(Default)]
struct StringPool {
    index: HashMap<String, u32>,
    strings: Vec<String>,
}

impl StringPool {
    fn intern(&mut self, value: &str) -> Result<u32, CompileError> {
        if let Some(&id) = self.index.get(value) {
            return Ok(id);
        }
        let id = u32::try_from(self.strings.len()).map_err(|_| CompileError::TooManyStrings)?;
        self.strings.push(value.to_owned());
        self.index.insert(value.to_owned(), id);
        Ok(id)
    }

    fn into_vec(self) -> Vec<String> {
        self.strings
    }
}

struct FnCompiler<'a> {
    program: &'a IrProgram,
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
    continue_target: u32,
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
