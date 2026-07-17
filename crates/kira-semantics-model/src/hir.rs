//! The high-level IR: a name-resolved, fully-typed form of a program.
//!
//! The HIR is the analyzer's output and the IR lowerer's input. Names are
//! resolved (variables to [`LocalId`], calls to a [`Callee`]) and every
//! expression carries its [`Type`]. Nodes live in per-program arenas and refer
//! to each other by index, so no HIR type carries a lifetime. Local indices
//! are scoped to their owning function.

use crate::ty::{StructId, StructTable, Type};
use kira_runtime_abi::Execution;
use kira_source::Span;
use kira_syntax_model::ownership::OwnershipMode;
use la_arena::{Arena, Idx};

/// Handle to a HIR expression.
pub type HirExprId = Idx<HirExpr>;
/// Handle to a HIR statement.
pub type HirStmtId = Idx<HirStmt>;

/// Index of a function within a [`HirProgram`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);

/// Index of a local (parameter or `let`/`var` binding) within a function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// A fully analyzed program.
///
/// Names in the HIR (and downstream IR/bytecode) are owned `String`s rather
/// than `kira_core::Symbol` — a deliberate exception to the interned-names
/// rule: after analysis, names survive only for diagnostics and disassembly,
/// and owning them keeps query results self-contained (no interner has to
/// outlive a salsa revision) and keeps the VM subtree decoupled from the
/// frontend's interner. Structs kept that shape: a struct's name and its field
/// names are diagnostic and codegen-naming material only — a field is resolved
/// to an index during analysis, and nothing downstream looks one up by name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HirProgram {
    /// Every analyzed function, in source order.
    pub functions: Vec<HirFunction>,
    /// Every declared struct.
    pub structs: StructTable,
    /// The index of the `@Main` entrypoint, when the program has a valid one.
    pub main: Option<FuncId>,
    /// Arena backing every [`HirExprId`].
    pub exprs: Arena<HirExpr>,
    /// Arena backing every [`HirStmtId`].
    pub stmts: Arena<HirStmt>,
}

impl HirProgram {
    /// Borrows a HIR expression by handle.
    pub fn expr(&self, id: HirExprId) -> &HirExpr {
        &self.exprs[id]
    }

    /// Borrows a HIR statement by handle.
    pub fn stmt(&self, id: HirStmtId) -> &HirStmt {
        &self.stmts[id]
    }
}

/// One analyzed function.
#[derive(Debug, Clone, PartialEq)]
pub struct HirFunction {
    /// The function's name.
    pub name: String,
    /// The number of leading locals that are parameters.
    pub param_count: u32,
    /// The declared return type (`Void` when none was written).
    pub return_type: Type,
    /// Every local slot, parameters first, then body bindings, in order.
    pub locals: Vec<HirLocal>,
    /// The function body as a statement list.
    pub body: Vec<HirStmtId>,
    /// Whether this is the `@Main` entrypoint.
    pub is_main: bool,
    /// The engine this function's body runs on, as written in the source.
    pub execution: Execution,
    /// Span of the function's name, for diagnostics.
    pub name_span: Span,
}

/// One local slot: a parameter or a `let`/`var` binding.
#[derive(Debug, Clone, PartialEq)]
pub struct HirLocal {
    /// The binding name.
    pub name: String,
    /// The binding's resolved type.
    pub ty: Type,
    /// Whether the binding may be reassigned (`var`) or not (`let`/param).
    pub mutable: bool,
    /// How this local holds its value.
    ///
    /// [`OwnershipMode::Owned`] for every body binding and every bare
    /// parameter; a borrow mode only for a parameter declared `borrow` /
    /// `borrow mut`. The analyzer needs it to reject `move` of a borrowed
    /// parameter ([`KSEM111`-class]); nothing below the HIR reads it, because
    /// for the current type lattice a borrow and a deep copy are
    /// indistinguishable at run time.
    ///
    /// [`KSEM111`-class]: https://docs.kira-lang.com/diagnostics
    pub ownership: OwnershipMode,
}

/// A statement in a function body.
#[derive(Debug, Clone, PartialEq)]
pub enum HirStmt {
    /// Initialize a freshly-declared local.
    Let {
        /// The local being initialized.
        local: LocalId,
        /// The initializing expression.
        init: HirExprId,
    },
    /// Write to an existing mutable place.
    Assign {
        /// The place being written.
        place: HirPlace,
        /// The new value.
        value: HirExprId,
    },
    /// Return from the function, optionally with a value.
    Return {
        /// The returned expression, if any.
        value: Option<HirExprId>,
    },
    /// Evaluate an expression for effect.
    Expr {
        /// The evaluated expression.
        expr: HirExprId,
    },
    /// Conditional execution.
    If {
        /// The (boolean) condition.
        cond: HirExprId,
        /// Statements run when the condition holds.
        then_body: Vec<HirStmtId>,
        /// Statements run otherwise (empty when there is no `else`).
        else_body: Vec<HirStmtId>,
    },
    /// A pre-tested loop.
    ///
    /// The only loop shape in the HIR: a `for` in the source is desugared to
    /// one during analysis, so nothing below this layer learns `for` exists.
    While {
        /// The (boolean) loop condition.
        cond: HirExprId,
        /// The loop body.
        body: Vec<HirStmtId>,
    },
    /// Leave the innermost enclosing loop.
    ///
    /// Analysis rejects one outside a loop, so a backend may assume an
    /// enclosing [`HirStmt::While`] exists.
    Break,
    /// Skip to the innermost enclosing loop's next iteration.
    ///
    /// As with [`HirStmt::Break`], analysis guarantees an enclosing loop.
    Continue,
}

/// A writable location: a local, optionally walked into by field indices.
///
/// `p` is the local with an empty path; `b.size.x` is the local `b` with the
/// path `[size, x]`, resolved to indices during analysis. Resolving the whole
/// path up front is what lets a backend write through it in place instead of
/// rebuilding the enclosing value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirPlace {
    /// The local the place is rooted at.
    pub local: LocalId,
    /// Field indices to walk, outermost first; empty writes the local itself.
    pub path: Vec<u32>,
}

/// An expression, carrying its resolved type.
#[derive(Debug, Clone, PartialEq)]
pub enum HirExpr {
    /// An integer constant.
    Int(i64),
    /// A floating-point constant.
    Float(f64),
    /// A boolean constant.
    Bool(bool),
    /// A string constant.
    Str(String),
    /// A read of a local slot.
    Local {
        /// The referenced local.
        local: LocalId,
        /// The local's type.
        ty: Type,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        op: HirUnaryOp,
        /// The operand.
        operand: HirExprId,
        /// The result type.
        ty: Type,
    },
    /// A binary operation.
    Binary {
        /// The operator (already resolved to a typed variant).
        op: HirBinaryOp,
        /// Left operand.
        lhs: HirExprId,
        /// Right operand.
        rhs: HirExprId,
        /// The result type.
        ty: Type,
    },
    /// A call to a builtin or user function.
    Call {
        /// What is being called.
        callee: Callee,
        /// The argument expressions.
        args: Vec<HirExprId>,
        /// The call's result type.
        ty: Type,
    },
    /// Construction of a struct value.
    ///
    /// Every field is present and in declaration order: the analyzer fills an
    /// omitted field with its declared default, so nothing downstream has to
    /// know defaults exist.
    StructNew {
        /// The struct being built.
        struct_id: StructId,
        /// One initializer per field, in declaration order.
        fields: Vec<HirExprId>,
    },
    /// A read of one field of a struct value.
    Field {
        /// The struct-typed expression being read.
        base: HirExprId,
        /// The field's index in declaration order.
        index: u32,
        /// The field's type.
        ty: Type,
    },
    /// A placeholder for an expression that failed to analyze.
    Error,
}

impl HirExpr {
    /// The resolved type of this expression.
    pub fn type_of(&self) -> Type {
        match self {
            HirExpr::Int(_) => Type::Int,
            HirExpr::Float(_) => Type::Float,
            HirExpr::Bool(_) => Type::Bool,
            HirExpr::Str(_) => Type::String,
            HirExpr::Local { ty, .. }
            | HirExpr::Unary { ty, .. }
            | HirExpr::Binary { ty, .. }
            | HirExpr::Call { ty, .. }
            | HirExpr::Field { ty, .. } => *ty,
            HirExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            HirExpr::Error => Type::Error,
        }
    }
}

/// The target of a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    /// A language builtin.
    Builtin(Builtin),
    /// A user-defined function.
    User(FuncId),
}

/// The builtins the v0 subset provides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `print(value)` — writes one formatted line of output.
    Print,
}

/// A type-resolved unary operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirUnaryOp {
    /// Integer negation.
    NegInt,
    /// Float negation.
    NegFloat,
    /// Boolean negation.
    Not,
}

/// A type-resolved binary operator: each variant fixes its operand types, so
/// backends never re-derive types from operands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinaryOp {
    /// Integer `+`, `-`, `*`, `/`, `%`.
    AddInt,
    /// Integer subtraction.
    SubInt,
    /// Integer multiplication.
    MulInt,
    /// Integer division (truncating).
    DivInt,
    /// Integer remainder.
    RemInt,
    /// Float addition.
    AddFloat,
    /// Float subtraction.
    SubFloat,
    /// Float multiplication.
    MulFloat,
    /// Float division.
    DivFloat,
    /// String concatenation (`+`).
    ConcatStr,
    /// Integer comparisons.
    EqInt,
    /// Integer inequality.
    NeInt,
    /// Integer less-than.
    LtInt,
    /// Integer less-or-equal.
    LeInt,
    /// Integer greater-than.
    GtInt,
    /// Integer greater-or-equal.
    GeInt,
    /// Float comparisons.
    EqFloat,
    /// Float inequality.
    NeFloat,
    /// Float less-than.
    LtFloat,
    /// Float less-or-equal.
    LeFloat,
    /// Float greater-than.
    GtFloat,
    /// Float greater-or-equal.
    GeFloat,
    /// Boolean equality.
    EqBool,
    /// Boolean inequality.
    NeBool,
    /// String equality.
    EqStr,
    /// String inequality.
    NeStr,
    /// Short-circuiting logical AND.
    And,
    /// Short-circuiting logical OR.
    Or,
}
