//! The concrete syntax tree the parser produces.
//!
//! The tree follows the index/arena law: expressions and statements live in
//! arenas and reference each other by [`la_arena::Idx`], so no node carries a
//! lifetime. Every node records a [`Span`]. The tree is error-resilient —
//! unparseable positions become [`Expr::Error`] / [`Stmt::Error`] / an
//! [`Item::Unsupported`] node rather than aborting the parse.

use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;
use la_arena::{Arena, Idx};

/// Handle to an expression stored in a [`SyntaxTree`].
pub type ExprId = Idx<Expr>;
/// Handle to a statement stored in a [`SyntaxTree`].
pub type StmtId = Idx<Stmt>;

/// A whole parsed source file: its top-level items plus the node arenas.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyntaxTree {
    /// Top-level items in source order.
    pub items: Vec<Item>,
    /// Arena backing every [`ExprId`].
    pub exprs: Arena<Expr>,
    /// Arena backing every [`StmtId`].
    pub stmts: Arena<Stmt>,
}

impl SyntaxTree {
    /// Creates an empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns an expression node, returning its handle.
    pub fn add_expr(&mut self, expr: Expr) -> ExprId {
        self.exprs.alloc(expr)
    }

    /// Interns a statement node, returning its handle.
    pub fn add_stmt(&mut self, stmt: Stmt) -> StmtId {
        self.stmts.alloc(stmt)
    }

    /// Borrows an expression by handle.
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id]
    }

    /// Borrows a statement by handle.
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id]
    }
}

/// A top-level declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A function declaration.
    Function(Function),
    /// A construct the v0 subset parses but does not yet analyze (struct,
    /// enum, class, import, …); recorded so semantics can report it cleanly.
    Unsupported(UnsupportedItem),
}

/// A function declaration: signature plus body.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// The function's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// Whether the declaration carried the `@Main` annotation.
    pub is_main: bool,
    /// The engine the declaration selected with `@Runtime` / `@Native`.
    ///
    /// [`Execution::Inherited`] when neither was written — the syntax tree
    /// records what the source said, and leaves resolving the default to the
    /// build.
    pub execution: Execution,
    /// Declared parameters, in order.
    pub params: Vec<Param>,
    /// Declared return type, if written (absent means `Void`).
    pub return_type: Option<TypeRef>,
    /// The function body.
    pub body: Block,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One declared function parameter.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// The parameter name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// The declared parameter type.
    pub ty: TypeRef,
    /// Span covering the whole parameter.
    pub span: Span,
}

/// A written type reference, e.g. `Int` or `String`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeRef {
    /// The type name as an interned symbol.
    pub name: Symbol,
    /// Where the type name appears.
    pub span: Span,
}

/// A parsed-but-unanalyzed top-level construct.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedItem {
    /// A short label naming the construct (`"struct"`, `"import"`, …).
    pub keyword: &'static str,
    /// Span covering the construct.
    pub span: Span,
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
        ty: Option<TypeRef>,
        /// The initializing expression.
        init: ExprId,
        /// Span covering the statement.
        span: Span,
    },
    /// An assignment to an existing binding (`name = value`).
    Assign {
        /// Target name.
        name: Symbol,
        /// Span of the target name.
        name_span: Span,
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
            | Stmt::Error { span } => *span,
        }
    }
}

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
    /// A call to a named function (or the `print` builtin).
    Call {
        /// The callee name.
        callee: Symbol,
        /// Span of the callee name.
        callee_span: Span,
        /// The argument expressions, in order.
        args: Vec<ExprId>,
        /// Span covering the whole call.
        span: Span,
    },
    /// An expression the parser could not parse; recovery inserts this.
    Error {
        /// Span of the malformed expression.
        span: Span,
    },
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
            | Expr::Call { span, .. }
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
        }
    }
}
