//! Bytecode format and compiler for the Kira VM.
//!
//! Layer 4 of the Kira package graph.
//!
//! The module format ([`Module`]), instruction set ([`Instruction`]), and their
//! byte encoding are designed fresh for the new VM. Wire formats are
//! append-only. Because a `Module` is a deserializable public artifact,
//! [`Module::validate`] proves its structural invariants before execution.
//! This crate is part of the portable core: no filesystem, process, thread, or
//! dynamic-loading calls, and it compiles for `wasm32-unknown-unknown`.

pub mod compile;
pub mod module;
pub mod op;
pub mod validate;

pub use compile::{CompileError, compile};
pub use module::{FuncProto, MAGIC, Module, ModuleDecodeError};
pub use op::{DecodeError, Instruction, decode, encode};
pub use validate::ModuleValidateError;

#[cfg(test)]
mod tests {
    use super::*;
    use kira_ir::{IrExpr, IrFunction, IrProgram, IrStmt, ir::IrCallee};

    fn single_main(
        body: Vec<IrStmt>,
        exprs: la_arena::Arena<IrExpr>,
        local_count: u32,
    ) -> IrProgram {
        IrProgram {
            functions: vec![IrFunction {
                name: "main".to_owned(),
                param_count: 0,
                // Bytecode only needs the slot count; the VM tags values
                // dynamically, so the slot types are immaterial here.
                locals: vec![kira_semantics_model::Type::Int; local_count as usize],
                return_type: kira_semantics_model::Type::Void,
                body,
            }],
            main: 0,
            exprs,
        }
    }

    #[test]
    fn compiles_print_of_a_constant() {
        let mut exprs = la_arena::Arena::new();
        let arg = exprs.alloc(IrExpr::Int(7));
        let call = exprs.alloc(IrExpr::Call {
            callee: IrCallee::Print,
            args: vec![arg],
            result: kira_semantics_model::Type::Void,
        });
        let program = single_main(vec![IrStmt::Eval { expr: call }], exprs, 0);
        let module = compile(&program).expect("compiles");
        assert_eq!(module.functions.len(), 1);
        let code = &module.functions[0].code;
        assert!(code.contains(&Instruction::ConstInt(7)));
        assert!(code.contains(&Instruction::Print));
        // Eval discards the print result, and the body ends with a unit return.
        assert!(code.contains(&Instruction::Pop));
        assert_eq!(code.last(), Some(&Instruction::ReturnVoid));
        // Every compiler-produced module passes structural validation.
        assert_eq!(module.validate(), Ok(()));
    }

    #[test]
    fn compiled_module_round_trips_through_bytes() {
        let mut exprs = la_arena::Arena::new();
        let arg = exprs.alloc(IrExpr::Str("hi".to_owned()));
        let call = exprs.alloc(IrExpr::Call {
            callee: IrCallee::Print,
            args: vec![arg],
            result: kira_semantics_model::Type::Void,
        });
        let program = single_main(vec![IrStmt::Eval { expr: call }], exprs, 0);
        let module = compile(&program).expect("compiles");
        let bytes = module.to_bytes();
        assert_eq!(Module::from_bytes(&bytes).unwrap(), module);
        assert_eq!(module.strings, vec!["hi".to_owned()]);
    }

    #[test]
    fn too_many_locals_is_a_typed_error() {
        // 70_000 locals exceed the format's u16 slot operand.
        let program = single_main(
            vec![IrStmt::Return { value: None }],
            la_arena::Arena::new(),
            70_000,
        );
        assert!(matches!(
            compile(&program),
            Err(CompileError::TooManyLocals { count: 70_000, .. })
        ));
    }

    #[test]
    fn out_of_range_local_slot_is_a_typed_error() {
        let mut exprs = la_arena::Arena::new();
        let read = exprs.alloc(IrExpr::Local(70_000));
        let program = single_main(vec![IrStmt::Eval { expr: read }], exprs, 1);
        assert!(matches!(
            compile(&program),
            Err(CompileError::LocalSlotOutOfRange { slot: 70_000, .. })
        ));
    }
}
