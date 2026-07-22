//! Kira mid-level IR, lowered from the HIR for the backends.
//!
//! Layer 3 of the Kira package graph.
//!
//! The IR is the verified-program contract every backend consumes: fully
//! resolved, fully typed, arena-backed, no lifetimes. [`lower`] is a pure
//! function from an analyzed [`HirProgram`](kira_semantics_model::HirProgram)
//! to an [`IrProgram`].
//!
//! This crate is part of the portable core's dependency cone (the VM consumes
//! bytecode compiled from this IR), so it deliberately depends only on the
//! semantic *model*, never on the salsa-driven analyzer: the query that wires
//! analysis to lowering lives in the embedder (the CLI), keeping the whole VM
//! subtree salsa-free and wasm-portable.

pub mod ir;
pub mod lower;

pub use ir::{
    ConvertKind, IrBinOp, IrCallee, IrExport, IrExpr, IrExprId, IrForeignImport, IrFunction,
    IrPlace, IrPlaceStep, IrProgram, IrStmt, IrUnOp,
};
pub use lower::lower;

#[cfg(test)]
mod tests {
    use super::*;
    use kira_semantics_model::Type;
    use kira_semantics_model::hir::{
        Builtin, Callee, FuncId, HirExpr, HirFunction, HirProgram, HirStmt,
    };
    use kira_source::Span;

    /// Hand-builds the HIR for `@Main function main() { print(1) return }`.
    fn tiny_program() -> HirProgram {
        let mut program = HirProgram::default();
        let one = program.exprs.alloc(HirExpr::Int(1));
        let call = program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::Print),
            args: vec![one],
            ty: Type::Void,
            writeback: None,
        });
        let print_stmt = program.stmts.alloc(HirStmt::Expr { expr: call });
        let return_stmt = program.stmts.alloc(HirStmt::Return { value: None });
        program.functions.push(HirFunction {
            name: "main".to_owned(),
            param_count: 0,
            return_type: Type::Void,
            locals: Vec::new(),
            body: vec![print_stmt, return_stmt],
            is_main: true,
            execution: kira_runtime_abi::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 4),
        });
        program.main = Some(FuncId(0));
        program
    }

    #[test]
    fn lowers_a_program_with_a_main() {
        let ir = lower(&tiny_program());
        assert_eq!(ir.functions.len(), 1);
        assert_eq!(ir.main, Some(0));
        assert_eq!(ir.functions[0].name, "main");
        // print(1) then return: an Eval of a Print call, then a Return.
        assert!(matches!(ir.functions[0].body[0], IrStmt::Eval { .. }));
        assert!(matches!(
            ir.functions[0].body[1],
            IrStmt::Return { value: None }
        ));
    }

    #[test]
    fn a_program_without_main_still_lowers_its_functions() {
        // A library: no entrypoint, but every function is still compiled. The
        // bodies are what a consumer calls.
        let mut program = tiny_program();
        program.main = None;
        let ir = lower(&program);
        assert_eq!(ir.main, None);
        assert!(ir.main_function().is_none());
        assert_eq!(ir.functions.len(), 1);
        assert_eq!(ir.functions[0].name, "main");
    }

    #[test]
    fn a_program_without_externs_lowers_to_an_empty_foreign_table() {
        let ir = lower(&tiny_program());
        assert!(ir.foreign_imports.is_empty());
    }

    #[test]
    fn foreign_imports_and_a_foreign_call_lower_across() {
        use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType};
        use kira_semantics_model::hir::{ForeignId, HirForeign};

        // A `main` that calls the extern `add`, plus the registry row for it.
        let mut program = tiny_program();
        program.foreign.push(HirForeign {
            kira_name: "add".to_owned(),
            library: "ffimath".to_owned(),
            symbol: "kira_ffi_add".to_owned(),
            abi: ForeignAbi::C,
            signature: ForeignSignature::new(
                [ForeignType::I32, ForeignType::I32],
                ForeignType::I32,
            ),
            param_wrappers: Box::from([None, None]),
            result_wrapper: None,
            name_span: Span::new(0, 3),
        });
        let a = program.exprs.alloc(HirExpr::Int(20));
        let b = program.exprs.alloc(HirExpr::Int(22));
        let call = program.exprs.alloc(HirExpr::Call {
            callee: Callee::Foreign(ForeignId(0)),
            args: vec![a, b],
            ty: Type::Int(kira_semantics_model::IntSpelling::I32),
            writeback: None,
        });
        let eval = program.stmts.alloc(HirStmt::Expr { expr: call });
        program.functions[0].body = vec![eval];

        let ir = lower(&program);
        // The registry lowers row-for-row, carrying the runtime-abi import.
        assert_eq!(ir.foreign_imports.len(), 1);
        assert_eq!(ir.foreign_imports[0].name, "add");
        assert_eq!(ir.foreign_imports[0].import.symbol(), "kira_ffi_add");
        assert_eq!(
            ir.foreign_imports[0].import.signature().parameters().len(),
            2
        );
        // The call lowers to a `Foreign` callee indexing that table.
        let IrStmt::Eval { expr } = ir.functions[0].body[0] else {
            panic!("expected the foreign call to lower to an Eval");
        };
        let IrExpr::Call { callee, .. } = ir.expr(expr) else {
            panic!("expected a call expression");
        };
        assert_eq!(*callee, IrCallee::Foreign(0));
    }
}
