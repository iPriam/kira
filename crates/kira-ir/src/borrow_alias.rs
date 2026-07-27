//! Which locals are a borrow under a second name.
//!
//! `var out = nodes` binds a *value* when `nodes` owns one, and that is a move.
//! When `nodes` is a `borrow`/`borrow mut` parameter there is no value to bind —
//! the parameter is the caller's storage — so the second name has to mean the
//! same storage the first one did. Copying instead is how this reads today:
//!
//! ```text
//! function appendOne(nodes: borrow mut [Int], value: Int) {
//!     var out = nodes
//!     out.append(value)   // appended to a copy nobody else can see
//! }
//! ```
//!
//! Every engine gets this wrong the same way, because every engine reads a
//! `Let` as "bind this value". So the fix is above them all: resolve the second
//! name to the first before the IR is built, and the `Let` disappears along with
//! the copy. A borrow-mut parameter's write-back chain then runs through the
//! original slot, which is the one the caller is watching.
//!
//! Only an unambiguous rebinding qualifies. A local bound to a borrow in one
//! place and to something else in another is two bindings sharing a slot, and a
//! local that is later assigned a whole new value is not a second name for
//! anything — both keep the copy they have always had.

use std::collections::{HashMap, HashSet};

use kira_semantics_model::OwnershipMode;
use kira_semantics_model::hir::{HirExpr, HirFunction, HirProgram, HirStmt, HirStmtId};

/// Maps each aliasing local to the local it is a second name for.
///
/// Resolved transitively, so `var a = nodes; var b = a` maps both to `nodes`.
/// The oracle refuses that chain as overlapping access rather than running it,
/// and this workspace has no such diagnostic yet; resolving the chain is the
/// conservative answer in the meantime — the second name means the borrow, as
/// it does everywhere else, instead of quietly becoming a copy. A local absent
/// from the map binds a value, as it always did.
pub(crate) fn borrow_aliases(program: &HirProgram, function: &HirFunction) -> HashMap<u32, u32> {
    let mut scan = Scan {
        program,
        function,
        candidates: HashMap::new(),
        disqualified: HashSet::new(),
        written: HashSet::new(),
    };
    scan.stmts(&function.body);
    let Scan {
        candidates,
        disqualified,
        written,
        ..
    } = scan;
    let direct: HashMap<u32, u32> = candidates
        .into_iter()
        .filter(|(local, source)| !disqualified.contains(local) && !disqualified.contains(source))
        // A read-only borrow lends no permission to write. Rebinding one and
        // then writing *through* the new name is how a callee makes its own
        // copy to modify, and it has to stay a copy — aliasing there would send
        // the write to the caller's value, which `borrow` promises it cannot
        // reach. A `borrow mut` is exactly the permission this needs, so it
        // aliases whether or not anyone writes.
        .filter(|(local, source)| !written.contains(local) || is_mutable_borrow(function, *source))
        .collect();
    direct
        .keys()
        .filter_map(|&local| Some((local, resolve(&direct, local)?)))
        .collect()
}

/// Whether `slot` is a parameter declared `borrow mut`.
fn is_mutable_borrow(function: &HirFunction, slot: u32) -> bool {
    function
        .locals
        .get(slot as usize)
        .is_some_and(|local| local.ownership == OwnershipMode::BorrowMut)
}

/// Follows `local` through the alias chain to the borrow it names.
///
/// Bounded by the number of aliases: a chain cannot revisit a local, because a
/// local is a candidate only once and points at a local bound before it. The
/// bound is a guard on that reasoning rather than a case that happens.
fn resolve(direct: &HashMap<u32, u32>, local: u32) -> Option<u32> {
    let mut at = *direct.get(&local)?;
    for _ in 0..direct.len() {
        match direct.get(&at) {
            Some(&next) => at = next,
            None => return Some(at),
        }
    }
    None
}

/// Walks a body collecting alias candidates and what rules them out.
struct Scan<'a> {
    program: &'a HirProgram,
    function: &'a HirFunction,
    candidates: HashMap<u32, u32>,
    disqualified: HashSet<u32>,
    /// Locals written through at all, by any path.
    written: HashSet<u32>,
}

impl Scan<'_> {
    fn stmts(&mut self, stmts: &[HirStmtId]) {
        for &id in stmts {
            self.stmt(id);
        }
    }

    fn stmt(&mut self, id: HirStmtId) {
        match self.program.stmt(id) {
            HirStmt::Let { local, init } => {
                let dest = local.0;
                match self.borrow_source(*init) {
                    // A second `Let` for the same slot naming the same borrow is
                    // the same rebinding reached twice (a loop body, say), which
                    // is still one alias. Naming a different one is two bindings
                    // sharing a slot, and neither can speak for the other.
                    Some(source) if self.candidates.get(&dest).is_none_or(|&had| had == source) => {
                        self.candidates.insert(dest, source);
                    }
                    _ => {
                        self.disqualified.insert(dest);
                    }
                }
            }
            HirStmt::Assign { place, .. } => {
                // Writing *through* a place (`out[0] = x`) is what an alias to a
                // `borrow mut` is for. Replacing the binding itself is not
                // aliasing at all.
                if place.path.is_empty() {
                    self.disqualified.insert(place.local.0);
                } else {
                    self.written.insert(place.local.0);
                }
            }
            HirStmt::If {
                then_body,
                else_body,
                ..
            } => {
                let (then_body, else_body) = (then_body.clone(), else_body.clone());
                self.stmts(&then_body);
                self.stmts(&else_body);
            }
            HirStmt::While { body, .. } => {
                let body = body.clone();
                self.stmts(&body);
            }
            HirStmt::Return { .. } | HirStmt::Expr { .. } | HirStmt::Break | HirStmt::Continue => {}
        }
    }

    /// The borrowed local `init` names outright, if it names one.
    ///
    /// A borrowed parameter qualifies, and so does a local already standing in
    /// for one — that is what makes `var b = a` an alias of what `a` aliases.
    fn borrow_source(&self, init: kira_semantics_model::hir::HirExprId) -> Option<u32> {
        let HirExpr::Local { local, .. } = self.program.expr(init) else {
            return None;
        };
        let slot = local.0;
        if self.candidates.contains_key(&slot) {
            return Some(slot);
        }
        let borrowed = self
            .function
            .locals
            .get(slot as usize)
            .is_some_and(|local| local.ownership.is_borrow());
        borrowed.then_some(slot)
    }
}
