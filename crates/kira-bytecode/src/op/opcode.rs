//! The opcode byte for each instruction. Append-only: never reorder or reuse.

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
// carries a wide function index plus a place operand (slot and path), so it
// is decoded in `Cursor::next_instruction` rather than as a nullary opcode.
pub const CALL_MUT: u8 = 0x48;

// Opaque native callback-state operations. Appended after `CALL_MUT`; the
// create/recover forms carry one `u64` type identity, while userdata/free are
// nullary.
pub const NATIVE_STATE: u8 = 0x49;
pub const NATIVE_USER_DATA: u8 = 0x4a;
pub const NATIVE_RECOVER: u8 = 0x4b;
pub const NATIVE_STATE_RELEASE: u8 = 0x4c;

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
// array. Appended after `CONVERT_BITS32_TO_FLOAT`; carries a wide slot.
pub const ARRAY_GET_LOCAL: u8 = 0x5b;
// A `Float` narrowed to its 32-bit pattern — the other direction of
// `CONVERT_BITS32_TO_FLOAT`. Appended after `ARRAY_GET_LOCAL`; adding an
// opcode is not an ABI change.
pub const CONVERT_FLOAT_TO_BITS32: u8 = 0x5c;
// Float remainder. Appended after `TASK_OP`; adding an opcode is not an ABI
// change.
pub const REM_FLOAT: u8 = 0x5e;
// One primitive of the deferred-task executor. Appended after
// `CONVERT_FLOAT_TO_BITS32`; carries one `TaskPrim` byte, whose own
// numbering is append-only, so a new primitive costs neither an opcode nor
// a version — the same arrangement `FILE_SYSTEM` uses.
pub const TASK_OP: u8 = 0x5d;
// The three capture-cell primitives. Appended after `REM_FLOAT`, which was
// the last opcode before them; adding an opcode is not an ABI change.
// `NEW_CELL` is nullary; the get and set forms carry a wide slot.
pub const NEW_CELL: u8 = 0x5f;
pub const CELL_GET: u8 = 0x60;
pub const CELL_SET: u8 = 0x61;
// One compiler operation. Appended after `CELL_SET`; carries one
// `CompilerOp` byte, whose own numbering is append-only, so a new operation
// costs neither an opcode nor a version — the same arrangement
// `FILE_SYSTEM` and `TASK_OP` use.
pub const COMPILER: u8 = 0x62;
// Structural equality and inequality of two erased values. Appended after
// `COMPILER`; adding an opcode is not an ABI change. Nullary like every
// other comparison — both operands come off the stack.
pub const EQ_ANY: u8 = 0x63;
pub const NE_ANY: u8 = 0x64;
// Erasure into `Any`, carrying the `ErasedTypeId` word as a `u64`
// immediate. Appended after `NE_ANY`; adding an opcode is not an ABI
// change.
pub const ERASE: u8 = 0x65;
// Every string operation past the first four. Appended after `ERASE`;
// carries one `StringOp` byte, whose own numbering is append-only, so a
// new string operation costs neither an opcode nor a version — the same
// arrangement `FILE_SYSTEM`, `TASK_OP` and `COMPILER` use.
pub const STRING_OP: u8 = 0x66;
// One environment read. Appended after `STRING_OP`, on the same
// arrangement every other operation family uses: the opcode says which
// family and the byte after it says which operation.
pub const ENV_OP: u8 = 0x67;
// A writeback call whose callee is native. Appended after `ENV_OP` rather
// than placed beside `CALL_WRITEBACK`, because the table is append-only and
// where an opcode reads well is not a reason to move one.
pub const CALL_NATIVE_WRITEBACK: u8 = 0x68;
/// See [`super::Instruction::MathOp`].
pub const MATH_OP: u8 = 0x6c;
/// See [`super::Instruction::ScalarText`].
pub const SCALAR_TEXT: u8 = 0x6d;
/// See [`super::Instruction::ArrayElements`].
pub const ARRAY_ELEMENTS: u8 = 0x6e;
/// See [`super::Instruction::ForeignLoad`].
pub const FOREIGN_LOAD: u8 = 0x69;
/// See [`super::Instruction::ForeignOffset`].
pub const FOREIGN_OFFSET: u8 = 0x6a;
/// See [`super::Instruction::ForeignIndex`].
pub const FOREIGN_INDEX: u8 = 0x6b;
// The explicit integer and opaque pointer-word conversions. Appended after
// `FOREIGN_INDEX`; both are nullary and only retag one 64-bit VM value.
pub const CONVERT_INT_TO_RAW_PTR: u8 = 0x6f;
pub const CONVERT_RAW_PTR_TO_INT: u8 = 0x70;

/// Construction of a struct whose type declares a user `Drop`. Appended
/// after `CONVERT_RAW_PTR_TO_INT`, which is where the set ended before it;
/// adding an opcode is not an ABI change.
pub const NEW_STRUCT_DROPPING: u8 = 0x71;

/// Reading a local that runs a user `Drop`, which takes it. Appended after
/// `NEW_STRUCT_DROPPING`; adding an opcode is not an ABI change.
pub const TAKE_LOCAL: u8 = 0x72;

/// Reading a module-constant slot. Appended after `TAKE_LOCAL`; adding an
/// opcode is not an ABI change. Carries a wide slot into the module's
/// constants table.
pub const LOAD_CONSTANT: u8 = 0x73;
// Main-thread event-loop operations. Appended after LOAD_CONSTANT; the
// operation byte is append-only in the runtime ABI.
pub const MAIN_THREAD_CALL: u8 = 0x74;
pub const MAIN_THREAD_JOIN: u8 = 0x75;
// Main-thread lifecycle entry marker. Appended after MAIN_THREAD_JOIN;
// nullary and valid only as instruction zero of the entrypoint.
pub const MAIN_THREAD_LIFECYCLE: u8 = 0x76;
/// Construction of a struct from fields evaluated out of declaration
/// order. Appended after `MAIN_THREAD_LIFECYCLE`; adding an opcode is not
/// an ABI change.
pub const NEW_STRUCT_ORDERED: u8 = 0x77;

/// Integer arithmetic that traps on overflow, and the width checks the
/// written spellings need. Appended after `NEW_STRUCT_ORDERED`; adding an
/// opcode is not an ABI change.
pub const ADD_INT_CHECKED: u8 = 0x78;
pub const SUB_INT_CHECKED: u8 = 0x79;
pub const MUL_INT_CHECKED: u8 = 0x7a;
pub const NEG_INT_CHECKED: u8 = 0x7b;
pub const DIV_INT_CHECKED: u8 = 0x7c;
pub const ADD_UINT_CHECKED: u8 = 0x7d;
pub const SUB_UINT_CHECKED: u8 = 0x7e;
pub const MUL_UINT_CHECKED: u8 = 0x7f;
pub const CHECK_INT: u8 = 0x80;
pub const WRAP_INT: u8 = 0x81;
pub const CHECK_SHIFT: u8 = 0x82;
pub const CONVERT_INT: u8 = 0x83;
pub const CONVERT_UINT_TO_FLOAT: u8 = 0x84;
pub const PRINT_UNSIGNED: u8 = 0x85;
pub const STRING_OF_UNSIGNED: u8 = 0x86;
pub const TYPE_TEST: u8 = 0x87;
pub const DOWNCAST: u8 = 0x88;
pub const NATIVE_STATE_RETAIN: u8 = 0x89;

/// The runtime type descriptor a value answers `.type` with: a constant for a
/// value whose type is known, and a read of the box for an `Any`. Appended
/// after `NATIVE_STATE_RETAIN`; adding an opcode is not an ABI change.
pub const CONST_TYPE: u8 = 0x8a;
pub const TYPE_OF: u8 = 0x8b;
pub const EQ_TYPE: u8 = 0x8c;
pub const NE_TYPE: u8 = 0x8d;
pub const TYPE_FIELD: u8 = 0x8e;
pub const TYPE_CAST_RESULT: u8 = 0x8f;

/// One channel-table primitive, carrying its `ChannelPrim` tag in a second
/// byte exactly as `TASK_OP` carries its own. Appended after
/// `TYPE_CAST_RESULT`; adding an opcode is not an ABI change.
pub const CHANNEL_OP: u8 = 0x90;

/// The unsigned float-to-integer conversion, for a `U64` destination whose
/// range the signed one cannot express. Appended after `CHANNEL_OP`; adding an
/// opcode is not an ABI change.
pub const CONVERT_FLOAT_TO_UINT: u8 = 0x91;
