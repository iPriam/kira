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

use kira_ir::{IrBinOp, IrCallee, IrExpr, IrExprId, IrProgram, IrStmt, IrUnOp};

use crate::module::{FuncProto, Module};
use crate::op::Instruction;
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
}

/// Compiles a lowered program into a runnable module.
pub fn compile(program: &IrProgram) -> Result<Module, CompileError> {
    let mut strings = StringPool::default();
    let mut functions = Vec::with_capacity(program.functions.len());
    for function in &program.functions {
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
        let mut compiler = FnCompiler {
            program,
            function_name: &function.name,
            strings: &mut strings,
            code: Vec::new(),
        };
        compiler.compile_body(&function.body)?;
        // Safety net: a function that falls off its end returns unit. (The
        // analyzer's definite-return check guarantees non-Void functions never
        // reach this instruction.)
        compiler.code.push(Instruction::ReturnVoid);
        functions.push(FuncProto {
            name: function.name.clone(),
            param_count,
            local_count,
            // A VM-only build runs the whole program on the VM: `@Native` is an
            // execution *boundary*, and with no native half there is no boundary
            // to honour. This is what keeps `--backend vm` and `--backend llvm`
            // agreeing on any program, annotated or not.
            execution: Execution::Runtime,
            code: compiler.code,
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
    code: Vec<Instruction>,
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
            IrStmt::Assign { local, value } => {
                self.compile_expr(*value)?;
                let slot = self.local_slot(*local)?;
                self.code.push(Instruction::StoreLocal(slot));
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
        }
        Ok(())
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

    fn compile_while(&mut self, cond: IrExprId, body: &[IrStmt]) -> Result<(), CompileError> {
        let loop_start = self.here();
        self.compile_expr(cond)?;
        let to_end = self.emit_placeholder_jump_if_false();
        self.compile_body(body)?;
        self.code.push(Instruction::Jump(loop_start));
        self.patch_to_here(to_end)
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
            IrExpr::Call { callee, args, .. } => {
                let callee = *callee;
                let args = args.clone();
                for arg in args {
                    self.compile_expr(arg)?;
                }
                match callee {
                    IrCallee::Print => self.code.push(Instruction::Print),
                    IrCallee::User(index) => self.code.push(Instruction::Call(index)),
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

    /// Converts an IR local index to a `u16` slot, or fails typed.
    fn local_slot(&self, slot: u32) -> Result<u16, CompileError> {
        u16::try_from(slot).map_err(|_| CompileError::LocalSlotOutOfRange {
            function: self.function_name.to_owned(),
            slot,
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
