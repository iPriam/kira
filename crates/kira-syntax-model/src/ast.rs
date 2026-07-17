//! The concrete syntax tree the parser produces.
//!
//! The tree follows the index/arena law: expressions and statements live in
//! arenas and reference each other by [`la_arena::Idx`], so no node carries a
//! lifetime. Every node records a [`Span`]. The tree is error-resilient —
//! unparseable positions become [`Expr::Error`] / [`Stmt::Error`] / an
//! [`Item::Unsupported`] node rather than aborting the parse.

use crate::ownership::{OwnershipMode, OwnershipOp};
use kira_core::Symbol;
use kira_runtime_abi::Execution;
use kira_source::Span;
use la_arena::{Arena, Idx};

/// Handle to an expression stored in a [`SyntaxTree`].
pub type ExprId = Idx<Expr>;
/// Handle to a statement stored in a [`SyntaxTree`].
pub type StmtId = Idx<Stmt>;
/// Handle to a written type reference stored in a [`SyntaxTree`].
pub type TypeRefId = Idx<TypeRef>;

/// A whole parsed source file: its top-level items plus the node arenas.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyntaxTree {
    /// Top-level items in source order.
    pub items: Vec<Item>,
    /// Arena backing every [`ExprId`].
    pub exprs: Arena<Expr>,
    /// Arena backing every [`StmtId`].
    pub stmts: Arena<Stmt>,
    /// Arena backing every [`TypeRefId`].
    pub types: Arena<TypeRef>,
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

    /// Interns a type reference node, returning its handle.
    pub fn add_type(&mut self, ty: TypeRef) -> TypeRefId {
        self.types.alloc(ty)
    }

    /// Borrows a type reference by handle.
    pub fn type_ref(&self, id: TypeRefId) -> &TypeRef {
        &self.types[id]
    }
}

/// A top-level declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A function declaration.
    Function(Function),
    /// A `struct` declaration: a non-inheriting value shape.
    Struct(StructDecl),
    /// An `enum` declaration: a tagged union of named variants.
    Enum(EnumDecl),
    /// A construct the v0 subset parses but does not yet analyze (class,
    /// import, …); recorded so semantics can report it cleanly.
    Unsupported(UnsupportedItem),
}

/// An `enum` declaration: a named set of variants, each optionally carrying a
/// single payload value.
///
/// Variants are separated by newlines or spaces — never commas — so the variant
/// name is what starts each one. A variant may carry a payload written either
/// `Name(Type)` or `Name: Type = default`; the second form supplies a default
/// used when the variant is constructed with no explicit payload.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// The enum's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The variants, in declaration order.
    pub variants: Vec<VariantDecl>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One variant of an [`EnumDecl`].
#[derive(Debug, Clone, PartialEq)]
pub struct VariantDecl {
    /// The variant's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// The written payload type, when the variant carries one.
    pub payload: Option<TypeRefId>,
    /// The default payload initializer, when one was written (the `= expr`
    /// form). Only meaningful when `payload` is present.
    pub default: Option<ExprId>,
    /// Span covering the whole variant.
    pub span: Span,
}

/// A `struct` declaration: a named, non-inheriting value shape.
///
/// Members are written with `let` (immutable) or `var` (mutable) and may carry
/// a default initializer. Members are separated by newlines or `;` — the
/// parser treats both as insignificant, so the member keyword is what starts
/// each one.
#[derive(Debug, Clone, PartialEq)]
pub struct StructDecl {
    /// The struct's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The stored members, in declaration order.
    pub fields: Vec<FieldDecl>,
    /// The methods declared in the body, in declaration order.
    ///
    /// A method is an ordinary [`Function`] here; what makes it a method is
    /// where it was written. Analysis is what gives it its receiver.
    pub methods: Vec<Function>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One stored member of a [`StructDecl`].
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDecl {
    /// The member's name.
    pub name: Symbol,
    /// Span of the name token.
    pub name_span: Span,
    /// `true` for `var`, `false` for `let`.
    pub mutable: bool,
    /// The declared member type.
    pub ty: TypeRefId,
    /// The default initializer, when one was written.
    pub default: Option<ExprId>,
    /// Span covering the whole member.
    pub span: Span,
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
    pub return_type: Option<TypeRefId>,
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
    /// How the parameter takes its argument.
    ///
    /// [`OwnershipMode::Owned`] when the type was written bare — an owned
    /// parameter is the default, not a special case.
    pub ownership: OwnershipMode,
    /// Span of the written ownership prefix (`borrow mut`), absent when the
    /// type was bare. Diagnostics point here to say where a mode came from.
    pub ownership_span: Option<Span>,
    /// The declared parameter type, with any ownership prefix stripped.
    pub ty: TypeRefId,
    /// Span covering the whole parameter.
    pub span: Span,
}

/// A written type reference, e.g. `Int`, `Point`, `[Int]`, or `[[Byte]]`.
///
/// An arena node rather than a flat `Copy` struct because an array type nests:
/// `[[Int]]`'s element is itself a written type. Following the index/arena law
/// — a [`TypeRefId`] into the tree's arena, never a `Box` — is what keeps this
/// free of the recursive-allocation-per-node cost and keeps the whole model
/// lifetime-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeRef {
    /// A named type: `Int`, `String`, `Point`.
    Named {
        /// The type name as an interned symbol.
        name: Symbol,
        /// Where the type name appears.
        span: Span,
    },
    /// An array type: `[Int]`.
    Array {
        /// The written element type.
        element: TypeRefId,
        /// Span covering the brackets and their contents.
        span: Span,
    },
    /// A type position the parser could not parse; recovery inserts this.
    ///
    /// A variant rather than a sentinel name, so analysis resolves it to
    /// `Type::Error` **silently**: the parser already said what was wrong, and
    /// a second "unknown type `<error>`" on top of it would name a type nobody
    /// wrote.
    Error {
        /// Span of the malformed type.
        span: Span,
    },
}

impl TypeRef {
    /// The span covering this type reference.
    pub fn span(&self) -> Span {
        match self {
            TypeRef::Named { span, .. } | TypeRef::Array { span, .. } | TypeRef::Error { span } => {
                *span
            }
        }
    }
}

/// A parsed-but-unanalyzed top-level construct.
#[derive(Debug, Clone, PartialEq)]
pub struct UnsupportedItem {
    /// A short label naming the construct (`"struct"`, `"import"`, …).
    pub keyword: &'static str,
    /// Span covering the construct.
    pub span: Span,
}

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
/// variant is what keeps a range out of [`Expr`] entirely.
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
        /// The argument expressions, in order, not counting the receiver.
        args: Vec<ExprId>,
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
    /// An expression the parser could not parse; recovery inserts this.
    Error {
        /// Span of the malformed expression.
        span: Span,
    },
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
            | Expr::Call { span, .. }
            | Expr::StructLit { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Field { span, .. }
            | Expr::ArrayLit { span, .. }
            | Expr::Index { span, .. }
            | Expr::DotMember { span, .. }
            | Expr::Ownership { span, .. }
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
