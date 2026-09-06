//! IR expressions: the node set every backend switches on.
//!
//! Split from the program and statement model on the file-size ladder, the way
//! `kira-semantics-model` splits its HIR. The cut is the one that keeps a
//! reader's question answerable in one file: what an expression node *is* lives
//! here, and what a program is made of lives beside it.
//!
//! [`IrProgram::expr_type`](super::IrProgram::expr_type) stays with the program
//! deliberately — it resolves a local against the enclosing function's slot
//! types, so it is a question about a program, not about a node.

use kira_runtime_abi::{
    ChannelPrim, CompilerOp, EnvOp, FileSystemOp, MainThreadOp, NativeStateTypeId, TaskPrim,
};
use kira_semantics_model::hir::FieldOrder;
use kira_semantics_model::{EnumId, StructId, Type};

use super::{ConvertKind, IrBinOp, IrExprId, IrPlace, IrUnOp, IrWriteback};

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
    /// A read of a module constant's global slot.
    ///
    /// The slot indexes [`IrProgram::constants`]; the backend's init sequence
    /// filled it before `main` ran, so a read copies the stored value.
    ///
    /// [`IrProgram::constants`]: super::IrProgram::constants
    ConstantGet {
        /// The constant's index in the program's evaluation-ordered table.
        constant: u32,
        /// The constant's type.
        ty: Type,
    },
    /// A unary operation.
    Unary {
        /// The operator.
        op: IrUnOp,
        /// The operand.
        operand: IrExprId,
        /// The result type, carrying the integer width the operation is
        /// checked at.
        ty: Type,
    },
    /// `value is Type`: whether the erased value's runtime identity is
    /// `target`'s. The value is consumed.
    TypeTest {
        /// The `Any` being asked.
        value: IrExprId,
        /// The erased identity tested for.
        target: kira_semantics_model::ErasedTypeId,
    },
    /// `value as Type`: the payload of an erased value whose identity is
    /// `target`'s, owned; any other identity traps.
    TypeCast {
        /// The `Any` being unboxed.
        value: IrExprId,
        /// The erased identity expected.
        target: kira_semantics_model::ErasedTypeId,
        /// The type the payload is read as.
        ty: Type,
    },
    /// A binary operation.
    Binary {
        /// The operator.
        op: IrBinOp,
        /// Left operand.
        lhs: IrExprId,
        /// Right operand.
        rhs: IrExprId,
        /// The result type, carrying the integer width the operation is
        /// checked at.
        ty: Type,
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
        ///
        /// Each expression produces an owned temporary consumed by this call.
        /// A user callee receives that ownership unless its parameter is a
        /// borrow; a foreign adapter borrows only for the duration of the C
        /// call, so the calling engine must reclaim every temporary after the
        /// adapter returns. Backends may optimize a borrow into a pointer, but
        /// may not silently let an evaluated argument escape ownership.
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
        /// The sequence the initializers are evaluated in.
        order: FieldOrder,
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
    /// Boxing a value into a fresh capture cell.
    ///
    /// The one producer of a [`Type::Cell`]: a `var` a closure captures is
    /// stored in one instead of inline, so the closure and the frame it was
    /// written in name the same storage. The value moves in.
    CellNew {
        /// The value moving into the box.
        value: IrExprId,
        /// The cell type produced.
        ty: Type,
    },
    /// A null capture-cell slot used by closure-representation padding.
    CellNull {
        /// The cell type represented by the null slot.
        ty: Type,
    },
    /// An **owned** read of what the capture cell in a local slot holds.
    ///
    /// Rooted at a slot rather than an expression: every cell lives in one, and
    /// naming the slot is what keeps a read from taking and dropping a share of
    /// the handle just to look inside it.
    CellGet {
        /// The slot holding the cell.
        slot: u32,
        /// The type inside the cell — the type of this expression.
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
    /// The address of a member whose bytes live inside the container.
    ///
    /// A nested struct or an inline array names a place, so there is nothing to
    /// load: the address is the container's plus the member's offset.
    ForeignMemberAddress {
        /// The pointer-typed expression being read through.
        base: IrExprId,
        /// The container's row in the program's C-layout aggregate table.
        aggregate: kira_runtime_abi::ForeignAggregateId,
        /// The member's index in declaration order.
        member: u32,
        /// The pointer type the address has.
        ty: Type,
    },
    /// The address of one element of a C array, by pointer arithmetic.
    ForeignElement {
        /// The pointer being indexed.
        base: IrExprId,
        /// The element's row in the program's C-layout aggregate table.
        aggregate: kira_runtime_abi::ForeignAggregateId,
        /// The element index.
        index: IrExprId,
        /// The pointer type the address has.
        ty: Type,
    },
    /// A member read through an `@FFI.Pointer`: a load from C memory.
    ///
    /// Carries the aggregate and member index rather than a byte offset: the
    /// offset depends on the target's pointer width, so each backend asks the
    /// aggregate table for the width it emits for.
    ForeignField {
        /// The pointer-typed expression being read through.
        base: IrExprId,
        /// The target's row in the program's C-layout aggregate table.
        aggregate: kira_runtime_abi::ForeignAggregateId,
        /// The member's index in declaration order.
        member: u32,
        /// The Kira type the member reads as.
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
    /// One of the string operations that share an opcode. See
    /// [`kira_semantics_model::hir::HirExpr::StringOperation`].
    /// The address of a C buffer holding an array's elements.
    ArrayElements {
        /// The array whose elements are written out.
        value: IrExprId,
        /// The seam type each element is written as.
        element: kira_runtime_abi::ForeignType,
    },
    /// The text of one Unicode scalar, from its code point.
    ScalarText {
        /// The code point.
        value: IrExprId,
    },
    /// A floating-point operation the hardware already has.
    MathOperation {
        /// Which operation to perform.
        op: kira_runtime_abi::MathOp,
        /// The values it is performed on, in source order — as many as the
        /// operation's own `argument_count`.
        operands: Vec<IrExprId>,
    },
    StringOperation {
        /// Which operation to perform.
        op: kira_runtime_abi::StringOp,
        /// The string it is performed on.
        text: IrExprId,
        /// Its arguments, in source order.
        arguments: Vec<IrExprId>,
        /// What it answers with.
        ty: Type,
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
    /// One compiler operation, performed by the engine on the host's behalf.
    ///
    /// See [`kira_semantics_model::hir::HirExpr::Compiler`]: an effect no Kira
    /// body can express, so each engine performs it its own way and the node
    /// survives lowering intact.
    Compiler {
        /// Which operation this performs.
        op: CompilerOp,
        /// Its arguments, in source order.
        args: Vec<IrExprId>,
        /// What the operation produces.
        ty: Type,
    },
    /// One environment read, which only the engine can perform.
    Env {
        /// Which operation this performs.
        op: EnvOp,
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
    /// Adds one owner to the callback state a handle or userdata token names.
    NativeStateRetain {
        /// The state handle or raw token.
        token: IrExprId,
    },
    /// Removes one owner from the callback state a handle or userdata token
    /// names. The last owner's release destroys the state.
    NativeStateRelease {
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
    /// A value crossing into the top type, `Any`.
    ///
    /// The two engines answer this very differently, and the difference is the
    /// design rather than a gap. The VM's `Value` is already a tagged union,
    /// but its box accounting is not: the bytecode compiler emits an `Erase`
    /// instruction that boxes the payload under the erased type id, which is
    /// what lets two erased structs of different declarations compare
    /// unequal on the VM exactly as they do on native. Native code is
    /// statically typed and has nowhere to put the tag, so the LLVM backend
    /// boxes — a tag, what the payload owns, and the payload word — and that
    /// box is what an `Any` is on that side.
    ///
    /// What both engines guarantee is the same observable behavior: an erased
    /// value copies and drops exactly as the value it erased did, so a program
    /// that balances on one balances on the other.
    IntoAny {
        /// The value being erased.
        value: IrExprId,
        /// The type it had before erasure, which is what says what it owns.
        from: Type,
        /// The runtime identity the box carries.
        ///
        /// Fixed at lowering rather than derived by a backend from `from`,
        /// because the two answer different questions: `from` is the machine
        /// form of the payload and is rewritten when a `distinct` becomes its
        /// representation, while this is what the language says the value *is*
        /// and must survive that rewrite.
        tag: kira_semantics_model::ErasedTypeId,
    },
    /// `value.type` where the value's type is known: the descriptor of that
    /// type, after evaluating and releasing the value for its effects.
    TypeConst {
        /// The value, evaluated and released.
        value: IrExprId,
        /// The identity its type was interned under.
        id: kira_semantics_model::ErasedTypeId,
    },
    /// `value.type` on an `Any`: the identity the erasure box carries.
    TypeOf {
        /// The `Any` being asked, consumed by the read.
        value: IrExprId,
    },
    /// `try value as Type`: the cast as a result a handler can answer.
    ///
    /// Builds `Ok(payload)` when the box holds `target` and
    /// `Error(Mismatch(type))` when it does not, so a failed cast is a value
    /// rather than a trap. Both engines lower it as a branch over the same box
    /// tag [`IrExpr::TypeTest`] reads.
    TypeCastResult {
        /// The `Any` being unboxed, consumed either way.
        value: IrExprId,
        /// The identity the payload must carry.
        target: kira_semantics_model::ErasedTypeId,
        /// The result row being built.
        result: EnumId,
        /// The failure row the error variant carries.
        failure: EnumId,
        /// The payload type read on the success path.
        payload: Type,
    },
    /// A property of a runtime type descriptor.
    TypeField {
        /// The descriptor being read.
        descriptor: IrExprId,
        /// Which property.
        field: kira_semantics_model::TypeField,
        /// The property's type.
        ty: Type,
    },
    /// A call routed through the host's main-thread event loop.
    MainThreadCall {
        /// The requested scheduling operation.
        operation: MainThreadOp,
        /// The target function in the program's function table.
        function: u32,
        /// Arguments evaluated by the requesting context.
        args: Vec<IrExprId>,
        /// The result type, including `MainThreadTask` for `spawn`.
        ty: Type,
    },
    /// A join of a handle returned by `MainThread.spawn`.
    MainThreadJoin {
        /// The handle expression.
        handle: IrExprId,
        /// The target's result type.
        ty: Type,
    },
    /// One primitive of the deferred-task executor.
    ///
    /// The whole async spine reaches the runtime through this node and nothing
    /// else: `Task { … }`, `.await`, `.detach()`, `.requestCancel()`,
    /// `taskYield()`, and `taskSleep(ms)` all lower to calls to functions
    /// [`crate::lower`] synthesizes, and *those* functions are what hold these.
    /// The scheduling policy is therefore ordinary Kira-shaped IR, which is
    /// what makes the VM and the native backend agree on it by construction
    /// rather than by two implementations being kept in step.
    ///
    /// Every primitive takes three `Int` operands and yields one. Operands a
    /// primitive does not use are the constant `0`.
    TaskOp {
        /// Which primitive this is.
        prim: TaskPrim,
        /// Its three operands, in order.
        operands: [IrExprId; 3],
    },
    /// One channel-table primitive.
    ///
    /// The channel surface reaches both engines through this node on the same
    /// terms [`IrExpr::TaskOp`] states: the ordering policy is synthesized
    /// Kira-shaped IR, and what a backend carries is the table.
    ///
    /// Every primitive takes three `Int` operands and yields one. Operands a
    /// primitive does not use are the constant `0`.
    ChannelOp {
        /// Which primitive this is.
        prim: ChannelPrim,
        /// Its three operands, in order.
        operands: [IrExprId; 3],
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
