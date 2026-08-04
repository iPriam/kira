//! Statements and the pieces that only appear inside them (blocks, `match`
//! arms, and what a `for` iterates).

use super::{ExprId, StmtId, TypeRefId};
use crate::ownership::OwnershipMode;
use kira_core::Symbol;
use kira_source::Span;

/// One arm of a [`Stmt::Match`].
///
/// The head is a *pattern*, not an expression: a bare variant name with an
/// optional binding for its payload. The name is
/// unqualified — a `match` already knows the subject's enum, so `Red` names
/// that enum's variant and `EmxColor.Red` is not the spelling.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    /// The variant this arm selects, unqualified.
    pub variant: Symbol,
    /// Span of the variant's name token.
    pub variant_span: Span,
    /// The name bound to the variant's payload, when the arm wrote one.
    pub binding: Option<MatchBinding>,
    /// The statements run when the subject holds this variant.
    ///
    /// An arrow arm (`Red -> return 1`) is a one-statement block; an
    /// arrow-block arm (`Red -> { … }`) is the block as written. They are the
    /// same thing by the time they are here.
    pub body: Block,
    /// Span covering the whole arm.
    pub span: Span,
}

/// A `match` arm's payload binding: `Label(text)` binds `text`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchBinding {
    /// The name bound to the payload, immutable for the arm's body.
    pub name: Symbol,
    /// Span of the binding's name token.
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
        /// The ownership prefix written on that annotation, if any.
        ///
        /// A binding may say how it takes its initializer — `let f: borrow (Int)
        /// -> Void = g` — exactly as a parameter says how it takes its argument.
        /// [`OwnershipMode::Owned`] is the default rather than a fallback.
        ownership: OwnershipMode,
        /// Span of the ownership prefix, for the diagnostic that refuses one.
        ownership_span: Option<Span>,
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
    /// A `match`: the arm naming the subject's variant runs.
    ///
    /// Selects on an enum's *variant* rather than on `==`, which is what lets
    /// an arm bind the variant's payload — and what makes coverage a question
    /// worth asking, so a `match` is checked exhaustive and checked for a
    /// variant matched twice. A chain of `==` comparisons is neither, because
    /// its labels are arbitrary expressions with no set to be exhaustive
    /// over.
    Match {
        /// The value being matched, evaluated once.
        subject: ExprId,
        /// The arms, in source order.
        arms: Vec<MatchArm>,
        /// Span covering the whole statement.
        span: Span,
    },
    /// An `attempt { … } handle { … }`: run a body that may `try`, and route
    /// the failure to the arm naming its variant.
    ///
    /// The handler arms carry the same shape as a [`Stmt::Match`] arm — a
    /// variant name with an optional payload binding — and reuse [`MatchArm`]
    /// for exactly that reason. Only the spelling differs: a handler arm is
    /// written `MissingNode(reason) { … }`, with no `->`.
    Attempt {
        /// The guarded body, in which `try` may appear.
        body: Block,
        /// The handler arms, in source order.
        handlers: Vec<MatchArm>,
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
            | Stmt::Match { span, .. }
            | Stmt::Attempt { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Error { span } => *span,
        }
    }
}
