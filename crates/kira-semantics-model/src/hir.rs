//! The high-level IR: a name-resolved, fully-typed form of a program.
//!
//! The HIR is the analyzer's output and the IR lowerer's input. Names are
//! resolved (variables to [`LocalId`], calls to a [`Callee`]) and every
//! expression carries its [`Type`]. Nodes live in per-program arenas and refer
//! to each other by index, so no HIR type carries a lifetime. Local indices
//! are scoped to their owning function.

use crate::ty::{EnumId, StructId, Type, TypeTable};
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
    /// Every shape the program's types name: its structs and its array types.
    pub types: TypeTable,
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

/// One step of a [`HirPlace`]'s walk into a value.
///
/// The two steps differ in *when* they are known, which is the whole reason
/// this is an enum rather than a list of numbers: a field index is resolved
/// during analysis and is a constant from here down, while an array index is
/// an expression that only has a value while the program runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HirPlaceStep {
    /// Walk into the field at this index, in declaration order.
    Field(u32),
    /// Walk into the array element this expression selects.
    Index(HirExprId),
}

/// A writable location: a local, optionally walked into by fields and indices.
///
/// `p` is the local with an empty path; `b.size.x` is the local `b` with the
/// path `[Field(size), Field(x)]`; `grid[0].cells[2].x` is `grid` with
/// `[Index(0), Field(cells), Index(2), Field(x)]`. Resolving the whole path up
/// front is what lets a backend write through it in place instead of rebuilding
/// the enclosing value — which is not an optimization but the semantics: an
/// array is a shared object, so a write that rebuilt its owner would land
/// somewhere nobody can see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirPlace {
    /// The local the place is rooted at.
    pub local: LocalId,
    /// Steps to walk, outermost first; empty writes the local itself.
    pub path: Vec<HirPlaceStep>,
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
    /// A conditional expression, `cond ? then : otherwise`.
    ///
    /// Kept as a node rather than desugared: a `? :` can sit anywhere an
    /// expression can, and rewriting it into an `if` statement over a temporary
    /// would need statement hoisting out of arbitrary expression position,
    /// which this lowering deliberately does not do. Every backend already
    /// branches at expression level for `&&`/`||`, so the node costs a reuse of
    /// that machinery rather than new machinery.
    Select {
        /// The `Bool` condition.
        cond: HirExprId,
        /// The value when the condition holds.
        then: HirExprId,
        /// The value when it does not.
        otherwise: HirExprId,
        /// The type both branches agreed on.
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
    /// Construction of an array value from its written elements.
    ///
    /// Carries its own type rather than deriving it from the elements: an
    /// empty literal (`[]`) has no element to ask, and the expected type is
    /// what decides it.
    ArrayNew {
        /// The array's type (an interned [`Type::Array`]).
        ty: Type,
        /// The elements, in written order.
        elements: Vec<HirExprId>,
    },
    /// A read of one element of an array (`xs[i]`).
    Index {
        /// The array-typed expression being read.
        base: HirExprId,
        /// The `Int`-typed index.
        index: HirExprId,
        /// The element's type.
        ty: Type,
    },
    /// An array's element count (`xs.count`) — a property, not a call.
    ArrayLen {
        /// The array-typed expression being measured.
        array: HirExprId,
    },
    /// `xs.append(v)`: push one element onto an array, in place.
    ///
    /// The receiver is a **place**, not an expression, and that is the whole
    /// correctness argument for this node: reading an array yields an
    /// independent value, so appending to a *read* would push onto something
    /// nobody else can see and silently lose the write. Resolving the receiver
    /// to a place is what makes `rows[0].xs.append(42)` land in `rows`.
    ArrayAppend {
        /// The array being appended to.
        place: HirPlace,
        /// The element to push.
        value: HirExprId,
    },
    /// Construction of an enum value: a variant of an enum, with its optional
    /// single payload.
    ///
    /// The `tag` is the variant's declaration index — the discriminant `==`
    /// compares and the runtime value stores. A payload-less variant carries
    /// `None`; a payload variant carries the value to box, which analysis has
    /// already filled from the variant's default when the site wrote none.
    EnumNew {
        /// The enum being built.
        enum_id: EnumId,
        /// The variant's declaration index.
        tag: u32,
        /// The payload value, or `None` for a payload-less variant.
        payload: Option<HirExprId>,
    },
    /// An enum value's discriminant tag, as an `Int`.
    ///
    /// Enum equality is tag equality, so the analyzer lowers `e == .V` to an
    /// `Int` comparison of two tags — this is how it reads one off an enum
    /// whose variant is only known at run time. A backend extracts the tag and
    /// releases the enum, exactly as `.count` does for an array.
    EnumTag {
        /// The enum-typed expression whose tag is read.
        value: HirExprId,
    },
    /// An enum value's payload, as an owned value of the variant's payload type.
    ///
    /// This is what a `match` arm's binding reads. The variant is *not* checked
    /// at run time: a `match` only projects a payload inside the arm its tag
    /// test already selected, so the tag is known to be the one whose payload
    /// `ty` describes. Reading it yields an owned copy — a `String` payload is
    /// cloned out of the box — so the binding outlives the enum it came from
    /// and the box still owns its own payload.
    EnumPayload {
        /// The enum-typed expression whose payload is read.
        value: HirExprId,
        /// The selected variant's declared payload type.
        ty: Type,
    },
    /// A placeholder for an expression that failed to analyze.
    Error,
}

impl HirExpr {
    /// The resolved type of this expression.
    pub fn type_of(&self) -> Type {
        match self {
            // A literal carries the *plain* spelling, which is the wildcard in
            // `Type::assignable_to`. That one fact is what lets `let x: U8 = 5`
            // check without any implicit-conversion rule: the literal is
            // assignable to every width rather than being converted to one.
            HirExpr::Int(_) => Type::INT,
            HirExpr::Float(_) => Type::FLOAT,
            HirExpr::Bool(_) => Type::Bool,
            HirExpr::Str(_) => Type::String,
            HirExpr::Local { ty, .. }
            | HirExpr::Unary { ty, .. }
            | HirExpr::Binary { ty, .. }
            | HirExpr::Select { ty, .. }
            | HirExpr::Call { ty, .. }
            | HirExpr::Field { ty, .. }
            | HirExpr::ArrayNew { ty, .. }
            | HirExpr::EnumPayload { ty, .. }
            | HirExpr::Index { ty, .. } => *ty,
            HirExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            HirExpr::EnumNew { enum_id, .. } => Type::Enum(*enum_id),
            // `.count` and a tag read are both `Int`; `.append` yields nothing.
            // None has a type that can vary, so none carries one.
            HirExpr::ArrayLen { .. } | HirExpr::EnumTag { .. } => Type::INT,
            HirExpr::ArrayAppend { .. } => Type::Void,
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
    /// Bitwise complement (`~`) on the raw 64-bit pattern.
    BitNot,
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
    /// Integer division (truncating), signed.
    DivInt,
    /// Integer remainder, signed.
    RemInt,
    /// Integer division (truncating), unsigned — the `U8`..`U64` spellings.
    ///
    /// Separate from [`HirBinaryOp::DivInt`] because signedness is the one
    /// thing an integer's written width decides. `+`, `-`, and `*` need no
    /// unsigned twin: two's-complement wrapping is bit-identical for both
    /// signednesses, so they would be the same instruction.
    DivUInt,
    /// Integer remainder, unsigned — the `U8`..`U64` spellings.
    RemUInt,
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
    /// Integer less-than, unsigned — the `U8`..`U64` spellings.
    ///
    /// Ordering needs an unsigned twin for the same reason division does, and
    /// equality does not: `==` compares bit patterns, which is signedness-free.
    LtUInt,
    /// Integer less-or-equal, unsigned.
    LeUInt,
    /// Integer greater-than, unsigned.
    GtUInt,
    /// Integer greater-or-equal, unsigned.
    GeUInt,
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
    /// Bitwise AND (`&`) on the raw 64-bit pattern.
    ///
    /// The three bitwise operators need no unsigned twin for the same reason
    /// `+` does not: they act on bits, and a bit has no sign.
    BitAnd,
    /// Bitwise OR (`|`) on the raw 64-bit pattern.
    BitOr,
    /// Bitwise XOR (`^`) on the raw 64-bit pattern.
    BitXor,
    /// Left shift (`<<`). The shift amount is taken modulo 64.
    ///
    /// Signedness-free: shifting bits left discards the high end either way.
    Shl,
    /// Arithmetic right shift (`>>`), sign-propagating — the signed spellings.
    ///
    /// Unlike `<<`, `>>` *does* need an unsigned twin: what fills the vacated
    /// high bits is exactly the question signedness answers.
    ShrInt,
    /// Logical right shift (`>>`), zero-filling — the `U8`..`U64` spellings.
    ShrUInt,
}
