//! The HIR expression tree, split out of [`super`] on the file-size ladder.
//!
//! One module because it is one enum plus the two small enums only it names and
//! the one function that reads it. Every arm of [`HirExpr::type_of`] is about a
//! variant declared here, so keeping the two together is what stops a new
//! variant being added in one place and forgotten in the other.

use super::ops::{Callee, HirBinaryOp, HirUnaryOp};
use super::{FuncId, HirPlace, HirWriteback, LocalId};
use crate::ty::{EnumId, StructId, Type};
use kira_runtime_abi::{
    CompilerOp, EnvOp, FileSystemOp, ForeignAggregateId, MainThreadOp, NativeStateTypeId,
};
use la_arena::Idx;

/// Handle to a HIR expression.
pub type HirExprId = Idx<HirExpr>;

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
    /// A read of a module-scope constant's global slot.
    ///
    /// The slot indexes [`HirProgram::constants`]: it was filled once at
    /// program start, so a read copies the stored value the way a field read
    /// copies a field — the constant keeps what it holds.
    ///
    /// [`HirProgram::constants`]: super::HirProgram::constants
    ConstantGet {
        /// The constant's index in the program's evaluation-ordered table.
        constant: u32,
        /// The constant's type.
        ty: Type,
    },
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
    /// Boxing a value into a fresh capture cell (`HirStmt::Let` of a boxed
    /// `var`).
    ///
    /// The one producer of a [`Type::Cell`]. The value moves in: whatever it
    /// owned is now owned by the box, and the box is owned by whoever holds the
    /// handle.
    CellNew {
        /// The value moving into the box.
        value: HirExprId,
        /// The cell type produced (an interned [`Type::Cell`]).
        ty: Type,
    },
    /// A null capture-cell slot used by a closure representation's padding.
    CellNull {
        /// The cell type represented by the null slot.
        ty: Type,
    },
    /// An **owned** read of what a capture cell holds.
    ///
    /// Rooted at a local rather than an expression because every cell the
    /// analyzer mints lives in one: a boxed `var` is a local of the enclosing
    /// frame, and a captured cell is copied out of the closure's
    /// representation struct into a local by the lifted body's prologue. Naming
    /// the slot is what lets a read avoid taking — and then dropping — a share
    /// of the handle just to look inside it.
    ///
    /// The value that comes back is the caller's, copied out exactly as a
    /// field or an element read is. A borrowing read would let the payload be
    /// freed by a write through some other holder while a caller still had it.
    CellGet {
        /// The cell-typed local read through.
        local: LocalId,
        /// The type inside the cell — the type of this expression.
        ty: Type,
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
    /// A member read through an `@FFI.Pointer` — a load from C memory.
    ///
    /// Unlike [`HirExpr::Field`], nothing here is a Kira value until the load
    /// happens: the base is a pointer word into storage C owns, and the member
    /// lives at a byte offset inside that struct's C layout.
    ///
    /// Carries the aggregate and member index rather than a byte offset: a C
    /// pointer is four bytes on `wasm32` and eight elsewhere, so a struct with a
    /// pointer member ahead of this one sits at a different offset per target.
    /// Each backend computes the offset for the width it emits for.
    /// The address of a member that names storage rather than a value.
    ///
    /// A nested struct and an inline array have bytes inside the container, so
    /// there is nothing to load: what the member names is a place. Its address
    /// is the container's plus the member's offset, and the result is a pointer
    /// that knows what it addresses — so `event.at.x` reads through it, and
    /// `event.touches` is the pointer to element zero that C's array-to-pointer
    /// decay produces.
    ForeignMemberAddress {
        /// The pointer-typed expression being read through.
        base: HirExprId,
        /// The container's row in the program's C-layout aggregate table.
        aggregate: ForeignAggregateId,
        /// The member's index in declaration order.
        member: u32,
        /// The pointer type the address has.
        ty: Type,
    },
    /// The address of one element of a C array, by pointer arithmetic.
    ///
    /// `pointer[index]` on a pointer into C storage, with the same meaning C
    /// gives it: the address `index` elements along. The element's size comes
    /// from the target's C layout, so it is computed for the target being built
    /// for.
    ForeignElement {
        /// The pointer being indexed.
        base: HirExprId,
        /// The element's row in the program's C-layout aggregate table.
        aggregate: ForeignAggregateId,
        /// The element index.
        index: HirExprId,
        /// The pointer type the address has.
        ty: Type,
    },
    ForeignField {
        /// The pointer-typed expression being read through.
        base: HirExprId,
        /// The target's row in the program's C-layout aggregate table.
        aggregate: ForeignAggregateId,
        /// The member's index in declaration order.
        member: u32,
        /// The Kira type the member reads as.
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
    /// A string's length in bytes (`s.count`) — a property, not a call.
    ///
    /// Bytes, not characters: `charAt` and `substring` index the same units,
    /// and a UTF-8 byte index is what the wire formats built on these
    /// primitives carve at.
    StringLen {
        /// The string-typed expression being measured.
        text: HirExprId,
    },
    /// The byte at an index of a string (`s.charAt(i)`).
    ///
    /// Traps when the index is outside `0 ..< s.count`, which is what makes an
    /// out-of-range read a deterministic failure on every backend rather than a
    /// value nothing agrees on.
    StringCharAt {
        /// The string being read.
        text: HirExprId,
        /// The byte index.
        index: HirExprId,
    },
    /// A half-open byte slice of a string (`s.substring(start, end)`).
    ///
    /// Traps when `start > end` or either bound is outside `0 ..= s.count`.
    StringSubstring {
        /// The string being sliced.
        text: HirExprId,
        /// The inclusive lower bound, in bytes.
        start: HirExprId,
        /// The exclusive upper bound, in bytes.
        end: HirExprId,
    },
    /// The byte index of the first occurrence of a needle (`s.indexOf(n)`), or
    /// `-1` when there is none.
    ///
    /// An empty needle matches at the front, so it answers `0`.
    StringIndexOf {
        /// The string being searched.
        text: HirExprId,
        /// The string being searched for.
        needle: HirExprId,
    },
    /// The address of a C buffer holding an array's elements.
    ///
    /// A Kira array is a heap object this runtime owns, and its elements are
    /// Kira's widths — a `[F32]` holds them as the seam's `float` only once
    /// something writes them out. So the seam writes them, exactly as it writes
    /// a C-layout struct's image for [`HirExpr::CLayoutAddress`], and hands over
    /// the address of what it wrote.
    ///
    /// The storage outlives the call for the reason a `CString` member's does:
    /// a C API given a buffer may keep it — `sg_make_buffer` reads it during the
    /// call, but nothing on this side knows which kind of callee it has.
    ///
    /// Written wherever a pointer word is: an extern's `RawPtr` argument, and a
    /// C-layout struct's `RawPtr` member. The second is what lets a descriptor
    /// carrying a data pointer — `sg_range { ptr: values, size: … }` — be built
    /// in Kira instead of in a C helper that exists only to name the address.
    ArrayElements {
        /// The array whose elements are written out.
        value: HirExprId,
        /// The seam type each element is written as.
        element: kira_runtime_abi::ForeignType,
    },
    /// The text of one Unicode scalar, from its code point.
    ///
    /// The inverse of reading a scalar out of text, and the operation a text
    /// field needs when a key press arrives as a code point. It is a
    /// constructor rather than a [`kira_runtime_abi::StringOp`] because it
    /// starts from a number instead of from text.
    ScalarText {
        /// The code point.
        value: HirExprId,
    },
    /// A floating-point operation the hardware already has.
    ///
    /// `sqrt(x)`, `sin(x)`, `pow(x, y)` and the rest. Written as an ordinary
    /// call, resolved here rather than to a user function, so a program cannot
    /// shadow one with a series expansion that answers slightly differently.
    MathOperation {
        /// Which operation to perform.
        op: kira_runtime_abi::MathOp,
        /// The values it is performed on, in source order. How many there are
        /// is the operation's own
        /// [`argument_count`](kira_runtime_abi::MathOp::argument_count) — held
        /// as a list rather than a variant per arity for the reason
        /// [`StringOperation`](Self::StringOperation) gives.
        operands: Vec<HirExprId>,
    },
    /// One of the string operations that share an opcode (`s.contains(n)`,
    /// `s.trim()`, `s.split(sep)`, …).
    ///
    /// Which operation, how many arguments it takes and what it answers with
    /// all follow from the [`StringOp`](kira_runtime_abi::StringOp) — it is the
    /// whole expression, not a hint. Grouped rather than given a variant each
    /// because the set is meant to keep growing, and a variant per operation
    /// makes every layer below grow with it.
    StringOperation {
        /// Which operation to perform.
        op: kira_runtime_abi::StringOp,
        /// The string it is performed on.
        text: HirExprId,
        /// Its arguments, in source order.
        arguments: Vec<HirExprId>,
        /// What it answers with.
        ///
        /// Carried rather than derived from `op`, because `split` answers
        /// `[String]` and an array type is a row in the program's table —
        /// interning one needs the program, which [`HirExpr::type_of`] does not
        /// have.
        ty: Type,
    },
    /// A scalar rendered as text (`String(x)`).
    ///
    /// The rendering is the one `print` gives, so a value printed and a value
    /// converted never disagree.
    StringOf {
        /// The value being rendered.
        value: HirExprId,
    },
    /// The address of a C-layout struct's image, in storage that outlives the
    /// call.
    ///
    /// What a call passes where a parameter was written as an `@FFI.Pointer` to
    /// that struct. The image is built once and never released, for the same
    /// reason a `CString` member is not: nothing here knows whether the callee
    /// kept the pointer, and a buffer freed when the call returns is a dangling
    /// pointer for every callee that did.
    CLayoutAddress {
        /// The struct value whose image is written.
        value: HirExprId,
        /// The aggregate row describing its C layout.
        aggregate: ForeignAggregateId,
    },
    /// A `String` copied into C storage that outlives the call.
    ///
    /// Inserted where a `String` fills a `CString` member of a C-layout struct.
    /// A `CString` *parameter* stays transient — C reads it during the call and
    /// the seam frees it after — but a member of a struct C keeps is read long
    /// after the call returns, so its storage is never released. See
    /// [`kira_runtime_abi::c_storage`] for why that is the safe answer rather
    /// than the lazy one.
    CStringNew {
        /// The string whose bytes are copied.
        text: HirExprId,
    },
    /// The null C string: what a `CString` member zero-fills to.
    CStringNull,
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
    /// One compiler operation, performed by the engine on the host's behalf.
    ///
    /// An intrinsic for the same reason [`HirExpr::FileSystem`] is: the engine
    /// reaches a compiler the way it reaches a filesystem, through its host, and
    /// no Kira function body can express that.
    Compiler {
        /// Which operation this performs.
        op: CompilerOp,
        /// Its arguments, in source order.
        args: Vec<HirExprId>,
        /// What the operation produces.
        ty: Type,
    },
    /// One environment read, which only the engine can perform.
    Env {
        /// Which operation this performs.
        op: EnvOp,
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
    /// A value crossing into the top type: `value`, with its type erased.
    ///
    /// Analysis inserts this wherever a concrete value lands in an `Any`
    /// position — a return, an argument, a `let` initializer, a field, an enum
    /// payload, an array element — so no backend has to compare an operand's
    /// type against its destination's to notice that an erasure happened.
    ///
    /// `from` is what was erased, and it is carried rather than re-derived
    /// because it is the only thing that says what the erased value owns: a
    /// backend that boxes needs it to pick the tag, and the one that does not
    /// still needs it to know there is nothing to do.
    IntoAny {
        /// The value being erased.
        value: HirExprId,
        /// The type it had before erasure.
        from: Type,
    },
    /// One generic instantiation carried into another whose type arguments are
    /// `Any`: `Result<Int, E>` where `Result<Any, E>` is written.
    ///
    /// A sibling of [`HirExpr::IntoAny`] rather than a case of it. That node
    /// wraps a value whose type stops being known; this one hands back a value
    /// whose type is still known exactly — a different enum row, with the same
    /// tag and a payload that crossed into `Any`. On the VM the two rows have
    /// one runtime form and this costs nothing; on a statically typed backend
    /// the payload's machine form differs, so this is a rebuild. Both types are
    /// carried because both name a row the rebuild reads its variants from.
    ///
    /// See [`crate::TypeTable::widens_to`] for which pairs are
    /// admitted, and why a payload behind an array is not among them.
    Widen {
        /// The value being widened.
        value: HirExprId,
        /// The instantiation it had.
        from: Type,
        /// The instantiation the position declared.
        to: Type,
    },
    /// `Task { work(a, b) }` — spawn a deferred task and yield its handle.
    ///
    /// The arguments are evaluated **here**, at the spawn site, and the body
    /// runs at the first drive. That split is the whole of what "deferred"
    /// means, and it is why the target and the arguments are separate fields
    /// rather than one nested [`HirExpr::Call`]: a call node would evaluate the
    /// callee at the wrong time on every backend that lowered it faithfully.
    TaskSpawn {
        /// Which body this task runs.
        target: TaskTarget,
        /// The evaluated arguments, in the target's parameter order.
        args: Vec<HirExprId>,
        /// The handle type, carrying what joining it yields.
        ty: Type,
    },
    /// `handle.await` — drive the task to completion and take its result.
    TaskJoin {
        /// The handle being joined.
        handle: HirExprId,
        /// The joined value's type.
        ty: Type,
    },
    /// `handle.detach()` — drive the task and discard its result.
    TaskDetach {
        /// The handle being detached.
        handle: HirExprId,
    },
    /// `handle.requestCancel()` — ask a task that has not run yet not to.
    TaskCancel {
        /// The handle being cancelled.
        handle: HirExprId,
    },
    /// A call routed through the host's main-thread event loop.
    ///
    /// The target is a named `@MainThread` function. Arguments are evaluated
    /// on the requesting context and copied into an owned state tree before
    /// the host sees them, so no VM heap or native local is shared between
    /// threads.
    MainThreadCall {
        /// The scheduling operation requested by the source.
        operation: MainThreadOp,
        /// The target function.
        function: FuncId,
        /// The evaluated call arguments, including a method receiver when the
        /// source used a method call.
        args: Vec<HirExprId>,
        /// The expression's result type. For `spawn`, this is the distinct
        /// main-thread task handle type.
        ty: Type,
    },
    /// Join a handle returned by `MainThread.spawn`.
    MainThreadJoin {
        /// The main-thread task handle.
        handle: HirExprId,
        /// The value returned by the target function.
        ty: Type,
    },
    /// A placeholder for an expression that failed to analyze.
    Error,
}

/// Which body a spawned task runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskTarget {
    /// `Task { 41 }` — the body is the literal value the spawn already
    /// computed, so driving the task just hands it back.
    Value,
    /// `Task { work(a, b) }` — the body is a call to a named function.
    Call(FuncId),
}

/// Which machine operation a scalar [`HirExpr::Convert`] performs.
///
/// The numeric kinds cover the two runtime representations (`Int` is `i64`,
/// `Float` is `f64`); the pointer-word kinds retag the VM's `Int` and `RawPtr`
/// values without changing their 64 bits. The kind is fixed at analysis, so
/// nothing below re-derives it: the VM and every backend read it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertKind {
    /// Integer to integer, any width to any width. An identity copy: widths
    /// share one 64-bit representation, so nothing is truncated or extended.
    IntToInt,
    /// Integer word to opaque pointer word. The VM changes only the value tag.
    IntToRawPtr,
    /// Opaque pointer word to the `U64` integer representation. The VM changes
    /// only the value tag.
    RawPtrToInt,
    /// Float to float (`Float`/`F32`). An identity copy: every float is
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
    /// A 32-bit IEEE-754 pattern read as Kira's 64-bit `Float`.
    Bits32ToFloat,
    /// A `Float` narrowed to its 32-bit IEEE-754 pattern, as a `U32`.
    ///
    /// The inverse of [`Self::Bits32ToFloat`]. Writing a 32-bit float costs a
    /// rounding step the 64-bit [`Self::FloatToBits`] does not have — the
    /// value narrows to `f32` first (round to nearest even, the one IEEE-754
    /// default), and only then are the bits taken.
    FloatToBits32,
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
            // Every one of them takes a `Float` and answers one.
            HirExpr::MathOperation { .. } => Type::FLOAT,
            HirExpr::ScalarText { .. } => Type::String,
            HirExpr::CStringNew { .. }
            | HirExpr::CStringNull
            | HirExpr::CLayoutAddress { .. }
            | HirExpr::ArrayElements { .. } => Type::CBlock,
            HirExpr::ConstantGet { ty, .. }
            | HirExpr::Local { ty, .. }
            | HirExpr::Unary { ty, .. }
            | HirExpr::Binary { ty, .. }
            | HirExpr::Select { ty, .. }
            | HirExpr::Call { ty, .. }
            | HirExpr::Field { ty, .. }
            | HirExpr::ForeignField { ty, .. }
            | HirExpr::ForeignMemberAddress { ty, .. }
            | HirExpr::ForeignElement { ty, .. }
            | HirExpr::ArrayNew { ty, .. }
            | HirExpr::EnumPayload { ty, .. }
            | HirExpr::NativeState { ty, .. }
            | HirExpr::NativeRecover { ty, .. }
            | HirExpr::Convert { ty, .. }
            | HirExpr::FileSystem { ty, .. }
            | HirExpr::Compiler { ty, .. }
            | HirExpr::Env { ty, .. }
            | HirExpr::TaskSpawn { ty, .. }
            | HirExpr::TaskJoin { ty, .. }
            | HirExpr::MainThreadCall { ty, .. }
            | HirExpr::MainThreadJoin { ty, .. }
            | HirExpr::CellNew { ty, .. }
            | HirExpr::CellNull { ty }
            | HirExpr::CellGet { ty, .. }
            | HirExpr::StringOperation { ty, .. }
            | HirExpr::Index { ty, .. } => *ty,
            HirExpr::StructNew { struct_id, .. } => Type::Struct(*struct_id),
            HirExpr::EnumNew { enum_id, .. } => Type::Enum(*enum_id),
            // `.count` and a tag read are both `Int`; `.append` yields nothing.
            // None has a type that can vary, so none carries one.
            HirExpr::ArrayLen { .. }
            | HirExpr::StringLen { .. }
            | HirExpr::StringCharAt { .. }
            | HirExpr::StringIndexOf { .. }
            | HirExpr::EnumTag { .. } => Type::INT,
            HirExpr::StringSubstring { .. } | HirExpr::StringOf { .. } => Type::String,
            HirExpr::NativeUserData { .. } => Type::RawPtr,
            HirExpr::IntoAny { .. } => Type::Any,
            HirExpr::Widen { to, .. } => *to,
            HirExpr::ArrayAppend { .. }
            | HirExpr::NativeStateFree { .. }
            | HirExpr::TaskDetach { .. }
            | HirExpr::TaskCancel { .. } => Type::Void,
            HirExpr::Error => Type::Error,
        }
    }
}
