//! Statements and the pieces that only appear inside them (blocks, `switch`
//! arms, and what a `for` iterates).

use super::{ExprId, StmtId, TypeRefId};
use kira_core::Symbol;
use kira_source::Span;

/// One `case` arm of a [`Stmt::Switch`].
///
/// The label is an expression rather than a pattern: Kira compares it to the
/// subject with `==`, so what may be written here is whatever `==` accepts
/// against the subject's type.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchCase {
    /// The label compared against the subject.
    pub label: ExprId,
    /// The statements run when the label matches.
    pub body: Block,
    /// Span covering the whole arm.
    pub span: Span,
}

/// What a [`Stmt::For`] iterates.
///
/// The two forms are told apart by the `..`, and they are separate variants
/// rather than one expression because a range is **not a value** in Kira: there
/// is no standalone range type, so `0..5` can only be written here. Making it a
/// variant is what keeps a range out of [`Expr`](super::Expr) entirely.
#[derive(Debug, Clone, PartialEq)]
pub enum ForIterable {
    /// A half-open integer range (`0..5`): `start` is included, `end` is not.
    Range {
        /// The inclusive lower bound.
        start: ExprId,
        /// The exclusive upper bound.
        end: ExprId,
    },
    /// Every element of an array, in order (`xs`).
    Each {
        /// The array-typed expression being iterated.
        array: ExprId,
    },
}

/// A brace-delimited sequence of statements.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Block {
    /// The statements in the block, in order.
    pub stmts: Vec<StmtId>,
    /// Span covering the braces and their contents.
    pub span: Span,
}

/// A statement inside a function body.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// A `let` / `var` binding with an initializer.
    Let {
        /// Bound name.
        name: Symbol,
        /// Span of the name token.
        name_span: Span,
        /// `true` for `var`, `false` for `let`.
        mutable: bool,
        /// Optional written type annotation.
        ty: Option<TypeRefId>,
        /// The initializing expression.
        init: ExprId,
        /// Span covering the statement.
        span: Span,
    },
    /// An assignment to an existing place (`name = value`, `p.x = value`).
    ///
    /// The target is an expression because a place is written with expression
    /// syntax; semantics is what decides whether it names a place at all.
    Assign {
        /// The assigned-to place, as written.
        target: ExprId,
        /// The value expression.
        value: ExprId,
        /// Span covering the statement.
        span: Span,
    },
    /// A `return` with an optional value.
    Return {
        /// The returned expression, if any.
        value: Option<ExprId>,
        /// Span covering the statement.
        span: Span,
    },
    /// An expression evaluated for its effect (e.g. a call to `print`).
    Expr {
        /// The evaluated expression.
        expr: ExprId,
        /// Span covering the statement.
        span: Span,
    },
    /// An `if` / optional `else`.
    If {
        /// The condition expression.
        cond: ExprId,
        /// The `then` block.
        then_block: Block,
        /// The optional `else` block.
        else_block: Option<Block>,
        /// Span covering the statement.
        span: Span,
    },
    /// A `while` loop.
    While {
        /// The loop condition.
        cond: ExprId,
        /// The loop body.
        body: Block,
        /// Span covering the statement.
        span: Span,
    },
    /// A `for` loop over a range (`for i in 0..5`) or an array
    /// (`for x in xs`).
    For {
        /// The loop variable, bound fresh and immutable on each iteration.
        name: Symbol,
        /// Span of the loop variable's name token.
        name_span: Span,
        /// What is being iterated.
        iterable: ForIterable,
        /// The loop body.
        body: Block,
        /// Span covering the statement.
        span: Span,
    },
    /// A `switch`: the first `case` whose label equals the subject runs.
    ///
    /// There is no fallthrough, and a `switch` is a statement rather than an
    /// expression — an arm that wants to produce a value assigns or returns.
    Switch {
        /// The value being matched, evaluated once.
        subject: ExprId,
        /// The `case` arms, in source order.
        cases: Vec<SwitchCase>,
        /// The `default` arm, when one was written.
        ///
        /// A `switch` with no `default` and no matching case does nothing.
        default_block: Option<Block>,
        /// Span covering the whole statement.
        span: Span,
    },
    /// A `break`: leave the innermost enclosing loop.
    Break {
        /// Span covering the statement.
        span: Span,
    },
    /// A `continue`: skip to the innermost enclosing loop's next iteration.
    Continue {
        /// Span covering the statement.
        span: Span,
    },
    /// A statement position the parser could not parse; recovery inserts this.
    Error {
        /// Span covering the skipped tokens.
        span: Span,
    },
}

impl Stmt {
    /// The span covering this statement.
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Expr { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Error { span } => *span,
        }
    }
}
