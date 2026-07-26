//! The mid-level IR: the verified, backend-facing form of a program.
//!
//! The IR is what every backend consumes. It is fully resolved and typed:
//! locals are slot indices, calls name a [`IrCallee`], and expression nodes
//! carry their [`Type`]. Nodes live in a per-program arena and reference each
//! other by [`IrExprId`], so no IR type carries a lifetime. Producing an
//! [`IrProgram`] is the contract that the program type-checked and has a valid
//! entrypoint.

use kira_runtime_abi::{
    Execution, FileSystemOp, ForeignAggregates, ForeignCallback, ForeignImport, NativeStateTypeId,
};
use kira_semantics_model::{EnumId, StructId, Type, TypeTable};
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
    /// Index of the `@Main` entrypoint within [`IrProgram::functions`], or
    /// `None` for a library.
    ///
    /// A library is entered by its consumer, one call at a time, so it has no
    /// single function that starts it. Backends read this to decide whether to
    /// emit an entry point at all.
    pub main: Option<u32>,
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
            IrExpr::Local(slot) => function
                .locals
                .get(*slot as usize)
                .copied()
                .unwrap_or(Type::Error),
            IrExpr::Unary { op, .. } => match op {
                IrUnOp::NegInt => Type::INT,
                IrUnOp::NegFloat => Type::FLOAT,
                IrUnOp::Not => Type::Bool,
                IrUnOp::BitNot => Type::INT,
            },
            IrExpr::Binary { op, .. } => binop_result(*op),
            IrExpr::Call { result, .. } => *result,
            IrExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            IrExpr::EnumNew { enum_id, .. } => Type::Enum(*enum_id),
            IrExpr::Field { ty, .. }
            | IrExpr::ArrayNew { ty, .. }
            | IrExpr::EnumPayload { ty, .. }
            | IrExpr::NativeState { ty, .. }
            | IrExpr::NativeRecover { ty, .. }
            | IrExpr::Convert { ty, .. }
            | IrExpr::FileSystem { ty, .. }
            | IrExpr::Index { ty, .. } => *ty,
            IrExpr::Select { ty, .. } => *ty,
            IrExpr::ArrayLen { .. }
            | IrExpr::StringLen { .. }
            | IrExpr::StringCharAt { .. }
            | IrExpr::StringIndexOf { .. }
            | IrExpr::EnumTag { .. } => Type::INT,
            IrExpr::StringSubstring { .. } | IrExpr::StringOf { .. } => Type::String,
            IrExpr::CStringNew { .. } => Type::CString,
            IrExpr::CLayoutAddress { .. } => Type::RawPtr,
            IrExpr::NativeUserData { .. } => Type::RawPtr,
            IrExpr::ArrayAppend { .. } | IrExpr::NativeStateFree { .. } => Type::Void,
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
        IrBinOp::AddFloat | IrBinOp::SubFloat | IrBinOp::MulFloat | IrBinOp::DivFloat => {
            Type::FLOAT
        }
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
    /// The parameter slots this function takes by reference, ascending.
    ///
    /// A statically-typed backend reads this to give those parameters a pointer
    /// type, so a call site that carries the matching writeback mutates the
    /// caller's storage in place. Two declarations put a slot here: a mutating
    /// method's receiver (slot 0) and a `borrow mut` parameter, wherever it
    /// sits. The VM ignores the list — its writeback is driven entirely by the
    /// call instruction. Empty for every ordinary function.
    pub by_reference_params: Vec<u32>,
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
    /// The null raw pointer — the zero a C-layout pointer member fills with.
    RawPtrNull,
    /// The address C enters a Kira function at, for callback `callback`.
    ///
    /// An index into [`IrProgram::foreign_callbacks`]; its value is a `RawPtr`.
    ForeignCallbackPtr {
        /// The callback entry this address enters.
        callback: u32,
    },
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
    /// A conditional expression, `cond ? then : otherwise`.
    ///
    /// Exactly one branch is evaluated, so this is control flow rather than a
    /// select instruction: every backend lowers it as a branch and a join, the
    /// same shape `And`/`Or` already use.
    Select {
        /// The `Bool` condition.
        cond: IrExprId,
        /// The value when the condition holds.
        then: IrExprId,
        /// The value when it does not.
        otherwise: IrExprId,
        /// The type both branches agreed on.
        ty: Type,
    },
    /// A call to a builtin or user function.
    Call {
        /// What is being called.
        callee: IrCallee,
        /// The arguments, in order.
        args: Vec<IrExprId>,
        /// The result type (`Void` for `print`).
        result: Type,
        /// Every argument the callee writes back into the caller, in parameter
        /// order.
        ///
        /// Empty for every ordinary call, which behaves exactly as before. Each
        /// entry names a parameter the callee may write through — a mutating
        /// method's receiver, or a `borrow mut` parameter — and the caller place
        /// its final value lands in after the call, which is what makes that
        /// write observable while the call still yields `result`.
        writebacks: Vec<IrWriteback>,
    },
    /// Construction of a struct value: one initializer per field, in
    /// declaration order, with defaults already filled in by analysis.
    StructNew {
        /// The struct being built.
        struct_id: StructId,
        /// One initializer per field, in declaration order.
        fields: Vec<IrExprId>,
    },
    /// Construction of an enum value: a variant (by `tag`) plus its optional
    /// single payload, defaults already filled in by analysis.
    EnumNew {
        /// The enum being built.
        enum_id: EnumId,
        /// The variant's declaration index — its discriminant.
        tag: u32,
        /// The payload value, or `None` for a payload-less variant.
        payload: Option<IrExprId>,
    },
    /// An enum value's discriminant tag, as an `Int` (`e`'s variant index).
    EnumTag {
        /// The enum-typed expression whose tag is read.
        value: IrExprId,
    },
    /// An enum value's payload, as an owned value of the variant's payload type.
    ///
    /// Emitted only inside a `match` arm the tag test already selected, so the
    /// payload is known to have type `ty`. A backend reads the payload out and
    /// hands back an owned copy.
    EnumPayload {
        /// The enum-typed expression whose payload is read.
        value: IrExprId,
        /// The selected variant's declared payload type.
        ty: Type,
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
    /// Construction of an array from its elements, in written order.
    ArrayNew {
        /// The array's type.
        ty: Type,
        /// The elements, in order.
        elements: Vec<IrExprId>,
    },
    /// A read of one element of an array (`xs[i]`).
    ///
    /// An out-of-range or negative index is a **runtime trap**, not a static
    /// check: an index is generally not a constant, so checking it here would
    /// reject working programs.
    Index {
        /// The array-typed expression being read.
        base: IrExprId,
        /// The `Int`-typed index.
        index: IrExprId,
        /// The element's type.
        ty: Type,
    },
    /// An array's element count (`xs.count`).
    ArrayLen {
        /// The array-typed expression being measured.
        array: IrExprId,
    },
    /// A string's length in bytes (`s.count`).
    StringLen {
        /// The string-typed expression being measured.
        text: IrExprId,
    },
    /// The byte at an index of a string (`s.charAt(i)`); traps out of range.
    StringCharAt {
        /// The string being read.
        text: IrExprId,
        /// The byte index.
        index: IrExprId,
    },
    /// A half-open byte slice of a string (`s.substring(start, end)`); traps on
    /// an inverted or out-of-range range.
    StringSubstring {
        /// The string being sliced.
        text: IrExprId,
        /// The inclusive lower bound, in bytes.
        start: IrExprId,
        /// The exclusive upper bound, in bytes.
        end: IrExprId,
    },
    /// The byte index of the first occurrence of a needle, or `-1`.
    StringIndexOf {
        /// The string being searched.
        text: IrExprId,
        /// The string being searched for.
        needle: IrExprId,
    },
    /// A scalar rendered as text (`String(x)`).
    StringOf {
        /// The value being rendered.
        value: IrExprId,
    },
    /// The address of a C-layout struct's image, in storage that outlives the
    /// call. See [`kira_semantics_model::hir::HirExpr::CLayoutAddress`].
    CLayoutAddress {
        /// The struct value whose image is written.
        value: IrExprId,
        /// The aggregate row describing its C layout.
        aggregate: kira_runtime_abi::ForeignAggregateId,
    },
    /// A `String` copied into C storage that outlives the call.
    ///
    /// See [`kira_semantics_model::hir::HirExpr::CStringNew`]. The null case
    /// lowers to [`IrExpr::RawPtrNull`] instead, because a null C string and a
    /// null pointer are the same zero word.
    CStringNew {
        /// The string whose bytes are copied.
        text: IrExprId,
    },
    /// One file-system operation, performed by the engine on the host's behalf.
    ///
    /// See [`kira_semantics_model::hir::HirExpr::FileSystem`]: an effect no Kira
    /// body can express, so each engine performs it its own way and the node
    /// survives lowering intact.
    FileSystem {
        /// Which operation this performs.
        op: FileSystemOp,
        /// Its arguments, in source order.
        args: Vec<IrExprId>,
        /// What the operation produces.
        ty: Type,
    },
    /// `xs.append(v)`: push one element onto an array, in place.
    ///
    /// The receiver is a place, not an expression — see
    /// [`kira_semantics_model::hir::HirExpr::ArrayAppend`] for why that is the
    /// whole correctness argument rather than an optimization.
    ArrayAppend {
        /// The array being appended to.
        place: IrPlace,
        /// The element to push.
        value: IrExprId,
    },
    /// Boxes a copy of a Kira-owned value in opaque callback-state storage.
    NativeState {
        /// The value copied into the box.
        value: IrExprId,
        /// The stable runtime identity of the boxed type.
        type_id: NativeStateTypeId,
        /// The opaque handle type returned to Kira.
        ty: Type,
    },
    /// Exports a callback-state handle's stable opaque userdata token.
    NativeUserData {
        /// The state handle.
        state: IrExprId,
    },
    /// Recovers typed mutable access through a returned userdata token.
    NativeRecover {
        /// The opaque raw userdata token.
        raw: IrExprId,
        /// The stable runtime identity recovery validates.
        type_id: NativeStateTypeId,
        /// The Kira value type exposed by the mutable view.
        ty: Type,
    },
    /// Releases a callback-state handle or userdata token exactly once.
    NativeStateFree {
        /// The state handle or raw token.
        token: IrExprId,
    },
    /// A scalar type-conversion, `Target(operand)`.
    ///
    /// The `kind` fixes the machine operation (see
    /// [`ConvertKind`]); `ty` carries the target type. A backend that ignored
    /// `kind` and read the operand's type would have to re-derive the same
    /// choice analysis already made.
    Convert {
        /// The value being converted.
        operand: IrExprId,
        /// Which machine conversion this is.
        kind: ConvertKind,
        /// The target type, carrying its width spelling.
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
    /// A foreign C function, indexed into [`IrProgram::foreign_imports`].
    ///
    /// The call site is ordinary Kira. A backend marshals the arguments to the
    /// import's exact-width signature and invokes the generated adapter (native
    /// engines) or the host's `call_foreign` (the VM).
    Foreign(u32),
}
