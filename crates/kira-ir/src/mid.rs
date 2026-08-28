//! The mid-level stage decides release ownership once for every backend.
//!
//! Lowering records one release plan for each owned local. Reading a local
//! copies its heap value, so one release per slot at return is sufficient.
//! Borrowed parameters and callback-state locals are excluded because their
//! storage belongs to the caller or to the state store.
//!
//! Whether a borrow is represented by a pointer is engine-specific: native code
//! lends the caller's storage, while the VM copies the value into the callee's
//! slot and moves it back. The plan therefore receives [`Lending`] from
//! lowering rather than deriving it in either backend. The bytecode compiler
//! serializes the same plan that LLVM consumes while emitting a `return`.
//!
//! Ownership has a second half: *when*. [`scope_releases`] walks each body and
//! places a [`IrStmt::ReleaseLocals`] wherever a block-scoped binding dies —
//! the end of its declaring block, and before every `break`/`continue` that
//! jumps past it. Placement asks only which bindings a block declares, which
//! no engine disagrees about; whether a named slot is this engine's to release
//! stays with the plan, which each backend consults when it lowers the
//! statement.

use std::collections::HashMap;

use la_arena::Arena;

use kira_semantics_model::TypeTable;

use crate::ir::{
    IrAttempt, IrAttemptStep, IrExpr, IrExprId, IrFunction, IrPlace, IrPlaceStep, IrProgram, IrStmt,
};

/// Why a release plan could not be built.
///
/// Each variant is a contradiction *within one function* — two facts that
/// cannot both be true of the same slot. They are compiler bugs rather than
/// program errors: nothing a user can write reaches one, because every input
/// here was resolved by lowering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MidError {
    /// A slot is both a by-reference parameter and a callback-state local.
    ///
    /// The two say opposite things about who owns the storage — the caller, or
    /// a store outside the call — and a slot cannot be both.
    #[error(
        "function `{function}` slot {slot} is both a by-reference parameter and a \
         callback-state local, which name different owners"
    )]
    ConflictingSlotRole {
        /// The function the slot belongs to.
        function: String,
        /// The slot in question.
        slot: u32,
    },
    /// A by-reference parameter names a slot the function does not have.
    ///
    /// Left as an error rather than ignored: a parameter index that resolves to
    /// nothing means lowering and this stage disagree about how many locals the
    /// function has, and guessing which is right would release the wrong slot.
    #[error("function `{function}` names by-reference parameter {slot}, which is not a local")]
    UnknownParameter {
        /// The function the parameter belongs to.
        function: String,
        /// The slot the parameter named.
        slot: u32,
    },
}

/// Which slots one function releases when it returns, in slot order.
///
/// Slot order rather than declaration or reverse order: nothing in the language
/// observes the order releases happen in — a release touches only the value's
/// own storage — and a fixed order is one fewer thing for two engines to
/// disagree about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleasePlan {
    slots: Vec<u32>,
}

impl ReleasePlan {
    /// The slots to release, ascending.
    pub fn slots(&self) -> &[u32] {
        &self.slots
    }

    /// Whether `slot` is released by this plan.
    pub fn releases(&self, slot: u32) -> bool {
        self.slots.binary_search(&slot).is_ok()
    }

    /// How many slots the plan releases.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the plan releases nothing, which is the common case.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Whether a borrowed parameter reaches a callee as a pointer into the
/// caller's storage, or as a value of its own.
///
/// A parameter of the [`BorrowLending::ByPointer`] kind is the caller's to
/// release; one of the [`BorrowLending::ByValue`] kind arrived as a copy the
/// callee owns and must release itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowLending {
    /// The borrow is a pointer into the caller's storage.
    ByPointer,
    /// The borrow arrived as the callee's own value.
    ByValue,
}

/// How an engine's calls lend the two kinds of borrowed parameter.
///
/// Two fields rather than one because the two kinds are lent independently.
/// Whether a `borrow mut` is a pointer is fixed per engine — the native backend
/// always passes one, the VM never can. Whether a plain `borrow` is a pointer
/// varies by module shape even within the native backend, since lending one
/// commits every call site and only a module that compiles all of them may
/// decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lending {
    /// How a `borrow` parameter arrives.
    pub read_only: BorrowLending,
    /// How a `borrow mut` parameter arrives.
    pub write_through: BorrowLending,
    /// How a `borrow` of a type that runs a user `Drop` arrives.
    ///
    /// Its own field because the two engines answer differently for a reason
    /// neither of the others carries. A VM copy is a share of one object, so a
    /// borrowed copy releases a share and runs no body; a native copy is a
    /// second value, and releasing it would run the body while the caller still
    /// holds what it was told about. So native lends one however it lends
    /// everything else, and the VM copies it however it copies everything else.
    pub user_drop: BorrowLending,
}

impl Lending {
    /// Every borrow arrives as the callee's own value, which is the VM's only
    /// option and what a native module that lends nothing does too.
    pub const BY_VALUE: Lending = Lending {
        read_only: BorrowLending::ByValue,
        write_through: BorrowLending::ByValue,
        user_drop: BorrowLending::ByValue,
    };
}

/// Builds the release plan for one function.
///
/// `drop_glue` says this function is the body of a type's user `Drop`. Its
/// receiver is then excluded: the storage belongs to whatever is releasing the
/// value, which releases the members itself once the body has run. Releasing it
/// here would re-enter the same body on the same value.
pub fn plan_function(
    function: &IrFunction,
    types: &TypeTable,
    lending: Lending,
    drop_glue: bool,
) -> Result<ReleasePlan, MidError> {
    let local_count = function.locals.len();
    for &slot in &function.by_reference_params {
        if slot as usize >= local_count {
            return Err(MidError::UnknownParameter {
                function: function.name.clone(),
                slot,
            });
        }
    }

    let mut slots = Vec::new();
    for (index, &ty) in function.locals.iter().enumerate() {
        let slot = index as u32;
        let written_through = function.by_reference_params.contains(&slot);
        let borrowed_drop = types.runs_user_drop(ty);
        let lent = match (written_through, function.by_pointer_params.contains(&slot)) {
            (true, _) => lending.write_through == BorrowLending::ByPointer,
            (false, true) if borrowed_drop => lending.user_drop == BorrowLending::ByPointer,
            (false, read_only) => read_only && lending.read_only == BorrowLending::ByPointer,
        };
        let state_local = function
            .native_state_locals
            .get(index)
            .copied()
            .flatten()
            .is_some();
        // The contradiction is in the function, not in this engine's lending:
        // a slot that is a parameter at all cannot also name a store outside
        // the call, however that parameter happens to arrive.
        if (written_through || function.by_pointer_params.contains(&slot)) && state_local {
            return Err(MidError::ConflictingSlotRole {
                function: function.name.clone(),
                slot,
            });
        }
        if lent || state_local || !types.owns_heap(ty) {
            continue;
        }
        if drop_glue && slot == 0 {
            continue;
        }
        slots.push(slot);
    }
    Ok(ReleasePlan { slots })
}

/// Builds a release plan for every function in `program`, in function order.
pub fn plan(program: &IrProgram, lending: Lending) -> Result<Vec<ReleasePlan>, MidError> {
    let glue: std::collections::BTreeSet<u32> = program
        .types
        .structs()
        .defs()
        .iter()
        .filter_map(|def| def.drop_glue)
        .collect();
    program
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            plan_function(
                function,
                &program.types,
                lending,
                glue.contains(&(index as u32)),
            )
        })
        .collect()
}

/// One statement list's identity within a body: the child indices taken on the
/// way down from the function body. Both walks below descend in one canonical
/// order — statements left to right; an `if`'s then-arm before its else-arm; a
/// step's setup, handler, and success before its trailing — which is what
/// makes the same path name the same list in both.
type ScopePath = Vec<u32>;

/// Inserts a [`IrStmt::ReleaseLocals`] everywhere a block-scoped binding dies.
///
/// A binding dies when the innermost statement list containing *every*
/// reference to it ends: usually its own declaring block, but an outer one
/// whenever something outside the declaring list still names it (a `try`'s
/// hidden bindings live in a step's setup while their uses sit beside the
/// attempt, so they die with the block that wrote the `try`, not with the
/// step). The end of a list releases exactly what dies there, once per
/// execution; a `break`/`continue` ends every list between itself and its
/// loop at once, so it releases their union first.
///
/// Candidates are the slots whose type owns heap storage and that no callback
/// state backs — the slots a release could mean something for. Parameters are
/// never candidates because only a `let` declares a binding into a scope, so
/// ownership of borrowed storage is decided by the plan alone, at return.
pub fn scope_releases(function: &mut IrFunction, exprs: &Arena<IrExpr>, types: &TypeTable) {
    let candidates: Vec<bool> = function
        .locals
        .iter()
        .enumerate()
        .map(|(index, &ty)| {
            types.owns_heap(ty)
                && function
                    .native_state_locals
                    .get(index)
                    .copied()
                    .flatten()
                    .is_none()
        })
        .collect();
    let scan = Scan {
        exprs,
        candidates: &candidates,
        param_count: function.param_count,
    };
    // Whatever survives to the root map dies with the function body itself,
    // where the frame plan releases it anyway.
    let root = scan.list(&function.body, &mut Vec::new(), &mut 0, None);
    let mut sets: HashMap<ScopePath, Vec<u32>> = HashMap::new();
    for (slot, owner) in root {
        sets.entry(owner).or_default().push(slot);
    }
    if sets.is_empty() {
        return;
    }
    for set in sets.values_mut() {
        set.sort_unstable();
    }
    Rewrite {
        sets: &mut sets,
        active: Vec::new(),
        loops: Vec::new(),
    }
    .list(&mut function.body, &mut Vec::new(), &mut 0);
}

/// The bottom-up half: which list each candidate dies at.
struct Scan<'a> {
    exprs: &'a Arena<IrExpr>,
    candidates: &'a [bool],
    param_count: u32,
}

impl Scan<'_> {
    /// Pins `slot` to the list being folded, whose own path is `path`.
    ///
    /// A direct reference is the strongest fact there is: any list containing
    /// everything seen so far must reach this one.
    fn pin(&self, slot: u32, path: &[u32], owners: &mut HashMap<u32, ScopePath>) {
        if slot >= self.param_count && self.candidates.get(slot as usize).copied().unwrap_or(false)
        {
            owners.insert(slot, path.to_vec());
        }
    }

    /// References do not determine a binding's lifetime. A local belongs to
    /// the lexical list that declared it, even when its last use is in a child
    /// list; releasing at that use would make Drop timing depend on whether a
    /// value happened to be read. The traversal still visits every expression
    /// so it remains total as the IR grows, but its local leaves are ignored.
    fn reference(&self, _slot: u32, _path: &[u32], _owners: &mut HashMap<u32, ScopePath>) {}

    /// Folds one statement list, answering where every candidate referenced
    /// inside it dies. `path` carries the ancestors' indices and `child`
    /// hands out this node's child indices in walk order.
    fn list(
        &self,
        stmts: &[IrStmt],
        path: &mut ScopePath,
        child: &mut u32,
        declaration_path: Option<&ScopePath>,
    ) -> HashMap<u32, ScopePath> {
        let mut owners = HashMap::new();
        for statement in stmts {
            match statement {
                IrStmt::Let { local, init } => {
                    self.pin(*local, declaration_path.unwrap_or(path), &mut owners);
                    self.expr(*init, path, &mut owners);
                }
                IrStmt::Assign { place, value } => {
                    self.place(place, path, &mut owners);
                    self.expr(*value, path, &mut owners);
                }
                IrStmt::CellSet { slot, value } => {
                    self.reference(*slot, path, &mut owners);
                    self.expr(*value, path, &mut owners);
                }
                IrStmt::Return { value } => {
                    if let Some(expr) = *value {
                        self.expr(expr, path, &mut owners);
                    }
                }
                IrStmt::Eval { expr } => self.expr(*expr, path, &mut owners),
                IrStmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    self.expr(*cond, path, &mut owners);
                    self.absorb(self.nest(then_body, path, child), &mut owners);
                    self.absorb(self.nest(else_body, path, child), &mut owners);
                }
                IrStmt::Attempt { attempt } => {
                    for step in &attempt.steps {
                        self.expr(step.error_condition, path, &mut owners);
                        self.absorb(self.nest_attempt(&step.setup, path, child), &mut owners);
                        self.absorb(self.nest(&step.handler, path, child), &mut owners);
                        self.absorb(self.nest_attempt(&step.success, path, child), &mut owners);
                    }
                    self.absorb(
                        self.nest_attempt(&attempt.trailing, path, child),
                        &mut owners,
                    );
                }
                IrStmt::While { cond, body } => {
                    self.expr(*cond, path, &mut owners);
                    self.absorb(self.nest(body, path, child), &mut owners);
                }
                // Neither names a binding, and neither outlives the loop it
                // leaves; the rewrite half places their releases.
                IrStmt::Break | IrStmt::Continue | IrStmt::ReleaseLocals { .. } => {}
            }
        }
        owners
    }

    /// Folds a child list, extending `path` with its index.
    fn nest(
        &self,
        stmts: &[IrStmt],
        path: &mut ScopePath,
        child: &mut u32,
    ) -> HashMap<u32, ScopePath> {
        path.push(*child);
        *child += 1;
        let folded = self.list(stmts, path, child, None);
        path.pop();
        folded
    }

    /// Folds one of an attempt's partitioned lists while keeping direct
    /// bindings in the attempt's source block. Lowering splits one source
    /// block across setup, success, and trailing vectors for control flow, but
    /// those vectors do not create lexical scopes of their own.
    fn nest_attempt(
        &self,
        stmts: &[IrStmt],
        path: &mut ScopePath,
        child: &mut u32,
    ) -> HashMap<u32, ScopePath> {
        let declaration_path = path.clone();
        path.push(*child);
        *child += 1;
        let folded = self.list(stmts, path, child, Some(&declaration_path));
        path.pop();
        folded
    }

    /// Absorbs a subtree's answers into this list's own. A slot answered once
    /// keeps its deeper owner; a slot answered twice dies at the common
    /// ancestor — this list — because neither earlier answer contains both
    /// halves.
    fn absorb(&self, folded: HashMap<u32, ScopePath>, owners: &mut HashMap<u32, ScopePath>) {
        for (slot, owner) in folded {
            owners.entry(slot).or_insert(owner);
        }
    }

    /// A direct answer for the slot a place is rooted at, plus its index
    /// expressions' own references.
    fn place(&self, place: &IrPlace, path: &ScopePath, owners: &mut HashMap<u32, ScopePath>) {
        self.reference(place.local, path, owners);
        for step in &place.path {
            if let IrPlaceStep::Index(index) = step {
                self.expr(*index, path, owners);
            }
        }
    }

    /// Records every local an expression names.
    fn expr(&self, id: IrExprId, path: &ScopePath, owners: &mut HashMap<u32, ScopePath>) {
        let exprs = self.exprs;
        match &exprs[id] {
            IrExpr::Int(_)
            | IrExpr::Float(_)
            | IrExpr::Bool(_)
            | IrExpr::Str(_)
            | IrExpr::RawPtrNull
            | IrExpr::ForeignCallbackPtr { .. }
            | IrExpr::CellNull { .. }
            | IrExpr::ConstantGet { .. } => {}
            IrExpr::Local(slot) => self.reference(*slot, path, owners),
            IrExpr::Unary { operand, .. } => self.expr(*operand, path, owners),
            IrExpr::Binary { lhs, rhs, .. } => {
                self.expr(*lhs, path, owners);
                self.expr(*rhs, path, owners);
            }
            IrExpr::Select {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.expr(*cond, path, owners);
                self.expr(*then, path, owners);
                self.expr(*otherwise, path, owners);
            }
            IrExpr::Call {
                args, writebacks, ..
            } => {
                for arg in args {
                    self.expr(*arg, path, owners);
                }
                for writeback in writebacks {
                    self.place(&writeback.place, path, owners);
                }
            }
            IrExpr::StructNew { fields, .. } => {
                for field in fields {
                    self.expr(*field, path, owners);
                }
            }
            IrExpr::EnumNew { payload, .. } => {
                if let Some(payload) = payload {
                    self.expr(*payload, path, owners);
                }
            }
            IrExpr::EnumTag { value } | IrExpr::EnumPayload { value, .. } => {
                self.expr(*value, path, owners)
            }
            IrExpr::CellNew { value, .. } => self.expr(*value, path, owners),
            IrExpr::CellGet { slot, .. } => self.reference(*slot, path, owners),
            IrExpr::Field { base, .. }
            | IrExpr::ForeignMemberAddress { base, .. }
            | IrExpr::ForeignField { base, .. } => self.expr(*base, path, owners),
            IrExpr::ForeignElement { base, index, .. } => {
                self.expr(*base, path, owners);
                self.expr(*index, path, owners);
            }
            IrExpr::ArrayNew { elements, .. } => {
                for element in elements {
                    self.expr(*element, path, owners);
                }
            }
            IrExpr::Index { base, index, .. } => {
                self.expr(*base, path, owners);
                self.expr(*index, path, owners);
            }
            IrExpr::ArrayLen { array } => self.expr(*array, path, owners),
            IrExpr::StringLen { text } => self.expr(*text, path, owners),
            IrExpr::StringCharAt { text, index } => {
                self.expr(*text, path, owners);
                self.expr(*index, path, owners);
            }
            IrExpr::StringSubstring { text, start, end } => {
                self.expr(*text, path, owners);
                self.expr(*start, path, owners);
                self.expr(*end, path, owners);
            }
            IrExpr::StringIndexOf { text, needle } => {
                self.expr(*text, path, owners);
                self.expr(*needle, path, owners);
            }
            IrExpr::ArrayElements { value, .. } | IrExpr::ScalarText { value } => {
                self.expr(*value, path, owners)
            }
            IrExpr::MathOperation { operands, .. } => {
                for operand in operands {
                    self.expr(*operand, path, owners);
                }
            }
            IrExpr::StringOperation {
                text, arguments, ..
            } => {
                self.expr(*text, path, owners);
                for argument in arguments {
                    self.expr(*argument, path, owners);
                }
            }
            IrExpr::StringOf { value } | IrExpr::CLayoutAddress { value, .. } => {
                self.expr(*value, path, owners)
            }
            IrExpr::CStringNew { text } => self.expr(*text, path, owners),
            IrExpr::FileSystem { args, .. }
            | IrExpr::Compiler { args, .. }
            | IrExpr::Env { args, .. } => {
                for arg in args {
                    self.expr(*arg, path, owners);
                }
            }
            IrExpr::ArrayAppend { place, value } => {
                self.place(place, path, owners);
                self.expr(*value, path, owners);
            }
            IrExpr::NativeState { value, .. } => self.expr(*value, path, owners),
            IrExpr::NativeUserData { state } => self.expr(*state, path, owners),
            IrExpr::NativeRecover { raw, .. } => self.expr(*raw, path, owners),
            IrExpr::NativeStateFree { token } => self.expr(*token, path, owners),
            IrExpr::Convert { operand, .. } => self.expr(*operand, path, owners),
            IrExpr::IntoAny { value, .. } | IrExpr::Widen { value, .. } => {
                self.expr(*value, path, owners)
            }
            IrExpr::TaskOp { operands, .. } => {
                for operand in operands {
                    self.expr(*operand, path, owners);
                }
            }
        }
    }
}

/// The top-down half: places the statements the sets describe.
struct Rewrite<'a> {
    /// The release set of every owning list, drained as it is consumed.
    sets: &'a mut HashMap<ScopePath, Vec<u32>>,
    /// The release set of every list on the way down, innermost last.
    active: Vec<Vec<u32>>,
    /// For each enclosing `while`, how deep [`Rewrite::active`] was when its
    /// body began — the boundary a `break`/`continue` releases up to.
    loops: Vec<usize>,
}

impl Rewrite<'_> {
    fn list(&mut self, stmts: &mut Vec<IrStmt>, path: &mut ScopePath, child: &mut u32) {
        let own = self.sets.remove(path).unwrap_or_default();
        self.active.push(own);
        let mut ended = Vec::new();
        for mut statement in std::mem::take(stmts) {
            match &mut statement {
                IrStmt::Break | IrStmt::Continue => {
                    // Analysis guarantees an enclosing loop; a body without
                    // one cannot name what a jump out of it would end, so
                    // there is nothing to release.
                    if let Some(&boundary) = self.loops.last() {
                        let locals = union_of(&self.active[boundary..]);
                        if !locals.is_empty() {
                            ended.push(IrStmt::ReleaseLocals { locals });
                        }
                    }
                }
                IrStmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    self.child(then_body, path, child);
                    self.child(else_body, path, child);
                }
                IrStmt::While { body, .. } => {
                    let boundary = self.active.len();
                    self.loops.push(boundary);
                    self.child(body, path, child);
                    self.loops.pop();
                }
                IrStmt::Attempt { attempt } => {
                    let IrAttempt {
                        steps, trailing, ..
                    } = attempt;
                    for step in steps {
                        let IrAttemptStep {
                            setup,
                            handler,
                            success,
                            ..
                        } = step;
                        self.child(setup, path, child);
                        self.child(handler, path, child);
                        self.child(success, path, child);
                    }
                    self.child(trailing, path, child);
                }
                _ => {}
            }
            ended.push(statement);
        }
        let own = self.active.pop().unwrap_or_default();
        if !own.is_empty() {
            ended.push(IrStmt::ReleaseLocals { locals: own });
        }
        *stmts = ended;
    }

    /// Rewrites a child list, extending `path` with its index.
    fn child(&mut self, stmts: &mut Vec<IrStmt>, path: &mut ScopePath, child: &mut u32) {
        path.push(*child);
        *child += 1;
        self.list(stmts, path, child);
        path.pop();
    }
}

/// Merges ascending slot sets into one ascending set.
fn union_of(sets: &[Vec<u32>]) -> Vec<u32> {
    let mut all: Vec<u32> = sets.iter().flatten().copied().collect();
    all.sort_unstable();
    all.dedup();
    all
}

#[cfg(test)]
mod tests;
