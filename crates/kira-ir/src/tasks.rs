//! The synthesized task executor: the async spine's scheduling policy, written
//! as IR rather than as runtime code.
//!
//! `Task { … }`, `.await`, `.detach()`, `.requestCancel()`, `taskYield()`, and
//! `taskSleep(ms)` each lower to a call to one of the functions this module
//! builds, and those functions reach the task table through
//! [`IrExpr::TaskOp`] and nothing else. Every backend therefore runs the *same*
//! scheduler: the VM interprets these functions and the native backend compiles
//! them, so "which task runs next" cannot drift between engines, because there
//! is only one copy of it and it is not in either engine.
//!
//! # What a drive is
//!
//! A driven task runs to completion, and a suspend point inside it hands the
//! next queued task a turn *nested* on the driver's stack — the fallback shape
//! the language documents for a body no state-machine transform applies to.
//! With one task per stack level, three tasks yielding to each other interleave
//! exactly as a round-robin queue orders them, which is what the observable
//! results depend on; what a saved-frame transform would add is bounded stack
//! depth, not a different answer.
//!
//! # The dispatcher
//!
//! A task body is a call to a named function with `Int`/`Float` parameters, so
//! one generated function — [`TaskFns::STEP`] — can hold a branch per spawned
//! target and read that target's arguments out of the task's slots. A `Float`
//! travels through a slot as its IEEE-754 bits, converted at both ends, because
//! a slot is one machine word and the conversion already exists in the IR.

use kira_runtime_abi::{Execution, TaskPrim};
use kira_semantics_model::hir::ConvertKind;
use kira_semantics_model::{FloatSpelling, Type};

use crate::ir::{IrBinOp, IrCallee, IrExpr, IrExprId, IrFunction, IrProgram, IrStmt};

/// How many argument slots one spawned task carries.
///
/// Mirrors [`kira_runtime_abi::TASK_SLOTS`]: the spawn helper takes exactly
/// this many arguments, so one helper serves every arity the analyzer admits.
const SLOTS: usize = kira_runtime_abi::TASK_SLOTS;

/// Where each synthesized function sits, relative to the first one.
///
/// The offsets are fixed and known before lowering starts, which is what lets a
/// `.await` lower to a call to a function that has not been built yet.
pub(crate) struct TaskFns;

impl TaskFns {
    /// `__kira_task_spawn(target, a0 … a7) -> Int`.
    pub(crate) const SPAWN: u32 = 0;
    /// `__kira_task_step(handle) -> Int` — the dispatcher.
    pub(crate) const STEP: u32 = 1;
    /// `__kira_task_await(handle) -> Int`.
    pub(crate) const AWAIT: u32 = 2;
    /// `__kira_task_detach(handle)`.
    pub(crate) const DETACH: u32 = 3;
    /// `__kira_task_cancel(handle)`.
    pub(crate) const CANCEL: u32 = 4;
    /// `__kira_task_yield()`.
    pub(crate) const YIELD: u32 = 5;
    /// `__kira_task_sleep(ms)`.
    pub(crate) const SLEEP: u32 = 6;
    /// How many functions the spine adds.
    pub(crate) const COUNT: u32 = 7;
}

/// What one spawn target needs to be dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskTargetInfo {
    /// The IR function the task body calls.
    pub(crate) function: u32,
    /// Its parameter types, in order.
    pub(crate) params: Vec<Type>,
    /// Its result type; [`Type::Void`] joins as `Int` `0`.
    pub(crate) result: Type,
}

/// The spawn targets a program used, in the order they were first spawned.
///
/// A target's dispatcher arm is its position here plus one; arm `0` is reserved
/// for a task whose body is a literal the spawn already evaluated.
#[derive(Debug, Default)]
pub(crate) struct TaskTargets {
    rows: Vec<TaskTargetInfo>,
}

impl TaskTargets {
    /// The arm `function` dispatches through, adding it if it is new.
    pub(crate) fn arm_for(&mut self, function: u32, params: Vec<Type>, result: Type) -> i64 {
        if let Some(index) = self.rows.iter().position(|row| row.function == function) {
            return index as i64 + 1;
        }
        self.rows.push(TaskTargetInfo {
            function,
            params,
            result,
        });
        self.rows.len() as i64
    }
}

/// Builds IR expressions and statements into one program's arena.
struct Build<'a> {
    program: &'a mut IrProgram,
}

impl Build<'_> {
    /// Allocates one expression.
    fn expr(&mut self, node: IrExpr) -> IrExprId {
        self.program.exprs.alloc(node)
    }

    /// The `Int` constant `value`.
    fn int(&mut self, value: i64) -> IrExprId {
        self.expr(IrExpr::Int(value))
    }

    /// A read of local slot `slot`.
    fn local(&mut self, slot: u32) -> IrExprId {
        self.expr(IrExpr::Local(slot))
    }

    /// A task primitive with the operands given, zero-filled to three.
    fn op(&mut self, prim: TaskPrim, operands: &[IrExprId]) -> IrExprId {
        let a = match operands.first() {
            Some(id) => *id,
            None => self.int(0),
        };
        let b = match operands.get(1) {
            Some(id) => *id,
            None => self.int(0),
        };
        let c = match operands.get(2) {
            Some(id) => *id,
            None => self.int(0),
        };
        self.expr(IrExpr::TaskOp {
            prim,
            operands: [a, b, c],
        })
    }

    /// `lhs == rhs` on two `Int`s.
    fn eq_int(&mut self, lhs: IrExprId, rhs: IrExprId) -> IrExprId {
        self.expr(IrExpr::Binary {
            op: IrBinOp::EqInt,
            lhs,
            rhs,
        })
    }

    /// A call to a synthesized or user function.
    fn call(&mut self, function: u32, args: Vec<IrExprId>, result: Type) -> IrExprId {
        self.expr(IrExpr::Call {
            callee: IrCallee::User(function),
            args,
            result,
            writebacks: Vec::new(),
        })
    }
}

/// Builds every function of the spine, given where the first one sits.
///
/// `base` is the index the spine's functions start at, which is the count of
/// the program's own functions: they are appended, never interleaved, so an
/// already-lowered call to `base + TaskFns::AWAIT` resolves to the right row.
pub(crate) fn synthesize(program: &mut IrProgram, base: u32, targets: &TaskTargets) {
    let rows = targets.rows.clone();
    let mut build = Build { program };
    let spawn = build_spawn(&mut build);
    let step = build_step(&mut build, &rows);
    let join = build_await(&mut build, base);
    let detach = build_detach(&mut build, base);
    let cancel = build_cancel(&mut build);
    let yielding = build_yield(&mut build, base);
    let sleep = build_sleep(&mut build, base);
    // Typed at the count the offsets are numbered against, so adding a helper
    // without renumbering fails to compile rather than shifting every already
    // lowered call by one.
    let spine: [IrFunction; TaskFns::COUNT as usize] =
        [spawn, step, join, detach, cancel, yielding, sleep];
    program.functions.extend(spine);
}

/// A function skeleton with `param_count` `Int` parameters and `extra` `Int`
/// scratch locals.
fn int_function(
    name: &str,
    param_count: u32,
    extra: u32,
    return_type: Type,
    body: Vec<IrStmt>,
) -> IrFunction {
    let slots = (param_count + extra) as usize;
    IrFunction {
        name: name.to_owned(),
        param_count,
        locals: vec![Type::INT; slots],
        native_state_locals: vec![None; slots],
        return_type,
        // Inherited, so the spine runs wherever the build puts unannotated
        // code: native under `--backend llvm`, the VM under `--backend vm`, and
        // the VM half under `--backend hybrid`. One table, one engine, whatever
        // the build is.
        execution: Execution::Inherited,
        by_reference_params: Vec::new(),
        by_pointer_params: Vec::new(),
        body,
    }
}

/// `__kira_task_spawn(target, a0 … a7) -> Int`.
///
/// One helper for every arity: the analyzer already bounded a task body's
/// parameter count by [`SLOTS`], and an unused slot is written `0`. That is
/// what keeps the spawn surface at one generated function rather than one per
/// arity.
fn build_spawn(build: &mut Build<'_>) -> IrFunction {
    let param_count = 1 + SLOTS as u32;
    let handle_slot = param_count;
    let target = build.local(0);
    let spawned = build.op(TaskPrim::Spawn, &[target]);
    let mut body = vec![IrStmt::Let {
        local: handle_slot,
        init: spawned,
    }];
    for slot in 0..SLOTS {
        let handle = build.local(handle_slot);
        let index = build.int(slot as i64);
        let value = build.local(slot as u32 + 1);
        let write = build.op(TaskPrim::SetArg, &[handle, index, value]);
        body.push(IrStmt::Eval { expr: write });
    }
    let handle = build.local(handle_slot);
    body.push(IrStmt::Return {
        value: Some(handle),
    });
    int_function("__kira_task_spawn", param_count, 1, Type::INT, body)
}

/// `__kira_task_step(handle) -> Int` — run one task's body to completion.
fn build_step(build: &mut Build<'_>, rows: &[TaskTargetInfo]) -> IrFunction {
    let mut body = Vec::new();
    // Arm `0`: the body was a literal, already sitting in slot 0.
    let handle = build.local(0);
    let arm = build.op(TaskPrim::TargetOf, &[handle]);
    let zero = build.int(0);
    let is_value = build.eq_int(arm, zero);
    let handle = build.local(0);
    let slot = build.int(0);
    let value = build.op(TaskPrim::SlotGet, &[handle, slot]);
    body.push(IrStmt::If {
        cond: is_value,
        then_body: vec![IrStmt::Return { value: Some(value) }],
        else_body: Vec::new(),
    });
    for (index, row) in rows.iter().enumerate() {
        let handle = build.local(0);
        let arm = build.op(TaskPrim::TargetOf, &[handle]);
        let wanted = build.int(index as i64 + 1);
        let matches = build.eq_int(arm, wanted);
        let mut args = Vec::with_capacity(row.params.len());
        for (slot, param) in row.params.iter().enumerate() {
            let handle = build.local(0);
            let index = build.int(slot as i64);
            let raw = build.op(TaskPrim::SlotGet, &[handle, index]);
            args.push(match param {
                // A slot is one machine word, so a `Float` argument travels as
                // its bit pattern and is rebuilt here.
                Type::Float(_) => build.expr(IrExpr::Convert {
                    operand: raw,
                    kind: ConvertKind::BitsToFloat,
                    ty: Type::Float(FloatSpelling::Plain),
                }),
                _ => raw,
            });
        }
        let call = build.call(row.function, args, row.result);
        let then_body = match row.result {
            // A `Void` body joins as `0`: the join is a sequencing point, and
            // sequencing still has to hand back a value.
            Type::Void => vec![IrStmt::Eval { expr: call }, {
                let zero = build.int(0);
                IrStmt::Return { value: Some(zero) }
            }],
            Type::Float(_) => {
                let bits = build.expr(IrExpr::Convert {
                    operand: call,
                    kind: ConvertKind::FloatToBits,
                    ty: Type::INT,
                });
                vec![IrStmt::Return { value: Some(bits) }]
            }
            _ => vec![IrStmt::Return { value: Some(call) }],
        };
        body.push(IrStmt::If {
            cond: matches,
            then_body,
            else_body: Vec::new(),
        });
    }
    // Unreachable for a well-formed module: every spawn recorded its arm. A
    // value rather than a trap, because a dispatcher that cannot fail is one
    // fewer failure mode to keep in step across two engines.
    let fallback = build.int(0);
    body.push(IrStmt::Return {
        value: Some(fallback),
    });
    int_function("__kira_task_step", 1, 0, Type::INT, body)
}

/// `__kira_task_await(handle) -> Int`.
fn build_await(build: &mut Build<'_>, base: u32) -> IrFunction {
    let body = drive_then(build, base, TaskPrim::BeginJoin, |build| {
        let handle = build.local(0);
        let result = build.op(TaskPrim::TakeResult, &[handle]);
        vec![IrStmt::Return {
            value: Some(result),
        }]
    });
    int_function("__kira_task_await", 1, 1, Type::INT, body)
}

/// `__kira_task_detach(handle)`.
fn build_detach(build: &mut Build<'_>, base: u32) -> IrFunction {
    let body = drive_then(build, base, TaskPrim::BeginDetach, |build| {
        let handle = build.local(0);
        let mark = build.op(TaskPrim::MarkDetached, &[handle]);
        vec![IrStmt::Eval { expr: mark }, IrStmt::Return { value: None }]
    });
    int_function("__kira_task_detach", 1, 1, Type::Void, body)
}

/// The shared shape of a join and a detach: claim the task, drive it if its
/// body has not run, then do whatever the caller does with the outcome.
fn drive_then(
    build: &mut Build<'_>,
    base: u32,
    claim: TaskPrim,
    tail: impl FnOnce(&mut Build<'_>) -> Vec<IrStmt>,
) -> Vec<IrStmt> {
    let handle = build.local(0);
    let claimed = build.op(claim, &[handle]);
    let one = build.int(1);
    let needs_drive = build.eq_int(claimed, one);
    let handle = build.local(0);
    let stepped = build.call(base + TaskFns::STEP, vec![handle], Type::INT);
    let handle = build.local(0);
    let value = build.local(1);
    let complete = build.op(TaskPrim::Complete, &[handle, value]);
    let mut body = vec![IrStmt::If {
        cond: needs_drive,
        then_body: vec![
            IrStmt::Let {
                local: 1,
                init: stepped,
            },
            IrStmt::Eval { expr: complete },
        ],
        else_body: Vec::new(),
    }];
    body.extend(tail(build));
    body
}

/// `__kira_task_cancel(handle)`.
fn build_cancel(build: &mut Build<'_>) -> IrFunction {
    let handle = build.local(0);
    let cancel = build.op(TaskPrim::Cancel, &[handle]);
    let body = vec![
        IrStmt::Eval { expr: cancel },
        IrStmt::Return { value: None },
    ];
    int_function("__kira_task_cancel", 1, 0, Type::Void, body)
}

/// `__kira_task_yield()` — hand the next queued task a turn.
///
/// With nothing queued this is a no-op, which is what makes `taskYield()` legal
/// outside a task body rather than a case the analyzer has to reject.
fn build_yield(build: &mut Build<'_>, base: u32) -> IrFunction {
    let picked = build.op(TaskPrim::PickReady, &[]);
    let next = build.local(0);
    let zero = build.int(0);
    let none_ready = build.eq_int(next, zero);
    let next = build.local(0);
    let stepped = build.call(base + TaskFns::STEP, vec![next], Type::INT);
    let next = build.local(0);
    let value = build.local(1);
    let complete = build.op(TaskPrim::Complete, &[next, value]);
    let body = vec![
        IrStmt::Let {
            local: 0,
            init: picked,
        },
        IrStmt::If {
            cond: none_ready,
            then_body: Vec::new(),
            else_body: vec![
                IrStmt::Let {
                    local: 1,
                    init: stepped,
                },
                IrStmt::Eval { expr: complete },
            ],
        },
        IrStmt::Return { value: None },
    ];
    int_function("__kira_task_yield", 0, 2, Type::Void, body)
}

/// `__kira_task_sleep(ms)` — move the virtual clock, then yield.
///
/// Nothing sleeps in real time. The clock is a counter the executor owns, so a
/// program orders its sleeping tasks the same way on every backend and a test
/// suite never waits.
fn build_sleep(build: &mut Build<'_>, base: u32) -> IrFunction {
    let ms = build.local(0);
    let advance = build.op(TaskPrim::AdvanceClock, &[ms]);
    let hand_over = build.call(base + TaskFns::YIELD, Vec::new(), Type::Void);
    let body = vec![
        IrStmt::Eval { expr: advance },
        IrStmt::Eval { expr: hand_over },
        IrStmt::Return { value: None },
    ];
    int_function("__kira_task_sleep", 1, 0, Type::Void, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_semantics_model::TypeTable;

    /// An empty program to append the spine onto.
    fn empty_program() -> IrProgram {
        IrProgram {
            functions: Vec::new(),
            types: TypeTable::default(),
            main: None,
            exports: Vec::new(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            exprs: la_arena::Arena::new(),
        }
    }

    #[test]
    fn the_spine_lands_at_the_offsets_its_callers_lowered_against() {
        let mut program = empty_program();
        synthesize(&mut program, 0, &TaskTargets::default());
        assert_eq!(program.functions.len(), TaskFns::COUNT as usize);
        let names: Vec<&str> = program
            .functions
            .iter()
            .map(|function| function.name.as_str())
            .collect();
        // A `.await` lowered to a call at `base + AWAIT` before this ran, so an
        // offset that moved would silently call the wrong helper.
        assert_eq!(names[TaskFns::SPAWN as usize], "__kira_task_spawn");
        assert_eq!(names[TaskFns::STEP as usize], "__kira_task_step");
        assert_eq!(names[TaskFns::AWAIT as usize], "__kira_task_await");
        assert_eq!(names[TaskFns::DETACH as usize], "__kira_task_detach");
        assert_eq!(names[TaskFns::CANCEL as usize], "__kira_task_cancel");
        assert_eq!(names[TaskFns::YIELD as usize], "__kira_task_yield");
        assert_eq!(names[TaskFns::SLEEP as usize], "__kira_task_sleep");
    }

    #[test]
    fn the_spawn_helper_takes_one_arm_and_every_slot() {
        let mut program = empty_program();
        synthesize(&mut program, 0, &TaskTargets::default());
        let spawn = &program.functions[TaskFns::SPAWN as usize];
        assert_eq!(spawn.param_count as usize, 1 + SLOTS);
        // One scratch slot for the handle the spawn hands back.
        assert_eq!(spawn.locals.len(), 2 + SLOTS);
    }

    #[test]
    fn a_target_takes_one_arm_however_often_it_is_spawned() {
        let mut targets = TaskTargets::default();
        let first = targets.arm_for(3, vec![Type::INT], Type::INT);
        let again = targets.arm_for(3, vec![Type::INT], Type::INT);
        let other = targets.arm_for(4, Vec::new(), Type::Void);
        assert_eq!(first, 1, "arm 0 is reserved for a literal body");
        assert_eq!(again, first);
        assert_eq!(other, 2);
    }

    #[test]
    fn every_spawned_target_gets_a_dispatcher_arm() {
        let mut targets = TaskTargets::default();
        targets.arm_for(0, vec![Type::INT, Type::INT], Type::INT);
        targets.arm_for(1, Vec::new(), Type::Void);
        let mut program = empty_program();
        synthesize(&mut program, 0, &targets);
        let step = &program.functions[TaskFns::STEP as usize];
        // One `if` per arm plus the literal arm, then the fallback return.
        let branches = step
            .body
            .iter()
            .filter(|stmt| matches!(stmt, IrStmt::If { .. }))
            .count();
        assert_eq!(branches, 3);
    }
}
