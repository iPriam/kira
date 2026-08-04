//! The KSL syntax tree: what a shader file says, before anything is resolved.
//!
//! Nodes carry written names as [`Symbol`]s and refer to each other by arena
//! handle, never by reference, so no node has a lifetime and the whole tree
//! moves as one owned value. Every node carries its [`Span`], because the same
//! tree serves diagnostics and, later, tooling.
//!
//! Nothing here is resolved. `Float4` and `Lighting.SceneLighting` are both
//! just written paths; deciding that one is a builtin vector and the other a
//! struct reached through an import alias is semantics' job.

use kira_core::Symbol;
use kira_source::Span;

use crate::tree::{ExprId, StmtId, TypeRefId};

/// One top-level declaration.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// `import A.B as C`
    Import(Import),
    /// `type Name { … }`
    Type(TypeDecl),
    /// `const name: Type = <literal>`
    Const(ConstDecl),
    /// `enum Name { A = 0, B = 1 }`
    Enum(EnumDecl),
    /// A free function, written outside any shader.
    Function(Function),
    /// `shader Name { … }`
    Shader(Shader),
}

impl Item {
    /// Where the item was written.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Item::Import(item) => item.span,
            Item::Type(item) => item.span,
            Item::Const(item) => item.span,
            Item::Enum(item) => item.span,
            Item::Function(item) => item.span,
            Item::Shader(item) => item.span,
        }
    }
}

/// `import Common.Lighting as Lighting`
#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    /// The dotted path, in written order.
    pub path: Vec<Symbol>,
    /// The name the module is reached by, when one was written.
    pub alias: Option<Symbol>,
    /// Where the import was written.
    pub span: Span,
}

/// `const name: Type = <literal>`
///
/// A name for a number, which KSL previously spelled as a zero-argument
/// function — `function washCeiling() -> Float { return 0.9 }` — because there
/// was nothing else to spell it with. Folded during checking, so nothing
/// downstream sees a constant at all.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    /// The declared name.
    pub name: Symbol,
    /// Its written type.
    pub ty: TypeRefId,
    /// Its value, which must be a literal.
    pub value: ExprId,
    /// Where the declaration was written.
    pub span: Span,
}

/// `enum Name { A = 0, B = 1 }`
///
/// Every variant carries its number, written out. A shader reads a value that
/// arrived from outside — a vertex attribute, a uniform — and the number is
/// what arrived, so declaration order would be a guess about someone else's
/// encoding. Writing it makes the shader's table and the host's the same table.
///
/// Folded to its variants' values during checking, exactly as an option is: no
/// backend learns the word `enum`.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumDecl {
    /// The declared name.
    pub name: Symbol,
    /// Its variants, in declaration order.
    pub variants: Vec<EnumVariant>,
    /// Where the declaration was written.
    pub span: Span,
}

/// One `A = 0` inside an `enum` body.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    /// The variant's name.
    pub name: Symbol,
    /// The value it stands for, which must be a literal.
    pub value: ExprId,
    /// Where the variant was written.
    pub span: Span,
}

/// `type Name { … }`
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    /// The declared name.
    pub name: Symbol,
    /// Its fields, in declaration order.
    pub fields: Vec<Field>,
    /// Where the declaration was written.
    pub span: Span,
}

/// One `let name: Type` inside a `type` block.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The field's name.
    pub name: Symbol,
    /// Its written type.
    pub ty: TypeRefId,
    /// The word inside `@builtin(…)`, when one was written.
    pub builtin: Option<Symbol>,
    /// The word inside `@interpolate(…)`, when one was written.
    pub interpolation: Option<Symbol>,
    /// Where the field was written, annotations included.
    pub span: Span,
}

/// A function, free or a stage entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    /// The declared name.
    pub name: Symbol,
    /// Its parameters, in order.
    pub params: Vec<Param>,
    /// Its written result type, absent when it returns nothing.
    pub result: Option<TypeRefId>,
    /// Its body.
    pub body: Block,
    /// Where the function was written.
    pub span: Span,
}

/// One `name: Type` in a parameter list.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// The parameter's name.
    pub name: Symbol,
    /// Its written type.
    pub ty: TypeRefId,
    /// Where the parameter was written.
    pub span: Span,
}

/// `shader Name { … }`
#[derive(Debug, Clone, PartialEq)]
pub struct Shader {
    /// The declared name.
    pub name: Symbol,
    /// Its compile-time options, in declaration order.
    pub options: Vec<OptionDecl>,
    /// Its resource groups, in declaration order.
    pub groups: Vec<Group>,
    /// Its stages, in declaration order.
    pub stages: Vec<StageDecl>,
    /// Where the shader was written.
    pub span: Span,
}

/// `option name: Type = value`
#[derive(Debug, Clone, PartialEq)]
pub struct OptionDecl {
    /// The option's name.
    pub name: Symbol,
    /// Its written type.
    pub ty: TypeRefId,
    /// Its default, which must be a constant.
    pub value: ExprId,
    /// Where the option was written.
    pub span: Span,
}

/// `group Name { … }`
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// The group's name, which decides its binding class.
    pub name: Symbol,
    /// Its resources, in declaration order.
    pub resources: Vec<Resource>,
    /// Where the group was written.
    pub span: Span,
}

/// One resource bound inside a group.
#[derive(Debug, Clone, PartialEq)]
pub struct Resource {
    /// Which declaration word introduced it.
    pub kind: ResourceKind,
    /// The access mode written after `storage`, absent for every other kind.
    pub access: Option<Access>,
    /// The resource's name.
    pub name: Symbol,
    /// The slot written as `@binding(n)`, absent when the slot is taken from
    /// the declaration's position in its group. A shader that must land on a
    /// layout the host already binds — one shared with another shader, say —
    /// says so here rather than padding its group to push a name into place.
    pub binding: Option<u32>,
    /// Its written type.
    pub ty: TypeRefId,
    /// Where the resource was written.
    pub span: Span,
}

/// The word a resource declaration opens with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// `uniform`
    Uniform,
    /// `storage`
    Storage,
    /// `texture`
    Texture,
    /// `sampler`
    Sampler,
}

/// The access mode written on a storage resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// `read`
    Read,
    /// `read_write`
    ReadWrite,
    /// `write`
    ///
    /// Only a texture takes this: a shader writes a storage texture without
    /// ever reading it, and saying so is what lets a backend declare the
    /// binding write-only rather than guessing.
    Write,
}

/// `vertex { … }`, `fragment { … }`, or `compute { … }`
#[derive(Debug, Clone, PartialEq)]
pub struct StageDecl {
    /// Which stage it declares.
    pub stage: StageWord,
    /// The type named by `input`, when one was written.
    pub input: Option<Symbol>,
    /// The type named by `output`, when one was written.
    pub output: Option<Symbol>,
    /// The three extents written in `threads(x, y, z)`.
    pub threads: Option<[ExprId; 3]>,
    /// The functions written inside the stage, entry point included.
    pub functions: Vec<Function>,
    /// Where the stage was written.
    pub span: Span,
}

/// Which stage a `StageDecl` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageWord {
    /// `vertex`
    Vertex,
    /// `fragment`
    Fragment,
    /// `compute`
    Compute,
}

impl StageWord {
    /// The word it is written as.
    #[must_use]
    pub fn spelling(self) -> &'static str {
        match self {
            StageWord::Vertex => "vertex",
            StageWord::Fragment => "fragment",
            StageWord::Compute => "compute",
        }
    }
}

/// A type as written, before resolution.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeRef {
    /// A dotted path: `Float4`, or `Lighting.SceneLighting`.
    Named {
        /// The path segments, in written order.
        path: Vec<Symbol>,
        /// Where it was written.
        span: Span,
    },
    /// `[T]`, an array whose length the binding decides.
    Array {
        /// The element type.
        element: TypeRefId,
        /// Where it was written.
        span: Span,
    },
}

impl TypeRef {
    /// Where the type was written.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            TypeRef::Named { span, .. } | TypeRef::Array { span, .. } => *span,
        }
    }
}

/// A braced sequence of statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// The statements, in order.
    pub stmts: Vec<StmtId>,
    /// The braces and everything between them.
    pub span: Span,
}

/// One statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let name: Type = value`, with the type or the initializer omitted.
    ///
    /// A `let` with no initializer declares storage the body fills in, which
    /// is how every stage entry point in the corpus builds its output value.
    Let {
        /// The bound name.
        name: Symbol,
        /// Its written type, when one was written.
        ty: Option<TypeRefId>,
        /// Its initial value, when one was written.
        init: Option<ExprId>,
        /// Where the statement was written.
        span: Span,
    },
    /// `place = value`, where `place` is a name, field, or index chain.
    Assign {
        /// What is written to.
        target: ExprId,
        /// What is written into it.
        value: ExprId,
        /// Where the statement was written.
        span: Span,
    },
    /// `if cond { … }`, with an optional `else`.
    If {
        /// The condition.
        cond: ExprId,
        /// The taken branch.
        then: Block,
        /// The `else` branch: another `If` for `else if`, or a `Block`.
        otherwise: Option<StmtId>,
        /// Where the statement was written.
        span: Span,
    },
    /// `while cond { … }`
    While {
        /// The condition, tested before each iteration.
        cond: ExprId,
        /// The body.
        body: Block,
        /// Where the statement was written.
        span: Span,
    },
    /// `return`, with or without a value.
    Return {
        /// The returned value, when one was written.
        value: Option<ExprId>,
        /// Where the statement was written.
        span: Span,
    },
    /// A bare block, which an `else` branch is.
    Block(Block),
    /// An expression evaluated for its effect.
    Expr {
        /// The expression.
        expr: ExprId,
        /// Where the statement was written.
        span: Span,
    },
}

impl Stmt {
    /// Where the statement was written.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::If { span, .. }
            | Stmt::While { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Expr { span, .. } => *span,
            Stmt::Block(block) => block.span,
        }
    }
}

/// One expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// An integer literal.
    Int {
        /// Its value.
        value: u64,
        /// Where it was written.
        span: Span,
    },
    /// A floating-point literal.
    Float {
        /// Its value.
        value: f64,
        /// Where it was written.
        span: Span,
    },
    /// `true` or `false`.
    Bool {
        /// Its value.
        value: bool,
        /// Where it was written.
        span: Span,
    },
    /// A bare name.
    Name {
        /// The written name.
        symbol: Symbol,
        /// Where it was written.
        span: Span,
    },
    /// `base.field`, which is also how a swizzle and a module member are
    /// written — telling those apart is semantics' job.
    Field {
        /// What is read from.
        base: ExprId,
        /// The member's name.
        field: Symbol,
        /// Where the whole access was written.
        span: Span,
    },
    /// `base[index]`
    Index {
        /// The indexed value.
        base: ExprId,
        /// The index.
        index: ExprId,
        /// Where the whole access was written.
        span: Span,
    },
    /// `callee(args…)`, which is also how a value is constructed.
    Call {
        /// What is called.
        callee: ExprId,
        /// Its arguments, in order.
        args: Vec<ExprId>,
        /// Where the whole call was written.
        span: Span,
    },
    /// A prefix operator applied to one operand.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// What it applies to.
        operand: ExprId,
        /// Where the whole expression was written.
        span: Span,
    },
    /// An infix operator applied to two operands.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: ExprId,
        /// The right operand.
        rhs: ExprId,
        /// Where the whole expression was written.
        span: Span,
    },
}

impl Expr {
    /// Where the expression was written.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Expr::Int { span, .. }
            | Expr::Float { span, .. }
            | Expr::Bool { span, .. }
            | Expr::Name { span, .. }
            | Expr::Field { span, .. }
            | Expr::Index { span, .. }
            | Expr::Call { span, .. }
            | Expr::Unary { span, .. }
            | Expr::Binary { span, .. } => *span,
        }
    }
}

/// A prefix operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// `-`
    Neg,
    /// `!`
    Not,
}

impl UnaryOp {
    /// How it is written.
    #[must_use]
    pub fn spelling(self) -> &'static str {
        match self {
            UnaryOp::Neg => "-",
            UnaryOp::Not => "!",
        }
    }
}

/// An infix operator.
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
    /// How it is written.
    #[must_use]
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

    /// Its binding power, higher binding tighter.
    ///
    /// The ladder is C's, which is what every shader dialect KSL emits to
    /// uses — so an expression written here keeps its meaning after lowering
    /// without the emitter having to re-parenthesize by precedence.
    #[must_use]
    pub fn precedence(self) -> u8 {
        match self {
            BinaryOp::Or => 1,
            BinaryOp::And => 2,
            BinaryOp::BitOr => 3,
            BinaryOp::BitXor => 4,
            BinaryOp::BitAnd => 5,
            BinaryOp::Eq | BinaryOp::Ne => 6,
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => 7,
            BinaryOp::Shl | BinaryOp::Shr => 8,
            BinaryOp::Add | BinaryOp::Sub => 9,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => 10,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_orders_the_ladder_the_way_c_does() {
        assert!(BinaryOp::Mul.precedence() > BinaryOp::Add.precedence());
        assert!(BinaryOp::Add.precedence() > BinaryOp::Shl.precedence());
        assert!(BinaryOp::Shl.precedence() > BinaryOp::Lt.precedence());
        assert!(BinaryOp::Lt.precedence() > BinaryOp::Eq.precedence());
        assert!(BinaryOp::Eq.precedence() > BinaryOp::BitAnd.precedence());
        assert!(BinaryOp::And.precedence() > BinaryOp::Or.precedence());
    }
}
