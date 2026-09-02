//! Evaluation order for module constants, decided on the resolved HIR.
//!
//! [`HirProgram::constants`] promises its rows sit in evaluation order: filling
//! the slots front to back at program start initializes every constant after
//! everything its initializer reads. Analysis hands the rows over in
//! declaration order; this pass runs once every body and synthesized function
//! is final, walks the real call graph each initializer can execute, sorts the
//! rows, and rewrites every [`HirExpr::ConstantGet`] slot to match.
//!
//! It runs on resolved references rather than on names because names collide:
//! two types may each have a method spelled `resolve`, and a dependency walk
//! that bridged them refused programs with no cycle in them. Here a call edge
//! is a [`FuncId`] and a read is a slot, so the only cycles left are real ones
//! — an initializer that reaches its own slot through the functions it calls —
//! and those are refused (KSEM317).
//!
//! [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram

use std::collections::{BTreeSet, HashMap};

use kira_semantics_model::hir::{Callee, HirExpr, HirExprId, HirStmt, HirStmtId, TaskTarget};

use crate::analyze::Analyzer;

/// Every constant slot and function one function's body references directly.
#[derive(Default, Clone)]
struct BodyRefs {
    /// Slots the body reads through [`HirExpr::ConstantGet`].
    constants: BTreeSet<u32>,
    /// Functions the body can enter: calls, task targets, main-thread
    /// dispatches, and callbacks whose address it takes.
    functions: BTreeSet<u32>,
}

impl Analyzer<'_> {
    /// Sorts [`HirProgram::constants`] into evaluation order and remaps every
    /// read to the moved slots, refusing an initializer that genuinely
    /// depends on its own value.
    ///
    /// [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram
    pub(crate) fn order_constant_evaluation(&mut self) {
        let count = self.program.constants.len();
        if count == 0 {
            return;
        }
        let mut body_refs: HashMap<u32, BodyRefs> = HashMap::new();
        let dependencies: Vec<BTreeSet<u32>> = (0..count)
            .map(|slot| self.reachable_constants(self.program.constants[slot].init, &mut body_refs))
            .collect();
        let order = self.solve_constant_order(&dependencies);
        self.apply_constant_order(&order);
    }

    /// Every constant slot the function `entry` can reach, transitively
    /// through every function its call graph references.
    fn reachable_constants(
        &self,
        entry: kira_semantics_model::hir::FuncId,
        body_refs: &mut HashMap<u32, BodyRefs>,
    ) -> BTreeSet<u32> {
        let mut constants = BTreeSet::new();
        let mut queue = vec![entry.0];
        let mut visited = BTreeSet::new();
        while let Some(function) = queue.pop() {
            if !visited.insert(function) {
                continue;
            }
            let refs = match body_refs.get(&function) {
                Some(refs) => refs.clone(),
                None => {
                    let refs = self.function_body_refs(function);
                    body_refs.insert(function, refs.clone());
                    refs
                }
            };
            constants.extend(refs.constants.iter().copied());
            queue.extend(refs.functions.iter().copied());
        }
        constants
    }

    /// The slots and functions one function's body references directly.
    fn function_body_refs(&self, function: u32) -> BodyRefs {
        let mut refs = BodyRefs::default();
        let Some(body) = self
            .program
            .functions
            .get(function as usize)
            .map(|function| function.body.clone())
        else {
            return refs;
        };
        let mut stmts: Vec<HirStmtId> = body;
        let mut exprs: Vec<HirExprId> = Vec::new();
        while let Some(id) = stmts.pop() {
            self.statement_refs(id, &mut stmts, &mut exprs);
            while let Some(id) = exprs.pop() {
                self.expression_refs(id, &mut refs, &mut exprs);
            }
        }
        refs
    }

    /// Queues one statement's expressions and nested statements.
    fn statement_refs(
        &self,
        id: HirStmtId,
        stmts: &mut Vec<HirStmtId>,
        exprs: &mut Vec<HirExprId>,
    ) {
        match self.program.stmt(id) {
            HirStmt::Let { init, .. } => exprs.push(*init),
            HirStmt::Assign { place, value } => {
                exprs.extend(place_indices(place));
                exprs.push(*value);
            }
            HirStmt::CellSet { value, .. } => exprs.push(*value),
            HirStmt::Return { value } => exprs.extend(value.iter().copied()),
            HirStmt::Expr { expr } => exprs.push(*expr),
            HirStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                exprs.push(*cond);
                stmts.extend(then_body.iter().copied());
                stmts.extend(else_body.iter().copied());
            }
            HirStmt::Attempt { attempt } => {
                for step in &attempt.steps {
                    stmts.extend(step.setup.iter().copied());
                    exprs.push(step.error_condition);
                    stmts.extend(step.handler.iter().copied());
                    stmts.extend(step.success.iter().copied());
                }
                stmts.extend(attempt.trailing.iter().copied());
            }
            HirStmt::While { cond, body } => {
                exprs.push(*cond);
                stmts.extend(body.iter().copied());
            }
            HirStmt::Break | HirStmt::Continue => {}
        }
    }

    /// Records one expression's references and queues its children.
    fn expression_refs(&self, id: HirExprId, refs: &mut BodyRefs, exprs: &mut Vec<HirExprId>) {
        match self.program.expr(id) {
            HirExpr::ConstantGet { constant, .. } => {
                refs.constants.insert(*constant);
            }
            HirExpr::ForeignCallbackPtr { callback } => {
                if let Some(row) = self.program.foreign_callbacks.get(*callback as usize) {
                    refs.functions.insert(row.function());
                }
            }
            HirExpr::Call {
                callee,
                args,
                writebacks,
                ..
            } => {
                if let Callee::User(id) = callee {
                    refs.functions.insert(id.0);
                }
                exprs.extend(args.iter().copied());
                for writeback in writebacks {
                    exprs.extend(place_indices(&writeback.place));
                }
            }
            HirExpr::TaskSpawn { target, args, .. } => {
                if let TaskTarget::Call(id) = target {
                    refs.functions.insert(id.0);
                }
                exprs.extend(args.iter().copied());
            }
            HirExpr::MainThreadCall { function, args, .. } => {
                refs.functions.insert(function.0);
                exprs.extend(args.iter().copied());
            }
            HirExpr::Unary { operand, .. }
            | HirExpr::CellNew { value: operand, .. }
            | HirExpr::Field { base: operand, .. }
            | HirExpr::ForeignMemberAddress { base: operand, .. }
            | HirExpr::ForeignField { base: operand, .. }
            | HirExpr::ArrayLen { array: operand }
            | HirExpr::StringLen { text: operand }
            | HirExpr::ArrayElements { value: operand, .. }
            | HirExpr::ScalarText { value: operand }
            | HirExpr::StringOf { value: operand }
            | HirExpr::CLayoutAddress { value: operand, .. }
            | HirExpr::CStringNew { text: operand }
            | HirExpr::EnumTag { value: operand }
            | HirExpr::EnumPayload { value: operand, .. }
            | HirExpr::NativeState { value: operand, .. }
            | HirExpr::NativeUserData { state: operand }
            | HirExpr::NativeRecover { raw: operand, .. }
            | HirExpr::NativeStateRetain { token: operand }
            | HirExpr::NativeStateRelease { token: operand }
            | HirExpr::Convert { operand, .. }
            | HirExpr::Distinct { value: operand, .. }
            | HirExpr::IntoAny { value: operand, .. }
            | HirExpr::TypeOf { value: operand, .. }
            | HirExpr::TypeField {
                descriptor: operand,
                ..
            }
            | HirExpr::TaskJoin {
                handle: operand, ..
            }
            | HirExpr::TaskDetach { handle: operand }
            | HirExpr::TaskCancel { handle: operand }
            | HirExpr::MainThreadJoin {
                handle: operand, ..
            } => exprs.push(*operand),
            HirExpr::Binary { lhs, rhs, .. } => {
                exprs.push(*lhs);
                exprs.push(*rhs);
            }
            HirExpr::Select {
                cond,
                then,
                otherwise,
                ..
            } => {
                exprs.push(*cond);
                exprs.push(*then);
                exprs.push(*otherwise);
            }
            HirExpr::Copy { value, .. }
            | HirExpr::TypeTest { value, .. }
            | HirExpr::TypeCast { value, .. } => exprs.push(*value),
            HirExpr::StructNew { fields, .. } => exprs.extend(fields.iter().copied()),
            HirExpr::ArrayNew { elements, .. } => exprs.extend(elements.iter().copied()),
            HirExpr::Index { base, index, .. } => {
                exprs.push(*base);
                exprs.push(*index);
            }
            HirExpr::StringCharAt { text, index } => {
                exprs.push(*text);
                exprs.push(*index);
            }
            HirExpr::StringSubstring { text, start, end } => {
                exprs.push(*text);
                exprs.push(*start);
                exprs.push(*end);
            }
            HirExpr::StringIndexOf { text, needle } => {
                exprs.push(*text);
                exprs.push(*needle);
            }
            HirExpr::StringOperation {
                text, arguments, ..
            } => {
                exprs.push(*text);
                exprs.extend(arguments.iter().copied());
            }
            HirExpr::MathOperation { operands, .. } => exprs.extend(operands.iter().copied()),
            HirExpr::FileSystem { args, .. }
            | HirExpr::Compiler { args, .. }
            | HirExpr::Env { args, .. } => exprs.extend(args.iter().copied()),
            HirExpr::ForeignElement { base, index, .. } => {
                exprs.push(*base);
                exprs.push(*index);
            }
            HirExpr::ArrayAppend { place, value } => {
                exprs.extend(place_indices(place));
                exprs.push(*value);
            }
            HirExpr::EnumNew { payload, .. } => exprs.extend(payload.iter().copied()),
            HirExpr::Int(_)
            | HirExpr::Float(_)
            | HirExpr::Bool(_)
            | HirExpr::Str(_)
            | HirExpr::RawPtrNull
            | HirExpr::CStringNull
            | HirExpr::CellNull { .. }
            | HirExpr::CellGet { .. }
            | HirExpr::Local { .. }
            | HirExpr::Error => {}
        }
    }

    /// Each slot after everything it depends on, ties broken by declaration
    /// order. A slot that cannot be placed sits on a real cycle and is
    /// refused.
    fn solve_constant_order(&mut self, dependencies: &[BTreeSet<u32>]) -> Vec<u32> {
        let count = dependencies.len();
        let mut placed = vec![false; count];
        let mut order = Vec::with_capacity(count);
        loop {
            let next = (0..count).find(|&slot| {
                !placed[slot]
                    && dependencies[slot]
                        .iter()
                        .all(|&dep| dep as usize == slot || placed[dep as usize])
                    && !dependencies[slot].contains(&(slot as u32))
            });
            match next {
                Some(slot) => {
                    placed[slot] = true;
                    order.push(slot as u32);
                }
                None => break,
            }
        }
        let stuck: Vec<u32> = (0..count as u32)
            .filter(|&slot| !placed[slot as usize])
            .collect();
        if let Some(&first) = stuck.first() {
            self.report_evaluation_cycle(dependencies, &placed, first);
            // Refused rows still need slots so every read stays in range; they
            // go last, in declaration order, and the diagnostic above stops
            // the build before anything would evaluate them.
            order.extend(stuck);
        }
        order
    }

    /// Reports one evaluation cycle, naming its members in walk order.
    fn report_evaluation_cycle(
        &mut self,
        dependencies: &[BTreeSet<u32>],
        placed: &[bool],
        start: u32,
    ) {
        let mut path = vec![start];
        let mut at = start;
        let cycle = loop {
            let next = dependencies[at as usize]
                .iter()
                .copied()
                .find(|&dep| !placed[dep as usize]);
            let Some(next) = next else {
                // Every stuck slot keeps at least one unplaced edge, so the
                // walk cannot dead-end; this arm exists so a logic error here
                // degrades to naming the path walked rather than panicking.
                break path.clone();
            };
            if let Some(position) = path.iter().position(|&seen| seen == next) {
                break path[position..].to_vec();
            }
            path.push(next);
            at = next;
        };
        let spelled: Vec<String> = cycle
            .iter()
            .chain(cycle.first())
            .map(|&slot| format!("`{}`", self.program.constants[slot as usize].name))
            .collect();
        let member = cycle[0] as usize;
        let (source, span) = (
            self.constants[member].source,
            self.constants[member].name_span,
        );
        self.source = source;
        self.emit(
            span,
            "KSEM317",
            format!(
                "module constants form a dependency cycle: {}; no member has a value to \
                 start from",
                spelled.join(" -> ")
            ),
        );
    }

    /// Moves the rows into `order` and rewrites every read to the slot its
    /// constant moved to.
    fn apply_constant_order(&mut self, order: &[u32]) {
        if order
            .iter()
            .enumerate()
            .all(|(new, &old)| new as u32 == old)
        {
            return;
        }
        let mut moved_to = vec![0u32; order.len()];
        for (new, &old) in order.iter().enumerate() {
            moved_to[old as usize] = new as u32;
        }
        let rows = std::mem::take(&mut self.program.constants);
        let mut slots: Vec<Option<kira_semantics_model::hir::HirConstant>> =
            rows.into_iter().map(Some).collect();
        for &old in order {
            if let Some(row) = slots[old as usize].take() {
                self.program.constants.push(row);
            }
        }
        let entries = std::mem::take(&mut self.constants);
        let mut entry_slots: Vec<Option<crate::constants::ConstantEntry>> =
            entries.into_iter().map(Some).collect();
        for &old in order {
            if let Some(entry) = entry_slots[old as usize].take() {
                self.constants.push(entry);
            }
        }
        for slot in self.constant_index.values_mut() {
            *slot = moved_to[*slot as usize];
        }
        for (_, expr) in self.program.exprs.iter_mut() {
            if let HirExpr::ConstantGet { constant, .. } = expr {
                *constant = moved_to[*constant as usize];
            }
        }
    }
}

/// The index expressions inside one place's path.
fn place_indices(place: &kira_semantics_model::hir::HirPlace) -> Vec<HirExprId> {
    place
        .path
        .iter()
        .filter_map(|step| match step {
            kira_semantics_model::hir::HirPlaceStep::Index(index) => Some(*index),
            kira_semantics_model::hir::HirPlaceStep::Field(_) => None,
        })
        .collect()
}
