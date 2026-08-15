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

use kira_semantics_model::OwnershipMode;
use kira_semantics_model::hir::{
    Builtin, Callee, HirAttempt, HirExpr, HirExprId, HirPlace, HirPlaceStep, HirProgram, HirStmt,
    HirStmtId, TaskTarget,
};

use crate::tasks::TaskTargets;

use crate::ir::{
    IrAttempt, IrAttemptStep, IrCallee, IrExport, IrExpr, IrExprId, IrForeignImport, IrFunction,
    IrPlace, IrPlaceStep, IrProgram, IrStmt, IrWriteback,
};

/// Lowers an analyzed program to IR.
///
/// Total: every analyzed program lowers, entrypoint or not. `ir.main` is
/// `None` for a library.
pub fn lower(program: &HirProgram) -> IrProgram {
    let mut ir = IrProgram {
        functions: Vec::with_capacity(program.functions.len()),
        types: program.types.clone(),
        main: program.main.map(|main| main.0),
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
        uses_tasks: false,
    };
    let functions: Vec<IrFunction> = program
        .functions
        .iter()
        .map(|function| lowerer.lower_function(function))
        .collect();
    let uses_tasks = lowerer.uses_tasks;
    let targets = std::mem::take(&mut lowerer.task_targets);
    ir.functions = functions;
    if uses_tasks {
        crate::tasks::synthesize(&mut ir, task_base, &targets);
    }
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
    types: &kira_semantics_model::TypeTable,
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
fn worth_lending(ty: kira_semantics_model::Type, types: &kira_semantics_model::TypeTable) -> bool {
    use kira_semantics_model::Type;
    matches!(ty, Type::Struct(_)) || types.owns_heap(ty)
}

impl Lowerer<'_> {
    fn lower_function(&mut self, function: &kira_semantics_model::hir::HirFunction) -> IrFunction {
        self.aliases = crate::borrow_alias::borrow_aliases(self.hir, function);
        let by_reference = by_reference_params(function);
        let body = self.lower_stmts(&function.body);
        IrFunction {
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
            by_reference_params: by_reference.clone(),
            by_pointer_params: by_pointer_params(function, &self.hir.types, &by_reference),
            body,
        }
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

    fn lower_expr(&mut self, id: HirExprId) -> IrExprId {
        let node = match self.hir.expr(id).clone() {
            HirExpr::Int(value) => IrExpr::Int(value),
            HirExpr::Float(value) => IrExpr::Float(value),
            HirExpr::Bool(value) => IrExpr::Bool(value),
            HirExpr::Str(value) => IrExpr::Str(value),
            HirExpr::RawPtrNull => IrExpr::RawPtrNull,
            HirExpr::ForeignCallbackPtr { callback } => IrExpr::ForeignCallbackPtr { callback },
            HirExpr::Local { local, .. } => IrExpr::Local(self.slot(local.0)),
            HirExpr::CellNew { value, ty } => IrExpr::CellNew {
                value: self.lower_expr(value),
                ty,
            },
            HirExpr::CellNull { ty } => IrExpr::CellNull { ty },
            HirExpr::CellGet { local, ty } => IrExpr::CellGet {
                slot: self.slot(local.0),
                ty,
            },
            HirExpr::Unary { op, operand, .. } => IrExpr::Unary {
                op,
                operand: self.lower_expr(operand),
            },
            HirExpr::Binary { op, lhs, rhs, .. } => IrExpr::Binary {
                op,
                lhs: self.lower_expr(lhs),
                rhs: self.lower_expr(rhs),
            },
            HirExpr::Select {
                cond,
                then,
                otherwise,
                ty,
            } => IrExpr::Select {
                cond: self.lower_expr(cond),
                then: self.lower_expr(then),
                otherwise: self.lower_expr(otherwise),
                ty,
            },
            HirExpr::Call {
                callee,
                args,
                ty,
                writebacks,
            } => {
                let ir_args = args.iter().map(|&arg| self.lower_expr(arg)).collect();
                let writebacks = writebacks
                    .iter()
                    .map(|writeback| IrWriteback {
                        param: writeback.param,
                        place: self.lower_place(&writeback.place),
                    })
                    .collect();
                IrExpr::Call {
                    callee: self.lower_callee(callee),
                    args: ir_args,
                    result: ty,
                    writebacks,
                }
            }
            HirExpr::StructNew { struct_id, fields } => {
                let ir_fields = fields.iter().map(|&field| self.lower_expr(field)).collect();
                IrExpr::StructNew {
                    struct_id,
                    fields: ir_fields,
                }
            }
            HirExpr::Field { base, index, ty } => IrExpr::Field {
                base: self.lower_expr(base),
                index,
                ty,
            },
            HirExpr::ForeignMemberAddress {
                base,
                aggregate,
                member,
                ty,
            } => IrExpr::ForeignMemberAddress {
                base: self.lower_expr(base),
                aggregate,
                member,
                ty,
            },
            HirExpr::ForeignElement {
                base,
                aggregate,
                index,
                ty,
            } => IrExpr::ForeignElement {
                base: self.lower_expr(base),
                aggregate,
                index: self.lower_expr(index),
                ty,
            },
            HirExpr::ArrayElements { value, element } => IrExpr::ArrayElements {
                value: self.lower_expr(value),
                element,
            },
            HirExpr::ScalarText { value } => IrExpr::ScalarText {
                value: self.lower_expr(value),
            },
            HirExpr::MathOperation { op, value } => IrExpr::MathOperation {
                op,
                value: self.lower_expr(value),
            },
            HirExpr::ForeignField {
                base,
                aggregate,
                member,
                ty,
            } => IrExpr::ForeignField {
                base: self.lower_expr(base),
                aggregate,
                member,
                ty,
            },
            HirExpr::ArrayNew { ty, elements } => {
                let ir_elements = elements
                    .iter()
                    .map(|&element| self.lower_expr(element))
                    .collect();
                IrExpr::ArrayNew {
                    ty,
                    elements: ir_elements,
                }
            }
            HirExpr::Index { base, index, ty } => IrExpr::Index {
                base: self.lower_expr(base),
                index: self.lower_expr(index),
                ty,
            },
            HirExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => IrExpr::EnumNew {
                enum_id,
                tag,
                payload: payload.map(|expr| self.lower_expr(expr)),
            },
            HirExpr::EnumTag { value } => IrExpr::EnumTag {
                value: self.lower_expr(value),
            },
            HirExpr::EnumPayload { value, ty } => IrExpr::EnumPayload {
                value: self.lower_expr(value),
                ty,
            },
            HirExpr::ArrayLen { array } => IrExpr::ArrayLen {
                array: self.lower_expr(array),
            },
            HirExpr::StringCharAt { text, index } => IrExpr::StringCharAt {
                text: self.lower_expr(text),
                index: self.lower_expr(index),
            },
            HirExpr::StringSubstring { text, start, end } => IrExpr::StringSubstring {
                text: self.lower_expr(text),
                start: self.lower_expr(start),
                end: self.lower_expr(end),
            },
            HirExpr::StringIndexOf { text, needle } => IrExpr::StringIndexOf {
                text: self.lower_expr(text),
                needle: self.lower_expr(needle),
            },
            HirExpr::StringOperation {
                op,
                text,
                arguments,
                ty,
            } => IrExpr::StringOperation {
                op,
                text: self.lower_expr(text),
                arguments: arguments
                    .into_iter()
                    .map(|argument| self.lower_expr(argument))
                    .collect(),
                ty,
            },
            HirExpr::StringOf { value } => IrExpr::StringOf {
                value: self.lower_expr(value),
            },
            HirExpr::StringLen { text } => IrExpr::StringLen {
                text: self.lower_expr(text),
            },
            HirExpr::CLayoutAddress { value, aggregate } => IrExpr::CLayoutAddress {
                value: self.lower_expr(value),
                aggregate,
            },
            HirExpr::CStringNew { text } => IrExpr::CStringNew {
                text: self.lower_expr(text),
            },
            // A null C string and a null pointer are one zero word, so this
            // needs no node of its own below the type checker.
            HirExpr::CStringNull => IrExpr::RawPtrNull,
            HirExpr::FileSystem { op, args, ty } => IrExpr::FileSystem {
                op,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::Compiler { op, args, ty } => IrExpr::Compiler {
                op,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::Env { op, args, ty } => IrExpr::Env {
                op,
                args: args.into_iter().map(|arg| self.lower_expr(arg)).collect(),
                ty,
            },
            HirExpr::ArrayAppend { place, value } => IrExpr::ArrayAppend {
                place: self.lower_place(&place),
                value: self.lower_expr(value),
            },
            HirExpr::NativeState { value, type_id, ty } => IrExpr::NativeState {
                value: self.lower_expr(value),
                type_id,
                ty,
            },
            HirExpr::NativeUserData { state } => IrExpr::NativeUserData {
                state: self.lower_expr(state),
            },
            HirExpr::NativeRecover { raw, type_id, ty } => IrExpr::NativeRecover {
                raw: self.lower_expr(raw),
                type_id,
                ty,
            },
            HirExpr::NativeStateFree { token } => IrExpr::NativeStateFree {
                token: self.lower_expr(token),
            },
            HirExpr::Convert { operand, kind, ty } => IrExpr::Convert {
                operand: self.lower_expr(operand),
                kind,
                ty,
            },
            HirExpr::IntoAny { value, from } => IrExpr::IntoAny {
                value: self.lower_expr(value),
                from,
            },
            HirExpr::Widen { value, from, to } => IrExpr::Widen {
                value: self.lower_expr(value),
                from,
                to,
            },
            // An error node can only be reached when analysis already reported
            // diagnostics and the program is never run; lower it to a harmless
            // constant so lowering stays total.
            HirExpr::Error => IrExpr::Int(0),
            HirExpr::TaskSpawn { target, args, ty } => {
                return self.lower_task_spawn(target, &args, ty);
            }
            HirExpr::TaskJoin { handle, ty } => return self.lower_task_join(handle, ty),
            HirExpr::TaskDetach { handle } => {
                return self.lower_task_handle_call(handle, crate::tasks::TaskFns::DETACH);
            }
            HirExpr::TaskCancel { handle } => {
                return self.lower_task_handle_call(handle, crate::tasks::TaskFns::CANCEL);
            }
        };
        self.ir.exprs.alloc(node)
    }

    /// The IR callee one HIR callee names.
    ///
    /// The two suspend-point builtins are calls to synthesized functions rather
    /// than to anything a backend implements, which is what keeps `taskYield()`
    /// and `taskSleep(ms)` from needing an opcode of their own.
    fn lower_callee(&mut self, callee: Callee) -> IrCallee {
        match callee {
            Callee::Builtin(Builtin::Print) => IrCallee::Print,
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
    fn lower_task_spawn(
        &mut self,
        target: TaskTarget,
        args: &[HirExprId],
        ty: kira_semantics_model::Type,
    ) -> IrExprId {
        use kira_semantics_model::Type;
        self.uses_tasks = true;
        let arm = match target {
            TaskTarget::Value => 0,
            TaskTarget::Call(id) => {
                let function = &self.hir.functions[id.0 as usize];
                let params: Vec<Type> = function
                    .locals
                    .iter()
                    .take(function.param_count as usize)
                    .map(|local| local.ty)
                    .collect();
                let result = function.return_type;
                self.task_targets.arm_for(id.0, params, result)
            }
        };
        let mut call_args = vec![self.ir.exprs.alloc(IrExpr::Int(arm))];
        for &arg in args {
            let value = self.lower_expr(arg);
            let lowered = match self.hir.expr(arg).type_of() {
                // A slot is one machine word, so a `Float` argument crosses as
                // its bit pattern and the dispatcher rebuilds it.
                Type::Float(_) => self.ir.exprs.alloc(IrExpr::Convert {
                    operand: value,
                    kind: kira_semantics_model::hir::ConvertKind::FloatToBits,
                    ty: Type::INT,
                }),
                _ => value,
            };
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

    /// Lowers `handle.await` to a call to the join helper.
    fn lower_task_join(&mut self, handle: HirExprId, ty: kira_semantics_model::Type) -> IrExprId {
        use kira_semantics_model::Type;
        self.uses_tasks = true;
        let handle = self.lower_expr(handle);
        let joined = self.ir.exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(self.task_base + crate::tasks::TaskFns::AWAIT),
            args: vec![handle],
            result: Type::INT,
            writebacks: Vec::new(),
        });
        match ty {
            Type::Float(_) => self.ir.exprs.alloc(IrExpr::Convert {
                operand: joined,
                kind: kira_semantics_model::hir::ConvertKind::BitsToFloat,
                ty,
            }),
            _ => joined,
        }
    }

    /// Lowers `handle.detach()` / `handle.requestCancel()` to their helper.
    fn lower_task_handle_call(&mut self, handle: HirExprId, helper: u32) -> IrExprId {
        self.uses_tasks = true;
        let handle = self.lower_expr(handle);
        self.ir.exprs.alloc(IrExpr::Call {
            callee: IrCallee::User(self.task_base + helper),
            args: vec![handle],
            result: kira_semantics_model::Type::Void,
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
