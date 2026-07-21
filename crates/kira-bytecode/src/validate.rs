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
//!   operand is in range,
//! - every export names a real function at the arity it claims, every handle
//!   names a listed class, and no consumer-facing name is claimed twice.

use crate::exports::ExportType;
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
    /// An export names a function index outside the function table.
    #[error(
        "export `{export}` names function {function}, which is out of range ({function_count} \
         functions)"
    )]
    ExportFunctionOutOfRange {
        /// The offending export's consumer-facing name.
        export: String,
        /// The index it claimed.
        function: u32,
        /// How many functions the module actually has.
        function_count: u32,
    },
    /// An export's declared arity disagrees with the function it names.
    #[error("export `{export}` declares {declared} parameters; its function takes {actual}")]
    ExportArityMismatch {
        /// The offending export's consumer-facing name.
        export: String,
        /// The arity the export table claimed.
        declared: usize,
        /// The arity the function actually has.
        actual: u16,
    },
    /// A handle type names a class outside the export table's class list.
    #[error("export `{export}` names class {class}, which is out of range ({class_count} classes)")]
    ExportClassOutOfRange {
        /// The offending export's consumer-facing name.
        export: String,
        /// The class index it claimed.
        class: u32,
        /// How many classes the table actually lists.
        class_count: u32,
    },
    /// Two exports claim the same consumer-facing name.
    #[error("two exports are named `{export}`")]
    DuplicateExport {
        /// The name claimed twice.
        export: String,
    },
}

impl Module {
    /// Verifies every structural invariant the interpreter relies on.
    pub fn validate(&self) -> Result<(), ModuleValidateError> {
        let function_count = self.functions.len() as u32;
        // A library carries no entrypoint, so there is no index to range-check.
        // Everything below still applies: a library's functions are validated
        // exactly as an application's are.
        if let Some(main) = self.main
            && main >= function_count
        {
            return Err(ModuleValidateError::MainOutOfRange {
                main,
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
                    // Like `Call`, a `CallMut` must land on a bytecode body:
                    // the writeback happens when that body returns, which a
                    // native callee never does here. Its `slot` roots the
                    // writeback place in this frame, so it is bounded too; the
                    // path steps are checked by the runtime against the value in
                    // hand, exactly as `StorePlace`'s are.
                    Instruction::CallMut { func, slot, .. } => {
                        *func < function_count
                            && !self.functions[*func as usize].is_native()
                            && *slot < function.local_count
                    }
                    // A `CallNative` id names a function in the *program*, and
                    // is resolved by the host against the hybrid manifest — not
                    // an index into this module's table, so there is nothing
                    // here to bound it against.
                    Instruction::CallNative(_) => true,
                    // A `CallForeign` id indexes this module's foreign-import
                    // table, so unlike a native id it is bounded here: an id
                    // past the table would have no signature to marshal against.
                    Instruction::CallForeign(id) => (*id as usize) < self.foreign_imports.len(),
                    Instruction::Jump(target) | Instruction::JumpIfFalse(target) => {
                        *target < code_len
                    }
                    // The slot a nested write is rooted at is an index into this
                    // frame, so it is bounded here like any other. The field
                    // steps are not: a struct's shape is not in the module (the
                    // VM is structurally typed), so the runtime checks each step
                    // against the value it actually finds and traps on a
                    // mismatch.
                    // The slot a place is rooted at is an index into this
                    // frame, so every instruction carrying one is bounded here.
                    Instruction::StoreField { slot, .. }
                    | Instruction::StorePlace { slot, .. }
                    | Instruction::ArrayAppend { slot, .. } => *slot < function.local_count,
                    // `NewStruct`, `GetField`, `NewArray`, `ArrayGet`, and
                    // `ArrayLen` carry only counts and indices that the runtime
                    // checks against the stack and the value in hand; there is
                    // nothing static to bound them against. An array index in
                    // particular is a *value*, not an operand — out of bounds
                    // is a runtime trap by design.
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
        self.validate_exports()
    }

    /// Proves the export table's indices and arities against this module.
    ///
    /// A module is a public artifact and the export table is what a consumer's
    /// generated wrapper trusts, so every claim in it is checked here: an export
    /// naming a function that does not exist, a handle naming a class the table
    /// does not list, or a signature whose arity disagrees with the function it
    /// names would each turn into a call made against the wrong frame.
    fn validate_exports(&self) -> Result<(), ModuleValidateError> {
        let function_count = self.functions.len() as u32;
        let class_count = self.exports.classes.len() as u32;
        let mut seen: Vec<&str> = Vec::with_capacity(self.exports.functions.len());
        for export in &self.exports.functions {
            let Some(function) = self.functions.get(export.function as usize) else {
                return Err(ModuleValidateError::ExportFunctionOutOfRange {
                    export: export.name.clone(),
                    function: export.function,
                    function_count,
                });
            };
            if export.params.len() != usize::from(function.param_count) {
                return Err(ModuleValidateError::ExportArityMismatch {
                    export: export.name.clone(),
                    declared: export.params.len(),
                    actual: function.param_count,
                });
            }
            for ty in export.params.iter().chain(std::iter::once(&export.result)) {
                if let ExportType::Handle { class } = ty
                    && *class >= class_count
                {
                    return Err(ModuleValidateError::ExportClassOutOfRange {
                        export: export.name.clone(),
                        class: *class,
                        class_count,
                    });
                }
            }
            // The frontend refuses a collision (two Kira names snake_casing onto
            // one consumer name), but this module need not be one it wrote, and
            // a consumer resolving a name to two functions has no way to choose.
            if seen.contains(&export.name.as_str()) {
                return Err(ModuleValidateError::DuplicateExport {
                    export: export.name.clone(),
                });
            }
            seen.push(&export.name);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::{ExportTable, ModuleExport};
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            functions,
            main: Some(main),
            strings,
        }
    }

    /// A library exporting one function that takes a string and hands back a
    /// `Button` handle — the smallest module that exercises every export check.
    fn exporting_library(exports: ExportTable) -> Module {
        Module {
            exports,
            foreign_imports: Vec::new(),
            functions: vec![func(
                "makeButton",
                1,
                1,
                vec![Instruction::LoadLocal(0), Instruction::Return],
            )],
            main: None,
            strings: vec![],
        }
    }

    fn make_button() -> ModuleExport {
        ModuleExport {
            name: "make_button".to_owned(),
            kira_name: "makeButton".to_owned(),
            function: 0,
            params: vec![ExportType::String],
            result: ExportType::Handle { class: 0 },
        }
    }

    #[test]
    fn a_well_formed_export_table_validates() {
        let module = exporting_library(ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![make_button()],
        });
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn an_export_naming_no_function_is_rejected() {
        let module = exporting_library(ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![ModuleExport {
                function: 9,
                ..make_button()
            }],
        });
        assert_eq!(
            module.validate(),
            Err(ModuleValidateError::ExportFunctionOutOfRange {
                export: "make_button".to_owned(),
                function: 9,
                function_count: 1,
            })
        );
    }

    #[test]
    fn an_export_whose_arity_disagrees_with_its_function_is_rejected() {
        let module = exporting_library(ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![ModuleExport {
                params: vec![ExportType::String, ExportType::Int],
                ..make_button()
            }],
        });
        assert_eq!(
            module.validate(),
            Err(ModuleValidateError::ExportArityMismatch {
                export: "make_button".to_owned(),
                declared: 2,
                actual: 1,
            })
        );
    }

    #[test]
    fn a_handle_naming_no_class_is_rejected() {
        // The class list is empty, so the handle denotes nothing a consumer
        // could name — an untyped word, which is what the class list exists to
        // prevent.
        let module = exporting_library(ExportTable {
            classes: Vec::new(),
            functions: vec![make_button()],
        });
        assert_eq!(
            module.validate(),
            Err(ModuleValidateError::ExportClassOutOfRange {
                export: "make_button".to_owned(),
                class: 0,
                class_count: 0,
            })
        );
    }

    #[test]
    fn two_exports_with_one_consumer_name_are_rejected() {
        let module = exporting_library(ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![make_button(), make_button()],
        });
        assert_eq!(
            module.validate(),
            Err(ModuleValidateError::DuplicateExport {
                export: "make_button".to_owned(),
            })
        );
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
    fn a_library_module_validates_with_no_entrypoint() {
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            functions: vec![func("add", 2, 2, vec![Instruction::ReturnVoid])],
            main: None,
            strings: vec![],
        };
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn a_library_module_still_validates_its_function_bodies() {
        // No entrypoint relaxes the entrypoint check and nothing else.
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            functions: vec![func("add", 0, 0, vec![])],
            main: None,
            strings: vec![],
        };
        assert!(matches!(
            module.validate(),
            Err(ModuleValidateError::EmptyCode { .. })
        ));
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
            // Foreign call to an id past the (empty) foreign-import table.
            vec![Instruction::CallForeign(0), Instruction::ReturnVoid],
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
