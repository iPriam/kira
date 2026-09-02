//! The Kira VM instruction set and its byte encoding.
//!
//! The interpreter executes the decoded [`Instruction`] form (match-in-loop),
//! while [`encode`]/[`decode`] provide the on-the-wire byte format. Opcodes are
//! Kira-owned, closed tags with explicit `u8` discriminants; the encoding is
//! **append-only** — new instructions take the next free opcode and existing
//! ones never move.
//!
//! Jump targets are absolute instruction indices within a function's code, so
//! the interpreter sets its program counter directly with no offset math.
//!
//! This module owns the *definitions* — the instruction set, the place-path
//! types, and the opcode constants. [`codec`] owns the only code that turns
//! them into bytes and back, so the append-only contract has one place to be
//! honored on each side.

mod codec;

pub(crate) use codec::decode_legacy;
pub use codec::{DecodeError, decode, encode, encode_one};

/// The deferred-task primitives, re-exported so an instruction names them from
/// the one place the executor defines them.
pub use kira_runtime_abi::TaskPrim;
pub use kira_runtime_abi::{CompilerOp, EnvOp, FileSystemOp, MathOp, StringOp};
use kira_runtime_abi::{ForeignType, MainThreadOp};

/// One decoded VM instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// Push an integer constant.
    ConstInt(i64),
    /// Push a floating-point constant.
    ConstFloat(f64),
    /// Push a boolean constant.
    ConstBool(bool),
    /// Push a fresh heap string cloned from the module's string pool.
    ConstStr(u64),
    /// Push the unit value.
    ConstVoid,
    /// Push a copy of local slot `n` (strings are cloned).
    LoadLocal(u64),
    /// Push local slot `n`, leaving unit behind.
    ///
    /// A value that runs a user `Drop` is never copied — binding one moves — so
    /// reading it *takes* it: the slot no longer holds anything, and the frame
    /// release must not run a body the value's new owner will run.
    TakeLocal(u64),
    /// Pop the stack top into local slot `n`, dropping the slot's old value.
    StoreLocal(u64),
    /// Push a copy of module-constant slot `n` (strings are cloned).
    ///
    /// The host filled every constant slot — each by one call of its init
    /// function, front to back in the module's table order — before the
    /// entrypoint ran, so a read never observes an empty slot.
    LoadConstant(u64),
    /// Pop and drop the stack top.
    Pop,
    /// Integer negation.
    NegInt,
    /// Float negation.
    NegFloat,
    /// Boolean negation.
    Not,
    /// Integer addition.
    AddInt,
    /// Integer subtraction.
    SubInt,
    /// Integer multiplication.
    MulInt,
    /// Integer division, signed (truncating; traps on divide-by-zero).
    DivInt,
    /// Integer remainder, signed (traps on divide-by-zero).
    RemInt,
    /// Integer division, unsigned (traps on divide-by-zero).
    ///
    /// Emitted for the `U8`..`U64` spellings. Unlike the signed form this
    /// cannot overflow, so it has no `MIN / -1` special case.
    DivUInt,
    /// Integer remainder, unsigned (traps on divide-by-zero).
    RemUInt,
    /// Float addition.
    AddFloat,
    /// Float subtraction.
    SubFloat,
    /// Float multiplication.
    MulFloat,
    /// Float division.
    DivFloat,
    /// Pop two `Float`s and push the truncated remainder of the first by the
    /// second.
    ///
    /// Truncated, not floored: the sign follows the dividend, which is what
    /// `fmod` does and what the language pins.
    RemFloat,
    /// String concatenation.
    ConcatStr,
    /// Integer equality.
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
    /// Integer less-than, unsigned.
    LtUInt,
    /// Integer less-or-equal, unsigned.
    LeUInt,
    /// Integer greater-than, unsigned.
    GtUInt,
    /// Integer greater-or-equal, unsigned.
    GeUInt,
    /// Bitwise AND on the raw 64-bit pattern.
    BitAnd,
    /// Bitwise OR on the raw 64-bit pattern.
    BitOr,
    /// Bitwise XOR on the raw 64-bit pattern.
    BitXor,
    /// Left shift; the shift amount is taken modulo 64.
    Shl,
    /// Arithmetic (sign-propagating) right shift; amount modulo 64.
    ShrInt,
    /// Logical (zero-filling) right shift; amount modulo 64.
    ShrUInt,
    /// Bitwise complement on the raw 64-bit pattern.
    BitNot,
    /// Float equality.
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
    /// Pop a value; push it erased, carrying the type it had on the way in.
    ///
    /// The immediate is the `ErasedTypeId` word of that type. Carrying it is
    /// the whole of this instruction's purpose: the VM's values are tagged
    /// already, so erasure would otherwise be free — and was, before `EqAny`
    /// needed to tell a `Point` from a `Rect` whose fields happen to match.
    Erase(u64),
    /// Pop two erased values; push whether they are structurally equal.
    ///
    /// Both operands are dropped, as every comparison here drops what it
    /// consumed. Values of different kinds are unequal rather than a trap:
    /// `Any` is the one type whose operands are not known to agree statically,
    /// so a mismatch is an ordinary answer.
    EqAny,
    /// Pop two erased values; push whether they are structurally unequal.
    NeAny,
    /// Unconditional jump to an absolute instruction index.
    Jump(u64),
    /// Pop a boolean; jump to an absolute index when it is `false`.
    JumpIfFalse(u64),
    /// Call the function at the given index; arguments are already on the stack.
    Call(u64),
    /// Call the *native* function with the given program-wide id; arguments are
    /// already on the stack, and the result is pushed.
    ///
    /// Emitted only by a hybrid build, where the callee's body lives in the
    /// native half rather than in this module's function table. The VM does not
    /// perform the call itself — it asks the embedder, which keeps the VM free
    /// of any FFI and still able to compile for wasm.
    CallNative(u32),
    /// Call the foreign C function with the given foreign-import id; arguments
    /// are already on the stack, and the result is pushed.
    ///
    /// The id indexes the module's foreign-import table, whose row carries the
    /// exact-width signature the VM marshals arguments to. Like
    /// [`Instruction::CallNative`], the VM performs no FFI itself: it converts
    /// its values to borrowed foreign arguments and asks the embedder through
    /// `HostCapabilities::call_foreign`, so the VM stays free of any
    /// dynamic-loading dependency and still compiles for wasm.
    CallForeign(u32),
    /// Call the mutating method at the given index, then write its final
    /// receiver back through a place in the caller's frame.
    ///
    /// The arguments — the receiver copy first, then the rest — are already on
    /// the stack, exactly as [`Instruction::Call`]; any array indices the place
    /// needs are pushed *after* them, per the [`PlacePath`] convention. The
    /// callee runs like any other, but on return the runtime moves its slot 0
    /// (the mutated receiver) into `slot` walked by `path` in the caller,
    /// dropping whatever was there, before pushing the call's result. An empty
    /// path writes the caller's local slot itself — the `g.mutate()` case.
    CallMut {
        /// The function index to call.
        func: u64,
        /// The caller-frame local slot the writeback place is rooted at.
        slot: u64,
        /// Steps to walk to the writeback location; may be empty.
        path: PlacePath,
    },
    /// Call the function at the given index, then write one or more of its
    /// final parameter slots back through places in the caller's frame.
    ///
    /// The general form of [`Instruction::CallMut`], which is the single-target
    /// case with the target fixed at callee slot 0. The arguments are already on
    /// the stack, exactly as [`Instruction::Call`]; each target's place indices
    /// are pushed after them, targets in order and indices outermost first, per
    /// the [`PlacePath`] convention. On return the runtime moves each named
    /// callee slot into its caller place, dropping what was there, before
    /// pushing the call's result.
    CallWriteback {
        /// The function index to call.
        func: u64,
        /// Where each written-through parameter lands, in parameter order.
        targets: Vec<WritebackTarget>,
    },
    /// [`Instruction::CallWriteback`], but the callee is on the native engine.
    ///
    /// The engine is in the instruction rather than looked up from the callee,
    /// exactly as [`Instruction::CallNative`] is distinct from
    /// [`Instruction::Call`]: what a call site emits is decided once, at compile
    /// time, by the split — and a runtime that had to consult a table to know
    /// which of two very different call protocols it was about to run would be
    /// deciding it again, from a second source that could disagree.
    ///
    /// The protocol differs from the same-engine form in what "writing back"
    /// means. There is no callee frame whose slots can be moved out of: the two
    /// engines share no heap, so a written-through parameter crosses as a copy
    /// and its final value comes back the way the result does. The runtime
    /// stores that returned value into the caller's place.
    CallNativeWriteback {
        /// The function index to call.
        func: u32,
        /// Where each written-through parameter lands, in parameter order.
        targets: Vec<WritebackTarget>,
    },
    /// Pop a value, box it as opaque callback state, and push its state handle.
    NativeState(u64),
    /// Pop a callback-state handle and push its stable raw userdata token.
    ///
    /// The token owns one reference to the state. A `shared` export reads a
    /// handle a local still owns, so it takes a reference of its own; an owned
    /// export consumes a temporary handle, whose reference becomes the token's.
    NativeUserData {
        /// Whether the handle stays owned by a local after the export.
        shared: bool,
    },
    /// Pop a raw userdata token and push a typed mutable callback-state view.
    NativeRecover(u64),
    /// Pop a callback-state handle or raw token, add one owner, and push unit.
    NativeStateRetain,
    /// Pop a callback-state handle or raw token, remove one owner, and push
    /// unit. The last owner's release destroys the state.
    NativeStateRelease,
    /// Push the null raw pointer.
    ConstRawPtrNull,
    /// Push the address C enters the Kira function this callback entry names.
    ForeignCallback(u32),
    /// Pop a value, format it, emit one output line, and push unit.
    Print,
    /// Return the stack top from the current function.
    Return,
    /// Return unit from the current function.
    ReturnVoid,
    /// Pop `n` values and push a struct holding them, first field deepest.
    ///
    /// The VM is structurally typed: a struct is a tuple of values and this
    /// carries its own arity, so the module needs no struct table and field
    /// names never reach the runtime. The compiler resolves names to indices
    /// and fills every field — defaults included — before emitting this.
    NewStruct(u64),
    /// Pop `fields` values and push a struct holding them, recording the
    /// function that runs its type's user `Drop` body.
    ///
    /// The VM is structurally typed, so a heap object cannot be asked what type
    /// it is when the last holder goes — and that is exactly the moment the
    /// body has to run. So the answer travels with the construction, which is
    /// the one place the type is known.
    ///
    /// A separate instruction rather than an operand on [`Instruction::NewStruct`]
    /// because every struct in every program that declares no `Drop` would
    /// otherwise carry eight bytes saying so.
    NewStructDropping {
        /// How many field values to pop, first field deepest.
        fields: u64,
        /// The function running the type's user `Drop` body.
        glue: u32,
    },
    /// Pop `order.len()` values pushed in *evaluation* order and push a struct
    /// holding them in *declaration* order.
    ///
    /// A literal that writes its fields out of declaration order evaluates
    /// them as written — the language's evaluation-order rule — while the
    /// struct stores them as declared. The value pushed `i`-th (deepest
    /// first) lands in field `order[i]`. Only a literal that reorders emits
    /// this; a construction in declaration order stays on
    /// [`Instruction::NewStruct`] / [`Instruction::NewStructDropping`].
    NewStructOrdered {
        /// The declared field index of each popped value, deepest first: a
        /// permutation of `0..order.len()`.
        order: Vec<u64>,
        /// The function running the type's user `Drop` body, when it has one.
        glue: Option<u32>,
    },
    /// Signed 64-bit addition that traps on overflow.
    ///
    /// Ordinary arithmetic in the language traps rather than wraps; the
    /// wrapping [`Instruction::AddInt`] serves `wrappingAdd` and the shifts.
    AddIntChecked,
    /// Signed 64-bit subtraction that traps on overflow.
    SubIntChecked,
    /// Signed 64-bit multiplication that traps on overflow.
    MulIntChecked,
    /// Signed 64-bit negation that traps on `i64::MIN`.
    NegIntChecked,
    /// Signed division that traps on a zero divisor and on `MIN / -1`.
    DivIntChecked,
    /// Unsigned 64-bit addition that traps on overflow.
    AddUIntChecked,
    /// Unsigned 64-bit subtraction that traps below zero.
    SubUIntChecked,
    /// Unsigned 64-bit multiplication that traps on overflow.
    MulUIntChecked,
    /// Traps unless the integer on top of the stack lies in the range of the
    /// spelling named by the code (see `IntSpelling::code`), read as a signed
    /// 64-bit result. Emitted after arithmetic at a narrower width.
    CheckInt(u8),
    /// Reduces the integer on top of the stack to the width of the spelling
    /// named by the code, the way a shift discards bits.
    WrapInt(u8),
    /// Traps unless the shift count on top of the stack lies in `0..bits`.
    CheckShift(u8),
    /// Converts the integer on top of the stack from the spelling `from` to
    /// the spelling `to` (both codes), trapping when the value is not
    /// representable at the destination.
    ConvertInt {
        /// The source spelling's code.
        from: u8,
        /// The destination spelling's code.
        to: u8,
    },
    /// Unsigned 64-bit integer to float, round to nearest ties to even.
    ConvertUIntToFloat,
    /// Pop a `U64` word and print it as the unsigned value it is.
    PrintUnsigned,
    /// Pop a `U64` word and push its unsigned decimal text.
    StringOfUnsigned,
    /// Pop an erased `Any` and push whether its runtime type identity is the
    /// operand's; the value is released.
    TypeTest(u64),
    /// Pop an erased `Any` whose runtime type identity must be the operand's,
    /// and push its payload as an owned value; any other identity traps.
    Downcast(u64),
    /// Pop a struct, push a copy of field `n`, and drop the struct.
    GetField(u64),
    /// Pop a pointer word and push it advanced by `offset` bytes.
    ///
    /// The one instruction that forms an address into memory Kira does not own.
    /// It exists so a C callback's `const T*` argument can have a member taken
    /// without a C accessor per field; the offset is resolved from the target's
    /// C layout at compile time, and the pointer comes from the foreign seam,
    /// never from Kira arithmetic. A member whose bytes live inside the
    /// container names a place, so what a read of it produces is an address
    /// rather than a value — reading *through* that address is
    /// [`Instruction::ForeignLoad`]'s job.
    ForeignOffset(u32),
    /// Pop an index and a pointer word, and push the pointer advanced by
    /// `index * stride` bytes — C's `pointer[index]` as an address.
    ForeignIndex(u32),
    ForeignLoad {
        /// Byte offset of the member within the pointed-to struct.
        offset: u32,
        /// The seam type to read the bytes as.
        ty: ForeignType,
    },
    /// Pop a value and store it into local `slot`, walking `path` field by
    /// field from the slot's struct. The overwritten value is dropped.
    ///
    /// The path is carried in the instruction rather than rebuilt from loads
    /// and stores so a nested write mutates in place: `b.size.x = 1` costs one
    /// instruction and no copy of `b`.
    StoreField {
        /// The local slot the place is rooted at.
        slot: u64,
        /// Field indices to walk, outermost first; never empty.
        path: FieldPath,
    },
    /// Pop `n` values and push an array holding them, first element deepest.
    ///
    /// The element count is a bytecode-owned `u64`.
    NewArray(u64),
    /// Pop an index, pop an array, push a copy of that element, and drop the
    /// array.
    ///
    /// Traps on a negative index and, separately, on one past the end — two
    /// distinct traps, because they are two distinct mistakes.
    ArrayGet,
    /// Pop an array, push its element count as an `Int`, and drop the array.
    ArrayLen,
    /// Pop a `Float`, push its IEEE-754 bit pattern as an `Int`.
    ///
    /// A reinterpretation, not a conversion: nothing rounds, nothing saturates,
    /// and a NaN keeps its exact payload — which is what makes it usable for
    /// serializing a float byte for byte.
    ConvertFloatToBits,
    /// Pop an `Int`, push the `Float` its bits denote. The exact inverse of
    /// [`Instruction::ConvertFloatToBits`].
    ConvertBitsToFloat,
    /// Pop a `U32`, read it as a 32-bit IEEE-754 float, push it as a `Float`.
    ///
    /// Kira's `Float` is 64-bit, so a 32-bit pattern cannot go through
    /// [`Instruction::ConvertBitsToFloat`] — the same bits mean a different
    /// number at the two widths. Binary data is full of 32-bit floats, and
    /// reading one is otherwise a hand-written decode of sign, exponent, and
    /// mantissa.
    ConvertBits32ToFloat,
    /// Pop a `Float`, narrow it to a 32-bit IEEE-754 float, push those bits as
    /// a `U32`.
    ///
    /// The inverse of [`Instruction::ConvertBits32ToFloat`], and not the same
    /// as [`Instruction::ConvertFloatToBits`] followed by a truncation: the
    /// value rounds to nearest even at 32 bits before its pattern is taken.
    ConvertFloatToBits32,
    /// Pop an `Int` index, push a copy of that element of the array in `slot`.
    ///
    /// The same answer as [`Instruction::LoadLocal`] followed by
    /// [`Instruction::ArrayGet`], without the copy of the whole array that pair
    /// makes. Reading one element cost the whole array before this, so a loop
    /// over `n` elements cost `O(n²)`.
    ArrayGetLocal(u64),
    /// Pop three `Int` operands (last pushed is the third), carry out one task
    /// primitive, and push its `Int` answer.
    ///
    /// The whole async spine reaches the VM through this one instruction: the
    /// scheduler itself is generated Kira, so what the interpreter implements
    /// is the task *table*, not the policy. Its native mirror is
    /// `kira_rt_task_op`, called with the same four numbers.
    TaskOp(TaskPrim),
    /// Pop `args` values, request `function` on the host main-thread loop, and
    /// push the operation's result or handle.
    MainThreadCall {
        /// The host operation to perform.
        operation: MainThreadOp,
        /// The target function index.
        function: u64,
        /// Number of values to take from the operand stack.
        args: u64,
    },
    /// Pop a main-thread task handle, join it through the host, and push its
    /// result.
    MainThreadJoin,
    /// Marks the entry function as owning the process main-thread lifecycle.
    ///
    /// Metadata and a runtime no-op. It must be instruction zero of the module
    /// entrypoint and may occur nowhere else.
    MainThreadLifecycle,
    /// Pop a string, push its length in bytes as an `Int`, and drop the string.
    ///
    /// Bytes, not characters: `charAt` and `substring` index the same units, so
    /// a character count would disagree with the primitives it sits beside.
    StringLen,
    /// Pop an index and a string, push the byte at that index as an `Int`, and
    /// drop the string.
    ///
    /// Traps when the index is outside `0 ..< len`, so an out-of-range read
    /// fails the same way on every engine instead of producing a value.
    StringCharAt,
    /// Pop an end, a start, and a string; push the half-open byte slice as a
    /// fresh string and drop the original.
    ///
    /// Traps when `start > end` or either bound is outside `0 ..= len`.
    StringSubstring,
    /// Pop a needle and a haystack, push the byte index of the first occurrence
    /// as an `Int` (or `-1`), and drop both.
    StringIndexOf,
    /// Pop a value, push its text rendering as a fresh string, and drop the
    /// value.
    ///
    /// The rendering `print` gives, so a value printed and a value converted
    /// never disagree.
    StringOf,
    /// Pop a string, push a pointer word to a copy of its bytes in C storage
    /// that is never released, and drop the string.
    ///
    /// See [`kira_runtime_abi::c_storage`]: a `CString` member of a struct C
    /// keeps is read long after the call that handed it over, so the only
    /// pointer that is safe there is one nothing frees.
    CStringNew,
    /// Pop a struct, write its C-layout image into storage that is never
    /// released, and push that storage's address as a pointer word.
    ///
    /// The operand names the aggregate row describing the layout. See
    /// [`kira_runtime_abi::c_storage`] for why the image outlives the call.
    CLayoutAddress(u32),
    /// Perform one file-system operation through the host.
    ///
    /// The operands are popped in reverse source order — the last argument is on
    /// top — and dropped, and the operation's own result is pushed. Which
    /// operands there are, and what the result is, follow from the
    /// [`FileSystemOp`]: it is the whole instruction, not a hint.
    ///
    /// A file that is missing, a directory that will not open, a write that is
    /// refused: none of those is a trap. Each is an ordinary value — `false`, an
    /// empty array, a zero — because a program has to be able to ask the outside
    /// world a question and hear no. The one failure is a host with no
    /// filesystem, which is a build-time mistake surfacing late.
    FileSystem(FileSystemOp),
    /// Perform one string operation.
    ///
    /// The same shape as [`Instruction::FileSystem`]: the receiver and any
    /// arguments are popped in reverse source order and dropped, the result is
    /// pushed, and which operands there are follows from the [`StringOp`]
    /// alone. Unlike a file-system request this reaches no host — text is the
    /// VM's own, so the work happens here.
    ///
    /// Sharing one opcode is deliberate. The four string primitives that came
    /// first each took a number of their own, and the opcode space is one byte
    /// wide; a language that means to keep growing its string surface cannot
    /// keep paying that. See [`StringOp`] for the numbering, which is
    /// append-only.
    StringOp(StringOp),
    /// Pop an array, push the address of a C buffer holding its elements as
    /// `ty`.
    ///
    /// The storage outlives the call, for the reason a `CString` member's does:
    /// nothing on this side knows whether the callee kept the pointer.
    ArrayElements(ForeignType),
    /// Pop a code point, push the text of that one Unicode scalar.
    ScalarText,
    /// Pop a float, push the result of one floating-point operation on it.
    ///
    /// One opcode for every operation, told apart by the operand byte, for the
    /// reason [`StringOp`] gives: the opcode space is one byte and the maths
    /// surface will keep growing.
    MathOp(MathOp),
    /// Perform one compiler operation through the host.
    ///
    /// The same shape as [`Instruction::FileSystem`], for the same reason: the
    /// operands are popped in reverse source order and dropped, the operation's
    /// result is pushed, and which operands there are follows from the
    /// [`CompilerOp`] alone.
    ///
    /// A package that does not compile is not a trap — its diagnostics are the
    /// result. The one failure is a host with no compiler, which the VM cannot
    /// have of its own: it sits below one.
    Compiler(CompilerOp),
    /// One environment read, its operation in the byte that follows.
    ///
    /// The same shape as [`Instruction::Compiler`]: the name is popped, the
    /// answer is pushed, and an unset variable is an answer rather than a trap.
    Env(EnvOp),
    /// Pop a value and store it through a place that may index arrays.
    ///
    /// The general form of [`Instruction::StoreField`], which stays for the
    /// all-fields case it already encodes. A field index is an immediate; an
    /// array index is not knowable until the program runs, so it arrives on the
    /// stack: **every `Index` step's value is pushed first, outermost to
    /// innermost, then the value to store.** The runtime pops the value, then
    /// the indices, which come off innermost-first.
    StorePlace {
        /// The local slot the place is rooted at.
        slot: u64,
        /// Steps to walk, outermost first; never empty.
        path: PlacePath,
    },
    /// Pop a value and append it to the array a place names.
    ///
    /// Same stack protocol as [`Instruction::StorePlace`]: indices first, then
    /// the value. An empty path appends to the slot's own array, which is what
    /// `xs.append(v)` compiles to.
    ArrayAppend {
        /// The local slot the place is rooted at.
        slot: u64,
        /// Steps to walk, outermost first; may be empty.
        path: PlacePath,
    },
    /// Push an enum value: a variant `tag`, taking a payload off the stack when
    /// `has_payload` is set.
    ///
    /// The VM is structurally typed, so this carries only what the runtime
    /// needs to build the box — the discriminant, and whether one value on the
    /// stack belongs to it. The payload is an ordinary [`crate::op`] value, so a
    /// string payload is a heap handle the enum takes ownership of, exactly as a
    /// struct field does.
    NewEnum {
        /// The variant's declaration index — its discriminant.
        tag: u64,
        /// Whether a payload value sits on top of the stack for this variant.
        has_payload: bool,
    },
    /// Pop an enum, push its discriminant `tag` as an `Int`, and drop the enum.
    EnumTag,
    /// Pop an enum, push an owned copy of its payload, and drop the enum.
    ///
    /// Emitted only inside a `match` arm whose tag test already selected the
    /// variant, so the payload is known to be present. The pushed value is
    /// independent of the box — a `String` payload is cloned — which is what
    /// lets the arm's binding outlive the enum.
    EnumPayload,
    /// Pop a value and push a fresh capture cell holding it.
    ///
    /// The value moves into the box: whatever it owned is the box's now. One
    /// cell per execution of this instruction, which is what makes a `var`
    /// declared inside a loop a fresh binding each time round.
    NewCell,
    /// Push an owned copy of what the cell in a local slot holds.
    ///
    /// Rooted at a slot rather than the stack so a read does not have to take —
    /// and then drop — a share of the handle just to look inside it, the same
    /// reason [`Instruction::ArrayGetLocal`] exists.
    CellGet(u64),
    /// Pop a value and store it in the cell a local slot holds, releasing
    /// whatever was there.
    ///
    /// **One instruction, not two.** A separate drop and store would leave a
    /// freed handle in the box for the window between them, and a trap in that
    /// window leaves it there for good.
    CellSet(u64),
    /// Pop an `Int`, push it as a `Float` (signed, round to nearest ties even).
    ///
    /// Emitted for a scalar conversion `Float(intValue)`. The integer-to-integer
    /// and float-to-float conversions emit *no* instruction — they are identity
    /// copies over one representation — so only the two cross-representation
    /// conversions have an opcode.
    ConvertIntToFloat,
    /// Pop a `Float`, push it as an `Int`: truncate toward zero, saturating an
    /// out-of-range value to `i64::MIN`/`i64::MAX` and mapping NaN to zero.
    ///
    /// Emitted for a scalar conversion `Int(floatValue)`. Never traps — the
    /// conversion is total over every float input.
    ConvertFloatToInt,
    /// Pop an `Int`, push the same 64-bit word as a `RawPtr`.
    ConvertIntToRawPtr,
    /// Pop a `RawPtr`, push the same 64-bit word as an `Int`.
    ConvertRawPtrToInt,
}

/// One step of a [`PlacePath`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStep {
    /// Walk into the field at this index.
    Field(u64),
    /// Walk into an array element, whose index is on the operand stack.
    Index,
}

/// The wire tag for each [`PathStep`]. Append-only, like every other tag.
mod step_tag {
    pub const FIELD: u8 = 0x00;
    pub const INDEX: u8 = 0x01;
}

/// A place path inside a [`Instruction::StorePlace`] or
/// [`Instruction::ArrayAppend`].
///
/// The generalization of [`FieldPath`]: a step is a constant field index or a
/// stack-supplied array index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlacePath {
    steps: Vec<PathStep>,
}

impl PlacePath {
    /// Builds a path.
    pub fn new(steps: Vec<PathStep>) -> Self {
        Self { steps }
    }

    /// The steps to walk, outermost first.
    pub fn steps(&self) -> &[PathStep] {
        &self.steps
    }

    /// How many steps the path walks.
    pub fn len(&self) -> u64 {
        self.steps.len() as u64
    }

    /// Whether the path walks no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// How many of the steps take their index from the stack.
    ///
    /// This is exactly how many values the runtime pops after the stored one,
    /// which is what makes the stack protocol checkable rather than assumed.
    pub fn index_count(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| matches!(step, PathStep::Index))
            .count()
    }
}

/// One target of an [`Instruction::CallWriteback`]: which callee parameter is
/// written back, and where in the caller it lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritebackTarget {
    /// The callee's local slot — its parameter — whose final value is moved out.
    pub param: u64,
    /// The caller-frame local slot the place is rooted at.
    pub slot: u64,
    /// Steps to walk to the writeback location; may be empty.
    pub path: PlacePath,
}

/// A field path inside a [`Instruction::StoreField`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldPath {
    steps: Vec<u64>,
}

impl FieldPath {
    /// Builds a field path.
    pub fn new(steps: Vec<u64>) -> Self {
        Self { steps }
    }

    /// The steps to walk, outermost first.
    pub fn steps(&self) -> &[u64] {
        &self.steps
    }

    /// How many steps the path walks.
    pub fn len(&self) -> u64 {
        self.steps.len() as u64
    }

    /// Whether the path walks no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

mod opcode;

#[cfg(test)]
#[path = "op_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "op_main_thread_tests.rs"]
mod main_thread_tests;

#[cfg(test)]
#[path = "op_legacy_tests.rs"]
mod legacy_tests;
