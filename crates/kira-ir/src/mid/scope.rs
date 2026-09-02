//! Placement of scope-end releases: where each block-scoped binding dies.
//!
//! [`plan`](super::plan) decides *whether* a slot is this engine's to release;
//! this module decides *when* the shared [`IrStmt::ReleaseLocals`] statements
//! say a binding's scope ends. Split from `mid.rs` along exactly that line.

use std::collections::HashMap;

use la_arena::Arena;

use kira_semantics_model::TypeTable;

use super::HeapModel;
use crate::ir::{
    IrAttempt, IrAttemptStep, IrExpr, IrExprId, IrFunction, IrPlace, IrPlaceStep, IrStmt,
};

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
    // The candidate set is the UNION of the engines' heap models — everything
    // any engine could own — because the statement this places is shared by
    // both backends. Each backend's release walk already no-ops a slot its own
    // model gives no storage: the native lowering's release walk returns
    // before emitting anything for a type that owns no heap there, and the
    // VM releases every boxed value. Narrowing this to one model would leak
    // on the other.
    let candidates: Vec<bool> = function
        .locals
        .iter()
        .enumerate()
        .map(|(index, &ty)| {
            HeapModel::Boxed.owns(types, ty)
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
            IrExpr::EnumTag { value }
            | IrExpr::EnumPayload { value, .. }
            | IrExpr::TypeTest { value, .. }
            | IrExpr::TypeCast { value, .. } => self.expr(*value, path, owners),
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
            IrExpr::NativeStateRetain { token } | IrExpr::NativeStateRelease { token } => {
                self.expr(*token, path, owners)
            }
            IrExpr::Convert { operand, .. } => self.expr(*operand, path, owners),
            IrExpr::IntoAny { value, .. } => self.expr(*value, path, owners),
            IrExpr::TaskOp { operands, .. } => {
                for operand in operands {
                    self.expr(*operand, path, owners);
                }
            }
            IrExpr::MainThreadCall { args, .. } => {
                for arg in args {
                    self.expr(*arg, path, owners);
                }
            }
            IrExpr::MainThreadJoin { handle, .. } => self.expr(*handle, path, owners),
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
