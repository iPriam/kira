//! The mid-level IR: the verified, backend-facing form of a program.
//!
//! The IR is what every backend consumes. It is fully resolved and typed:
//! locals are slot indices, calls name a [`IrCallee`], and expression nodes
//! carry their [`Type`]. Nodes live in a per-program arena and reference each
//! other by [`IrExprId`], so no IR type carries a lifetime. Producing an
//! [`IrProgram`] is the contract that the program type-checked and has a valid
//! entrypoint.

use kira_runtime_abi::Execution;
use kira_semantics_model::{StructId, StructTable, Type};
use la_arena::{Arena, Idx};

/// The typed binary operators, reused from the analyzer's instruction
/// selection so the IR does not re-derive types.
pub use kira_semantics_model::hir::HirBinaryOp as IrBinOp;
/// The typed unary operators, reused from the analyzer's instruction selection.
pub use kira_semantics_model::hir::HirUnaryOp as IrUnOp;

/// Handle to an IR expression.
pub type IrExprId = Idx<IrExpr>;

/// A fully lowered program ready for a backend.
#[derive(Debug, Clone, PartialEq)]
pub struct IrProgram {
    /// Every function, in a stable order; [`IrProgram::main`] indexes into it.
    pub functions: Vec<IrFunction>,
    /// Every struct the program declares: the one source of field layout.
    pub structs: StructTable,
    /// Index of the `@Main` entrypoint within [`IrProgram::functions`].
    pub main: u32,
    /// Arena backing every [`IrExprId`] across all functions.
    pub exprs: Arena<IrExpr>,
}

impl IrProgram {
    /// Borrows an IR expression by handle.
    pub fn expr(&self, id: IrExprId) -> &IrExpr {
        &self.exprs[id]
    }

    /// The entrypoint function.
    pub fn main_function(&self) -> &IrFunction {
        &self.functions[self.main as usize]
    }

    /// The static type of expression `id`, evaluated in `function`'s scope.
    ///
    /// Every IR node's type is recoverable without re-analysis: literals and
    /// operators fix their own type, a local read resolves against the
    /// function's slot types, and a call carries its result type. A
    /// statically-typed backend uses this to choose storage and instruction
    /// shapes; the VM never needs it.
    pub fn expr_type(&self, function: &IrFunction, id: IrExprId) -> Type {
        match &self.exprs[id] {
            IrExpr::Int(_) => Type::Int,
            IrExpr::Float(_) => Type::Float,
            IrExpr::Bool(_) => Type::Bool,
            IrExpr::Str(_) => Type::String,
            IrExpr::Local(slot) => function
                .locals
                .get(*slot as usize)
                .copied()
                .unwrap_or(Type::Error),
            IrExpr::Unary { op, .. } => match op {
                IrUnOp::NegInt => Type::Int,
                IrUnOp::NegFloat => Type::Float,
                IrUnOp::Not => Type::Bool,
            },
            IrExpr::Binary { op, .. } => binop_result(*op),
            IrExpr::Call { result, .. } => *result,
            IrExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            IrExpr::Field { ty, .. } => *ty,
        }
    }

    /// The type stored at `place`, evaluated in `function`'s scope.
    ///
    /// Walks the place's field path through the struct table, so a backend
    /// choosing storage for an assignment does not re-resolve it.
    pub fn place_type(&self, function: &IrFunction, place: &IrPlace) -> Type {
        let mut ty = function
            .locals
            .get(place.local as usize)
            .copied()
            .unwrap_or(Type::Error);
        for &index in &place.path {
            ty = match ty {
                Type::Struct(id) => self
                    .structs
                    .get(id)
                    .and_then(|def| def.field(index))
                    .map_or(Type::Error, |field| field.ty),
                _ => Type::Error,
            };
        }
        ty
    }
}

/// The result type of a typed binary operator.
fn binop_result(op: IrBinOp) -> Type {
    match op {
        IrBinOp::AddInt | IrBinOp::SubInt | IrBinOp::MulInt | IrBinOp::DivInt | IrBinOp::RemInt => {
            Type::Int
        }
        IrBinOp::AddFloat | IrBinOp::SubFloat | IrBinOp::MulFloat | IrBinOp::DivFloat => {
            Type::Float
        }
        IrBinOp::ConcatStr => Type::String,
        IrBinOp::EqInt
        | IrBinOp::NeInt
        | IrBinOp::LtInt
        | IrBinOp::LeInt
        | IrBinOp::GtInt
        | IrBinOp::GeInt
        | IrBinOp::EqFloat
        | IrBinOp::NeFloat
        | IrBinOp::LtFloat
        | IrBinOp::LeFloat
        | IrBinOp::GtFloat
        | IrBinOp::GeFloat
        | IrBinOp::EqBool
        | IrBinOp::NeBool
        | IrBinOp::EqStr
        | IrBinOp::NeStr
        | IrBinOp::And
        | IrBinOp::Or => Type::Bool,
    }
}

/// One lowered function.
#[derive(Debug, Clone, PartialEq)]
pub struct IrFunction {
    /// The function's name.
    pub name: String,
    /// How many of the local slots are parameters (the first `param_count`).
    pub param_count: u32,
    /// The type of every local slot, parameters first, in slot order.
    ///
    /// The VM ignores these (its values are dynamically tagged), but a
    /// statically-typed backend (LLVM/native) needs each slot's type to pick
    /// its storage and its load/store shape. `locals.len()` is the slot count.
    pub locals: Vec<Type>,
    /// The function's return type.
    pub return_type: Type,
    /// The engine this function's body runs on, as written in the source.
    ///
    /// A hybrid build splits the program on this; a single-backend build
    /// resolves it against that backend's default.
    pub execution: Execution,
    /// The function body.
    pub body: Vec<IrStmt>,
}

impl IrFunction {
    /// The total number of local slots this function needs.
    pub fn local_count(&self) -> u32 {
        self.locals.len() as u32
    }

    /// The type of parameter slot `index`, or `None` when out of range.
    pub fn param_type(&self, index: u32) -> Option<Type> {
        (index < self.param_count)
            .then(|| self.locals.get(index as usize).copied())
            .flatten()
    }
}

/// A statement in a lowered function body.
#[derive(Debug, Clone, PartialEq)]
pub enum IrStmt {
    /// Store an expression into a local slot for the first time.
    Let {
        /// Destination slot.
        local: u32,
        /// Value to store.
        init: IrExprId,
    },
    /// Write to an existing place: a local slot, or a field path within one.
    Assign {
        /// Destination place.
        place: IrPlace,
        /// Value to store.
        value: IrExprId,
    },
    /// Return from the function, optionally with a value.
    Return {
        /// The returned expression, if any.
        value: Option<IrExprId>,
    },
    /// Evaluate an expression for effect and discard its result.
    Eval {
        /// The evaluated expression.
        expr: IrExprId,
    },
    /// Conditional execution.
    If {
        /// The boolean condition.
        cond: IrExprId,
        /// Statements run when the condition holds.
        then_body: Vec<IrStmt>,
        /// Statements run otherwise.
        else_body: Vec<IrStmt>,
    },
    /// A pre-tested loop.
    While {
        /// The boolean loop condition.
        cond: IrExprId,
        /// The loop body.
        body: Vec<IrStmt>,
    },
}

/// A writable location: a local slot, optionally walked into by field indices.
///
/// The path is resolved at analysis time, so a backend writes through it
/// directly — it never rebuilds the enclosing struct to change one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrPlace {
    /// The local slot the place is rooted at.
    pub local: u32,
    /// Field indices to walk, outermost first; empty writes the slot itself.
    pub path: Vec<u32>,
}

/// An expression in the IR.
#[derive(Debug, Clone, PartialEq)]
pub enum IrExpr {
    /// An integer constant.
    Int(i64),
    /// A floating-point constant.
    Float(f64),
    /// A boolean constant.
    Bool(bool),
    /// A string constant.
    Str(String),
    /// A read of a local slot.
    Local(u32),
    /// A unary operation.
    Unary {
        /// The operator.
        op: IrUnOp,
        /// The operand.
        operand: IrExprId,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: IrBinOp,
        /// Left operand.
        lhs: IrExprId,
        /// Right operand.
        rhs: IrExprId,
    },
    /// A call to a builtin or user function.
    Call {
        /// What is being called.
        callee: IrCallee,
        /// The arguments, in order.
        args: Vec<IrExprId>,
        /// The result type (`Void` for `print`).
        result: Type,
    },
    /// Construction of a struct value: one initializer per field, in
    /// declaration order, with defaults already filled in by analysis.
    StructNew {
        /// The struct being built.
        struct_id: StructId,
        /// One initializer per field, in declaration order.
        fields: Vec<IrExprId>,
    },
    /// A read of one field of a struct value.
    Field {
        /// The struct-typed expression being read.
        base: IrExprId,
        /// The field's index in declaration order.
        index: u32,
        /// The field's type.
        ty: Type,
    },
}

/// The target of an IR call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IrCallee {
    /// The `print` builtin: consume one argument, emit one output line.
    Print,
    /// A user function, indexed into [`IrProgram::functions`].
    User(u32),
}
