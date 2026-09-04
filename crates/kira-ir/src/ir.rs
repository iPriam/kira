//! The mid-level IR: the verified, backend-facing form of a program.
//!
//! The IR is what every backend consumes. It is fully resolved and typed:
//! locals are slot indices, calls name a [`IrCallee`], and expression nodes
//! carry their [`Type`]. Nodes live in a per-program arena and reference each
//! other by [`IrExprId`], so no IR type carries a lifetime. Producing an
//! [`IrProgram`] is the contract that the program type-checked and has a valid
//! entrypoint.

use kira_runtime_abi::{
    Execution, ForeignAggregates, ForeignCallback, ForeignImport, NativeStateTypeId,
};
use kira_semantics_model::{Type, TypeDescriptorTable, TypeTable};
use la_arena::{Arena, Idx};

/// The scalar-conversion machine kinds, reused from the analyzer so the IR does
/// not re-derive which conversion a `Target(operand)` cast performs.
pub use kira_semantics_model::hir::ConvertKind;
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
    /// Every shape the program's types name: the one source of field layout
    /// and of array element types.
    pub types: TypeTable,
    /// The runtime identity of every type the program erases, tests, casts to,
    /// or asks for with `value.type`.
    ///
    /// Built during lowering, while a `distinct` is still itself: the rewrite
    /// that turns one into its representation is about the machine form of the
    /// value, and the identity is what the language says the value is. A
    /// backend looks rows up and never mints one, so both halves of a hybrid
    /// program name a type by the same id.
    pub descriptors: TypeDescriptorTable,
    /// Index of the `@Main` entrypoint within [`IrProgram::functions`], or
    /// `None` for a library.
    ///
    /// A library is entered by its consumer, one call at a time, so it has no
    /// single function that starts it. Backends read this to decide whether to
    /// emit an entry point at all.
    pub main: Option<u32>,
    /// Indices of every `@MainThreadLifecycle` function within
    /// [`IrProgram::functions`], in declaration order.
    ///
    /// Independent of [`IrProgram::main`]: these run on the process main
    /// thread, several at a time, while the entrypoint runs on the application
    /// thread.
    pub main_thread_lifecycles: Vec<u32>,
    /// The `@Export` surface a library offers its consumer, in declaration
    /// order; empty for an application and for a library that exports nothing.
    ///
    /// Carried across from the HIR unchanged: nothing about lowering can add
    /// to it or take from it, because whether a function may be exported was
    /// decided in the frontend, above the backend split.
    pub exports: Vec<IrExport>,
    /// The foreign (`@FFI.Extern`) imports, in declaration order.
    ///
    /// An [`IrCallee::Foreign`] indexes this vector. Carried across from the
    /// HIR unchanged: whether a foreign signature is legal was decided in the
    /// frontend, above the backend split, so lowering neither adds nor removes
    /// a row. Empty for a program that declares no extern.
    pub foreign_imports: Vec<IrForeignImport>,
    /// The C-layout aggregates the foreign signatures name by index.
    ///
    /// Carried across from the HIR unchanged, for the same reason the imports
    /// are: what a struct's C layout is was decided in the frontend, above the
    /// backend split. Empty for a program whose externs pass only scalars.
    pub foreign_aggregates: ForeignAggregates,
    /// The Kira functions reachable from C as function pointers, carried across
    /// from the HIR unchanged.
    ///
    /// Each row's index is the callback id an [`IrExpr::ForeignCallback`]
    /// carries, and the name the backend gives the entry thunk it emits.
    pub foreign_callbacks: Vec<ForeignCallback>,
    /// Arena backing every [`IrExprId`] across all functions.
    pub exprs: Arena<IrExpr>,
    /// Every module-scope constant, in evaluation order.
    ///
    /// The order is the contract: a backend fills the slots front to back,
    /// once, before `main` runs — each row after every row it depends on, as
    /// analysis ordered them. An [`IrExpr::ConstantGet`] indexes this vector.
    pub constants: Vec<IrConstant>,
}

/// One module-scope `let`: a single value computed once before `main`.
#[derive(Debug, Clone, PartialEq)]
pub struct IrConstant {
    /// The constant's name, for diagnostics and disassembly.
    pub name: String,
    /// The constant's type; the backend's global slot is shaped by it.
    pub ty: Type,
    /// Index within [`IrProgram::functions`] of the synthesized zero-argument
    /// function whose call computes the slot's value.
    pub init: u32,
}

/// One foreign C function the program calls through the FFI seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrForeignImport {
    /// The function's name as the Kira author wrote it, for diagnostics.
    pub name: String,
    /// The library, symbol, ABI, and exact-width signature a backend binds and
    /// calls. The `library` name is what a native-library catalog resolves.
    pub import: ForeignImport,
}

/// One function a library offers its consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrExport {
    /// The function's name as the Kira author wrote it (`makeButton`).
    pub kira_name: String,
    /// The name a consumer calls it by: `kira_name` in snake_case
    /// (`make_button`).
    pub exported_name: String,
    /// Index of the exported function within [`IrProgram::functions`].
    pub function: u32,
    /// The parameter types, in declaration order.
    pub params: Vec<Type>,
    /// The result type ([`Type::Void`] when the function returns nothing).
    pub result: Type,
}

impl IrProgram {
    /// Borrows an IR expression by handle.
    pub fn expr(&self, id: IrExprId) -> &IrExpr {
        &self.exprs[id]
    }

    /// The entrypoint function, or `None` for a library or an out-of-range
    /// index.
    ///
    /// Returns an option rather than indexing: a library genuinely has no
    /// entrypoint, and a caller that needs one has to say what it does without
    /// it instead of panicking on a program that is merely a library.
    pub fn main_function(&self) -> Option<&IrFunction> {
        self.functions.get(self.main? as usize)
    }

    /// Whether any expression in the program reaches the compiler capability.
    ///
    /// A native build asks this because the answer decides which runtime
    /// archive it links: the compiler's `kira_rt_compiler_*` helpers cannot
    /// live in the base archive — that would put the frontend inside every
    /// program Kira ever produces — so they live in an archive that carries
    /// them and this says whether the program needs it.
    #[must_use]
    pub fn uses_compiler(&self) -> bool {
        self.exprs
            .iter()
            .any(|(_, expr)| matches!(expr, IrExpr::Compiler { .. }))
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
            IrExpr::Int(_) => Type::INT,
            IrExpr::Float(_) => Type::FLOAT,
            IrExpr::Bool(_) => Type::Bool,
            IrExpr::Str(_) => Type::String,
            IrExpr::RawPtrNull | IrExpr::ForeignCallbackPtr { .. } => Type::RawPtr,
            IrExpr::MathOperation { .. } => Type::FLOAT,
            IrExpr::ScalarText { .. } => Type::String,
            IrExpr::ArrayElements { .. } => Type::CBlock,
            IrExpr::Local(slot) => function
                .locals
                .get(*slot as usize)
                .copied()
                .unwrap_or(Type::Error),
            IrExpr::Unary { op, ty, .. } => match op {
                IrUnOp::NegInt => *ty,
                IrUnOp::NegFloat => Type::FLOAT,
                IrUnOp::Not => Type::Bool,
                IrUnOp::BitNot => Type::INT,
            },
            IrExpr::Binary { op, ty, .. } => match binop_result(*op) {
                Type::INT => *ty,
                other => other,
            },
            IrExpr::Call { result, .. } => *result,
            IrExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            IrExpr::EnumNew { enum_id, .. } => Type::Enum(*enum_id),
            IrExpr::Field { ty, .. }
            | IrExpr::ForeignField { ty, .. }
            | IrExpr::ForeignMemberAddress { ty, .. }
            | IrExpr::ForeignElement { ty, .. }
            | IrExpr::ArrayNew { ty, .. }
            | IrExpr::EnumPayload { ty, .. }
            | IrExpr::NativeState { ty, .. }
            | IrExpr::NativeRecover { ty, .. }
            | IrExpr::Convert { ty, .. }
            | IrExpr::FileSystem { ty, .. }
            | IrExpr::Compiler { ty, .. }
            | IrExpr::Env { ty, .. }
            | IrExpr::CellNew { ty, .. }
            | IrExpr::CellNull { ty }
            | IrExpr::CellGet { ty, .. }
            | IrExpr::StringOperation { ty, .. }
            | IrExpr::Index { ty, .. } => *ty,
            IrExpr::Select { ty, .. } => *ty,
            IrExpr::TypeTest { .. } => Type::Bool,
            IrExpr::TypeCast { ty, .. } => *ty,
            IrExpr::ConstantGet { ty, .. } => *ty,
            IrExpr::StringCharAt { .. } => Type::Int(kira_semantics_model::IntSpelling::U8),
            IrExpr::ArrayLen { .. }
            | IrExpr::StringLen { .. }
            | IrExpr::StringIndexOf { .. }
            | IrExpr::EnumTag { .. } => Type::INT,
            IrExpr::StringSubstring { .. } | IrExpr::StringOf { .. } => Type::String,
            IrExpr::CStringNew { .. } | IrExpr::CLayoutAddress { .. } => Type::CBlock,
            IrExpr::NativeUserData { .. } => Type::RawPtr,
            IrExpr::IntoAny { .. } => Type::Any,
            IrExpr::TypeConst { .. } | IrExpr::TypeOf { .. } => Type::RuntimeType,
            IrExpr::TypeField { ty, .. } => *ty,
            IrExpr::TypeCastResult { result, .. } => Type::Enum(*result),
            IrExpr::MainThreadCall { ty, .. } | IrExpr::MainThreadJoin { ty, .. } => *ty,
            IrExpr::ArrayAppend { .. }
            | IrExpr::NativeStateRetain { .. }
            | IrExpr::NativeStateRelease { .. } => Type::Void,
            // Every primitive answers with one machine word, spelled `Int`.
            IrExpr::TaskOp { .. } | IrExpr::ChannelOp { .. } => Type::INT,
        }
    }

    /// The type stored at `place`, evaluated in `function`'s scope.
    ///
    /// Walks the place's path through the type table, so a backend choosing
    /// storage for an assignment does not re-resolve it.
    pub fn place_type(&self, function: &IrFunction, place: &IrPlace) -> Type {
        let mut ty = function
            .locals
            .get(place.local as usize)
            .copied()
            .unwrap_or(Type::Error);
        for step in &place.path {
            ty = match (step, ty) {
                (IrPlaceStep::Field(index), Type::Struct(id)) => self
                    .types
                    .structs()
                    .get(id)
                    .and_then(|def| def.field(*index))
                    .map_or(Type::Error, |field| field.ty),
                (IrPlaceStep::Index(_), array) => {
                    self.types.element_of(array).unwrap_or(Type::Error)
                }
                _ => Type::Error,
            };
        }
        ty
    }
}

/// The result type of a typed binary operator.
fn binop_result(op: IrBinOp) -> Type {
    match op {
        // Every integer width shares one representation, so the *result* of
        // integer arithmetic is reported as plain `Int` whatever the operands
        // were spelled. Nothing downstream re-derives signedness from this: the
        // operator itself already carries it (`DivUInt` versus `DivInt`).
        IrBinOp::AddInt
        | IrBinOp::SubInt
        | IrBinOp::MulInt
        | IrBinOp::WrappingAddInt
        | IrBinOp::WrappingSubInt
        | IrBinOp::WrappingMulInt
        | IrBinOp::DivInt
        | IrBinOp::RemInt
        | IrBinOp::DivUInt
        | IrBinOp::RemUInt
        | IrBinOp::BitAnd
        | IrBinOp::BitOr
        | IrBinOp::BitXor
        | IrBinOp::Shl
        | IrBinOp::ShrInt
        | IrBinOp::ShrUInt => Type::INT,
        IrBinOp::AddFloat
        | IrBinOp::SubFloat
        | IrBinOp::MulFloat
        | IrBinOp::DivFloat
        | IrBinOp::RemFloat => Type::FLOAT,
        IrBinOp::ConcatStr => Type::String,
        IrBinOp::EqInt
        | IrBinOp::NeInt
        | IrBinOp::LtInt
        | IrBinOp::LeInt
        | IrBinOp::GtInt
        | IrBinOp::GeInt
        | IrBinOp::LtUInt
        | IrBinOp::LeUInt
        | IrBinOp::GtUInt
        | IrBinOp::GeUInt
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
        | IrBinOp::EqAny
        | IrBinOp::NeAny
        | IrBinOp::EqType
        | IrBinOp::NeType
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
    /// Callback-state type ids for locals that are mutable recovered views.
    ///
    /// Positionally aligned with [`IrFunction::locals`]; `None` is an ordinary
    /// value slot, while `Some` stores only the opaque token and materializes the
    /// Kira value on reads.
    pub native_state_locals: Vec<Option<NativeStateTypeId>>,
    /// The function's return type.
    pub return_type: Type,
    /// The engine this function's body runs on, as written in the source.
    ///
    /// A hybrid build splits the program on this; a single-backend build
    /// resolves it against that backend's default.
    pub execution: Execution,
    /// Whether this function is entered through the host main-thread runtime.
    pub is_main_thread: bool,
    /// The parameter slots this function takes by reference, ascending.
    ///
    /// A statically-typed backend reads this to give those parameters a pointer
    /// type, so a call site that carries the matching writeback mutates the
    /// caller's storage in place. Two declarations put a slot here: a mutating
    /// method's receiver (slot 0) and a `borrow mut` parameter, wherever it
    /// sits. The VM ignores the list — its writeback is driven entirely by the
    /// call instruction. Empty for every ordinary function.
    pub by_reference_params: Vec<u32>,
    /// The parameter slots a read-only borrow can be *lent* through, ascending.
    ///
    /// `borrow` says the caller keeps the value, so there is nothing for the
    /// callee to own and nothing to copy — it can read the caller's storage
    /// where it sits. A backend that passes these by value instead copies the
    /// whole argument at every call: a view tree recursing over its children
    /// copies each child's entire subtree, and a layout tree passed down beside
    /// it copies every node and descriptor at every level.
    ///
    /// Only slots whose type costs something to copy are listed; an `Int` is
    /// cheaper in a register than behind a pointer.
    ///
    /// Distinct from [`Self::by_reference_params`], which exists so a *write*
    /// reaches the caller and drives a write-back. Nothing is written back
    /// through one of these, and the VM ignores the list entirely — it is a
    /// calling convention, not a semantic.
    pub by_pointer_params: Vec<u32>,
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

    /// Whether parameter slot `index` is passed as a pointer to the caller's
    /// storage rather than by value.
    pub fn param_by_reference(&self, index: u32) -> bool {
        self.by_reference_params.contains(&index)
    }

    /// Whether parameter slot `index` is lent as a pointer to the caller's
    /// storage, read through and never written or freed.
    pub fn param_by_pointer(&self, index: u32) -> bool {
        self.by_pointer_params.contains(&index)
    }

    /// Whether parameter slot `index` arrives as a pointer, either way.
    pub fn param_is_pointer(&self, index: u32) -> bool {
        self.param_by_reference(index) || self.param_by_pointer(index)
    }
}

/// A linear `attempt`/`try`/`handle` region.
///
/// The handler edge skips the remaining steps and lands at the region's common
/// end. This gives backends a direct control-flow shape without exposing the
/// source construct to each engine as a separate expression or opcode.
#[derive(Debug, Clone, PartialEq)]
pub struct IrAttempt {
    /// The guarded steps, in source order.
    pub steps: Vec<IrAttemptStep>,
    /// Statements after the final successful step.
    pub trailing: Vec<IrStmt>,
}

/// One step in an [`IrAttempt`].
#[derive(Debug, Clone, PartialEq)]
pub struct IrAttemptStep {
    /// Ordinary setup, including the hidden result and tag bindings.
    pub setup: Vec<IrStmt>,
    /// True when this step's result carries an `Error` value.
    pub error_condition: IrExprId,
    /// Failure handler dispatch.
    pub handler: Vec<IrStmt>,
    /// The successful `Ok` payload binding.
    pub success: Vec<IrStmt>,
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
    /// Replace what the capture cell in a local slot holds, in one step.
    ///
    /// One primitive, never a drop followed by a store: a split path traps
    /// between the two and leaves a freed handle in the box. Nothing is ever
    /// handed a pointer into the payload slot for the same reason.
    CellSet {
        /// The slot holding the cell.
        slot: u32,
        /// The value moving into the box; whatever was there is released.
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
    /// A linear, typed `attempt` region.
    Attempt {
        /// The guarded region.
        attempt: IrAttempt,
    },
    /// A pre-tested loop.
    ///
    /// The only loop shape the IR has: a source `for` is desugared into one
    /// during analysis, so no backend implements looping twice.
    While {
        /// The boolean loop condition.
        cond: IrExprId,
        /// The loop body.
        body: Vec<IrStmt>,
    },
    /// Leave the innermost enclosing [`IrStmt::While`].
    ///
    /// Analysis rejects one written outside a loop, so a backend may assume
    /// the enclosing loop exists rather than checking for it.
    Break,
    /// Skip to the innermost enclosing [`IrStmt::While`]'s next iteration.
    ///
    /// Jumps to the condition test. As with [`IrStmt::Break`], an enclosing
    /// loop is guaranteed by analysis.
    Continue,
    /// Release the bindings that die here, because the block that declared
    /// them is ending.
    ///
    /// Lowering places one at the end of every statement list whose bindings
    /// are dead past it, and before every [`IrStmt::Break`] and
    /// [`IrStmt::Continue`] — which end every block between themselves and
    /// their loop at once. A `Return` needs none: a return releases the whole
    /// frame plan anyway.
    ///
    /// The list names candidate slots as lowering computed them; each engine
    /// releases those *its* release plan owns (a lent borrow parameter never
    /// appears, but a slot's candidacy is engine-independent while ownership
    /// is not). A slot whose value was moved out holds nothing and releases
    /// nothing, so re-executing the same statement in a loop releases exactly
    /// once per iteration.
    ReleaseLocals {
        /// The slots whose bindings die at this point, ascending.
        locals: Vec<u32>,
    },
}

/// One step of an [`IrPlace`]'s walk.
///
/// A field index is a constant the analyzer resolved; an array index is an
/// expression with a value only at run time. Every backend has to treat the two
/// differently, so they are different variants rather than one number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrPlaceStep {
    /// Walk into the field at this index, in declaration order.
    Field(u32),
    /// Walk into the array element this expression selects.
    Index(IrExprId),
}

/// A writable location: a local slot, optionally walked into by fields and
/// indices.
///
/// The path is resolved at analysis time, so a backend writes through it
/// directly — it never rebuilds the enclosing value to change one part of it.
///
/// **Evaluation order is fixed here**: a place's index expressions are
/// evaluated left to right, and all of them before the assigned value. Every
/// backend follows it, which is what keeps `xs[next()] = next()` agreeing on
/// all four.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrPlace {
    /// The local slot the place is rooted at.
    pub local: u32,
    /// Steps to walk, outermost first; empty writes the slot itself.
    pub path: Vec<IrPlaceStep>,
}

impl IrPlace {
    /// The index expressions this place evaluates, outermost first.
    ///
    /// The order is the contract: a backend pushes these in exactly this order
    /// and the runtime consumes them in exactly this order.
    pub fn indices(&self) -> impl Iterator<Item = IrExprId> + '_ {
        self.path.iter().filter_map(|step| match step {
            IrPlaceStep::Index(expr) => Some(*expr),
            IrPlaceStep::Field(_) => None,
        })
    }

    /// Whether every step is a field index — the shape that predates arrays.
    pub fn is_all_fields(&self) -> bool {
        self.path
            .iter()
            .all(|step| matches!(step, IrPlaceStep::Field(_)))
    }
}

/// One argument a call writes back into the caller when it returns.
///
/// The lowered form of [`kira_semantics_model::HirWriteback`]: which parameter
/// the callee may write through, and where in the caller its final value goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrWriteback {
    /// The callee parameter slot whose final value is written back.
    pub param: u32,
    /// Where in the caller that value lands.
    pub place: IrPlace,
}

mod exprs;

/// The node set every backend switches on, and the target of a call.
///
/// Re-exported flat: `kira_ir::ir::IrExpr` is where every consumer already
/// names it, and which file it is written in is this crate's business.
pub use exprs::{IrCallee, IrExpr};

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::CompilerOp;

    /// A program with no functions, to hang expressions off.
    fn empty_program() -> IrProgram {
        IrProgram {
            functions: Vec::new(),
            types: TypeTable::default(),
            descriptors: Default::default(),
            main: None,
            main_thread_lifecycles: Vec::new(),
            exports: Vec::new(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            exprs: Arena::new(),
            constants: Vec::new(),
        }
    }

    /// The question a native build asks to choose its runtime archive, so a
    /// wrong answer is a link failure or a compiler in every binary.
    #[test]
    fn a_program_reports_whether_it_reaches_the_compiler() {
        let mut program = empty_program();
        let request = program.exprs.alloc(IrExpr::ArrayNew {
            elements: Vec::new(),
            ty: Type::String,
        });
        assert!(!program.uses_compiler());

        program.exprs.alloc(IrExpr::Compiler {
            op: CompilerOp::CheckPackages,
            args: vec![request],
            ty: Type::String,
        });
        assert!(program.uses_compiler());
    }
}
