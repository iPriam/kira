//! Structural validation of a [`Module`] before execution.
//!
//! A `Module` is a public, deserializable artifact, so the VM cannot trust its
//! invariants: [`Module::validate`] proves every index and operand in range —
//! after it passes, the interpreter's direct indexing cannot go out of bounds.
//! The checks are:
//!
//! - the entrypoint index names a real function,
//! - every function has non-empty, return-terminated code,
//! - `param_count <= local_count` for every function,
//! - every `ConstStr`/`LoadLocal`/`StoreLocal`/`Call`/`Jump`/`JumpIfFalse`
//!   operand is in range.

use crate::module::Module;
use crate::op::Instruction;

/// A structural fault found by [`Module::validate`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ModuleValidateError {
    /// The entrypoint index is outside the function table.
    #[error("entrypoint index {main} is out of range ({function_count} functions)")]
    MainOutOfRange {
        /// The module's claimed entrypoint index.
        main: u32,
        /// How many functions the module actually has.
        function_count: u32,
    },
    /// A native function carried a bytecode body, which nothing would run.
    #[error("native function `{function}` carries a bytecode body")]
    NativeWithCode {
        /// The offending function's name.
        function: String,
    },
    /// A function has no instructions at all.
    #[error("function `{function}` has empty code")]
    EmptyCode {
        /// The offending function's name.
        function: String,
    },
    /// A function's code does not end in `Return`/`ReturnVoid`, so execution
    /// could fall off its end.
    #[error("function `{function}` does not end in a return instruction")]
    NotReturnTerminated {
        /// The offending function's name.
        function: String,
    },
    /// A function claims more parameters than local slots.
    #[error("function `{function}` declares more parameters than local slots")]
    ParamsExceedLocals {
        /// The offending function's name.
        function: String,
    },
    /// An instruction operand points outside its table (string pool, local
    /// slots, function table, or code range).
    #[error(
        "function `{function}`: instruction {index} ({instruction:?}) has an out-of-range operand"
    )]
    OperandOutOfRange {
        /// The offending function's name.
        function: String,
        /// The instruction's index within the function's code.
        index: usize,
        /// The offending instruction.
        instruction: Instruction,
    },
}

impl Module {
    /// Verifies every structural invariant the interpreter relies on.
    pub fn validate(&self) -> Result<(), ModuleValidateError> {
        let function_count = self.functions.len() as u32;
        if self.main >= function_count {
            return Err(ModuleValidateError::MainOutOfRange {
                main: self.main,
                function_count,
            });
        }
        for function in &self.functions {
            // A native function's body lives in the other half of a hybrid
            // program, so it is the one kind that legitimately carries no code.
            // It still has to be well-formed: a signature to marshal against,
            // and nothing pretending to be a body.
            if function.is_native() {
                if !function.code.is_empty() {
                    return Err(ModuleValidateError::NativeWithCode {
                        function: function.name.clone(),
                    });
                }
                continue;
            }
            if function.code.is_empty() {
                return Err(ModuleValidateError::EmptyCode {
                    function: function.name.clone(),
                });
            }
            if !matches!(
                function.code.last(),
                Some(Instruction::Return | Instruction::ReturnVoid)
            ) {
                return Err(ModuleValidateError::NotReturnTerminated {
                    function: function.name.clone(),
                });
            }
            if function.param_count > function.local_count {
                return Err(ModuleValidateError::ParamsExceedLocals {
                    function: function.name.clone(),
                });
            }
            let code_len = function.code.len() as u32;
            for (index, instruction) in function.code.iter().enumerate() {
                let in_range = match instruction {
                    Instruction::ConstStr(string) => (*string as usize) < self.strings.len(),
                    Instruction::LoadLocal(slot) | Instruction::StoreLocal(slot) => {
                        *slot < function.local_count
                    }
                    // A bytecode `Call` must land on a bytecode body. A native
                    // callee is reached with `CallNative`, which goes through
                    // the host; letting `Call` target one would push a frame
                    // over an empty body.
                    Instruction::Call(callee) => {
                        *callee < function_count && !self.functions[*callee as usize].is_native()
                    }
                    // A `CallNative` id names a function in the *program*, and
                    // is resolved by the host against the hybrid manifest — not
                    // an index into this module's table, so there is nothing
                    // here to bound it against.
                    Instruction::CallNative(_) => true,
                    Instruction::Jump(target) | Instruction::JumpIfFalse(target) => {
                        *target < code_len
                    }
                    // The slot a nested write is rooted at is an index into this
                    // frame, so it is bounded here like any other. The field
                    // steps are not: a struct's shape is not in the module (the
                    // VM is structurally typed), so the runtime checks each step
                    // against the value it actually finds and traps on a
                    // mismatch.
                    Instruction::StoreField { slot, .. } => *slot < function.local_count,
                    // `NewStruct` and `GetField` carry only counts and indices
                    // that the runtime checks against the stack and the value in
                    // hand; there is nothing static to bound them against.
                    _ => true,
                };
                if !in_range {
                    return Err(ModuleValidateError::OperandOutOfRange {
                        function: function.name.clone(),
                        index,
                        instruction: instruction.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::FuncProto;

    fn func(name: &str, params: u16, locals: u16, code: Vec<Instruction>) -> FuncProto {
        FuncProto {
            name: name.to_owned(),
            param_count: params,
            local_count: locals,
            execution: kira_runtime_abi::Execution::Runtime,
            code,
        }
    }

    fn module_of(functions: Vec<FuncProto>, main: u32, strings: Vec<String>) -> Module {
        Module {
            functions,
            main,
            strings,
        }
    }

    #[test]
    fn a_well_formed_module_validates() {
        let module = module_of(
            vec![func(
                "main",
                0,
                1,
                vec![
                    Instruction::ConstStr(0),
                    Instruction::StoreLocal(0),
                    Instruction::LoadLocal(0),
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            )],
            0,
            vec!["hi".to_owned()],
        );
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn main_out_of_range_is_rejected() {
        let module = module_of(
            vec![func("f", 0, 0, vec![Instruction::ReturnVoid])],
            3,
            vec![],
        );
        assert!(matches!(
            module.validate(),
            Err(ModuleValidateError::MainOutOfRange { main: 3, .. })
        ));
    }

    #[test]
    fn empty_code_is_rejected() {
        let module = module_of(vec![func("f", 0, 0, vec![])], 0, vec![]);
        assert!(matches!(
            module.validate(),
            Err(ModuleValidateError::EmptyCode { .. })
        ));
    }

    #[test]
    fn non_return_terminated_code_is_rejected() {
        let module = module_of(
            vec![func("f", 0, 0, vec![Instruction::ConstInt(1)])],
            0,
            vec![],
        );
        assert!(matches!(
            module.validate(),
            Err(ModuleValidateError::NotReturnTerminated { .. })
        ));
    }

    #[test]
    fn params_exceeding_locals_are_rejected() {
        let module = module_of(
            vec![func("f", 2, 1, vec![Instruction::ReturnVoid])],
            0,
            vec![],
        );
        assert!(matches!(
            module.validate(),
            Err(ModuleValidateError::ParamsExceedLocals { .. })
        ));
    }

    #[test]
    fn out_of_range_operands_are_rejected() {
        let cases = vec![
            // ConstStr into an empty string pool.
            vec![Instruction::ConstStr(0), Instruction::ReturnVoid],
            // LoadLocal beyond local_count (0 locals).
            vec![Instruction::LoadLocal(0), Instruction::ReturnVoid],
            // StoreLocal beyond local_count.
            vec![Instruction::StoreLocal(5), Instruction::ReturnVoid],
            // Call to a function index that does not exist.
            vec![Instruction::Call(9), Instruction::ReturnVoid],
            // Jump past the end of the code.
            vec![Instruction::Jump(99), Instruction::ReturnVoid],
            // Conditional jump past the end of the code.
            vec![Instruction::JumpIfFalse(2), Instruction::ReturnVoid],
        ];
        for code in cases {
            let module = module_of(vec![func("f", 0, 0, code.clone())], 0, vec![]);
            assert!(
                matches!(
                    module.validate(),
                    Err(ModuleValidateError::OperandOutOfRange { .. })
                ),
                "expected rejection for {code:?}"
            );
        }
    }
}
