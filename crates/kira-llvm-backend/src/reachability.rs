use std::collections::{BTreeSet, VecDeque};

use kira_ir::{IrCallee, IrExpr, IrExprId, IrPlace, IrProgram, IrStmt};

#[derive(Default)]
struct BodyFacts {
    calls: BTreeSet<u32>,
    uses_compiler: bool,
}

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
        for callee in body_facts(program, &program.functions[index].body).calls {
            pending.push_back(callee);
        }
    }
    reachable
}

pub(crate) fn hybrid_native_functions(program: &IrProgram) -> Vec<bool> {
    if program.main.is_some() {
        native_functions(program)
    } else {
        vec![true; program.functions.len()]
    }
}

pub(crate) fn hybrid_uses_compiler(program: &IrProgram) -> bool {
    let reachable = hybrid_native_functions(program);
    program
        .functions
        .iter()
        .enumerate()
        .any(|(index, function)| {
            function
                .execution
                .resolve(kira_runtime_abi::Execution::Runtime)
                == kira_runtime_abi::Execution::Native
                && reachable.get(index).copied().unwrap_or(false)
                && body_facts(program, &function.body).uses_compiler
        })
}

fn body_facts(program: &IrProgram, body: &[IrStmt]) -> BodyFacts {
    let mut facts = BodyFacts::default();
    for statement in body {
        walk_stmt(program, statement, &mut facts);
    }
    facts
}

fn walk_stmt(program: &IrProgram, statement: &IrStmt, facts: &mut BodyFacts) {
    match statement {
        IrStmt::Let { init, .. } => walk_expr(program, *init, facts),
        IrStmt::Assign { place, value } => {
            walk_place(program, place, facts);
            walk_expr(program, *value, facts);
        }
        IrStmt::Return { value } => {
            if let Some(value) = value {
                walk_expr(program, *value, facts);
            }
        }
        IrStmt::Eval { expr } => walk_expr(program, *expr, facts),
        IrStmt::CellSet { value, .. } => walk_expr(program, *value, facts),
        IrStmt::If {
            cond,
            then_body,
            else_body,
        } => {
            walk_expr(program, *cond, facts);
            for statement in then_body.iter().chain(else_body) {
                walk_stmt(program, statement, facts);
            }
        }
        IrStmt::Attempt { attempt } => {
            for step in &attempt.steps {
                walk_expr(program, step.error_condition, facts);
                for statement in step.setup.iter().chain(&step.handler).chain(&step.success) {
                    walk_stmt(program, statement, facts);
                }
            }
            for statement in &attempt.trailing {
                walk_stmt(program, statement, facts);
            }
        }
        IrStmt::While { cond, body } => {
            walk_expr(program, *cond, facts);
            for statement in body {
                walk_stmt(program, statement, facts);
            }
        }
        IrStmt::Break | IrStmt::Continue => {}
    }
}

fn walk_place(program: &IrProgram, place: &IrPlace, facts: &mut BodyFacts) {
    for index in place.indices() {
        walk_expr(program, index, facts);
    }
}

fn walk_expr(program: &IrProgram, id: IrExprId, facts: &mut BodyFacts) {
    match program.expr(id) {
        IrExpr::Call { callee, args, .. } => {
            if let IrCallee::User(index) = callee {
                facts.calls.insert(*index);
            }
            for arg in args {
                walk_expr(program, *arg, facts);
            }
        }
        IrExpr::Unary { operand, .. } => walk_expr(program, *operand, facts),
        IrExpr::Binary { lhs, rhs, .. } => {
            walk_expr(program, *lhs, facts);
            walk_expr(program, *rhs, facts);
        }
        IrExpr::Select {
            cond,
            then,
            otherwise,
            ..
        } => {
            walk_expr(program, *cond, facts);
            walk_expr(program, *then, facts);
            walk_expr(program, *otherwise, facts);
        }
        IrExpr::StructNew { fields, .. } => {
            for field in fields {
                walk_expr(program, *field, facts);
            }
        }
        IrExpr::EnumNew { payload, .. } => {
            if let Some(payload) = payload {
                walk_expr(program, *payload, facts);
            }
        }
        IrExpr::EnumTag { value } | IrExpr::EnumPayload { value, .. } => {
            walk_expr(program, *value, facts)
        }
        IrExpr::Field { base, .. }
        | IrExpr::ForeignField { base, .. }
        | IrExpr::ForeignMemberAddress { base, .. } => walk_expr(program, *base, facts),
        IrExpr::ForeignElement { base, index, .. } => {
            walk_expr(program, *base, facts);
            walk_expr(program, *index, facts);
        }
        IrExpr::MathOperation { value, .. }
        | IrExpr::ScalarText { value }
        | IrExpr::ArrayElements { value, .. }
        | IrExpr::StringLen { text: value }
        | IrExpr::StringOf { value }
        | IrExpr::ArrayLen { array: value } => walk_expr(program, *value, facts),
        IrExpr::StringCharAt { text, index } => {
            walk_expr(program, *text, facts);
            walk_expr(program, *index, facts);
        }
        IrExpr::StringIndexOf { text, needle } => {
            walk_expr(program, *text, facts);
            walk_expr(program, *needle, facts);
        }
        IrExpr::StringOperation {
            text, arguments, ..
        } => {
            walk_expr(program, *text, facts);
            for argument in arguments {
                walk_expr(program, *argument, facts);
            }
        }
        IrExpr::StringSubstring { text, start, end } => {
            walk_expr(program, *text, facts);
            walk_expr(program, *start, facts);
            walk_expr(program, *end, facts);
        }
        IrExpr::CStringNew { text } => walk_expr(program, *text, facts),
        IrExpr::CLayoutAddress { value, .. } => walk_expr(program, *value, facts),
        IrExpr::FileSystem { args, .. } | IrExpr::Env { args, .. } => {
            for arg in args {
                walk_expr(program, *arg, facts);
            }
        }
        IrExpr::Compiler { args, .. } => {
            facts.uses_compiler = true;
            for arg in args {
                walk_expr(program, *arg, facts);
            }
        }
        IrExpr::ArrayNew { elements, .. } => {
            for element in elements {
                walk_expr(program, *element, facts);
            }
        }
        IrExpr::Index { base, index, .. } => {
            walk_expr(program, *base, facts);
            walk_expr(program, *index, facts);
        }
        IrExpr::TaskOp { operands, .. } => {
            for operand in operands {
                walk_expr(program, *operand, facts);
            }
        }
        IrExpr::ArrayAppend { place, value } => {
            walk_place(program, place, facts);
            walk_expr(program, *value, facts);
        }
        IrExpr::Convert { operand, .. }
        | IrExpr::CellNew { value: operand, .. }
        | IrExpr::IntoAny { value: operand, .. }
        | IrExpr::Widen { value: operand, .. }
        | IrExpr::NativeState { value: operand, .. }
        | IrExpr::NativeUserData { state: operand }
        | IrExpr::NativeRecover { raw: operand, .. }
        | IrExpr::NativeStateFree { token: operand } => walk_expr(program, *operand, facts),
        IrExpr::ForeignCallbackPtr { .. }
        | IrExpr::Int(_)
        | IrExpr::Float(_)
        | IrExpr::Bool(_)
        | IrExpr::Str(_)
        | IrExpr::RawPtrNull
        | IrExpr::CellNull { .. }
        | IrExpr::Local(_)
        | IrExpr::CellGet { .. } => {}
    }
}
