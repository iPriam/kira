//! The high-level IR: a name-resolved, fully-typed form of a program.
//!
//! The HIR is the analyzer's output and the IR lowerer's input. Names are
//! resolved (variables to [`LocalId`], calls to a [`Callee`]) and every
//! expression carries its [`Type`]. Nodes live in per-program arenas and refer
//! to each other by index, so no HIR type carries a lifetime. Local indices
//! are scoped to their owning function.

use crate::ty::{EnumId, StructId, Type, TypeTable};
use kira_runtime_abi::{
    Execution, FileSystemOp, ForeignAbi, ForeignAggregates, ForeignCallback, ForeignSignature,
    NativeStateTypeId,
};
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

/// Index of a foreign callable within a [`HirProgram`]'s foreign registry.
///
/// A bodyless `@FFI.Extern` declaration is **never** a [`HirFunction`] with an
/// empty body: it is a row in [`HirProgram::foreign`], and a call to it names
/// this id through [`Callee::Foreign`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForeignId(pub u32);

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
    /// The `@Export` surface, in declaration order.
    ///
    /// Empty for an application and for a library that exports nothing. Only
    /// exports that passed every boundary check are listed: a refused one is a
    /// compile error, and recording it would hand a backend a signature the
    /// frontend already rejected.
    pub exports: Vec<HirExport>,
    /// The foreign-callable registry, in declaration order.
    ///
    /// One row per `@FFI.Extern` declaration that passed every signature and
    /// annotation check. A [`Callee::Foreign`] indexes this vector; a refused
    /// extern is a compile error and is never recorded, so a backend only ever
    /// sees a signature the frontend accepted.
    pub foreign: Vec<HirForeign>,
    /// The C-layout aggregates the foreign signatures name by index.
    ///
    /// Built while validating the `@FFI.Extern` declarations: a Kira struct
    /// that crosses the seam is described here once, by its member tree, and
    /// every position naming it holds the same index. Empty for a program whose
    /// externs pass only scalars.
    pub foreign_aggregates: ForeignAggregates,
    /// The Kira functions reachable from C as function pointers.
    ///
    /// One row per distinct (function, signature) pair a `@FFI.Callback`-typed
    /// position was filled with. The id a row sits at is what the backend names
    /// its generated entry thunk after, so nothing has to match up by name.
    pub foreign_callbacks: Vec<ForeignCallback>,
    /// Arena backing every [`HirExprId`].
    pub exprs: Arena<HirExpr>,
    /// Arena backing every [`HirStmtId`].
    pub stmts: Arena<HirStmt>,
}

/// One `@FFI.Extern` foreign callable: a C symbol Kira calls seamlessly.
///
/// New Kira design: the oracle has no foreign-call concept. The record carries
/// everything a backend needs to bind and call the C symbol — library, symbol,
/// ABI, and the exact-width [`ForeignSignature`] — resolved once here so every
/// engine (VM, LLVM/native, hybrid) consumes the same row.
#[derive(Debug, Clone, PartialEq)]
pub struct HirForeign {
    /// The function's name as the Kira author wrote it.
    pub kira_name: String,
    /// The native-library name from the annotation's `library` field.
    pub library: String,
    /// The C symbol from the annotation's `symbol` field.
    pub symbol: String,
    /// The declared ABI (only [`ForeignAbi::C`] in this slice).
    pub abi: ForeignAbi,
    /// The exact-width parameter and result types.
    pub signature: ForeignSignature,
    /// Per-parameter wrapper struct, one entry per signature parameter.
    ///
    /// `Some(id)` marks a parameter written as a single-scalar-field struct
    /// (a C handle like `sg_image { id: U32 }`): it crosses the seam as its
    /// field's scalar — the entry in [`Self::signature`] — and the call reads
    /// that field out of the argument. `None` is an ordinary scalar parameter.
    /// This is a Kira-side rebuild detail; it never reaches the wire signature.
    pub param_wrappers: Box<[Option<StructId>]>,
    /// The result's wrapper struct, `Some(id)` when the result is a
    /// single-scalar-field struct rebuilt from the seam scalar at the call.
    pub result_wrapper: Option<StructId>,
    /// Span of the function's name, for diagnostics.
    pub name_span: Span,
}

/// One function a library offers its consumer.
///
/// New Kira design: the oracle has no export concept. The exported name is
/// derived, never written — `@Export` takes no symbol override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirExport {
    /// The function's name as the Kira author wrote it (`makeButton`).
    pub kira_name: String,
    /// The name a consumer calls it by: `kira_name` in snake_case
    /// (`make_button`). Two exports mapping to one of these is an error.
    pub exported_name: String,
    /// The function this export names.
    pub function: FuncId,
    /// The resolved parameter types, in declaration order.
    ///
    /// Recorded here rather than looked up from the function later because the
    /// export surface is what a consumer's generated wrapper is built against:
    /// it has to travel with the export, all the way into the artifact.
    pub params: Vec<Type>,
    /// The resolved result type ([`Type::Void`] when none was written).
    pub result: Type,
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
    /// Whether this function is a method that mutates its receiver.
    ///
    /// `true` only for a method whose body assigns to `self`, appends through
    /// `self`, or calls another mutating method on `self` (transitively). A
    /// mutating method takes its receiver by reference so the mutation is
    /// written back to the caller; every other function carries `false`.
    pub mutates_self: bool,
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
    /// The boxed type id when this slot is a mutable `nativeRecover<Value>` view.
    pub native_state: Option<NativeStateTypeId>,
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

/// One argument a call writes back into the caller when it returns.
///
/// A callee that may write through a parameter — a mutating method's receiver,
/// or a `borrow mut` parameter — is given the caller's storage rather than a
/// copy of it. This says which parameter, and where in the caller its final
/// value belongs; it is the only shape in which a callee's write is observable
/// to its caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirWriteback {
    /// The callee parameter slot whose final value is written back.
    pub param: u32,
    /// Where in the caller that value lands.
    pub place: HirPlace,
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
    /// The address C enters a Kira function at, for callback `callback`.
    ///
    /// An index into [`HirProgram::foreign_callbacks`]. The value is a
    /// `RawPtr`: the backend generates one entry thunk per row and this is its
    /// address, so nothing about a function value has to exist for C to hold
    /// one.
    ForeignCallbackPtr {
        /// The callback entry this address enters.
        callback: u32,
    },
    /// The null raw pointer.
    ///
    /// The one `RawPtr` constant Kira spells. It exists because a C-layout
    /// struct zero-fills a pointer member to `NULL`, and a zero-fill that could
    /// not name its own zero would have to refuse the field instead.
    RawPtrNull,
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
        /// Every argument the callee writes back into the caller's storage, in
        /// parameter order.
        ///
        /// Empty for every ordinary call, which then behaves exactly as it did
        /// before value-semantics writeback existed. A mutating method
        /// contributes one entry for its receiver (`args[0]`); a `borrow mut`
        /// parameter contributes one for its own position. Each entry's final
        /// callee-side value is stored back into its place after the call — the
        /// side effect that makes a write inside the callee observable to the
        /// caller, while the call still yields the declared return value.
        writebacks: Vec<HirWriteback>,
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
    /// A string's character count (`s.count`) — a property, not a call.
    ///
    /// Characters, not bytes: `charAt` and `substring` index the same units, so
    /// a count in bytes would disagree with the two primitives it sits beside.
    StringLen {
        /// The string-typed expression being measured.
        text: HirExprId,
    },
    /// One file-system operation, performed by the engine on the host's behalf.
    ///
    /// An intrinsic rather than a call because reaching the outside world is an
    /// effect no Kira function body can express. The result type is carried
    /// rather than derived: it follows from the operation alone, and storing it
    /// keeps every consumer from re-deriving the same table.
    FileSystem {
        /// Which operation this performs.
        op: FileSystemOp,
        /// Its arguments, in source order.
        args: Vec<HirExprId>,
        /// What the operation produces.
        ty: Type,
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
    /// Boxes a copy of a Kira-owned value in opaque callback-state storage.
    NativeState {
        /// The value copied into the box.
        value: HirExprId,
        /// The stable runtime identity of the boxed type.
        type_id: NativeStateTypeId,
        /// The opaque handle type returned to Kira.
        ty: Type,
    },
    /// Exports a callback-state handle's stable opaque userdata token.
    NativeUserData {
        /// The state handle.
        state: HirExprId,
    },
    /// Recovers typed mutable access through a returned userdata token.
    NativeRecover {
        /// The opaque raw userdata token.
        raw: HirExprId,
        /// The stable runtime identity recovery validates.
        type_id: NativeStateTypeId,
        /// The Kira value type exposed by the mutable view.
        ty: Type,
    },
    /// Releases a callback-state handle or userdata token exactly once.
    NativeStateFree {
        /// The state handle or raw token.
        token: HirExprId,
    },
    /// A scalar type-conversion call, `Target(operand)` where `Target` is a numeric
    /// scalar type.
    ///
    /// This is a value conversion, not a function call: `Int(2.9)` is `2`,
    /// `Float(7)` is `7.0`. The `kind` fixes the machine operation so no
    /// backend re-derives it from the operand and target types — the same
    /// split [`HirBinaryOp`] uses to bake signedness into the operator. The
    /// integer-width spelling is carried in `ty`, not in a runtime narrowing:
    /// every integer shares one 64-bit representation, so an int-to-int
    /// conversion re-tags the type and copies the value unchanged.
    Convert {
        /// The value being converted.
        operand: HirExprId,
        /// Which machine conversion this is.
        kind: ConvertKind,
        /// The target type, carrying its width spelling.
        ty: Type,
    },
    /// A placeholder for an expression that failed to analyze.
    Error,
}

/// Which machine operation a scalar [`HirExpr::Convert`] performs.
///
/// The four kinds are the cross product of the two numeric runtime
/// representations (`Int` is `i64`, `Float` is `f64`). Two are identity copies
/// — an integer width is a type-level annotation over one representation, and
/// float width likewise — and two do real work. The kind is fixed at analysis,
/// so nothing below re-derives it: the VM and every backend read it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertKind {
    /// Integer to integer, any width to any width. An identity copy: widths
    /// share one 64-bit representation, so nothing is truncated or extended.
    IntToInt,
    /// Float to float (`F32`/`F64`/`Float`). An identity copy: every float is
    /// one 64-bit representation, and float arithmetic runs at that width.
    FloatToFloat,
    /// Integer to float, a signed conversion (round to nearest, ties to even).
    IntToFloat,
    /// Float to integer: truncate toward zero, saturating out-of-range inputs
    /// to `i64::MIN`/`i64::MAX` and mapping NaN to zero. Never traps.
    FloatToInt,
    /// `floatToBits`: the IEEE-754 bit pattern of a `Float`, as a `U64`.
    ///
    /// A reinterpretation, not a conversion — the opposite of [`Self::FloatToInt`]
    /// in every way that matters. Nothing rounds, nothing saturates, and NaN
    /// keeps the exact payload it had, which is what makes it usable for
    /// serializing a float byte for byte.
    FloatToBits,
    /// `bitsToFloat`: the `Float` an IEEE-754 bit pattern denotes.
    ///
    /// The exact inverse of [`Self::FloatToBits`], so a round trip through the
    /// two is the identity for every value including NaN.
    BitsToFloat,
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
            HirExpr::RawPtrNull | HirExpr::ForeignCallbackPtr { .. } => Type::RawPtr,
            HirExpr::Local { ty, .. }
            | HirExpr::Unary { ty, .. }
            | HirExpr::Binary { ty, .. }
            | HirExpr::Select { ty, .. }
            | HirExpr::Call { ty, .. }
            | HirExpr::Field { ty, .. }
            | HirExpr::ArrayNew { ty, .. }
            | HirExpr::EnumPayload { ty, .. }
            | HirExpr::NativeState { ty, .. }
            | HirExpr::NativeRecover { ty, .. }
            | HirExpr::Convert { ty, .. }
            | HirExpr::FileSystem { ty, .. }
            | HirExpr::Index { ty, .. } => *ty,
            HirExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            HirExpr::EnumNew { enum_id, .. } => Type::Enum(*enum_id),
            // `.count` and a tag read are both `Int`; `.append` yields nothing.
            // None has a type that can vary, so none carries one.
            HirExpr::ArrayLen { .. } | HirExpr::StringLen { .. } | HirExpr::EnumTag { .. } => {
                Type::INT
            }
            HirExpr::NativeUserData { .. } => Type::RawPtr,
            HirExpr::ArrayAppend { .. } | HirExpr::NativeStateFree { .. } => Type::Void,
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
    /// A foreign C function, indexed into [`HirProgram::foreign`].
    ///
    /// The call site is ordinary Kira — no `@Native`, no ceremony — and the
    /// registry row carries the exact-width signature the call was checked
    /// against.
    Foreign(ForeignId),
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
