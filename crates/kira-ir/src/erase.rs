//! Erasing `distinct` types out of a lowered program.
//!
//! The IR is the verified-program contract every backend consumes, and this is
//! the pass that makes one guarantee about it: **an [`IrProgram`] holds no
//! [`Type::Distinct`]**. A distinct type is a frontend fact — a name the type
//! checker refuses to let a value cross without being asked — and it has
//! nothing to say to a backend, which lays out the scalar underneath either
//! way.
//!
//! Erasing here rather than teaching each backend to see through the type is
//! what makes "same size, same alignment, same ABI, no wrapper" a property of
//! the *program* rather than a claim each backend has to reproduce. There is no
//! per-backend distinct path to get wrong because there is no distinct type
//! left to get wrong.
//!
//! The crossings themselves are gone before this runs: [`crate::lower`] lowers
//! `HirExpr::Distinct` to the value that crossed, so what is left here is the
//! *types* the lowering copied across.

use kira_semantics_model::Type;

use crate::ir::{IrAttempt, IrExpr, IrProgram, IrStmt};

/// Rewrites every type in `program` to its representation and drops the
/// distinct rows from its type table.
///
/// A no-op — one scan and no writes — for a program that declares none.
pub(crate) fn erase_distinct_types(program: &mut IrProgram) {
    if program.types.distincts().is_empty() {
        return;
    }
    // Read the mapping out before the tables are touched, so every rewrite
    // below sees one consistent answer.
    let representations: Vec<Type> = program
        .types
        .distincts()
        .rows()
        .map(|(_, def)| def.representation)
        .collect();
    let erase = |ty: &mut Type| {
        if let Type::Distinct(id) = *ty {
            *ty = representations
                .get(id.index() as usize)
                .copied()
                .unwrap_or(Type::Error);
        }
    };

    for expr in program.exprs.values_mut() {
        visit_expr_types(expr, &erase);
    }
    for function in &mut program.functions {
        for local in &mut function.locals {
            erase(local);
        }
        erase(&mut function.return_type);
        for stmt in &mut function.body {
            visit_stmt_types(stmt, &erase);
        }
    }
    for constant in &mut program.constants {
        erase(&mut constant.ty);
    }
    for export in &mut program.exports {
        for param in &mut export.params {
            erase(param);
        }
        erase(&mut export.result);
    }
    // The foreign imports carry `kira_runtime_abi` wire types rather than
    // `Type`, and the frontend already mapped a distinct parameter through its
    // representation to build them — see `kira-semantics`'s `foreign_seam_of`.
    // So a C prototype needs nothing here, and that is the FFI half of the
    // transparency claim rather than an omission.
    program.types.erase_distinct_types();
}

/// Rewrites every type a statement holds, and every type in the statements it
/// contains.
fn visit_stmt_types(stmt: &mut IrStmt, erase: &dyn Fn(&mut Type)) {
    match stmt {
        IrStmt::If {
            then_body,
            else_body,
            ..
        } => {
            for inner in then_body.iter_mut().chain(else_body.iter_mut()) {
                visit_stmt_types(inner, erase);
            }
        }
        IrStmt::While { body, .. } => {
            for inner in body {
                visit_stmt_types(inner, erase);
            }
        }
        IrStmt::Attempt { attempt } => visit_attempt_types(attempt, erase),
        // Every other statement names slots, places, and expressions, and each
        // of those carries its type on the expression the arena already holds.
        IrStmt::Let { .. }
        | IrStmt::Assign { .. }
        | IrStmt::CellSet { .. }
        | IrStmt::Return { .. }
        | IrStmt::Eval { .. }
        | IrStmt::Break
        | IrStmt::Continue
        | IrStmt::ReleaseLocals { .. } => {}
    }
}

/// Rewrites every type an `attempt` region holds.
///
/// The region names no type of its own — its result and payload bindings are
/// ordinary slots, already rewritten with the function's locals — so this walks
/// the statements it contains and nothing else.
fn visit_attempt_types(attempt: &mut IrAttempt, erase: &dyn Fn(&mut Type)) {
    for step in &mut attempt.steps {
        for stmt in step
            .setup
            .iter_mut()
            .chain(step.handler.iter_mut())
            .chain(step.success.iter_mut())
        {
            visit_stmt_types(stmt, erase);
        }
    }
    for stmt in &mut attempt.trailing {
        visit_stmt_types(stmt, erase);
    }
}

/// Rewrites every type one expression node holds.
///
/// Exhaustive on purpose: a variant added with a `Type` in it has to be
/// answered here, and the compiler is what asks. Nothing recurses — the arena
/// walk in [`erase_distinct_types`] visits every node once, so a child is
/// reached on its own.
fn visit_expr_types(expr: &mut IrExpr, erase: &dyn Fn(&mut Type)) {
    match expr {
        IrExpr::ConstantGet { ty, .. }
        | IrExpr::Select { ty, .. }
        | IrExpr::Call { result: ty, .. }
        | IrExpr::EnumPayload { ty, .. }
        | IrExpr::TypeCast { ty, .. }
        | IrExpr::CellNew { ty, .. }
        | IrExpr::CellNull { ty }
        | IrExpr::CellGet { ty, .. }
        | IrExpr::Field { ty, .. }
        | IrExpr::ForeignMemberAddress { ty, .. }
        | IrExpr::ForeignElement { ty, .. }
        | IrExpr::ForeignField { ty, .. }
        | IrExpr::ArrayNew { ty, .. }
        | IrExpr::Index { ty, .. }
        | IrExpr::StringOperation { ty, .. }
        | IrExpr::FileSystem { ty, .. }
        | IrExpr::Compiler { ty, .. }
        | IrExpr::Env { ty, .. }
        | IrExpr::NativeState { ty, .. }
        | IrExpr::NativeRecover { ty, .. }
        | IrExpr::Convert { ty, .. }
        | IrExpr::MainThreadCall { ty, .. }
        | IrExpr::MainThreadJoin { ty, .. } => erase(ty),
        IrExpr::IntoAny { from, .. } => erase(from),
        // The rest carry no type: a literal is its own, a struct or enum
        // construction names its row, and everything else reads its type off
        // the table or off the node it was built from.
        IrExpr::Int(_)
        | IrExpr::Float(_)
        | IrExpr::Bool(_)
        | IrExpr::Str(_)
        | IrExpr::RawPtrNull
        | IrExpr::ForeignCallbackPtr { .. }
        | IrExpr::Local(_)
        | IrExpr::Unary { .. }
        | IrExpr::Binary { .. }
        | IrExpr::StructNew { .. }
        | IrExpr::EnumNew { .. }
        | IrExpr::EnumTag { .. }
        | IrExpr::TypeTest { .. }
        | IrExpr::ArrayLen { .. }
        | IrExpr::StringLen { .. }
        | IrExpr::StringCharAt { .. }
        | IrExpr::StringSubstring { .. }
        | IrExpr::StringIndexOf { .. }
        | IrExpr::ArrayElements { .. }
        | IrExpr::ScalarText { .. }
        | IrExpr::MathOperation { .. }
        | IrExpr::StringOf { .. }
        | IrExpr::CLayoutAddress { .. }
        | IrExpr::CStringNew { .. }
        | IrExpr::ArrayAppend { .. }
        | IrExpr::NativeUserData { .. }
        | IrExpr::NativeStateRetain { .. }
        | IrExpr::NativeStateRelease { .. }
        | IrExpr::TaskOp { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use kira_semantics_model::hir::CallableSignature;
    use kira_semantics_model::hir::{HirExpr, HirFunction, HirLocal, HirProgram, HirStmt, LocalId};
    use kira_semantics_model::{
        FieldDef, IntSpelling, OwnershipMode, StructDef, StructId, Type, TypeTable,
    };
    use kira_source::Span;

    const U32: Type = Type::Int(IntSpelling::U32);

    /// A table holding `distinct TabId = U32`, a `[TabId]`, and a struct with a
    /// `TabId` field, plus the id of that struct.
    fn table_with_a_distinct_type() -> (TypeTable, Type, StructId) {
        let mut types = TypeTable::new();
        let tab_id = types.declare_distinct("TabId".to_owned(), U32);
        types.array_of(tab_id);
        let struct_id = types
            .structs_mut()
            .declare(StructDef {
                name: "Tab".to_owned(),
                fields: vec![FieldDef {
                    name: "id".to_owned(),
                    ty: tab_id,
                    mutable: false,
                }],
                c_layout: false,
                drop_glue: None,
            })
            .expect("a fresh name declares");
        (types, tab_id, struct_id)
    }

    /// A one-function program whose only local is of type `local_type`.
    fn program_with_a_local(types: TypeTable, local_type: Type) -> HirProgram {
        let mut program = HirProgram {
            types,
            ..HirProgram::default()
        };
        let word = program.exprs.alloc(HirExpr::Int(7));
        // The crossing the type checker builds for `TabId(7)`.
        let crossed = program.exprs.alloc(HirExpr::Distinct {
            value: word,
            ty: local_type,
        });
        let bind = program.stmts.alloc(HirStmt::Let {
            local: LocalId(0),
            init: crossed,
        });
        let ret = program.stmts.alloc(HirStmt::Return { value: None });
        program.functions.push(HirFunction {
            name: "main".to_owned(),
            param_count: 0,
            return_type: Type::Void,
            locals: vec![HirLocal {
                name: "id".to_owned(),
                ty: local_type,
                mutable: false,
                ownership: OwnershipMode::Owned,
                native_state: None,
            }],
            body: vec![bind, ret],
            is_main: true,
            is_main_thread: false,
            is_async: false,
            execution: kira_runtime_abi::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 4),
            signature: CallableSignature::synthesized(&[], Type::Void),
        });
        program.main = Some(kira_semantics_model::hir::FuncId(0));
        program
    }

    /// The contract this pass exists for: the IR a backend consumes carries no
    /// distinct type anywhere — not in a slot, not in a field, not in an array
    /// element, and not as a row of its own.
    #[test]
    fn the_lowered_program_holds_no_distinct_type() {
        let (types, tab_id, struct_id) = table_with_a_distinct_type();
        let ir = crate::lower(&program_with_a_local(types, tab_id));

        assert!(
            ir.types.distincts().is_empty(),
            "the distinct rows are dropped once nothing names them"
        );
        assert_eq!(
            ir.functions[0].locals,
            vec![U32],
            "a `TabId` slot is a `U32` slot"
        );
        assert_eq!(
            ir.types
                .structs()
                .get(struct_id)
                .expect("the struct")
                .fields[0]
                .ty,
            U32,
            "a `TabId` field is a `U32` field, at the same offset and the same width"
        );
        assert_eq!(
            ir.types.arrays().elements(),
            &[U32],
            "`[TabId]` is `[U32]` once the frontend is done with it"
        );
    }

    /// Same size, same alignment, same ABI: proven by producing the *same
    /// program*. A declaration over `U32` and a bare `U32` lower to IR that is
    /// equal field for field and slot for slot, so no layout question can have
    /// two answers.
    #[test]
    fn a_distinct_type_lowers_to_exactly_what_its_representation_does() {
        let (distinct_types, tab_id, _) = table_with_a_distinct_type();
        let distinct_ir = crate::lower(&program_with_a_local(distinct_types, tab_id));

        // The same program with the representation written out by hand.
        let mut plain_types = TypeTable::new();
        plain_types.array_of(U32);
        plain_types
            .structs_mut()
            .declare(StructDef {
                name: "Tab".to_owned(),
                fields: vec![FieldDef {
                    name: "id".to_owned(),
                    ty: U32,
                    mutable: false,
                }],
                c_layout: false,
                drop_glue: None,
            })
            .expect("a fresh name declares");
        let mut plain = program_with_a_local(plain_types, U32);
        // The hand-written program has no crossing to lower, which is the one
        // difference: replace the `Distinct` node with the value it carried.
        let word = plain.exprs.alloc(HirExpr::Int(7));
        plain.stmts[kira_semantics_model::hir::HirStmtId::from_raw(0.into())] = HirStmt::Let {
            local: LocalId(0),
            init: word,
        };
        let plain_ir = crate::lower(&plain);

        assert_eq!(distinct_ir.types, plain_ir.types);
        assert_eq!(
            distinct_ir.functions[0].locals,
            plain_ir.functions[0].locals
        );
        assert_eq!(
            distinct_ir.functions[0].return_type,
            plain_ir.functions[0].return_type
        );
    }

    /// A program that declares no distinct type is untouched: the pass scans the
    /// table once and returns.
    #[test]
    fn a_program_without_one_is_unchanged() {
        let mut types = TypeTable::new();
        types.array_of(U32);
        let ir = crate::lower(&program_with_a_local(types.clone(), U32));
        assert_eq!(ir.types.arrays().elements(), &[U32]);
        assert!(ir.types.distincts().is_empty());
    }
}
