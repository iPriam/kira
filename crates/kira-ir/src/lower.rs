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
    Callee, HirExpr, HirExprId, HirPlace, HirPlaceStep, HirProgram, HirStmt, HirStmtId,
};

use crate::ir::{
    IrCallee, IrExport, IrExpr, IrExprId, IrForeignImport, IrFunction, IrPlace, IrPlaceStep,
    IrProgram, IrStmt, IrWriteback,
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
    let mut lowerer = Lowerer {
        hir: program,
        ir: &mut ir,
        aliases: std::collections::HashMap::new(),
    };
    let functions: Vec<IrFunction> = program
        .functions
        .iter()
        .map(|function| lowerer.lower_function(function))
        .collect();
    ir.functions = functions;
    ir
}

struct Lowerer<'a> {
    hir: &'a HirProgram,
    ir: &'a mut IrProgram,
    /// The locals of the function being lowered that are a borrow under a
    /// second name, mapped to the local they name. See [`crate::borrow_alias`].
    aliases: std::collections::HashMap<u32, u32>,
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

impl Lowerer<'_> {
    fn lower_function(&mut self, function: &kira_semantics_model::hir::HirFunction) -> IrFunction {
        self.aliases = crate::borrow_alias::borrow_aliases(self.hir, function);
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
            by_reference_params: by_reference_params(function),
            body,
        }
    }

    fn lower_stmts(&mut self, stmts: &[HirStmtId]) -> Vec<IrStmt> {
        stmts.iter().filter_map(|&id| self.lower_stmt(id)).collect()
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
                    callee: lower_callee(callee),
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
            // An error node can only be reached when analysis already reported
            // diagnostics and the program is never run; lower it to a harmless
            // constant so lowering stays total.
            HirExpr::Error => IrExpr::Int(0),
        };
        self.ir.exprs.alloc(node)
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

fn lower_callee(callee: Callee) -> IrCallee {
    match callee {
        Callee::Builtin(kira_semantics_model::hir::Builtin::Print) => IrCallee::Print,
        Callee::User(id) => IrCallee::User(id.0),
        Callee::Foreign(id) => IrCallee::Foreign(id.0),
    }
}
