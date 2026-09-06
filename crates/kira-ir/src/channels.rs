//! The synthesized channel receiver: waiting, written as IR rather than as
//! runtime code.
//!
//! `receiver.receive()` lowers to a call to one of the functions this module
//! builds, and those functions reach the channel table through
//! [`IrExpr::ChannelOp`] and nothing else. Every backend therefore runs the
//! *same* wait: the VM interprets these functions and the native backend
//! compiles them, so "when does a receive come back" cannot drift between
//! engines, because there is only one copy of it and it is not in either
//! engine.
//!
//! This is the same argument [`crate::tasks`] makes, and it is made the same
//! way for the same reason.
//!
//! # What waiting is
//!
//! An empty channel whose sender is still live hands the next runnable task a
//! turn and asks again. That is what makes a receive a suspension point rather
//! than a spin: the value a receiver is waiting for can only come from work
//! that has not run yet, so a receive that did not yield would be a deadlock
//! written as a loop.
//!
//! # Why one function per payload
//!
//! The wrapping is what differs: a receive answers `Ok(payload)` or
//! `Error(ChannelError.Closed)`, and the result row is minted per payload
//! type. The waiting is identical, so it is written once here and instantiated
//! per row, exactly as analysis mints one result row per payload.

use kira_runtime_abi::{ChannelPrim, Execution, TaskPrim};
use kira_semantics_model::channel as wire;
use kira_semantics_model::{EnumId, Type};

use crate::ir::{IrBinOp, IrCallee, IrExpr, IrExprId, IrFunction, IrProgram, IrStmt};

/// What one payload's receiver needs to know to be built.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ReceiverRow {
    /// The result row the function returns.
    pub(crate) result: EnumId,
    /// The failure row the error variant carries.
    pub(crate) failure: EnumId,
    /// The payload type, which is the success variant's.
    pub(crate) payload: Type,
    /// How the queued word becomes the payload again.
    pub(crate) wire: wire::Crossing,
}

/// Builds one `__kira_channel_receive_N(receiver)` per row.
///
/// `base` is the index the first of them sits at, and `task_yield` the index of
/// the task spine's yield helper: they are appended after the spine, so both
/// are known before lowering starts and an already-lowered call resolves.
pub(crate) fn synthesize(program: &mut IrProgram, rows: &[ReceiverRow], task_step: u32) {
    if rows.is_empty() {
        return;
    }
    // The step helper sits first, so a receiver built below can call it at a
    // row it knows before either exists.
    let step_at = program.functions.len() as u32;
    let step = build_step(program, task_step);
    program.functions.push(step);
    for (index, row) in rows.iter().enumerate() {
        let function = build_receive(program, *row, index, step_at);
        program.functions.push(function);
    }
}

/// How many functions sit ahead of the first receiver.
pub(crate) const STEP_HELPERS: u32 = 1;

/// `__kira_channel_step() -> Int`: run one queued task, answering whether there
/// was one.
///
/// The same body the task spine's yield has, with the one fact a waiting
/// receive needs added: whether anything ran. A receive that yields into an
/// empty scheduler is waiting for a value nothing can send, and it has to be
/// able to tell that from a turn that made progress.
fn build_step(program: &mut IrProgram, task_step: u32) -> IrFunction {
    let mut build = Build { program };
    let picked = build.op_task(TaskPrim::PickReady, &[]);
    let next = build.expr(IrExpr::Local(0));
    let zero = build.int(0);
    let none_ready = build.eq(next, zero);
    let next = build.expr(IrExpr::Local(0));
    let stepped = build.call_int(task_step, vec![next]);
    let next = build.expr(IrExpr::Local(0));
    let value = build.expr(IrExpr::Local(1));
    let complete = build.op_task(TaskPrim::Complete, &[next, value]);
    let nothing = build.int(0);
    let ran = build.int(1);
    let body = vec![
        IrStmt::Let {
            local: 0,
            init: picked,
        },
        IrStmt::If {
            cond: none_ready,
            then_body: vec![IrStmt::Return {
                value: Some(nothing),
            }],
            else_body: Vec::new(),
        },
        IrStmt::Let {
            local: 1,
            init: stepped,
        },
        IrStmt::Eval { expr: complete },
        IrStmt::Return { value: Some(ran) },
    ];
    IrFunction {
        name: "__kira_channel_step".to_owned(),
        param_count: 0,
        locals: vec![Type::INT, Type::INT],
        native_state_locals: vec![None, None],
        return_type: Type::INT,
        execution: Execution::Inherited,
        is_main_thread: false,
        by_reference_params: Vec::new(),
        by_pointer_params: Vec::new(),
        body,
    }
}

/// `__kira_channel_receive_N(receiver) -> ReceiveResult<T>`.
///
/// ```text
/// var status = Poll(receiver)
/// while status == EMPTY {
///     if __kira_channel_step() == 0 { Deadlock() }
///     status = Poll(receiver)
/// }
/// if status == READY {
///     return Ok(Take(receiver))
/// }
/// return Error(Closed)
/// ```
///
/// The poll is repeated after the yield rather than trusted from before it:
/// the whole point of the yield is that other work runs, and that work is what
/// changes the answer.
fn build_receive(
    program: &mut IrProgram,
    row: ReceiverRow,
    index: usize,
    step_at: u32,
) -> IrFunction {
    let mut build = Build { program };
    // Slot 0 is the receiver end; slot 1 holds the poll status across the wait.
    let status_slot = 1;

    let first_poll = build.poll(0);
    let empty = build.status_is(status_slot, wire::POLL_EMPTY);
    // Waiting means letting other work run. Nothing runnable and an empty live
    // channel is a receive nothing can answer, and a hang is not an answer.
    let stepped = build.call_int(step_at, Vec::new());
    let none = build.int(0);
    let nothing_ran = build.eq(stepped, none);
    let deadlock = build.op(ChannelPrim::Deadlock, &[]);
    let repoll = build.poll(0);
    let ready = build.status_is(status_slot, wire::POLL_READY);

    let end = build.expr(IrExpr::Local(0));
    let taken = build.op(ChannelPrim::Take, &[end]);
    // Whatever the sender turned the value into, turned back. A float crossed
    // as its bits; a value that owns storage crossed as a token, and reading it
    // out is the last thing that token is needed for, so it is released here
    // rather than left for a receiver to remember.
    // A boxed payload needs two slots and three statements rather than one
    // expression: the token has to outlive the recovery that reads through it
    // and be released once it has, which is a sequence and not a conversion.
    let token_slot = 2;
    let value_slot = 3;
    let (taken, unbox, extra_locals) = match row.wire {
        wire::Crossing::Word => (taken, Vec::new(), 0),
        wire::Crossing::FloatBits => (
            build.expr(IrExpr::Convert {
                operand: taken,
                kind: kira_semantics_model::hir::ConvertKind::BitsToFloat,
                ty: row.payload,
            }),
            Vec::new(),
            0,
        ),
        wire::Crossing::Boxed(type_id) => {
            let token = build.expr(IrExpr::Local(token_slot));
            let recovered = build.expr(IrExpr::NativeRecover {
                raw: token,
                type_id,
                ty: row.payload,
            });
            let token_again = build.expr(IrExpr::Local(token_slot));
            let release = build.expr(IrExpr::NativeStateRelease { token: token_again });
            let value = build.expr(IrExpr::Local(value_slot));
            (
                value,
                vec![
                    IrStmt::Let {
                        local: token_slot,
                        init: taken,
                    },
                    IrStmt::Let {
                        local: value_slot,
                        init: recovered,
                    },
                    // The queue's owner, given up now that the value is out.
                    // Nothing else holds one, so this is what frees the
                    // storage a delivered payload was travelling in.
                    IrStmt::Eval { expr: release },
                ],
                2,
            )
        }
    };
    let ok = build.expr(IrExpr::EnumNew {
        enum_id: row.result,
        tag: wire::OK_TAG,
        payload: Some(taken),
    });
    let closed = build.expr(IrExpr::EnumNew {
        enum_id: row.failure,
        tag: wire::CLOSED_TAG,
        payload: None,
    });
    let error = build.expr(IrExpr::EnumNew {
        enum_id: row.result,
        tag: wire::ERROR_TAG,
        payload: Some(closed),
    });

    let body = vec![
        IrStmt::Let {
            local: status_slot,
            init: first_poll,
        },
        IrStmt::While {
            cond: empty,
            body: vec![
                IrStmt::If {
                    cond: nothing_ran,
                    then_body: vec![IrStmt::Eval { expr: deadlock }],
                    else_body: Vec::new(),
                },
                IrStmt::Assign {
                    place: crate::ir::IrPlace {
                        local: status_slot,
                        path: Vec::new(),
                    },
                    value: repoll,
                },
            ],
        },
        IrStmt::If {
            cond: ready,
            then_body: {
                let mut arm = unbox;
                arm.push(IrStmt::Return { value: Some(ok) });
                arm
            },
            else_body: Vec::new(),
        },
        IrStmt::Return { value: Some(error) },
    ];

    IrFunction {
        name: format!("__kira_channel_receive_{index}"),
        param_count: 1,
        locals: {
            let mut locals = vec![Type::INT, Type::INT];
            if extra_locals == 2 {
                locals.push(Type::INT);
                locals.push(row.payload);
            }
            locals
        },
        native_state_locals: vec![None; 2 + extra_locals],
        return_type: Type::Enum(row.result),
        // Inherited, so a receive runs wherever the build puts unannotated
        // code, exactly as the task spine does.
        execution: Execution::Inherited,
        is_main_thread: false,
        by_reference_params: Vec::new(),
        by_pointer_params: Vec::new(),
        body,
    }
}

/// The same small builder [`crate::tasks`] uses, over channel primitives.
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

    /// A channel primitive with the operands given, zero-filled to three.
    fn op(&mut self, prim: ChannelPrim, operands: &[IrExprId]) -> IrExprId {
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
        self.expr(IrExpr::ChannelOp {
            prim,
            operands: [a, b, c],
        })
    }

    /// `Poll(local(slot))`.
    fn poll(&mut self, slot: u32) -> IrExprId {
        let end = self.expr(IrExpr::Local(slot));
        self.op(ChannelPrim::Poll, &[end])
    }

    /// `local(slot) == status`.
    fn status_is(&mut self, slot: u32, status: i64) -> IrExprId {
        let lhs = self.expr(IrExpr::Local(slot));
        let rhs = self.int(status);
        self.eq(lhs, rhs)
    }

    /// `lhs == rhs` on two `Int`s.
    fn eq(&mut self, lhs: IrExprId, rhs: IrExprId) -> IrExprId {
        self.expr(IrExpr::Binary {
            op: IrBinOp::EqInt,
            lhs,
            rhs,
            ty: Type::Bool,
        })
    }

    /// A task primitive with the operands given, zero-filled to three.
    fn op_task(&mut self, prim: TaskPrim, operands: &[IrExprId]) -> IrExprId {
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

    /// A call to a synthesized function returning an `Int`.
    fn call_int(&mut self, function: u32, args: Vec<IrExprId>) -> IrExprId {
        self.expr(IrExpr::Call {
            callee: IrCallee::User(function),
            args,
            result: Type::INT,
            writebacks: Vec::new(),
        })
    }
}
