//! Lowering from the typed HIR to the backend-facing IR.
//!
//! Lowering is a mechanical, total pass: it copies the resolved HIR into the
//! IR's own arena, translating builtin calls to [`IrCallee::Print`] and user
//! calls to slot-indexed [`IrCallee::User`].
//!
//! It carries the entrypoint across as an option rather than requiring one. A
//! library has no `@Main` by definition and still has every function in it
//! worth compiling, so "no entrypoint" is a property of the program the
//! backends read, not a reason to lower nothing. Whether a missing entrypoint
//! is an *error* was already decided upstream, by
//! [`kira_semantics::BuildKind`].

use kira_semantics_model::channel::Crossing;
use kira_semantics_model::hir::{
    Builtin, Callee, HirAttempt, HirExpr, HirExprId, HirPlace, HirPlaceStep, HirProgram, HirStmt,
    HirStmtId, TaskTarget,
};
use kira_semantics_model::{OwnershipMode, Type, TypeDescriptorTable, TypeTable};

use crate::tasks::TaskTargets;

use kira_runtime_abi::ChannelPrim;

use crate::ir::{
    IrAttempt, IrAttemptStep, IrBinOp, IrCallee, IrExport, IrExpr, IrExprId, IrForeignImport,
    IrFunction, IrPlace, IrPlaceStep, IrProgram, IrStmt, IrWriteback,
};

/// Lowers an analyzed program to IR.
///
/// Total: every analyzed program lowers, entrypoint or not. `ir.main` is
/// `None` for a library.
pub fn lower(program: &HirProgram) -> IrProgram {
    let mut ir = IrProgram {
        functions: Vec::with_capacity(program.functions.len()),
        types: program.types.clone(),
        descriptors: TypeDescriptorTable::new(),
        main: program.main.map(|main| main.0),
        main_thread_lifecycles: program
            .main_thread_lifecycles
            .iter()
            .map(|lifecycle| lifecycle.0)
            .collect(),
        exports: program
            .exports
            .iter()
            .map(|export| IrExport {
                kira_name: export.kira_name.clone(),
                exported_name: export.exported_name.clone(),
                function: export.function.0,
                params: export.params.clone(),
                result: export.result,
            })
            .collect(),
        foreign_imports: program
            .foreign
            .iter()
            .map(|foreign| IrForeignImport {
                name: foreign.kira_name.clone(),
                import: kira_runtime_abi::ForeignImport::new(
                    foreign.library.clone(),
                    foreign.symbol.clone(),
                    foreign.abi,
                    foreign.signature.clone(),
                ),
            })
            .collect(),
        foreign_aggregates: program.foreign_aggregates.clone(),
        foreign_callbacks: program.foreign_callbacks.clone(),
        exprs: la_arena::Arena::new(),
        constants: program
            .constants
            .iter()
            .map(|constant| crate::ir::IrConstant {
                name: constant.name.clone(),
                ty: constant.ty,
                init: constant.init.0,
            })
            .collect(),
    };
    // Task functions are appended after source functions. Reserve their base
    // index before lowering so `.await` can reference their eventual rows.
    let task_base = program.functions.len() as u32;
    let mut lowerer = Lowerer {
        hir: program,
        ir: &mut ir,
        aliases: std::collections::HashMap::new(),
        task_base,
        task_targets: TaskTargets::default(),
        channel_rows: Vec::new(),
        uses_tasks: false,
    };
    let functions: Vec<IrFunction> = program
        .functions
        .iter()
        .map(|function| lowerer.lower_function(function))
        .collect();
    let uses_tasks = lowerer.uses_tasks;
    let targets = std::mem::take(&mut lowerer.task_targets);
    let channel_rows = std::mem::take(&mut lowerer.channel_rows);
    ir.functions = functions;
    // A receive yields to the task spine while it waits, so a program with a
    // channel has the spine whether or not it wrote `Task`.
    if uses_tasks || !channel_rows.is_empty() {
        crate::tasks::synthesize(&mut ir, task_base, &targets);
        crate::channels::synthesize(
            &mut ir,
            &channel_rows,
            task_base + crate::tasks::TaskFns::STEP,
        );
        for function in ir.functions.iter_mut().skip(task_base as usize) {
            crate::mid::scope_releases(function, &ir.exprs, &ir.types);
        }
    }
    // Once every body has been lowered, so every type a program can ask about
    // has its row: what each of them conforms to.
    ir.descriptors.record_conformances(&program.conformances);
    // Last, once every function and every synthesized task helper exists: the
    // IR a backend consumes carries no `distinct` type, so nothing below here
    // has a distinct path to lay out, box, copy, release, or lower to C.
    crate::erase::erase_distinct_types(&mut ir);
    ir
}

struct Lowerer<'a> {
    hir: &'a HirProgram,
    ir: &'a mut IrProgram,
    /// The locals of the function being lowered that are a borrow under a
    /// second name, mapped to the local they name. See [`crate::borrow_alias`].
    aliases: std::collections::HashMap<u32, u32>,
    /// The row the task spine's first synthesized function will land at.
    task_base: u32,
    /// The spawn targets seen so far, each holding the dispatcher arm it took.
    task_targets: TaskTargets,
    /// The receive rows seen so far, in first-use order: one per payload type,
    /// each the function a receive of that payload calls.
    channel_rows: Vec<crate::channels::ReceiverRow>,
    /// Whether anything in the program reached the task spine.
    ///
    /// A program that never spawns, joins, or yields gets no synthesized
    /// functions at all: the spine is a feature a program opts into by using
    /// it, not seven dead rows in every module.
    uses_tasks: bool,
}

/// The parameter slots a function takes by reference, ascending.
///
/// Two declarations put a slot here and they are read from two places: a
/// mutating method's receiver is slot 0 and comes from the method flag, while a
/// `borrow mut` parameter announces itself on its own local. Both mean the same
/// thing below the IR — the caller hands over storage, not a copy.
fn by_reference_params(function: &kira_semantics_model::hir::HirFunction) -> Vec<u32> {
    let mut slots = Vec::new();
    if function.mutates_self {
        slots.push(0);
    }
    for slot in 0..function.param_count {
        if slots.contains(&slot) {
            continue;
        }
        if function
            .locals
            .get(slot as usize)
            .is_some_and(|local| local.ownership == OwnershipMode::BorrowMut)
        {
            slots.push(slot);
        }
    }
    slots.sort_unstable();
    slots
}

/// The parameter slots a read-only borrow can be lent through, ascending.
///
/// A `borrow` parameter of a type that costs something to copy: the caller
/// keeps the value, so the callee reads it where it lies instead of taking a
/// duplicate. A scalar is left alone — a register beats a pointer — and so is a
/// slot already taken by reference, which is a pointer for a stronger reason.
fn by_pointer_params(
    function: &kira_semantics_model::hir::HirFunction,
    types: &TypeTable,
    by_reference: &[u32],
) -> Vec<u32> {
    (0..function.param_count)
        .filter(|slot| !by_reference.contains(slot))
        .filter(|slot| {
            function.locals.get(*slot as usize).is_some_and(|local| {
                local.ownership == OwnershipMode::BorrowRead && worth_lending(local.ty, types)
            })
        })
        .collect()
}

/// Whether a value of this type costs enough to copy to be worth lending.
///
/// Anything with storage behind it — a string, an array, an enum box — and any
/// struct, whose fields may hold all three and which is copied field by field
/// either way.
fn worth_lending(ty: Type, types: &TypeTable) -> bool {
    use kira_semantics_model::Type;
    matches!(ty, Type::Struct(_)) || types.owns_heap(ty)
}

impl Lowerer<'_> {
    fn lower_function(&mut self, function: &kira_semantics_model::hir::HirFunction) -> IrFunction {
        self.aliases = crate::borrow_alias::borrow_aliases(self.hir, function);
        let by_reference = by_reference_params(function);
        let body = self.lower_stmts(&function.body);
        let mut lowered = IrFunction {
            name: function.name.clone(),
            param_count: function.param_count,
            locals: function.locals.iter().map(|local| local.ty).collect(),
            native_state_locals: function
                .locals
                .iter()
                .map(|local| local.native_state)
                .collect(),
            return_type: function.return_type,
            execution: function.execution,
            is_main_thread: function.is_main_thread,
            by_reference_params: by_reference.clone(),
            by_pointer_params: by_pointer_params(function, &self.hir.types, &by_reference),
            body,
        };
        // Scope-exit releases are placed once, here where the source's block
        // structure is still what was walked; both engines lower the statements
        // this adds.
        crate::mid::scope_releases(&mut lowered, &self.ir.exprs, &self.hir.types);
        lowered
    }

    fn lower_stmts(&mut self, stmts: &[HirStmtId]) -> Vec<IrStmt> {
        let mut lowered = Vec::new();
        for &id in stmts {
            if let HirStmt::Attempt { attempt } = self.hir.stmt(id).clone() {
                lowered.push(self.lower_attempt(&attempt));
            } else if let Some(statement) = self.lower_stmt(id) {
                lowered.push(statement);
            }
        }
        lowered
    }

    /// Lowers the linear HIR attempt into the backend-facing structured form.
    fn lower_attempt(&mut self, attempt: &HirAttempt) -> IrStmt {
        let steps = attempt
            .steps
            .iter()
            .map(|step| IrAttemptStep {
                setup: self.lower_stmts(&step.setup),
                error_condition: self.lower_expr(step.error_condition),
                handler: self.lower_stmts(&step.handler),
                success: self.lower_stmts(&step.success),
            })
            .collect();
        IrStmt::Attempt {
            attempt: IrAttempt {
                steps,
                trailing: self.lower_stmts(&attempt.trailing),
            },
        }
    }

    /// Lowers one statement, or nothing when it has already been accounted for.
    ///
    /// A `Let` that only gives a borrow a second name lowers to nothing at all:
    /// the uses of that name lower to the borrow itself, so there is no binding
    /// left to initialize. Its initializer is a bare local read, so dropping it
    /// drops no work anyone can observe.
    fn lower_stmt(&mut self, id: HirStmtId) -> Option<IrStmt> {
        Some(match self.hir.stmt(id).clone() {
            HirStmt::Let { local, init } => {
                if self.aliases.contains_key(&local.0) {
                    return None;
                }
                IrStmt::Let {
                    local: local.0,
                    init: self.lower_expr(init),
                }
            }
            HirStmt::Assign { place, value } => IrStmt::Assign {
                place: self.lower_place(&place),
                value: self.lower_expr(value),
            },
            HirStmt::CellSet { local, value } => IrStmt::CellSet {
                slot: self.slot(local.0),
                value: self.lower_expr(value),
            },
            HirStmt::Return { value } => IrStmt::Return {
                value: value.map(|expr| self.lower_expr(expr)),
            },
            HirStmt::Expr { expr } => IrStmt::Eval {
                expr: self.lower_expr(expr),
            },
            HirStmt::If {
                cond,
                then_body,
                else_body,
            } => IrStmt::If {
                cond: self.lower_expr(cond),
                then_body: self.lower_stmts(&then_body),
                else_body: self.lower_stmts(&else_body),
            },
            HirStmt::Attempt { .. } => return None,
            HirStmt::While { cond, body } => IrStmt::While {
                cond: self.lower_expr(cond),
                body: self.lower_stmts(&body),
            },
            HirStmt::Break => IrStmt::Break,
            HirStmt::Continue => IrStmt::Continue,
        })
    }

    /// The slot a local reads and writes, following any borrow alias.
    fn slot(&self, local: u32) -> u32 {
        self.aliases.get(&local).copied().unwrap_or(local)
    }
}

mod expr;

impl Lowerer<'_> {
    fn descriptor_of(&mut self, ty: Type) -> kira_semantics_model::ErasedTypeId {
        let types = &self.ir.types;
        kira_semantics_model::ErasedTypeId::of(&mut self.ir.descriptors, types, ty)
            .expect("analysis admits only types that name a value")
    }

    /// The IR callee one HIR callee names.
    ///
    /// The two suspend-point builtins are calls to synthesized functions rather
    /// than to anything a backend implements, which is what keeps `taskYield()`
    /// and `taskSleep(ms)` from needing an opcode of their own.
    fn lower_callee(&mut self, callee: Callee) -> IrCallee {
        match callee {
            Callee::Builtin(Builtin::Print) => IrCallee::Print,
            Callee::Builtin(Builtin::Abort) => IrCallee::Abort,
            Callee::Builtin(Builtin::FromCode) => IrCallee::FromCode,
            Callee::Builtin(Builtin::Hash) => IrCallee::Hash,
            Callee::Builtin(Builtin::TaskYield) => {
                self.uses_tasks = true;
                IrCallee::User(self.task_base + crate::tasks::TaskFns::YIELD)
            }
            Callee::Builtin(Builtin::TaskSleep) => {
                self.uses_tasks = true;
                IrCallee::User(self.task_base + crate::tasks::TaskFns::SLEEP)
            }
            Callee::User(id) => IrCallee::User(id.0),
            Callee::Foreign(id) => IrCallee::Foreign(id.0),
        }
    }

    /// Lowers `Task { … }` to a call to the spawn helper.
    ///
    /// The helper takes a fixed argument list, so a body with fewer arguments
    /// pads with zeros: one generated function serves every arity, and the
    /// dispatcher reads back exactly as many slots as its target declares.
    fn lower_task_spawn(&mut self, target: TaskTarget, args: &[HirExprId], ty: Type) -> IrExprId {
        use kira_semantics_model::Type;
        self.uses_tasks = true;
        // Each argument crosses as the one machine word its parameter's slot
        // holds. A literal body has no parameters, so its single argument is
        // classified by its own type — only a numeric literal reaches that
        // path, so it is a `Word` or `FloatBits` either way.
        let (arm, crossings) = match target {
            TaskTarget::Value => {
                let crossings = args
                    .iter()
                    .map(|&arg| Crossing::classify(&self.ir.types, self.hir.expr(arg).type_of()))
                    .collect::<Vec<_>>();
                (0, crossings)
            }
            TaskTarget::Call(id) => {
                let function = &self.hir.functions[id.0 as usize];
                let params: Vec<Type> = function
                    .locals
                    .iter()
                    .take(function.param_count as usize)
                    .map(|local| local.ty)
                    .collect();
                let result = function.return_type;
                let crossings = params
                    .iter()
                    .map(|&param| Crossing::classify(&self.ir.types, param))
                    .collect::<Vec<_>>();
                let arm = self.task_targets.arm_for(id.0, params, result);
                (arm, crossings)
            }
        };
        let mut call_args = vec![self.ir.exprs.alloc(IrExpr::Int(arm))];
        for (index, &arg) in args.iter().enumerate() {
            let value = self.lower_expr(arg);
            let crossing = crossings.get(index).copied().unwrap_or(Crossing::Word);
            let lowered = self.box_for_slot(value, crossing, self.hir.expr(arg).type_of());
            call_args.push(lowered);
        }
        while call_args.len() < 1 + kira_runtime_abi::TASK_SLOTS {
            call_args.push(self.ir.exprs.alloc(IrExpr::Int(0)));
        }
        self.ir.exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(self.task_base + crate::tasks::TaskFns::SPAWN),
            args: call_args,
            result: ty,
            writebacks: Vec::new(),
        })
    }

    /// Turns a lowered value into the one `Int`-kind word a task or channel slot
    /// holds, by its [`Crossing`].
    ///
    /// A scalar is already the word; a `Float` crosses as its bits; a value that
    /// owns storage crosses as a native-state token, retagged to `Int` so every
    /// slot word is the one kind the executors move.
    fn box_for_slot(&mut self, value: IrExprId, crossing: Crossing, value_ty: Type) -> IrExprId {
        use kira_semantics_model::hir::ConvertKind;
        match crossing {
            Crossing::Word => value,
            Crossing::FloatBits => self.ir.exprs.alloc(IrExpr::Convert {
                operand: value,
                kind: ConvertKind::FloatToBits,
                ty: Type::INT,
            }),
            Crossing::Boxed(type_id) => {
                let drop_glue = self.ir.types.user_drop(value_ty);
                let boxed = self.ir.exprs.alloc(IrExpr::NativeState {
                    value,
                    type_id,
                    drop_glue,
                    ty: Type::INT,
                });
                let token = self.ir.exprs.alloc(IrExpr::NativeUserData {
                    state: boxed,
                    borrowed: false,
                    ty: Type::RawPtr,
                });
                self.ir.exprs.alloc(IrExpr::Convert {
                    operand: token,
                    kind: ConvertKind::RawPtrToInt,
                    ty: Type::INT,
                })
            }
        }
    }

    /// Recovers the value a slot word names, by its [`Crossing`], into `ty`.
    ///
    /// The inverse of [`Self::box_for_slot`]: a boxed word is retagged back to a
    /// pointer and the value taken out of the store, releasing the token, which
    /// is the last thing it is needed for.
    fn recover_from_slot(&mut self, word: IrExprId, crossing: Crossing, ty: Type) -> IrExprId {
        use kira_semantics_model::hir::ConvertKind;
        match crossing {
            Crossing::Word => word,
            Crossing::FloatBits => self.ir.exprs.alloc(IrExpr::Convert {
                operand: word,
                kind: ConvertKind::BitsToFloat,
                ty,
            }),
            Crossing::Boxed(type_id) => {
                let raw = self.ir.exprs.alloc(IrExpr::Convert {
                    operand: word,
                    kind: ConvertKind::IntToRawPtr,
                    ty: Type::RawPtr,
                });
                self.ir
                    .exprs
                    .alloc(IrExpr::NativeStateTake { raw, type_id, ty })
            }
        }
    }

    /// Lowers `handle.await` to a call to the join helper.
    /// One channel primitive, its operands zero-filled to three.
    fn channel_op(&mut self, prim: ChannelPrim, operands: Vec<IrExprId>) -> IrExprId {
        let mut filled = operands;
        while filled.len() < 3 {
            filled.push(self.ir.exprs.alloc(IrExpr::Int(0)));
        }
        let operands = [filled[0], filled[1], filled[2]];
        self.ir.exprs.alloc(IrExpr::ChannelOp { prim, operands })
    }

    /// `receiver.receive()`: a call to the synthesized receiver for its payload.
    ///
    /// The waiting itself is not here. It is one synthesized function per
    /// payload, so the VM and the native backend run the same wait rather than
    /// two copies of it — the argument [`crate::tasks`] makes for the
    /// scheduler, made again for the one other place a program blocks.
    fn lower_channel_receive(
        &mut self,
        receiver: HirExprId,
        payload: Type,
        wire: Crossing,
        failure: kira_semantics_model::EnumId,
        ty: Type,
    ) -> IrExprId {
        // A receive yields while it waits, so it needs the task spine.
        self.uses_tasks = true;
        let Type::Enum(result) = ty else {
            let receiver = self.lower_expr(receiver);
            return receiver;
        };
        let index = match self
            .channel_rows
            .iter()
            .position(|row| row.result == result)
        {
            Some(index) => index,
            None => {
                self.channel_rows.push(crate::channels::ReceiverRow {
                    result,
                    failure,
                    payload,
                    wire,
                });
                self.channel_rows.len() - 1
            }
        };
        let receiver = self.lower_expr(receiver);
        // The channel receivers are appended after the whole task spine and
        // the one step helper they share.
        let callee = self.task_base
            + crate::tasks::TaskFns::COUNT
            + crate::channels::STEP_HELPERS
            + index as u32;
        self.ir.exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(callee),
            args: vec![receiver],
            result: ty,
            writebacks: Vec::new(),
        })
    }

    fn lower_task_join(&mut self, handle: HirExprId, ty: Type) -> IrExprId {
        use kira_semantics_model::Type;
        self.uses_tasks = true;
        let handle = self.lower_expr(handle);
        let joined = self.ir.exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(self.task_base + crate::tasks::TaskFns::AWAIT),
            args: vec![handle],
            result: Type::INT,
            writebacks: Vec::new(),
        });
        let crossing = Crossing::classify(&self.ir.types, ty);
        self.recover_from_slot(joined, crossing, ty)
    }

    /// Lowers `handle.detach()` / `handle.requestCancel()` to their helper.
    fn lower_task_handle_call(&mut self, handle: HirExprId, helper: u32) -> IrExprId {
        self.uses_tasks = true;
        let handle = self.lower_expr(handle);
        self.ir.exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(self.task_base + helper),
            args: vec![handle],
            result: Type::Void,
            writebacks: Vec::new(),
        })
    }

    /// Lowers a place, lowering the index expressions its path carries.
    ///
    /// A place is not a plain data copy any more: an `Index` step holds an
    /// expression, which has to land in the IR's own arena like every other.
    fn lower_place(&mut self, place: &HirPlace) -> IrPlace {
        let path = place
            .path
            .iter()
            .map(|step| match step {
                HirPlaceStep::Field(index) => IrPlaceStep::Field(*index),
                HirPlaceStep::Index(expr) => IrPlaceStep::Index(self.lower_expr(*expr)),
            })
            .collect();
        IrPlace {
            local: self.slot(place.local.0),
            path,
        }
    }
}
