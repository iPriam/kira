use std::collections::{BTreeSet, VecDeque};

use kira_ir::{IrCallee, IrExpr, IrExprId, IrPlace, IrProgram, IrStmt};

pub(crate) fn native_functions(program: &IrProgram) -> Vec<bool> {
    let mut reachable = vec![false; program.functions.len()];
    let mut pending = VecDeque::new();
    if let Some(main) = program.main {
        pending.push_back(main);
    }
    pending.extend(
        program
            .foreign_callbacks
            .iter()
            .map(kira_runtime_abi::ForeignCallback::function),
    );

    while let Some(index) = pending.pop_front() {
        let index = index as usize;
        if index >= reachable.len() || std::mem::replace(&mut reachable[index], true) {
            continue;
        }
        for callee in direct_calls(program, &program.functions[index].body) {
            pending.push_back(callee);
        }
    }
    reachable
}

fn direct_calls(program: &IrProgram, body: &[IrStmt]) -> BTreeSet<u32> {
    let mut found = BTreeSet::new();
    for statement in body {
        walk_stmt(program, statement, &mut found);
    }
    found
}

fn walk_stmt(program: &IrProgram, statement: &IrStmt, found: &mut BTreeSet<u32>) {
    match statement {
        IrStmt::Let { init, .. } => walk_expr(program, *init, found),
        IrStmt::Assign { place, value } => {
            walk_place(program, place, found);
            walk_expr(program, *value, found);
        }
        IrStmt::Return { value } => {
            if let Some(value) = value {
                walk_expr(program, *value, found);
            }
        }
        IrStmt::Eval { expr } => walk_expr(program, *expr, found),
        IrStmt::CellSet { value, .. } => walk_expr(program, *value, found),
        IrStmt::If {
            cond,
            then_body,
            else_body,
        } => {
            walk_expr(program, *cond, found);
            for statement in then_body.iter().chain(else_body) {
                walk_stmt(program, statement, found);
            }
        }
        IrStmt::While { cond, body } => {
            walk_expr(program, *cond, found);
            for statement in body {
                walk_stmt(program, statement, found);
            }
        }
        IrStmt::Break | IrStmt::Continue => {}
    }
}

fn walk_place(program: &IrProgram, place: &IrPlace, found: &mut BTreeSet<u32>) {
    for index in place.indices() {
        walk_expr(program, index, found);
    }
}

fn walk_expr(program: &IrProgram, id: IrExprId, found: &mut BTreeSet<u32>) {
    match program.expr(id) {
        IrExpr::Call { callee, args, .. } => {
            if let IrCallee::User(index) = callee {
                found.insert(*index);
            }
            for arg in args {
                walk_expr(program, *arg, found);
            }
        }
        IrExpr::Unary { operand, .. } => walk_expr(program, *operand, found),
        IrExpr::Binary { lhs, rhs, .. } => {
            walk_expr(program, *lhs, found);
            walk_expr(program, *rhs, found);
        }
        IrExpr::Select {
            cond,
            then,
            otherwise,
            ..
        } => {
            walk_expr(program, *cond, found);
            walk_expr(program, *then, found);
            walk_expr(program, *otherwise, found);
        }
        IrExpr::StructNew { fields, .. } => {
            for field in fields {
                walk_expr(program, *field, found);
            }
        }
        IrExpr::EnumNew { payload, .. } => {
            if let Some(payload) = payload {
                walk_expr(program, *payload, found);
            }
        }
        IrExpr::EnumTag { value } | IrExpr::EnumPayload { value, .. } => {
            walk_expr(program, *value, found)
        }
        IrExpr::Field { base, .. }
        | IrExpr::ForeignField { base, .. }
        | IrExpr::ForeignMemberAddress { base, .. } => walk_expr(program, *base, found),
        IrExpr::ForeignElement { base, index, .. } => {
            walk_expr(program, *base, found);
            walk_expr(program, *index, found);
        }
        IrExpr::MathOperation { value, .. }
        | IrExpr::ScalarText { value }
        | IrExpr::ArrayElements { value, .. }
        | IrExpr::StringLen { text: value }
        | IrExpr::StringOf { value }
        | IrExpr::ArrayLen { array: value } => walk_expr(program, *value, found),
        IrExpr::StringCharAt { text, index } => {
            walk_expr(program, *text, found);
            walk_expr(program, *index, found);
        }
        IrExpr::StringIndexOf { text, needle } => {
            walk_expr(program, *text, found);
            walk_expr(program, *needle, found);
        }
        IrExpr::StringOperation {
            text, arguments, ..
        } => {
            walk_expr(program, *text, found);
            for argument in arguments {
                walk_expr(program, *argument, found);
            }
        }
        IrExpr::StringSubstring { text, start, end } => {
            walk_expr(program, *text, found);
            walk_expr(program, *start, found);
            walk_expr(program, *end, found);
        }
        IrExpr::CStringNew { text } => walk_expr(program, *text, found),
        IrExpr::CLayoutAddress { value, .. } => walk_expr(program, *value, found),
        IrExpr::FileSystem { args, .. }
        | IrExpr::Compiler { args, .. }
        | IrExpr::Env { args, .. } => {
            for arg in args {
                walk_expr(program, *arg, found);
            }
        }
        IrExpr::ArrayNew { elements, .. } => {
            for element in elements {
                walk_expr(program, *element, found);
            }
        }
        IrExpr::Index { base, index, .. } => {
            walk_expr(program, *base, found);
            walk_expr(program, *index, found);
        }
        IrExpr::TaskOp { operands, .. } => {
            for operand in operands {
                walk_expr(program, *operand, found);
            }
        }
        IrExpr::ArrayAppend { place, value } => {
            walk_place(program, place, found);
            walk_expr(program, *value, found);
        }
        IrExpr::Convert { operand, .. }
        | IrExpr::CellNew { value: operand, .. }
        | IrExpr::IntoAny { value: operand, .. }
        | IrExpr::Widen { value: operand, .. }
        | IrExpr::NativeState { value: operand, .. }
        | IrExpr::NativeUserData { state: operand }
        | IrExpr::NativeRecover { raw: operand, .. }
        | IrExpr::NativeStateFree { token: operand } => walk_expr(program, *operand, found),
        IrExpr::ForeignCallbackPtr { .. }
        | IrExpr::Int(_)
        | IrExpr::Float(_)
        | IrExpr::Bool(_)
        | IrExpr::Str(_)
        | IrExpr::RawPtrNull
        | IrExpr::Local(_)
        | IrExpr::CellGet { .. } => {}
    }
}
