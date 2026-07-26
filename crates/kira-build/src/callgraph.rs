//! Which user functions a function's body calls.
//!
//! One walk over a lowered body, collecting [`IrCallee::User`] targets. It
//! exists for one question the hybrid library build has to answer before it
//! writes anything — *does this `@Native` function call back into the runtime?*
//! — and it answers it from the IR rather than from a generated artifact,
//! because a refusal that names a function is only useful before the build is
//! over.
//!
//! # Why it is exhaustive rather than a shortcut
//!
//! Every expression arm is matched by name, with no `_` catch-all. A new
//! expression kind that can contain a call must then fail to compile here rather
//! than silently drop out of the walk — which would turn this from a refusal
//! into a hole, quietly, in whichever change added the arm.

use std::collections::BTreeSet;

use kira_ir::{IrCallee, IrExpr, IrExprId, IrPlace, IrProgram, IrStmt};

/// Every user function `body` calls, directly, by index.
///
/// Direct calls only: a `@Native` function that calls another `@Native` function
/// that calls a `@Runtime` one is caught because *that* function is walked too,
/// so the whole native half is covered one function at a time.
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
        IrExpr::EnumTag { value } => walk_expr(program, *value, found),
        IrExpr::EnumPayload { value, .. } => walk_expr(program, *value, found),
        IrExpr::Field { base, .. } => walk_expr(program, *base, found),
        IrExpr::ArrayNew { elements, .. } => {
            for element in elements {
                walk_expr(program, *element, found);
            }
        }
        IrExpr::Index { base, index, .. } => {
            walk_expr(program, *base, found);
            walk_expr(program, *index, found);
        }
        IrExpr::ArrayLen { array } => walk_expr(program, *array, found),
        IrExpr::StringLen { text } => walk_expr(program, *text, found),
        IrExpr::CStringNew { text } => walk_expr(program, *text, found),
        IrExpr::CLayoutAddress { value, .. } => walk_expr(program, *value, found),
        IrExpr::FileSystem { args, .. } => {
            for &arg in args {
                walk_expr(program, arg, found);
            }
        }
        IrExpr::ArrayAppend { place, value } => {
            walk_place(program, place, found);
            walk_expr(program, *value, found);
        }
        IrExpr::Convert { operand, .. } => walk_expr(program, *operand, found),
        IrExpr::NativeState { value, .. } => walk_expr(program, *value, found),
        IrExpr::NativeUserData { state } => walk_expr(program, *state, found),
        IrExpr::NativeRecover { raw, .. } => walk_expr(program, *raw, found),
        IrExpr::NativeStateFree { token } => walk_expr(program, *token, found),
        // Leaves: nothing inside can be a call.
        IrExpr::Int(_)
        | IrExpr::Float(_)
        | IrExpr::Bool(_)
        | IrExpr::Str(_)
        | IrExpr::RawPtrNull
        | IrExpr::ForeignCallbackPtr { .. }
        | IrExpr::Local(_) => {}
    }
}
