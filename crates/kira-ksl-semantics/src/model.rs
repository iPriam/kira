//! The checked module: a KSL program with every name resolved and every
//! expression typed.
//!
//! Names are `String`s here rather than interned symbols, because a checked
//! module is joined from several parsed files and each of those carries its own
//! interner. Resolving to text once, at the point where the files meet, is
//! cheaper than threading a merged interner through every consumer — and the
//! backends all emit text anyway.
//!
//! Expressions and statements live in arenas and refer to each other by handle,
//! so no node has a lifetime and the module moves as one owned value.

use kira_shader_model::{
    AccessMode, Builtin, GroupClass, Interpolation, ResourceKind, Stage, Type,
};
use la_arena::{Arena, Idx};

/// Handle to a checked expression.
pub type CheckedExprId = Idx<CheckedExpr>;
/// Handle to a checked statement.
pub type CheckedStmtId = Idx<CheckedStmt>;

/// A whole KSL program after checking.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckedModule {
    /// Every struct type, imports included, in dependency-safe order.
    pub structs: Vec<CheckedStruct>,
    /// Every free function, imports included.
    pub functions: Vec<CheckedFunction>,
    /// The shader the file declares, when it declares one.
    pub shader: Option<CheckedShader>,
    /// Arena backing every [`CheckedExprId`].
    pub exprs: Arena<CheckedExpr>,
    /// Arena backing every [`CheckedStmtId`].
    pub stmts: Arena<CheckedStmt>,
}

impl CheckedModule {
    /// The expression `id` handles.
    #[must_use]
    pub fn expr(&self, id: CheckedExprId) -> &CheckedExpr {
        &self.exprs[id]
    }

    /// The statement `id` handles.
    #[must_use]
    pub fn stmt(&self, id: CheckedStmtId) -> &CheckedStmt {
        &self.stmts[id]
    }

    /// The struct named `name`, when the module declares one.
    #[must_use]
    pub fn struct_named(&self, name: &str) -> Option<&CheckedStruct> {
        self.structs.iter().find(|declared| declared.name == name)
    }
}

/// A checked struct type.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedStruct {
    /// Its emitted name, which is unique across the joined module.
    pub name: String,
    /// Its fields, in declaration order.
    pub fields: Vec<CheckedField>,
}

/// One field of a checked struct.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedField {
    /// The field's name.
    pub name: String,
    /// Its type.
    pub ty: Type,
    /// The stage builtin it carries, when annotated.
    pub builtin: Option<Builtin>,
    /// Its interpolation qualifier, when annotated.
    pub interpolation: Option<Interpolation>,
}

/// A checked function.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedFunction {
    /// Its emitted name.
    pub name: String,
    /// Its parameters, in order.
    pub params: Vec<CheckedParam>,
    /// What it returns, [`Type::Void`] when it returns nothing.
    pub result: Type,
    /// Its body.
    pub body: Vec<CheckedStmtId>,
}

/// One parameter of a checked function.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedParam {
    /// The parameter's name.
    pub name: String,
    /// Its type.
    pub ty: Type,
}

/// A checked shader.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedShader {
    /// Its declared name.
    pub name: String,
    /// Its compile-time options.
    pub options: Vec<CheckedOption>,
    /// Its resource groups, in declaration order.
    pub groups: Vec<CheckedGroup>,
    /// Its stages, in declaration order.
    pub stages: Vec<CheckedStage>,
}

/// A checked compile-time option.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedOption {
    /// The option's name.
    pub name: String,
    /// Its type.
    pub ty: Type,
    /// Its default value.
    pub value: ConstValue,
}

/// A value an option can hold.
///
/// Options are the only compile-time constants KSL has, and a `threads` extent
/// may name one — so these are folded during checking rather than emitted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstValue {
    /// A `Bool`.
    Bool(bool),
    /// A signed integer.
    Int(i64),
    /// An unsigned integer.
    Uint(u64),
    /// A float.
    Float(f64),
}

impl ConstValue {
    /// The value as a thread-group extent, when it is a whole positive number.
    #[must_use]
    pub fn as_extent(self) -> Option<u32> {
        match self {
            // Zero is refused along with the negatives: no dialect accepts a
            // workgroup dimension of zero, so folding one here would only move
            // the failure into backend output.
            ConstValue::Uint(value) => u32::try_from(value).ok().filter(|extent| *extent >= 1),
            ConstValue::Int(value) => u32::try_from(value).ok().filter(|extent| *extent >= 1),
            ConstValue::Bool(_) | ConstValue::Float(_) => None,
        }
    }
}

/// A checked resource group.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedGroup {
    /// The group's name as written.
    pub name: String,
    /// The binding class its name puts it in.
    pub class: GroupClass,
    /// Its resources, in declaration order.
    pub resources: Vec<CheckedResource>,
}

/// A checked bound resource.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedResource {
    /// The resource's name, which is how a body refers to it.
    pub name: String,
    /// Which kind of binding it is.
    pub kind: ResourceKind,
    /// Its access mode, present only on storage.
    pub access: Option<AccessMode>,
    /// The slot written as `@binding(n)`, absent when position decides it.
    pub binding: Option<u32>,
    /// Its type.
    pub ty: Type,
}

/// A checked stage.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedStage {
    /// Which stage it is.
    pub stage: Stage,
    /// The struct named by `input`, when one was written.
    pub input: Option<String>,
    /// The struct named by `output`, when one was written.
    pub output: Option<String>,
    /// The thread-group extents, folded to numbers.
    pub threads: Option<[u32; 3]>,
    /// The entry point.
    pub entry: CheckedFunction,
    /// Every other function written inside the stage.
    pub helpers: Vec<CheckedFunction>,
}

/// One checked statement.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckedStmt {
    /// A local binding, with or without an initializer.
    Let {
        /// The bound name.
        name: String,
        /// Its type, always known after checking.
        ty: Type,
        /// Its initial value, absent when the body fills it in later.
        init: Option<CheckedExprId>,
    },
    /// A write through a place expression.
    Assign {
        /// The place written to.
        target: CheckedExprId,
        /// The value written.
        value: CheckedExprId,
    },
    /// A conditional.
    If {
        /// The condition, always `Bool`.
        cond: CheckedExprId,
        /// The taken branch.
        then: Vec<CheckedStmtId>,
        /// The `else` branch, when one was written.
        otherwise: Option<Vec<CheckedStmtId>>,
    },
    /// A pre-tested loop.
    While {
        /// The condition, always `Bool`.
        cond: CheckedExprId,
        /// The body.
        body: Vec<CheckedStmtId>,
    },
    /// A return, with or without a value.
    Return(Option<CheckedExprId>),
    /// An expression evaluated for its effect.
    Expr(CheckedExprId),
}

/// A checked expression: what it computes, and what type it has.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedExpr {
    /// Its type.
    pub ty: Type,
    /// What it computes.
    pub kind: CheckedExprKind,
}

/// What a checked expression computes.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckedExprKind {
    /// A literal.
    Const(ConstValue),
    /// A local binding or parameter.
    Local(String),
    /// A resource bound by the shader's groups.
    Resource(String),
    /// A compile-time option, read as its folded value's name.
    Option(String),
    /// A struct member.
    Field {
        /// The struct value.
        base: CheckedExprId,
        /// The member's name.
        field: String,
    },
    /// A vector component selection.
    ///
    /// Kept apart from [`CheckedExprKind::Field`] because a backend spells a
    /// swizzle by index and a member by name, and telling them apart later
    /// would mean re-deciding what the type already settled.
    Swizzle {
        /// The vector value.
        base: CheckedExprId,
        /// The selected component indices, in written order.
        components: Vec<u8>,
    },
    /// How many elements a runtime-sized array holds, written `array.count`.
    ///
    /// Every dialect spells this differently and some need the binding rather
    /// than the value, so it is its own node instead of a member read.
    ArrayLength {
        /// The array.
        base: CheckedExprId,
    },
    /// An element of an array.
    Index {
        /// The array value.
        base: CheckedExprId,
        /// The element's index.
        index: CheckedExprId,
    },
    /// A value built from its parts: `Float4(x, y, z, w)`, or a struct.
    Construct {
        /// The arguments, in order.
        args: Vec<CheckedExprId>,
    },
    /// A call of a function the module declares.
    Call {
        /// The callee's emitted name.
        name: String,
        /// The arguments, in order.
        args: Vec<CheckedExprId>,
    },
    /// A call of a function the language provides.
    Builtin {
        /// Which builtin.
        which: BuiltinFn,
        /// The arguments, in order.
        args: Vec<CheckedExprId>,
    },
    /// A prefix operator.
    Unary {
        /// The operator.
        op: UnaryOp,
        /// Its operand.
        operand: CheckedExprId,
    },
    /// An infix operator.
    Binary {
        /// The operator.
        op: BinaryOp,
        /// The left operand.
        lhs: CheckedExprId,
        /// The right operand.
        rhs: CheckedExprId,
    },
    /// A widening or narrowing between numeric types, made explicit.
    ///
    /// KSL's own conversions are written as constructor calls (`UInt(x)`), and
    /// this is what one becomes — so no backend has to infer a cast that the
    /// checker already decided.
    Cast {
        /// The value converted.
        value: CheckedExprId,
    },
    /// An expression that failed to check.
    ///
    /// Checking is total: a rejected expression becomes this and its enclosing
    /// function is still checked, so one bad line does not hide the rest.
    Invalid,
}

/// A prefix operator that survived checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation.
    Neg,
    /// Logical negation.
    Not,
}

/// An infix operator that survived checking.
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
    /// Whether the operator answers a `Bool` whatever its operands are.
    #[must_use]
    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
        )
    }

    /// Whether the operator orders one value against another.
    ///
    /// Split from [`Self::is_comparison`] because equality over booleans is
    /// legal in every dialect while an *ordering* over them is not: `true <
    /// false` is a type error everywhere, so it is one here too.
    pub fn is_ordering(self) -> bool {
        matches!(
            self,
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge
        )
    }

    /// Whether the operator is a logical connective over two booleans.
    ///
    /// Split from [`Self::is_comparison`] because the two demand different
    /// operands: a comparison needs two values of one (orderable or equatable)
    /// type and answers `Bool`, while `&&`/`||` need *Bool* operands — every
    /// target dialect treats `1 && 2` as a type error, not as C's truthiness.
    pub fn is_logical(self) -> bool {
        matches!(self, BinaryOp::And | BinaryOp::Or)
    }
}

/// A function KSL provides rather than a shader declaring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinFn {
    /// `mul(matrix, vector)` — the one call whose operand order every backend
    /// spells differently, which is why it is a builtin rather than `*`.
    Mul,
    /// `dot(a, b)`
    Dot,
    /// `cross(a, b)`
    Cross,
    /// `normalize(v)`
    Normalize,
    /// `length(v)`
    Length,
    /// `abs(x)`
    Abs,
    /// `floor(x)`
    Floor,
    /// `ceil(x)`
    Ceil,
    /// `min(a, b)`
    Min,
    /// `max(a, b)`
    Max,
    /// `clamp(x, low, high)`
    Clamp,
    /// `mix(a, b, t)`
    Mix,
    /// `step(edge, x)`
    Step,
    /// `smoothstep(low, high, x)`
    Smoothstep,
    /// `pow(x, y)`
    Pow,
    /// `sqrt(x)`
    Sqrt,
    /// `sin(x)`
    Sin,
    /// `cos(x)`
    Cos,
    /// `tan(x)`
    Tan,
    /// `atan2(y, x)`
    Atan2,
    /// `exp(x)`
    Exp,
    /// `log(x)`
    Log,
    /// `fract(x)`
    Fract,
    /// `sample(texture, sampler, uv)`
    Sample,
    /// `load(texture, coordinate)`
    Load,
    /// `store(texture, coord, value)` — writing one texel of a storage
    /// texture. Returns nothing: it is the one builtin called for its effect.
    Store,
    /// `atomicAdd(buffer, index, value)`
    AtomicAdd,
}

impl BuiltinFn {
    /// Whether every argument must be a float scalar or float vector of one
    /// shape — the component-wise math family.
    pub fn wants_floats(self) -> bool {
        matches!(
            self,
            BuiltinFn::Abs
                | BuiltinFn::Floor
                | BuiltinFn::Ceil
                | BuiltinFn::Min
                | BuiltinFn::Max
                | BuiltinFn::Clamp
                | BuiltinFn::Mix
                | BuiltinFn::Step
                | BuiltinFn::Smoothstep
                | BuiltinFn::Pow
                | BuiltinFn::Sqrt
                | BuiltinFn::Sin
                | BuiltinFn::Cos
                | BuiltinFn::Tan
                | BuiltinFn::Exp
                | BuiltinFn::Log
                | BuiltinFn::Fract
        )
    }

    /// Whether every argument must be a float vector of one shape.
    pub fn wants_vectors(self) -> bool {
        matches!(
            self,
            BuiltinFn::Dot | BuiltinFn::Cross | BuiltinFn::Normalize | BuiltinFn::Length
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_shader_model::ScalarType;

    #[test]
    fn a_comparison_answers_bool_whatever_it_compares() {
        assert!(BinaryOp::Lt.is_comparison());
        assert!(BinaryOp::Eq.is_comparison());
        assert!(!BinaryOp::Add.is_comparison());
        // A logical connective is NOT one. `&&` answers a `Bool`, but only
        // because it already took two — `check::expr` settles it in its own
        // branch before this question is asked, and letting it through here
        // would type `1 && 2` as a comparison of two equal types.
        assert!(!BinaryOp::And.is_comparison());
        assert!(!BinaryOp::Or.is_comparison());
    }

    #[test]
    fn an_option_only_becomes_an_extent_when_it_is_a_whole_count() {
        assert_eq!(ConstValue::Uint(64).as_extent(), Some(64));
        assert_eq!(ConstValue::Int(-1).as_extent(), None);
        assert_eq!(ConstValue::Float(64.0).as_extent(), None);
        assert_eq!(ConstValue::Bool(true).as_extent(), None);
    }

    #[test]
    fn a_module_finds_the_struct_it_declares() {
        let module = CheckedModule {
            structs: vec![CheckedStruct {
                name: "VertexIn".to_owned(),
                fields: vec![CheckedField {
                    name: "position".to_owned(),
                    ty: Type::Scalar(ScalarType::Float),
                    builtin: None,
                    interpolation: None,
                }],
            }],
            ..CheckedModule::default()
        };
        assert!(module.struct_named("VertexIn").is_some());
        assert!(module.struct_named("Missing").is_none());
    }
}
