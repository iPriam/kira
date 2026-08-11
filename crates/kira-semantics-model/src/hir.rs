//! The high-level IR: a name-resolved, fully-typed form of a program.
//!
//! The HIR is the analyzer's output and the IR lowerer's input. Names are
//! resolved (variables to [`LocalId`], calls to a [`Callee`]) and every
//! expression carries its [`Type`]. Nodes live in per-program arenas and refer
//! to each other by index, so no HIR type carries a lifetime. Local indices
//! are scoped to their owning function.
//!
//! This file holds the *program* model — what a program, a function, a local,
//! and a statement are. The expression tree is [`exprs`] and the operator
//! vocabulary it refers to is [`ops`], both split out on the file-size ladder
//! and re-exported here, so `kira_semantics_model::hir::HirExpr` is still one
//! path however the file is divided.

use crate::ty::{StructId, Type, TypeTable};
use kira_runtime_abi::{
    Execution, ForeignAbi, ForeignAggregateId, ForeignAggregates, ForeignCallback,
    ForeignSignature, NativeStateTypeId,
};
use kira_source::Span;
use kira_syntax_model::ownership::OwnershipMode;
use la_arena::{Arena, Idx};

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
    /// Per-parameter pointee struct, one entry per signature parameter.
    ///
    /// `Some(id)` marks a parameter written as an `@FFI.Pointer` to a C-layout
    /// struct. The wire position is a pointer word either way; this records what
    /// it points at, so a call may pass the struct and have its address taken.
    pub param_pointees: Box<[Option<ForeignPointee>]>,
    /// The result's pointer target, `Some(id)` when the result was written as
    /// an `@FFI.Pointer` to a C-layout struct.
    ///
    /// The wire position is a pointer word, which is all the signature records.
    /// This is what a *call* needs to hand back a pointer that still knows what
    /// it addresses, so members can be read through the returned pointer.
    pub result_pointee: Option<StructId>,
    /// The result's wrapper struct, `Some(id)` when the result is a
    /// single-scalar-field struct rebuilt from the seam scalar at the call.
    pub result_wrapper: Option<StructId>,
    /// Span of the function's name, for diagnostics.
    pub name_span: Span,
}

/// What an `@FFI.Pointer` parameter points at.
///
/// A pointer parameter's wire position is one pointer word. This is the extra
/// fact a *call* needs: which struct it may be handed instead, and how that
/// struct's C-layout image is described, so the seam can take its address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignPointee {
    /// The Kira struct a call may pass by address.
    pub struct_id: StructId,
    /// Its row in the program's C-layout aggregate table.
    pub aggregate: ForeignAggregateId,
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
    /// Whether the declaration was written `async function`.
    ///
    /// An `async` body is an ordinary body when it is *called*: the marker says
    /// the function is meant to be spawned, not that calling it does something
    /// different. It is carried here so a later phase can act on it without
    /// re-reading syntax.
    pub is_async: bool,
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

/// A linear `attempt` body.
///
/// Each step evaluates one `try` after its setup, branches to that step's
/// handler on failure, and continues with the success bindings otherwise.
/// Keeping the steps as a first-class control-flow shape avoids turning a long
/// source attempt into a recursively nested success tree.
#[derive(Debug, Clone, PartialEq)]
pub struct HirAttempt {
    /// The guarded steps, in source order.
    pub steps: Vec<HirAttemptStep>,
    /// Statements after the final `try`'s success binding.
    pub trailing: Vec<HirStmtId>,
}

/// One linear step in a [`HirAttempt`].
#[derive(Debug, Clone, PartialEq)]
pub struct HirAttemptStep {
    /// Ordinary statements before the `try`, followed by its hidden result and
    /// tag bindings.
    pub setup: Vec<HirStmtId>,
    /// The boolean test for the result's `Error` variant.
    pub error_condition: HirExprId,
    /// The handler dispatch for this step.
    pub handler: Vec<HirStmtId>,
    /// The `Ok` payload binding for this step.
    pub success: Vec<HirStmtId>,
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
    /// Replace what a capture cell holds, in one step.
    ///
    /// Deliberately **one** primitive rather than a drop followed by a store:
    /// a split path traps between the two and leaves a freed handle in the box.
    /// For the same reason nothing is ever handed a raw pointer into the
    /// payload slot — the only ways in and out are this and
    /// [`HirExpr::CellGet`].
    ///
    /// Writing *through* a cell into an aggregate it holds is not this
    /// statement: the analyzer reads the aggregate out, writes into the copy —
    /// which is where an array buys elements of its own — and stores the
    /// possibly-new handle back with this. Skipping the store-back would mutate
    /// a copy nobody can see.
    CellSet {
        /// The cell-typed local written through.
        local: LocalId,
        /// The value moving into the box; whatever was there is released.
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
    /// A linear `attempt`/`try`/`handle` region.
    Attempt {
        /// The analyzed guarded region.
        attempt: HirAttempt,
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

mod exprs;
mod ops;

pub use exprs::{ConvertKind, HirExpr, HirExprId, TaskTarget};
pub use ops::{Builtin, Callee, HirBinaryOp, HirUnaryOp};
