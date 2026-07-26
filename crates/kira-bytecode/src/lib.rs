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
pub mod exports;
pub mod module;
mod module_foreign;
pub mod op;
pub mod validate;

pub use compile::{CompileError, compile, compile_hybrid};
pub use exports::{ExportTable, ExportType, ModuleExport};
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
                locals: vec![kira_semantics_model::Type::INT; local_count as usize],
                native_state_locals: vec![None; local_count as usize],
                return_type: kira_semantics_model::Type::Void,
                execution: kira_runtime_abi::Execution::Inherited,
                mutates_self: false,
                body,
            }],
            types: Default::default(),
            main: Some(0),
            exports: Vec::new(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            exprs,
        }
    }

    /// The hybrid split: a native callee keeps its slot and signature but no
    /// body, and its callers reach it through `CallNative` rather than `Call`.
    #[test]
    fn a_hybrid_build_splits_the_program_on_its_annotations() {
        use kira_runtime_abi::Execution;
        let mut exprs = la_arena::Arena::new();
        let call = exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(1),
            args: vec![],
            result: kira_semantics_model::Type::Void,
            writeback: None,
        });
        let mut program = single_main(vec![IrStmt::Eval { expr: call }], exprs, 0);
        program.functions.push(IrFunction {
            name: "hot".to_owned(),
            param_count: 0,
            locals: Vec::new(),
            native_state_locals: Vec::new(),
            return_type: kira_semantics_model::Type::Void,
            execution: Execution::Native,
            mutates_self: false,
            body: Vec::new(),
        });

        let hybrid = compile_hybrid(&program).expect("compiles");
        assert!(
            hybrid.functions[0]
                .code
                .contains(&Instruction::CallNative(1))
        );
        assert!(!hybrid.functions[0].code.contains(&Instruction::Call(1)));
        assert!(hybrid.functions[1].is_native());
        assert!(
            hybrid.functions[1].code.is_empty(),
            "a native body lives in the shared library, not here",
        );
        hybrid.validate().expect("a hybrid module is well-formed");

        // The same program built for the VM has no boundary to honour: every
        // function is bytecode, reached by an ordinary call.
        let vm = compile(&program).expect("compiles");
        assert!(vm.functions[0].code.contains(&Instruction::Call(1)));
        assert!(!vm.functions[1].is_native());
        assert!(!vm.functions[1].code.is_empty());
        vm.validate().expect("a vm module is well-formed");
    }

    #[test]
    fn compiles_print_of_a_constant() {
        let mut exprs = la_arena::Arena::new();
        let arg = exprs.alloc(IrExpr::Int(7));
        let call = exprs.alloc(IrExpr::Call {
            callee: IrCallee::Print,
            args: vec![arg],
            result: kira_semantics_model::Type::Void,
            writeback: None,
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
            writeback: None,
        });
        let program = single_main(vec![IrStmt::Eval { expr: call }], exprs, 0);
        let module = compile(&program).expect("compiles");
        let bytes = module.to_bytes();
        assert_eq!(Module::from_bytes(&bytes).unwrap(), module);
        assert_eq!(module.strings, vec!["hi".to_owned()]);
    }

    /// A compiled library carries its export surface into the artifact, and the
    /// artifact survives a round trip through bytes with it.
    ///
    /// This is the whole point of the section: the consumer's generated wrapper
    /// is checked against what the module says about itself, so what the module
    /// says has to make the trip.
    #[test]
    fn a_library_compiles_its_export_surface_into_the_module() {
        use kira_ir::ir::IrExport;
        use kira_semantics_model::{Type, ty::StructDef};

        let mut program = IrProgram {
            functions: vec![IrFunction {
                name: "makeButton".to_owned(),
                param_count: 1,
                locals: vec![Type::String],
                native_state_locals: vec![None],
                return_type: Type::Void,
                execution: kira_runtime_abi::Execution::Inherited,
                mutates_self: false,
                body: vec![IrStmt::Return { value: None }],
            }],
            types: Default::default(),
            main: None,
            exports: Vec::new(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            exprs: la_arena::Arena::new(),
        };
        let button = program
            .types
            .structs_mut()
            .declare(StructDef {
                name: "Button".to_owned(),
                fields: Vec::new(),
            })
            .expect("a fresh struct table takes the declaration");
        program.exports.push(IrExport {
            kira_name: "makeButton".to_owned(),
            exported_name: "make_button".to_owned(),
            function: 0,
            params: vec![Type::String],
            result: Type::Struct(button),
        });

        let module = compile(&program).expect("compiles");
        assert_eq!(module.main, None);
        // The class list is derived from the signatures that mention it, so the
        // handle's index and the list cannot disagree.
        assert_eq!(module.exports.classes, ["Button"]);
        let export = &module.exports.functions[0];
        assert_eq!(export.name, "make_button");
        assert_eq!(export.kira_name, "makeButton");
        assert_eq!(export.params, vec![ExportType::String]);
        assert_eq!(export.result, ExportType::Handle { class: 0 });
        assert_eq!(module.validate(), Ok(()));
        assert_eq!(Module::from_bytes(&module.to_bytes()).unwrap(), module);
    }

    /// An application carries no export table at all — including in its bytes.
    #[test]
    fn an_application_carries_no_export_table() {
        let program = single_main(
            vec![IrStmt::Return { value: None }],
            la_arena::Arena::new(),
            0,
        );
        let module = compile(&program).expect("compiles");
        assert!(module.exports.is_empty());
        assert_eq!(
            Module::from_bytes(&module.to_bytes()).unwrap().exports,
            module.exports
        );
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
