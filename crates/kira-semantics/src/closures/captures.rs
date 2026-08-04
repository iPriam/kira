//! Which names a function's closures mention, decided before its body is
//! analyzed.
//!
//! A `var` a closure captures has to live in a shared box ([`Type::Cell`]), and
//! that decision has to be made where the binding is *declared* — long before
//! the closure that captures it has been analyzed, and sometimes before it has
//! even been reached. So one syntactic pass runs first and answers the only
//! question the declaration needs: **could a closure in this function possibly
//! name this binding?**
//!
//! [`Type::Cell`]: kira_semantics_model::Type::Cell
//!
//! # It over-approximates, deliberately
//!
//! The answer is by *name*, not by resolution: every identifier written
//! anywhere inside a closure literal in this function counts, whatever it turns
//! out to mean. So a `var total` in a function whose closure mentions an
//! unrelated `total` is boxed too.
//!
//! That direction is the safe one, and the asymmetry is why the pass is shaped
//! this way: one extra box costs an allocation and a pair of indirections; one
//! *missing* box is a closure and a frame writing different storage, or —
//! once the closure escapes — a write through a slot the frame has left. The
//! analysis is a name scan rather than a resolution because a resolution would
//! have to be the real one, and the real one happens later.
//!
//! It is not the coarsest possible answer either. Boxing every `var` in any
//! function that contains a closure would need no scan at all and would put a
//! heap box behind every loop counter in every function with a callback in it.
//! Filtering by name keeps that cost off the bindings no closure could reach.
//!
//! # Why a walk and not a span comparison
//!
//! Spans are per-file and the syntax tree holds every file's items in one pair
//! of arenas, so two files' spans overlap numerically. A walk from the function
//! body down is the only reading of "inside this function" that cannot confuse
//! one file's closure for another's.

use std::collections::HashSet;

use kira_core::Names;
use kira_syntax_model::ast::{Block, Expr, ExprId, ForIterable, Stmt, StmtId, SyntaxTree};

/// Every name written inside a closure literal somewhere in `body`.
///
/// Includes names inside closures nested in closures, which is what makes the
/// answer right for a `var` declared in one closure and captured by another.
pub(crate) fn names_closures_mention(
    tree: &SyntaxTree,
    interner: &Names,
    body: &Block,
) -> HashSet<String> {
    let mut scan = Scan {
        tree,
        interner,
        inside_closure: 0,
        found: HashSet::new(),
    };
    scan.block(body);
    scan.found
}

/// The walk, carrying how deep inside a closure literal it currently is.
struct Scan<'a> {
    tree: &'a SyntaxTree,
    interner: &'a Names,
    /// How many closure literals enclose the node being visited.
    ///
    /// A depth rather than a flag so leaving an inner closure does not
    /// un-mark the outer one.
    inside_closure: u32,
    found: HashSet<String>,
}

impl Scan<'_> {
    /// Records `name` when the walk is inside a closure literal.
    fn note(&mut self, symbol: kira_core::Symbol) {
        if self.inside_closure > 0 {
            self.found.insert(self.interner.resolve(symbol).to_owned());
        }
    }

    fn block(&mut self, block: &Block) {
        for &stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, id: StmtId) {
        match self.tree.stmt(id) {
            Stmt::Let { init, .. } => self.expr(*init),
            Stmt::Assign { target, value, .. } => {
                // The target is an expression, so its root name is recorded by
                // the `Name` arm — which is what makes `total = total + x`
                // inside a closure box `total` rather than only read it.
                self.expr(*target);
                self.expr(*value);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = *value {
                    self.expr(value);
                }
            }
            Stmt::Expr { expr, .. } => self.expr(*expr),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let (cond, then_block, else_block) =
                    (*cond, then_block.clone(), else_block.clone());
                self.expr(cond);
                self.block(&then_block);
                if let Some(block) = &else_block {
                    self.block(block);
                }
            }
            Stmt::While { cond, body, .. } => {
                let (cond, body) = (*cond, body.clone());
                self.expr(cond);
                self.block(&body);
            }
            Stmt::For { iterable, body, .. } => {
                let (iterable, body) = (iterable.clone(), body.clone());
                match iterable {
                    ForIterable::Range { start, end } => {
                        self.expr(start);
                        self.expr(end);
                    }
                    ForIterable::Each { array } => self.expr(array),
                }
                self.block(&body);
            }
            Stmt::Match { subject, arms, .. } => {
                let (subject, arms) = (*subject, arms.clone());
                self.expr(subject);
                for arm in &arms {
                    self.block(&arm.body);
                }
            }
            Stmt::Attempt { body, handlers, .. } => {
                let (body, handlers) = (body.clone(), handlers.clone());
                self.block(&body);
                for handler in &handlers {
                    self.block(&handler.body);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Error { .. } => {}
        }
    }

    fn expr(&mut self, id: ExprId) {
        match self.tree.expr(id) {
            Expr::Name { symbol, .. } => {
                let symbol = *symbol;
                self.note(symbol);
            }
            Expr::Closure { body, .. } => {
                let body = body.clone();
                self.inside_closure += 1;
                self.block(&body);
                self.inside_closure -= 1;
            }
            Expr::Unary { operand, .. } => self.expr(*operand),
            Expr::Binary { lhs, rhs, .. } => {
                let (lhs, rhs) = (*lhs, *rhs);
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Conditional {
                cond,
                then,
                otherwise,
                ..
            } => {
                let (cond, then, otherwise) = (*cond, *then, *otherwise);
                self.expr(cond);
                self.expr(then);
                self.expr(otherwise);
            }
            Expr::Call { args, children, .. } => {
                let (args, children) = (args.clone(), children.clone());
                for arg in &args {
                    self.expr(arg.value);
                }
                for &child in &children {
                    self.expr(child);
                }
            }
            Expr::StructLit { fields, .. } => {
                let fields = fields.clone();
                for field in &fields {
                    self.expr(field.value);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                let (receiver, args) = (*receiver, args.clone());
                self.expr(receiver);
                for arg in &args {
                    self.expr(arg.value);
                }
            }
            Expr::Field { base, .. } => self.expr(*base),
            Expr::ArrayLit { elements, .. } => {
                let elements = elements.clone();
                for &element in &elements {
                    self.expr(element);
                }
            }
            Expr::Index { base, index, .. } => {
                let (base, index) = (*base, *index);
                self.expr(base);
                self.expr(index);
            }
            Expr::DotMember { args, .. } => {
                let args = args.clone();
                for &arg in args.iter().flatten() {
                    self.expr(arg);
                }
            }
            Expr::Try { value, .. } => self.expr(*value),
            Expr::Ownership { operand, .. } => self.expr(*operand),
            Expr::ContentFor { iterable, body, .. } => {
                let (iterable, body) = (*iterable, body.clone());
                self.expr(iterable);
                for &item in &body {
                    self.expr(item);
                }
            }
            Expr::ContentIf {
                cond,
                then_body,
                else_body,
                ..
            } => {
                let (cond, then_body, else_body) = (*cond, then_body.clone(), else_body.clone());
                self.expr(cond);
                for &item in then_body.iter().chain(else_body.iter()) {
                    self.expr(item);
                }
            }
            // A task body escapes its frame exactly as a closure does, but it
            // is not a closure: it names no captures of its own, and whatever
            // it mentions is analyzed in the enclosing frame. So the walk goes
            // through it without raising the depth.
            Expr::TaskSpawn { body, .. } => self.expr(*body),
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Str { .. }
            | Expr::Error { .. } => {}
        }
    }
}
