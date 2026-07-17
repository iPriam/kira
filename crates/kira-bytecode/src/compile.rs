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

use kira_ir::{
    IrBinOp, IrCallee, IrExpr, IrExprId, IrPlace, IrPlaceStep, IrProgram, IrStmt, IrUnOp,
};

use crate::module::{FuncProto, Module};
use crate::op::{FieldPath, Instruction, PathStep, PlacePath};
use kira_runtime_abi::Execution;

/// An error raised while lowering IR to bytecode.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompileError {
    /// A function needs more local slots than the format's `u16` can address.
    #[error("function `{function}` needs {count} local slots; the bytecode format allows 65535")]
    TooManyLocals {
        /// The offending function's name.
        function: String,
        /// The requested number of local slots.
        count: u32,
    },
    /// An IR expression referenced a local slot beyond the `u16` range.
    #[error("function `{function}` references local slot {slot}, beyond the format's 65535")]
    LocalSlotOutOfRange {
        /// The offending function's name.
        function: String,
        /// The out-of-range slot index.
        slot: u32,
    },
    /// The program has more distinct string constants than the pool can index.
    #[error("program has too many distinct string constants for the bytecode format")]
    TooManyStrings,
    /// Internal invariant: a jump patch landed on a non-jump instruction.
    #[error("bytecode compiler invariant violated: patch target is not a jump")]
    PatchedNonJump,
    /// Internal invariant: a short-circuit operator reached opcode selection.
    #[error("bytecode compiler invariant violated: short-circuit operator has no opcode")]
    ShortCircuitOpcode,
    /// Internal invariant: a `break`/`continue` reached codegen with no
    /// enclosing loop, which analysis is supposed to have rejected.
    #[error(
        "bytecode compiler invariant violated: `break`/`continue` outside a loop in `{function}`"
    )]
    JumpOutsideLoop {
        /// The offending function's name.
        function: String,
    },
    /// A struct has more fields than the format's `u16` operand can count.
    #[error("function `{function}` builds a struct of {count} fields; the format allows 65535")]
    TooManyFields {
        /// The offending function's name.
        function: String,
        /// The requested number of fields.
        count: usize,
    },
    /// A nested field assignment walks deeper than the format can encode.
    #[error("function `{function}` assigns through {count} nested fields; the format allows 65535")]
    FieldPathTooDeep {
        /// The offending function's name.
        function: String,
        /// The requested path depth.
        count: usize,
    },
    /// An array literal has more elements than the format's `u32` can count.
    #[error("function `{function}` builds an array of {count} elements; the format allows 2^32-1")]
    TooManyElements {
        /// The offending function's name.
        function: String,
        /// The requested number of elements.
        count: usize,
    },
    /// Internal invariant: a place with an array index reached the static
    /// field-path encoder, which cannot express one.
    #[error(
        "bytecode compiler invariant violated: dynamic index in a static field path in `{function}`"
    )]
    DynamicFieldPath {
        /// The offending function's name.
        function: String,
    },
}

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
    Ok(Module {
        functions,
        main: program.main,
        strings: strings.into_vec(),
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

impl FnCompiler<'_> {
    fn compile_body(&mut self, stmts: &[IrStmt]) -> Result<(), CompileError> {
        for stmt in stmts {
            self.compile_stmt(stmt)?;
        }
        Ok(())
    }

    fn compile_stmt(&mut self, stmt: &IrStmt) -> Result<(), CompileError> {
        match stmt {
            IrStmt::Let { local, init } => {
                self.compile_expr(*init)?;
                let slot = self.local_slot(*local)?;
                self.code.push(Instruction::StoreLocal(slot));
            }
            IrStmt::Assign { place, value } => {
                let slot = self.local_slot(place.local)?;
                if place.path.is_empty() {
                    self.compile_expr(*value)?;
                    self.code.push(Instruction::StoreLocal(slot));
                } else if place.is_all_fields() {
                    // The shape that predates arrays keeps the encoding it
                    // already had: a static field path needs nothing on the
                    // stack but the value.
                    self.compile_expr(*value)?;
                    let path = self.field_path(place)?;
                    self.code.push(Instruction::StoreField { slot, path });
                } else {
                    // Indices first, outermost to innermost, then the value.
                    // That order is the IR's contract — see `IrPlace` — and
                    // every backend follows it.
                    let path = self.compile_place_indices(place)?;
                    self.compile_expr(*value)?;
                    self.code.push(Instruction::StorePlace { slot, path });
                }
            }
            IrStmt::Return { value } => match value {
                Some(expr) => {
                    self.compile_expr(*expr)?;
                    self.code.push(Instruction::Return);
                }
                None => self.code.push(Instruction::ReturnVoid),
            },
            IrStmt::Eval { expr } => {
                self.compile_expr(*expr)?;
                // Discard the result of an expression evaluated for effect.
                self.code.push(Instruction::Pop);
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => self.compile_if(*cond, then_body, else_body)?,
            IrStmt::While { cond, body } => self.compile_while(*cond, body)?,
            IrStmt::Break => {
                let placeholder = self.emit_placeholder_jump();
                self.innermost_loop()?.break_jumps.push(placeholder);
            }
            IrStmt::Continue => {
                let target = self.innermost_loop()?.continue_target;
                self.code.push(Instruction::Jump(target));
            }
        }
        Ok(())
    }

    /// The innermost enclosing loop's frame.
    ///
    /// Analysis rejects a `break`/`continue` outside a loop, so reaching this
    /// with an empty stack means the frontend let one through — a typed error
    /// rather than a panic, because a compiler never gets to end its caller.
    fn innermost_loop(&mut self) -> Result<&mut LoopFrame, CompileError> {
        let function = self.function_name;
        self.loops
            .last_mut()
            .ok_or_else(|| CompileError::JumpOutsideLoop {
                function: function.to_owned(),
            })
    }

    fn compile_if(
        &mut self,
        cond: IrExprId,
        then_body: &[IrStmt],
        else_body: &[IrStmt],
    ) -> Result<(), CompileError> {
        self.compile_expr(cond)?;
        let to_else = self.emit_placeholder_jump_if_false();
        self.compile_body(then_body)?;
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_else)?;
        self.compile_body(else_body)?;
        self.patch_to_here(to_end)
    }

    /// Compiles a loop, resolving any `break`/`continue` inside it.
    ///
    /// `continue` targets the condition test rather than the body, so a
    /// `continue` re-tests before iterating — the same thing falling off the
    /// end of the body does.
    fn compile_while(&mut self, cond: IrExprId, body: &[IrStmt]) -> Result<(), CompileError> {
        let loop_start = self.here();
        self.compile_expr(cond)?;
        let to_end = self.emit_placeholder_jump_if_false();
        self.loops.push(LoopFrame {
            continue_target: loop_start,
            break_jumps: Vec::new(),
        });
        let body_result = self.compile_body(body);
        let frame = self.loops.pop();
        body_result?;
        self.code.push(Instruction::Jump(loop_start));
        self.patch_to_here(to_end)?;
        // Every `break` lands after the loop's backward jump, which is exactly
        // where the failed condition test lands too.
        for placeholder in frame.map(|frame| frame.break_jumps).unwrap_or_default() {
            self.patch_to_here(placeholder)?;
        }
        Ok(())
    }

    fn compile_expr(&mut self, id: IrExprId) -> Result<(), CompileError> {
        match self.program.expr(id) {
            IrExpr::Int(value) => self.code.push(Instruction::ConstInt(*value)),
            IrExpr::Float(value) => self.code.push(Instruction::ConstFloat(*value)),
            IrExpr::Bool(value) => self.code.push(Instruction::ConstBool(*value)),
            IrExpr::Str(value) => {
                let pool = self.strings.intern(value)?;
                self.code.push(Instruction::ConstStr(pool));
            }
            IrExpr::Local(slot) => {
                let slot = self.local_slot(*slot)?;
                self.code.push(Instruction::LoadLocal(slot));
            }
            IrExpr::Unary { op, operand } => {
                let operand = *operand;
                let op = *op;
                self.compile_expr(operand)?;
                self.code.push(unary_instruction(op));
            }
            IrExpr::Binary { op, lhs, rhs } => self.compile_binary(*op, *lhs, *rhs)?,
            IrExpr::StructNew { fields, .. } => {
                let fields = fields.clone();
                let count =
                    u16::try_from(fields.len()).map_err(|_| CompileError::TooManyFields {
                        function: self.function_name.to_owned(),
                        count: fields.len(),
                    })?;
                // Fields are pushed in declaration order, so the struct the VM
                // builds has them in layout order with no reordering.
                for field in fields {
                    self.compile_expr(field)?;
                }
                self.code.push(Instruction::NewStruct(count));
            }
            IrExpr::Field { base, index, .. } => {
                let base = *base;
                let index = self.field_index(*index)?;
                self.compile_expr(base)?;
                self.code.push(Instruction::GetField(index));
            }
            IrExpr::ArrayNew { elements, .. } => {
                let elements = elements.clone();
                let count =
                    u32::try_from(elements.len()).map_err(|_| CompileError::TooManyElements {
                        function: self.function_name.to_owned(),
                        count: elements.len(),
                    })?;
                // Elements are pushed in written order, so the array the VM
                // builds is in that order with no reordering.
                for element in elements {
                    self.compile_expr(element)?;
                }
                self.code.push(Instruction::NewArray(count));
            }
            IrExpr::Index { base, index, .. } => {
                let (base, index) = (*base, *index);
                self.compile_expr(base)?;
                self.compile_expr(index)?;
                self.code.push(Instruction::ArrayGet);
            }
            IrExpr::ArrayLen { array } => {
                let array = *array;
                self.compile_expr(array)?;
                self.code.push(Instruction::ArrayLen);
            }
            IrExpr::ArrayAppend { place, value } => {
                let (place, value) = (place.clone(), *value);
                let slot = self.local_slot(place.local)?;
                let path = self.compile_place_indices(&place)?;
                self.compile_expr(value)?;
                self.code.push(Instruction::ArrayAppend { slot, path });
                // `append` yields `Void`, and every expression leaves exactly
                // one value: the statement that discards it pops this.
                self.code.push(Instruction::ConstVoid);
            }
            IrExpr::Call { callee, args, .. } => {
                let callee = *callee;
                let args = args.clone();
                for arg in args {
                    self.compile_expr(arg)?;
                }
                match callee {
                    IrCallee::Print => self.code.push(Instruction::Print),
                    // Which engine owns the callee is known here, at compile
                    // time, so the boundary costs a different opcode rather
                    // than a branch on every call.
                    IrCallee::User(index) => {
                        let native = self
                            .engines
                            .get(index as usize)
                            .is_some_and(|engine| *engine == Execution::Native);
                        self.code.push(if native {
                            Instruction::CallNative(index)
                        } else {
                            Instruction::Call(index)
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn compile_binary(
        &mut self,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<(), CompileError> {
        match op {
            IrBinOp::And => self.compile_and(lhs, rhs),
            IrBinOp::Or => self.compile_or(lhs, rhs),
            other => {
                self.compile_expr(lhs)?;
                self.compile_expr(rhs)?;
                self.code.push(binary_instruction(other)?);
                Ok(())
            }
        }
    }

    /// `a && b`: evaluate `b` only when `a` is true.
    fn compile_and(&mut self, lhs: IrExprId, rhs: IrExprId) -> Result<(), CompileError> {
        self.compile_expr(lhs)?;
        let to_false = self.emit_placeholder_jump_if_false();
        self.compile_expr(rhs)?;
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_false)?;
        self.code.push(Instruction::ConstBool(false));
        self.patch_to_here(to_end)
    }

    /// `a || b`: evaluate `b` only when `a` is false.
    fn compile_or(&mut self, lhs: IrExprId, rhs: IrExprId) -> Result<(), CompileError> {
        self.compile_expr(lhs)?;
        let to_rhs = self.emit_placeholder_jump_if_false();
        self.code.push(Instruction::ConstBool(true));
        let to_end = self.emit_placeholder_jump();
        self.patch_to_here(to_rhs)?;
        self.compile_expr(rhs)?;
        self.patch_to_here(to_end)
    }

    /// Builds the static field path of an all-fields place.
    fn field_path(&self, place: &IrPlace) -> Result<FieldPath, CompileError> {
        let mut steps = Vec::with_capacity(place.path.len());
        for step in &place.path {
            match step {
                IrPlaceStep::Field(index) => steps.push(self.field_index(*index)?),
                // `is_all_fields` is what makes this unreachable; it is a typed
                // error rather than an unwrap because a compiler never gets to
                // end its caller's process.
                IrPlaceStep::Index(_) => {
                    return Err(CompileError::DynamicFieldPath {
                        function: self.function_name.to_owned(),
                    });
                }
            }
        }
        FieldPath::new(steps).map_err(|error| CompileError::FieldPathTooDeep {
            function: self.function_name.to_owned(),
            count: error.count,
        })
    }

    /// Emits a place's index expressions, outermost first, and returns the
    /// path describing the walk.
    ///
    /// The emission order *is* the runtime contract: the values land on the
    /// stack in path order, and the runtime pops them back off in reverse.
    fn compile_place_indices(&mut self, place: &IrPlace) -> Result<PlacePath, CompileError> {
        let mut steps = Vec::with_capacity(place.path.len());
        for step in &place.path {
            match step {
                IrPlaceStep::Field(index) => steps.push(PathStep::Field(self.field_index(*index)?)),
                IrPlaceStep::Index(expr) => {
                    self.compile_expr(*expr)?;
                    steps.push(PathStep::Index);
                }
            }
        }
        PlacePath::new(steps).map_err(|error| CompileError::FieldPathTooDeep {
            function: self.function_name.to_owned(),
            count: error.count,
        })
    }

    /// Converts an IR local index to a `u16` slot, or fails typed.
    fn local_slot(&self, slot: u32) -> Result<u16, CompileError> {
        u16::try_from(slot).map_err(|_| CompileError::LocalSlotOutOfRange {
            function: self.function_name.to_owned(),
            slot,
        })
    }

    /// Converts an IR field index to a `u16` operand, or fails typed.
    fn field_index(&self, index: u32) -> Result<u16, CompileError> {
        u16::try_from(index).map_err(|_| CompileError::TooManyFields {
            function: self.function_name.to_owned(),
            count: index as usize,
        })
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    fn emit_placeholder_jump(&mut self) -> usize {
        let index = self.code.len();
        self.code.push(Instruction::Jump(0));
        index
    }

    fn emit_placeholder_jump_if_false(&mut self) -> usize {
        let index = self.code.len();
        self.code.push(Instruction::JumpIfFalse(0));
        index
    }

    fn patch_to_here(&mut self, placeholder: usize) -> Result<(), CompileError> {
        let target = self.code.len() as u32;
        match self.code.get_mut(placeholder) {
            Some(Instruction::Jump(slot)) | Some(Instruction::JumpIfFalse(slot)) => {
                *slot = target;
                Ok(())
            }
            _ => Err(CompileError::PatchedNonJump),
        }
    }
}

fn unary_instruction(op: IrUnOp) -> Instruction {
    match op {
        IrUnOp::NegInt => Instruction::NegInt,
        IrUnOp::NegFloat => Instruction::NegFloat,
        IrUnOp::Not => Instruction::Not,
    }
}

fn binary_instruction(op: IrBinOp) -> Result<Instruction, CompileError> {
    let instruction = match op {
        IrBinOp::AddInt => Instruction::AddInt,
        IrBinOp::SubInt => Instruction::SubInt,
        IrBinOp::MulInt => Instruction::MulInt,
        IrBinOp::DivInt => Instruction::DivInt,
        IrBinOp::RemInt => Instruction::RemInt,
        IrBinOp::AddFloat => Instruction::AddFloat,
        IrBinOp::SubFloat => Instruction::SubFloat,
        IrBinOp::MulFloat => Instruction::MulFloat,
        IrBinOp::DivFloat => Instruction::DivFloat,
        IrBinOp::ConcatStr => Instruction::ConcatStr,
        IrBinOp::EqInt => Instruction::EqInt,
        IrBinOp::NeInt => Instruction::NeInt,
        IrBinOp::LtInt => Instruction::LtInt,
        IrBinOp::LeInt => Instruction::LeInt,
        IrBinOp::GtInt => Instruction::GtInt,
        IrBinOp::GeInt => Instruction::GeInt,
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
        // Short-circuit operators are compiled as control flow, never as a
        // single opcode; reaching here is a compiler bug surfaced typed.
        IrBinOp::And | IrBinOp::Or => return Err(CompileError::ShortCircuitOpcode),
    };
    Ok(instruction)
}
