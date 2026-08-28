use kira_runtime_abi::Execution;
use kira_semantics_model::hir::{HirExpr, HirFunction, HirLocal, HirProgram, HirStmt, LocalId};
use kira_semantics_model::{FieldDef, OwnershipMode, StructDef, Type};
use kira_source::Span;

use crate::codegen::Module;

/// A generated C binding struct holds `const char *` members, and Kira keeps
/// each as a `CString` field — an opaque word it stores and hands back. When
/// such a struct crosses a hybrid program's `@Native`/`@Runtime` seam it is
/// encoded field by field into a state-value tree, so the tree has to have a
/// node for that word.
#[test]
fn a_c_string_member_crosses_the_hybrid_seam_as_an_opaque_word() {
    let mut program = HirProgram::default();
    let struct_id = program
        .types
        .structs_mut()
        .declare(StructDef {
            name: "Binding".to_owned(),
            fields: vec![
                FieldDef {
                    name: "slot".to_owned(),
                    ty: Type::INT,
                    mutable: false,
                },
                FieldDef {
                    name: "glsl_name".to_owned(),
                    ty: Type::CString,
                    mutable: false,
                },
            ],
            c_layout: true,
            drop_glue: None,
        })
        .expect("the struct table accepts one declaration");
    let ty = Type::Struct(struct_id);
    let value = program.exprs.alloc(HirExpr::Local {
        local: LocalId(0),
        ty,
    });
    let ret = program.stmts.alloc(HirStmt::Return { value: Some(value) });
    program.functions.push(HirFunction {
        name: "echoBinding".to_owned(),
        param_count: 1,
        return_type: ty,
        locals: vec![HirLocal {
            name: "binding".to_owned(),
            ty,
            mutable: false,
            ownership: OwnershipMode::Owned,
            native_state: None,
        }],
        body: vec![ret],
        is_main: false,
        is_async: false,
        execution: Execution::Native,
        mutates_self: false,
        name_span: Span::new(0, 12),
    });
    let ir = kira_ir::lower(&program);

    Module::build_hybrid(&ir, "cstring_member_probe", &[])
        .expect("a `CString` member has a state-value node on both sides of the seam");
}
