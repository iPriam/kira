//! Expressions, their field-initializer helper, and the operator enums.

use super::{Block, ExprId, TypeRefId};
use crate::ownership::OwnershipOp;
use kira_core::Symbol;
use kira_source::Span;

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// An integer literal.
    Int {
        /// The parsed value.
        value: i64,
        /// Span of the literal.
        span: Span,
    },
    /// A floating-point literal.
    Float {
        /// The parsed value.
        value: f64,
        /// Span of the literal.
        span: Span,
    },
    /// A boolean literal.
    Bool {
        /// The parsed value.
        value: bool,
        /// Span of the literal.
        span: Span,
    },
    /// A string literal (decoded contents, escapes resolved).
    Str {
        /// The decoded text.
        value: String,
        /// Span of the literal including quotes.
        span: Span,
    },
    /// A reference to a name (variable, parameter, or function).
    Name {
        /// The referenced symbol.
        symbol: Symbol,
        /// Span of the reference.
        span: Span,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// The operand.
        operand: ExprId,
        /// Span covering operator and operand.
        span: Span,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// Left-hand operand.
        lhs: ExprId,
        /// Right-hand operand.
        rhs: ExprId,
        /// Span covering both operands.
        span: Span,
    },
    /// A conditional expression, `cond ? then : otherwise`.
    ///
    /// The one expression in the language that is control flow: exactly one
    /// branch is evaluated, which is why it cannot be desugared into a call
    /// and why every backend lowers it as a branch rather than a select
    /// instruction.
    Conditional {
        /// The `Bool` condition.
        cond: ExprId,
        /// The value when the condition holds.
        then: ExprId,
        /// The value when it does not.
        otherwise: ExprId,
        /// Span covering the condition through the else branch.
        span: Span,
    },
    /// A call to a named function (or the `print` builtin).
    Call {
        /// The callee name.
        callee: Symbol,
        /// Span of the callee name.
        callee_span: Span,
        /// Whether the call used a bare braced construction form (`Name { … }`).
        ///
        /// Empty braces and `let field = value` overrides are ambiguous with
        /// struct literals at parse time. Keeping this bit lets semantics
        /// choose a construct-backed value or a plain struct literal once it
        /// knows the declaration and local scopes.
        braced: bool,
        /// Generic type arguments written between the name and value arguments.
        type_args: Vec<TypeRefId>,
        /// The arguments, in written order, each optionally labeled with the
        /// parameter it binds.
        ///
        /// A construction's named child fills (`detail: { … }`) arrive here too,
        /// labeled with the slot they name; analysis tells a fill from an input
        /// by looking the label up among the declaration's child slots.
        args: Vec<CallArg>,
        /// The bare children of a trailing `{ … }` content block, in order;
        /// empty when none was written.
        ///
        /// Only a construct-backed declaration accepts child content — these
        /// fill its **first** child slot, and any other slot is filled by name
        /// through [`args`](Expr::Call::args). Analysis refuses children on
        /// anything else. A closure trailing block is not this — that attaches
        /// as a final argument in [`args`](Expr::Call::args).
        children: Vec<ExprId>,
        /// Span covering the whole call.
        span: Span,
    },
    /// A struct literal (`Point { x = 1, y = 2 }`).
    StructLit {
        /// The struct's name.
        name: Symbol,
        /// Span of the name.
        name_span: Span,
        /// The written field initializers, in source order.
        fields: Vec<FieldInit>,
        /// Span covering the whole literal.
        span: Span,
    },
    /// A method call (`p.sum()`).
    MethodCall {
        /// The expression the method is called on.
        receiver: ExprId,
        /// The method's name.
        method: Symbol,
        /// Span of the method name.
        method_span: Span,
        /// The arguments, in written order, not counting the receiver, each
        /// optionally labeled with the parameter it binds.
        args: Vec<CallArg>,
        /// Span covering the whole call.
        span: Span,
    },
    /// A field read (`p.x`).
    Field {
        /// The expression the field is read from.
        base: ExprId,
        /// The field's name.
        field: Symbol,
        /// Span of the field name.
        field_span: Span,
        /// Span covering base and field.
        span: Span,
    },
    /// An array literal (`[1, 2, 3]`, `[]`).
    ///
    /// Commas are *optional* separators: `[1 2 3]` and one element per line are
    /// both legal, so the parser ends an element where an element ends rather
    /// than at a comma.
    ArrayLit {
        /// The written elements, in order.
        elements: Vec<ExprId>,
        /// Span covering the brackets and their contents.
        span: Span,
    },
    /// An index read (`xs[0]`).
    Index {
        /// The indexed expression.
        base: ExprId,
        /// The index expression.
        index: ExprId,
        /// Span covering base and brackets.
        span: Span,
    },
    /// A leading-dot member (`.Green`, `.Ok(12)`).
    ///
    /// The base is left implicit: what it resolves against is the *expected
    /// type* at this position, which only analysis knows. In the v0 subset the
    /// expected type must be an enum, so this is how a variant is written —
    /// `.Red` for a payload-less one, `.Ok(12)` for one with a payload.
    DotMember {
        /// The member name, as written after the `.`.
        name: Symbol,
        /// Span of the member name.
        name_span: Span,
        /// The parenthesized arguments, or `None` when no `(` followed the
        /// name. `Some(vec)` distinguishes `.Red()` from `.Red`; analysis
        /// checks the count against the variant's payload.
        args: Option<Vec<ExprId>>,
        /// Span covering the whole member expression.
        span: Span,
    },
    /// A `try`: unwrap a `Result`-shaped value, routing its failure to the
    /// enclosing `attempt`'s handlers.
    ///
    /// This is an expression because that is how it is written
    /// (`let v = try f(n)`), not because it is one everywhere: analysis accepts
    /// it only as the whole initializer of a `let` directly inside an
    /// `attempt` body, and reports any other position. See
    /// `kira_semantics::stmt::attempts` for why that restriction is the
    /// reference's, not a shortcut.
    Try {
        /// The `Result`-shaped operand being unwrapped.
        value: ExprId,
        /// Span covering `try <value>`.
        span: Span,
    },
    /// An ownership transfer written on an expression (`move mesh`,
    /// `copy count`).
    ///
    /// `move` and `copy` are *contextual* identifiers, not reserved keywords:
    /// this node exists only where the token is followed by something that
    /// starts an operand, so `let move = 1` still declares a local named
    /// `move`.
    Ownership {
        /// Which operator was written.
        op: OwnershipOp,
        /// The operand the transfer applies to.
        operand: ExprId,
        /// Span covering operator and operand.
        span: Span,
    },
    /// A closure literal (`{ value in body }`, `{ in body }`, `{ a, b in … }`).
    ///
    /// The parameter list carries names only: a closure never writes its
    /// parameter types, so what they are comes from the *expected type* at this
    /// position, which only analysis knows. The bare leading `in` of a
    /// zero-parameter closure is what distinguishes `{ in update() }` from a
    /// block, and an empty parameter list is what records it.
    Closure {
        /// The parameter names, in order; empty for `{ in … }`.
        params: Vec<ClosureParam>,
        /// The closure body.
        body: Block,
        /// Span covering the braces and their contents.
        span: Span,
    },
    /// A `For` builder item inside a construction's content block:
    /// `For(x in xs) { child … }`.
    ///
    /// Not an expression anywhere else — it appears only among the
    /// [`children`](Expr::Call::children) of a construction, where it expands to
    /// one child per iteration. Its body is itself a list of content items, so a
    /// builder may nest.
    ContentFor {
        /// The loop variable's name.
        binding: Symbol,
        /// Span of the loop variable's name.
        binding_span: Span,
        /// The iterated value — an array or a `a..b` range.
        iterable: ExprId,
        /// The content items produced each iteration, in order.
        body: Vec<ExprId>,
        /// Span covering the whole `For( … ) { … }`.
        span: Span,
    },
    /// An `if` builder item inside a construction's content block:
    /// `if cond { child … } else if cond { … } else { … }`.
    ///
    /// Like [`ContentFor`](Expr::ContentFor), it appears only among a
    /// construction's [`children`](Expr::Call::children): the taken branch's
    /// items are produced and the others contribute nothing. An `else if` chain
    /// nests as a single-item `else` branch holding another `ContentIf`.
    ContentIf {
        /// The `Bool` condition.
        cond: ExprId,
        /// The content items produced when the condition holds.
        then_body: Vec<ExprId>,
        /// The content items produced otherwise; empty when no `else` was
        /// written.
        else_body: Vec<ExprId>,
        /// Span covering the whole `if … { … }`.
        span: Span,
    },
    /// A bare `{ … }` content block written as a named child fill's value:
    /// the `{ … }` of `NavigationSplitView { … } detail: { … }`.
    ///
    /// Holds content items, so a `For`/`if` builder inside it produces children
    /// exactly as one inside a trailing block does. It is not a value: analysis
    /// accepts it only where a named child fill is expected and refuses it
    /// everywhere else.
    Content {
        /// The block's content items, in order.
        children: Vec<ExprId>,
        /// Span covering the braces and their contents.
        span: Span,
    },
    /// A deferred task spawn (`Task { work(1, 2) }`).
    ///
    /// The braces hold one expression, not a content block: `Task` is not a
    /// construct, and its block is the *body* the task defers rather than a
    /// list of children. Analysis decides which bodies the executable slice
    /// accepts; the parser records the expression and stops there.
    TaskSpawn {
        /// The deferred body.
        body: ExprId,
        /// Span covering `Task { … }`.
        span: Span,
    },
    /// An expression the parser could not parse; recovery inserts this.
    Error {
        /// Span of the malformed expression.
        span: Span,
    },
}

/// One parameter of an [`Expr::Closure`].
///
/// A name and nothing else: closure parameters are never annotated, so there is
/// no type to record here.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosureParam {
    /// The parameter name.
    pub name: Symbol,
    /// Span of the name token.
    pub span: Span,
}

/// One argument of an [`Expr::Call`] or [`Expr::MethodCall`].
///
/// A bare argument (`f(x)`) carries no label; a labeled argument
/// (`f(index: x)`, `f(index = x)`) names the parameter it binds. Both binders
/// are accepted — `=` is canonical, `:` stays valid for the transition window
/// — and normalize to this one node, so nothing downstream can tell which was
/// written. Whether a label reorders or merely names its position is a
/// question for analysis, which holds the callee's parameters; the parser
/// records the name and stops there.
#[derive(Debug, Clone, PartialEq)]
pub struct CallArg {
    /// The parameter name written as a label, or `None` for a positional
    /// argument.
    pub label: Option<Symbol>,
    /// Span of the label name, present exactly when `label` is.
    pub label_span: Option<Span>,
    /// The argument value.
    pub value: ExprId,
    /// Span covering the label (if any) and the value.
    pub span: Span,
}

/// One field initializer inside a [`Expr::StructLit`].
///
/// Both binders are accepted: `=` is canonical and `:` stays valid for the
/// transition window. They normalize to this one node, so nothing downstream
/// can tell which was written.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInit {
    /// The initialized field's name.
    pub name: Symbol,
    /// Span of the field name.
    pub name_span: Span,
    /// The value bound to the field.
    pub value: ExprId,
    /// Span covering the whole initializer.
    pub span: Span,
}

impl Expr {
    /// The span covering this expression.
    pub fn span(&self) -> Span {
        match self {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::Bool { span, .. }
            | Expr::Str { span, .. }
            | Expr::Name { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Conditional { span, .. }
            | Expr::Call { span, .. }
            | Expr::StructLit { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Field { span, .. }
            | Expr::ArrayLit { span, .. }
            | Expr::Index { span, .. }
            | Expr::DotMember { span, .. }
            | Expr::Try { span, .. }
            | Expr::Ownership { span, .. }
            | Expr::Closure { span, .. }
            | Expr::ContentFor { span, .. }
            | Expr::ContentIf { span, .. }
            | Expr::Content { span, .. }
            | Expr::TaskSpawn { span, .. }
            | Expr::Error { span } => *span,
        }
    }
}

/// A unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation (`-x`).
    Neg,
    /// Logical negation (`!x`).
    Not,
    /// Bitwise complement (`~x`).
    BitNot,
}

/// A binary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    And,
    /// `||`
    Or,
    /// `&`
    BitAnd,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `<<`
    Shl,
    /// `>>`
    Shr,
}

impl BinaryOp {
    /// A short symbolic spelling of the operator, for diagnostics.
    pub fn spelling(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
        }
    }
}
