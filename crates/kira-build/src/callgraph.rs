//! Which user functions a function's body calls.
//!
//! The walk is exhaustive so a new expression kind that can contain a call must
//! be handled here before it can disappear from call analysis.

use std::collections::BTreeSet;

use kira_ir::{IrCallee, IrExpr, IrExprId, IrPlace, IrProgram, IrStmt};

/// Every user function `body` calls, directly, by index.
///
/// Direct calls only, returned in stable index order.
pub fn direct_calls(program: &IrProgram, body: &[IrStmt]) -> BTreeSet<u32> {
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
        IrStmt::Attempt { attempt } => {
            for step in &attempt.steps {
                walk_expr(program, step.error_condition, found);
                for statement in step.setup.iter().chain(&step.handler).chain(&step.success) {
                    walk_stmt(program, statement, found);
                }
            }
            for statement in &attempt.trailing {
                walk_stmt(program, statement, found);
            }
        }
        IrStmt::While { cond, body } => {
            walk_expr(program, *cond, found);
            for statement in body {
                walk_stmt(program, statement, found);
            }
        }
        IrStmt::Break | IrStmt::Continue | IrStmt::ReleaseLocals { .. } => {}
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
        // A constant read names a slot, not a callee; the slot's init runs
        // before any body does, outside every call graph this answers.
        IrExpr::ConstantGet { .. } => {}
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
        IrExpr::EnumTag { value } => walk_expr(program, *value, found),
        IrExpr::EnumPayload { value, .. } => walk_expr(program, *value, found),
        IrExpr::TypeTest { value, .. } | IrExpr::TypeCast { value, .. } => {
            walk_expr(program, *value, found)
        }
        IrExpr::Field { base, .. } => walk_expr(program, *base, found),
        IrExpr::ScalarText { value } | IrExpr::ArrayElements { value, .. } => {
            walk_expr(program, *value, found)
        }
        IrExpr::MathOperation { operands, .. } => {
            for operand in operands {
                walk_expr(program, *operand, found);
            }
        }
        IrExpr::ForeignField { base, .. } | IrExpr::ForeignMemberAddress { base, .. } => {
            walk_expr(program, *base, found)
        }
        IrExpr::ForeignElement { base, index, .. } => {
            walk_expr(program, *base, found);
            walk_expr(program, *index, found);
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
        IrExpr::MainThreadCall { function, args, .. } => {
            found.insert(*function);
            for arg in args {
                walk_expr(program, *arg, found);
            }
        }
        IrExpr::MainThreadJoin { handle, .. } => walk_expr(program, *handle, found),
        IrExpr::ArrayLen { array } => walk_expr(program, *array, found),
        IrExpr::StringLen { text } => walk_expr(program, *text, found),
        IrExpr::StringOf { value } => walk_expr(program, *value, found),
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
            for &argument in arguments {
                walk_expr(program, argument, found);
            }
        }
        IrExpr::StringSubstring { text, start, end } => {
            walk_expr(program, *text, found);
            walk_expr(program, *start, found);
            walk_expr(program, *end, found);
        }
        IrExpr::CStringNew { text } => walk_expr(program, *text, found),
        IrExpr::CLayoutAddress { value, .. } => walk_expr(program, *value, found),
        IrExpr::FileSystem { args, .. } => {
            for &arg in args {
                walk_expr(program, arg, found);
            }
        }
        IrExpr::Compiler { args, .. } | IrExpr::Env { args, .. } => {
            for &arg in args {
                walk_expr(program, arg, found);
            }
        }
        IrExpr::ArrayAppend { place, value } => {
            walk_place(program, place, found);
            walk_expr(program, *value, found);
        }
        IrExpr::Convert { operand, .. } => walk_expr(program, *operand, found),
        // Boxing a `var` wraps the value it boxes, so a call inside that value
        // is reachable exactly as it would be anywhere else.
        IrExpr::CellNew { value, .. } => walk_expr(program, *value, found),
        // An erasure wraps the value it erases, and a call inside that value is
        // reachable exactly as it would be anywhere else.
        IrExpr::IntoAny { value, .. } => walk_expr(program, *value, found),
        IrExpr::NativeState { value, .. } => walk_expr(program, *value, found),
        IrExpr::NativeUserData { state } => walk_expr(program, *state, found),
        IrExpr::NativeRecover { raw, .. } => walk_expr(program, *raw, found),
        IrExpr::NativeStateRetain { token } | IrExpr::NativeStateRelease { token } => {
            walk_expr(program, *token, found)
        }
        // Leaves: nothing inside can be a call.
        IrExpr::Int(_)
        | IrExpr::Float(_)
        | IrExpr::Bool(_)
        | IrExpr::Str(_)
        | IrExpr::RawPtrNull
        | IrExpr::CellNull { .. }
        | IrExpr::ForeignCallbackPtr { .. }
        | IrExpr::Local(_)
        | IrExpr::CellGet { .. } => {}
    }
}
