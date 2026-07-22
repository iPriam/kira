//! Detecting which methods mutate their receiver.
//!
//! A method "mutates self" when its body, rooted at the `self` name, assigns to
//! `self` (`self.field = x`), appends through `self` (`self.xs.append(v)`), or
//! calls another mutating method on `self` (`self.child.m()`), transitively. The
//! last case is what makes this a fixpoint rather than a single scan: a method
//! that calls one that is only later discovered to mutate has to be revisited.
//!
//! The result is a per-[`FuncId`] flag, computed once after signatures are
//! collected and before any body is analyzed — because a body analyzes `self`
//! as mutable exactly when this says the method mutates it, and a call site
//! writes the receiver back exactly when this says the callee does.
//!
//! Scanning is deliberately over the *syntax* rather than the HIR: the HIR does
//! not exist yet, and the shapes that count — an assignment target rooted at
//! `self`, an `append`, a method call on a `self`-rooted place — are all
//! recognizable from the tree plus the struct table the signatures already
//! built. A closure body is skipped: a closure captures `self` by value, so a
//! mutation inside one never reaches the method's receiver.

use kira_semantics_model::hir::FuncId;
use kira_semantics_model::{StructId, Type};
use kira_syntax_model::ast::{Block, Expr, ExprId, ForIterable, Function, Stmt, StmtId};

use crate::analyze::{Analyzer, Callable};

impl<'a> Analyzer<'a> {
    /// Computes [`Analyzer::mutating_methods`] to a fixpoint.
    ///
    /// Each pass marks a method mutating when its body directly mutates `self`
    /// or calls a method already marked mutating; the flag only ever flips from
    /// `false` to `true`, so the iteration is monotonic and terminates when a
    /// pass changes nothing.
    pub(crate) fn collect_mutating_methods(&mut self, callables: &[Callable<'a>]) {
        self.mutating_methods = vec![false; callables.len()];
        loop {
            let mut changed = false;
            for (index, callable) in callables.iter().enumerate() {
                if self.mutating_methods[index] {
                    continue;
                }
                let Some(owner) = callable.receiver else {
                    continue;
                };
                if self.body_mutates_self(callable.function, owner) {
                    self.mutating_methods[index] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// Whether the method with id `id` mutates its receiver.
    ///
    /// `false` for a free function, a synthesized function, or any id past the
    /// table — only a declared method can carry the flag.
    pub(crate) fn mutates_self(&self, id: FuncId) -> bool {
        self.mutating_methods
            .get(id.0 as usize)
            .copied()
            .unwrap_or(false)
    }

    fn body_mutates_self(&self, function: &Function, owner: StructId) -> bool {
        self.block_mutates_self(&function.body, owner)
    }

    fn block_mutates_self(&self, block: &Block, owner: StructId) -> bool {
        block
            .stmts
            .iter()
            .any(|&stmt| self.stmt_mutates_self(stmt, owner))
    }

    fn stmt_mutates_self(&self, id: StmtId, owner: StructId) -> bool {
        match self.tree.stmt(id) {
            // The direct assignment case: `self.PATH = value`. A target whose
            // root is `self` writes into the receiver, whatever the path.
            Stmt::Assign { target, value, .. } => {
                self.self_rooted_type(*target, owner).is_some()
                    || self.expr_mutates_self(*target, owner)
                    || self.expr_mutates_self(*value, owner)
            }
            Stmt::Let { init, .. } => self.expr_mutates_self(*init, owner),
            Stmt::Return { value, .. } => {
                value.is_some_and(|value| self.expr_mutates_self(value, owner))
            }
            Stmt::Expr { expr, .. } => self.expr_mutates_self(*expr, owner),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr_mutates_self(*cond, owner)
                    || self.block_mutates_self(then_block, owner)
                    || else_block
                        .as_ref()
                        .is_some_and(|block| self.block_mutates_self(block, owner))
            }
            Stmt::While { cond, body, .. } => {
                self.expr_mutates_self(*cond, owner) || self.block_mutates_self(body, owner)
            }
            Stmt::For { iterable, body, .. } => {
                let iter = match iterable {
                    ForIterable::Range { start, end } => {
                        self.expr_mutates_self(*start, owner) || self.expr_mutates_self(*end, owner)
                    }
                    ForIterable::Each { array } => self.expr_mutates_self(*array, owner),
                };
                iter || self.block_mutates_self(body, owner)
            }
            Stmt::Switch {
                subject,
                cases,
                default_block,
                ..
            } => {
                self.expr_mutates_self(*subject, owner)
                    || cases.iter().any(|case| {
                        self.expr_mutates_self(case.label, owner)
                            || self.block_mutates_self(&case.body, owner)
                    })
                    || default_block
                        .as_ref()
                        .is_some_and(|block| self.block_mutates_self(block, owner))
            }
            Stmt::Match { subject, arms, .. } => {
                self.expr_mutates_self(*subject, owner)
                    || arms
                        .iter()
                        .any(|arm| self.block_mutates_self(&arm.body, owner))
            }
            Stmt::Attempt { body, handlers, .. } => {
                self.block_mutates_self(body, owner)
                    || handlers
                        .iter()
                        .any(|arm| self.block_mutates_self(&arm.body, owner))
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Error { .. } => false,
        }
    }

    /// Whether the expression subtree contains a mutation of `self`.
    ///
    /// A method call on a `self`-rooted place mutates `self` when it is an
    /// `append` or when the method resolves to one already marked mutating; a
    /// bare call names one of the receiver's own methods (the implicit-`self`
    /// form), and mutates `self` when that method does.
    fn expr_mutates_self(&self, id: ExprId, owner: StructId) -> bool {
        match self.tree.expr(id) {
            Expr::MethodCall {
                receiver,
                method,
                args,
                ..
            } => {
                let direct = self.self_rooted_type(*receiver, owner).is_some_and(|ty| {
                    let name = self.interner.resolve(*method);
                    name == "append" || self.method_mutates(ty, name)
                });
                direct
                    || self.expr_mutates_self(*receiver, owner)
                    || args
                        .iter()
                        .any(|arg| self.expr_mutates_self(arg.value, owner))
            }
            Expr::Call { callee, args, .. } => {
                let name = self.interner.resolve(*callee);
                self.method_mutates(Type::Struct(owner), name)
                    || args
                        .iter()
                        .any(|arg| self.expr_mutates_self(arg.value, owner))
            }
            Expr::Field { base, .. } => self.expr_mutates_self(*base, owner),
            Expr::Index { base, index, .. } => {
                self.expr_mutates_self(*base, owner) || self.expr_mutates_self(*index, owner)
            }
            Expr::Unary { operand, .. } | Expr::Ownership { operand, .. } => {
                self.expr_mutates_self(*operand, owner)
            }
            Expr::Try { value, .. } => self.expr_mutates_self(*value, owner),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr_mutates_self(*lhs, owner) || self.expr_mutates_self(*rhs, owner)
            }
            Expr::Conditional {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.expr_mutates_self(*cond, owner)
                    || self.expr_mutates_self(*then, owner)
                    || self.expr_mutates_self(*otherwise, owner)
            }
            Expr::StructLit { fields, .. } => fields
                .iter()
                .any(|field| self.expr_mutates_self(field.value, owner)),
            Expr::ArrayLit { elements, .. } => elements
                .iter()
                .any(|&element| self.expr_mutates_self(element, owner)),
            Expr::DotMember {
                args: Some(args), ..
            } => args.iter().any(|&arg| self.expr_mutates_self(arg, owner)),
            Expr::ContentFor { iterable, body, .. } => {
                self.expr_mutates_self(*iterable, owner)
                    || body.iter().any(|&item| self.expr_mutates_self(item, owner))
            }
            Expr::ContentIf {
                cond,
                then_body,
                else_body,
                ..
            } => {
                self.expr_mutates_self(*cond, owner)
                    || then_body
                        .iter()
                        .any(|&item| self.expr_mutates_self(item, owner))
                    || else_body
                        .iter()
                        .any(|&item| self.expr_mutates_self(item, owner))
            }
            // A closure captures `self` by value, so a mutation inside its body
            // never reaches the enclosing method's receiver — it is not scanned.
            Expr::Closure { .. }
            | Expr::DotMember { args: None, .. }
            | Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Str { .. }
            | Expr::Name { .. }
            | Expr::Error { .. } => false,
        }
    }

    /// Whether the method named `method` on receiver type `ty` is currently
    /// marked mutating.
    fn method_mutates(&self, ty: Type, method: &str) -> bool {
        let Type::Struct(id) = ty else {
            return false;
        };
        let qualified = format!("{}.{method}", self.type_name(Type::Struct(id)));
        self.lookup_function(&qualified)
            .is_some_and(|(id, _, _)| self.mutates_self(id))
    }

    /// The type a `self`-rooted place expression names, or `None` when it is not
    /// rooted at `self`.
    ///
    /// `self` is the receiver struct; a field walks into that field's type; an
    /// index walks into the element type. Anything else is not a `self`-rooted
    /// place.
    fn self_rooted_type(&self, id: ExprId, owner: StructId) -> Option<Type> {
        match self.tree.expr(id) {
            Expr::Name { symbol, .. } => {
                (self.interner.resolve(*symbol) == "self").then_some(Type::Struct(owner))
            }
            Expr::Field { base, field, .. } => {
                let Type::Struct(base_id) = self.self_rooted_type(*base, owner)? else {
                    return None;
                };
                let def = self.program.types.structs().get(base_id)?;
                let index = def.field_index(self.interner.resolve(*field))?;
                Some(def.field(index)?.ty)
            }
            Expr::Index { base, .. } => {
                let base_ty = self.self_rooted_type(*base, owner)?;
                self.program.types.element_of(base_ty)
            }
            _ => None,
        }
    }
}
