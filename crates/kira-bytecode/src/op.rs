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

pub use codec::{DecodeError, decode, encode, encode_one};
pub use kira_runtime_abi::FileSystemOp;

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
    ConstStr(u32),
    /// Push the unit value.
    ConstVoid,
    /// Push a copy of local slot `n` (strings are cloned).
    LoadLocal(u16),
    /// Pop the stack top into local slot `n`, dropping the slot's old value.
    StoreLocal(u16),
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
    /// Unconditional jump to an absolute instruction index.
    Jump(u32),
    /// Pop a boolean; jump to an absolute index when it is `false`.
    JumpIfFalse(u32),
    /// Call the function at the given index; arguments are already on the stack.
    Call(u32),
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
        func: u32,
        /// The caller-frame local slot the writeback place is rooted at.
        slot: u16,
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
        func: u32,
        /// Where each written-through parameter lands, in parameter order.
        targets: Vec<WritebackTarget>,
    },
    /// Pop a value, box it as opaque callback state, and push its state handle.
    NativeState(u64),
    /// Pop a callback-state handle and push its stable raw userdata token.
    NativeUserData,
    /// Pop a raw userdata token and push a typed mutable callback-state view.
    NativeRecover(u64),
    /// Pop a callback-state handle or raw token, release it, and push unit.
    NativeStateFree,
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
    NewStruct(u16),
    /// Pop a struct, push a copy of field `n`, and drop the struct.
    GetField(u16),
    /// Pop a value and store it into local `slot`, walking `path` field by
    /// field from the slot's struct. The overwritten value is dropped.
    ///
    /// The path is carried in the instruction rather than rebuilt from loads
    /// and stores so a nested write mutates in place: `b.size.x = 1` costs one
    /// instruction and no copy of `b`.
    StoreField {
        /// The local slot the place is rooted at.
        slot: u16,
        /// Field indices to walk, outermost first; never empty.
        path: FieldPath,
    },
    /// Pop `n` values and push an array holding them, first element deepest.
    ///
    /// The count is a `u32` rather than the `u16` [`Instruction::NewStruct`]
    /// uses: a struct's field count is written by hand, but an array literal's
    /// element count is as long as someone cares to make it.
    NewArray(u32),
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
    /// Pop an `Int` index, push a copy of that element of the array in `slot`.
    ///
    /// The same answer as [`Instruction::LoadLocal`] followed by
    /// [`Instruction::ArrayGet`], without the copy of the whole array that pair
    /// makes. Reading one element cost the whole array before this, so a loop
    /// over `n` elements cost `O(n²)`.
    ArrayGetLocal(u16),
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
        slot: u16,
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
        slot: u16,
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
        tag: u16,
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
}

/// One step of a [`PlacePath`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStep {
    /// Walk into the field at this index.
    Field(u16),
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
/// stack-supplied array index. As with `FieldPath`, the length is a `u16` on
/// the wire and the cap lives in the one constructor, so an unencodable path
/// cannot be built and [`encode_one`] never has to truncate one or fail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlacePath {
    steps: Vec<PathStep>,
}

impl PlacePath {
    /// Builds a path, or fails when it is too deep to encode.
    pub fn new(steps: Vec<PathStep>) -> Result<Self, FieldPathTooDeep> {
        if u16::try_from(steps.len()).is_err() {
            return Err(FieldPathTooDeep { count: steps.len() });
        }
        Ok(Self { steps })
    }

    /// The steps to walk, outermost first.
    pub fn steps(&self) -> &[PathStep] {
        &self.steps
    }

    /// How many steps the path walks. Always fits in a `u16`.
    pub fn len(&self) -> u16 {
        // Guaranteed by the only constructor.
        self.steps.len() as u16
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
    pub param: u16,
    /// The caller-frame local slot the place is rooted at.
    pub slot: u16,
    /// Steps to walk to the writeback location; may be empty.
    pub path: PlacePath,
}

/// A field path inside a [`Instruction::StoreField`], short enough to encode.
///
/// The length is a `u16` on the wire, so a path is capped at `u16::MAX` steps.
/// The cap lives in the one constructor and the steps are private, which is
/// what makes [`encode_one`] total: an unencodable path cannot be built, so
/// encoding never has to truncate one and never has to fail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldPath {
    steps: Vec<u16>,
}

/// A field path with more steps than the bytecode format can encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a field path of {count} steps exceeds the bytecode format's 65535")]
pub struct FieldPathTooDeep {
    /// How many steps were requested.
    pub count: usize,
}

impl FieldPath {
    /// Builds a path, or fails when it is too deep to encode.
    pub fn new(steps: Vec<u16>) -> Result<Self, FieldPathTooDeep> {
        if u16::try_from(steps.len()).is_err() {
            return Err(FieldPathTooDeep { count: steps.len() });
        }
        Ok(Self { steps })
    }

    /// The steps to walk, outermost first.
    pub fn steps(&self) -> &[u16] {
        &self.steps
    }

    /// How many steps the path walks. Always fits in a `u16`.
    pub fn len(&self) -> u16 {
        // Guaranteed by the only constructor.
        self.steps.len() as u16
    }

    /// Whether the path walks no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// The opcode byte for each instruction. Append-only: never reorder or reuse.
mod opcode {
    pub const CONST_INT: u8 = 0x01;
    pub const CONST_FLOAT: u8 = 0x02;
    pub const CONST_BOOL: u8 = 0x03;
    pub const CONST_STR: u8 = 0x04;
    pub const CONST_VOID: u8 = 0x05;
    pub const LOAD_LOCAL: u8 = 0x06;
    pub const STORE_LOCAL: u8 = 0x07;
    pub const POP: u8 = 0x08;
    pub const NEG_INT: u8 = 0x09;
    pub const NEG_FLOAT: u8 = 0x0a;
    pub const NOT: u8 = 0x0b;
    pub const ADD_INT: u8 = 0x0c;
    pub const SUB_INT: u8 = 0x0d;
    pub const MUL_INT: u8 = 0x0e;
    pub const DIV_INT: u8 = 0x0f;
    pub const REM_INT: u8 = 0x10;
    pub const ADD_FLOAT: u8 = 0x11;
    pub const SUB_FLOAT: u8 = 0x12;
    pub const MUL_FLOAT: u8 = 0x13;
    pub const DIV_FLOAT: u8 = 0x14;
    pub const CONCAT_STR: u8 = 0x15;
    pub const EQ_INT: u8 = 0x16;
    pub const NE_INT: u8 = 0x17;
    pub const LT_INT: u8 = 0x18;
    pub const LE_INT: u8 = 0x19;
    pub const GT_INT: u8 = 0x1a;
    pub const GE_INT: u8 = 0x1b;
    pub const EQ_FLOAT: u8 = 0x1c;
    pub const NE_FLOAT: u8 = 0x1d;
    pub const LT_FLOAT: u8 = 0x1e;
    pub const LE_FLOAT: u8 = 0x1f;
    pub const GT_FLOAT: u8 = 0x20;
    pub const GE_FLOAT: u8 = 0x21;
    pub const EQ_BOOL: u8 = 0x22;
    pub const NE_BOOL: u8 = 0x23;
    pub const EQ_STR: u8 = 0x24;
    pub const NE_STR: u8 = 0x25;
    pub const JUMP: u8 = 0x26;
    pub const JUMP_IF_FALSE: u8 = 0x27;
    pub const CALL: u8 = 0x28;
    pub const PRINT: u8 = 0x29;
    pub const RETURN: u8 = 0x2a;
    pub const RETURN_VOID: u8 = 0x2b;
    pub const CALL_NATIVE: u8 = 0x2c;
    pub const NEW_STRUCT: u8 = 0x2d;
    pub const GET_FIELD: u8 = 0x2e;
    pub const STORE_FIELD: u8 = 0x2f;
    // Arrays. Appended after `STORE_FIELD`, which is where the set ended
    // before them; adding an opcode is not an ABI change.
    pub const NEW_ARRAY: u8 = 0x30;
    pub const ARRAY_GET: u8 = 0x31;
    pub const ARRAY_LEN: u8 = 0x32;
    pub const STORE_PLACE: u8 = 0x33;
    pub const ARRAY_APPEND: u8 = 0x34;
    // Enums. Appended after the array opcodes, which is where the set ended
    // before them; adding an opcode is not an ABI change.
    pub const NEW_ENUM: u8 = 0x35;
    pub const ENUM_TAG: u8 = 0x36;
    /// Appended for `match` payload bindings; adding an opcode is not an ABI
    /// change.
    pub const ENUM_PAYLOAD: u8 = 0x37;
    // Unsigned integer division, remainder, and ordering, for the `U8`..`U64`
    // spellings. Appended after `ENUM_PAYLOAD`, which is where the set ended
    // before them; adding an opcode is not an ABI change.
    //
    // There is deliberately no unsigned add/sub/mul and no unsigned equality:
    // two's-complement wrapping and bitwise equality are identical under either
    // signedness, so those would duplicate an existing opcode.
    pub const DIV_UINT: u8 = 0x38;
    pub const REM_UINT: u8 = 0x39;
    pub const LT_UINT: u8 = 0x3a;
    pub const LE_UINT: u8 = 0x3b;
    pub const GT_UINT: u8 = 0x3c;
    pub const GE_UINT: u8 = 0x3d;

    // The bitwise operators and shifts. Appended after `GE_UINT`, which is
    // where the set ended before them; adding an opcode is not an ABI change.
    //
    // As with add/sub/mul, there is deliberately no unsigned twin for `&`, `|`,
    // `^`, or `<<`: those act on bits, and a bit has no sign. Only `>>` needs
    // both, because what fills the vacated high bits is precisely the question
    // signedness answers.
    pub const BIT_AND: u8 = 0x3e;
    pub const BIT_OR: u8 = 0x3f;
    pub const BIT_XOR: u8 = 0x40;
    pub const SHL: u8 = 0x41;
    pub const SHR_INT: u8 = 0x42;
    pub const SHR_UINT: u8 = 0x43;
    pub const BIT_NOT: u8 = 0x44;

    // The foreign call. Appended after `BIT_NOT`, which is where the set ended
    // before it; adding an opcode is not an ABI change. It carries a `u32`
    // foreign-import id, so it is decoded in `Cursor::next_instruction` rather
    // than as a nullary opcode.
    pub const CALL_FOREIGN: u8 = 0x45;

    // The scalar numeric conversions. Appended after `CALL_FOREIGN`, which was
    // the last opcode before them; adding an opcode is not an ABI change. Only
    // the two cross-representation conversions (`Int`<->`Float`) get an opcode:
    // an integer-width or float-width conversion is an identity copy over one
    // runtime representation, so it emits no instruction at all.
    pub const CONVERT_INT_TO_FLOAT: u8 = 0x46;
    pub const CONVERT_FLOAT_TO_INT: u8 = 0x47;

    // The mutating-method call. Appended after `CONVERT_FLOAT_TO_INT`, which was
    // the last opcode before it; adding an opcode is not an ABI change. It
    // carries a `u32` function index plus a place operand (slot and path), so it
    // is decoded in `Cursor::next_instruction` rather than as a nullary opcode.
    pub const CALL_MUT: u8 = 0x48;

    // Opaque native callback-state operations. Appended after `CALL_MUT`; the
    // create/recover forms carry one `u64` type identity, while userdata/free are
    // nullary.
    pub const NATIVE_STATE: u8 = 0x49;
    pub const NATIVE_USER_DATA: u8 = 0x4a;
    pub const NATIVE_RECOVER: u8 = 0x4b;
    pub const NATIVE_STATE_FREE: u8 = 0x4c;

    // The null raw pointer. Appended after the callback-state group; nullary,
    // and the only `RawPtr` constant the language spells.
    pub const RAW_PTR_NULL: u8 = 0x4d;

    // The address C enters a Kira function at. Appended after `RAW_PTR_NULL`;
    // carries a `u32` callback index, which the host resolves to a thunk.
    pub const FOREIGN_CALLBACK: u8 = 0x4e;

    // The general writeback call. Appended after `FOREIGN_CALLBACK`; adding an
    // opcode is not an ABI change. `CALL_MUT` is its one-target special case
    // and stays exactly as it was, so every module already written still
    // decodes and runs — this carries a count and one (parameter, place) row
    // per target, which is what a call with several `borrow mut` arguments
    // needs and `CALL_MUT`'s fixed slot-0 target cannot express.
    pub const CALL_WRITEBACK: u8 = 0x4f;
    // A string's character count. Appended after `CALL_WRITEBACK`; adding an
    // opcode is not an ABI change.
    pub const STRING_LEN: u8 = 0x50;
    // The two IEEE-754 bit reinterpretations. Appended after `STRING_LEN`;
    // adding an opcode is not an ABI change.
    pub const CONVERT_FLOAT_TO_BITS: u8 = 0x51;
    pub const CONVERT_BITS_TO_FLOAT: u8 = 0x52;
    // Retained C string storage. Appended after `FILE_SYSTEM`; nullary.
    pub const CSTRING_NEW: u8 = 0x54;
    // The address of a retained C-layout image. Appended after `CSTRING_NEW`;
    // carries the `u32` aggregate row describing the layout.
    pub const CLAYOUT_ADDRESS: u8 = 0x55;
    // The four remaining string primitives. Appended after `CLAYOUT_ADDRESS`;
    // adding an opcode is not an ABI change.
    pub const STRING_CHAR_AT: u8 = 0x56;
    pub const STRING_SUBSTRING: u8 = 0x57;
    pub const STRING_INDEX_OF: u8 = 0x58;
    pub const STRING_OF: u8 = 0x59;
    // One file-system operation. Appended after the bit reinterpretations;
    // carries one `FileSystemOp` byte, whose own numbering is append-only, so a
    // new operation costs neither an opcode nor a version.
    pub const FILE_SYSTEM: u8 = 0x53;
    // A 32-bit float's bits widened to Kira's `Float`. Appended after
    // `STRING_OF`; adding an opcode is not an ABI change.
    pub const CONVERT_BITS32_TO_FLOAT: u8 = 0x5a;
    // Reading one element of an array a local holds, without copying the
    // array. Appended after `CONVERT_BITS32_TO_FLOAT`; carries the `u32` slot.
    pub const ARRAY_GET_LOCAL: u8 = 0x5b;
}

#[cfg(test)]
#[path = "op_tests.rs"]
mod tests;
