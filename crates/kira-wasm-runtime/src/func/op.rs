//! The opcodes this backend emits.
//!
//! One constant per instruction, spelled as the format spells it. Every one of
//! them is used: an opcode nothing emits is vocabulary nothing proves, and the
//! encoder has no way to be wrong about a byte it never writes.

/// `unreachable`
pub const UNREACHABLE: u8 = 0x00;
/// `block`
pub const BLOCK: u8 = 0x02;
/// `loop`
pub const LOOP: u8 = 0x03;
/// `if`
pub const IF: u8 = 0x04;
/// `else`
pub const ELSE: u8 = 0x05;
/// `end`
pub const END: u8 = 0x0b;
/// `br`
pub const BR: u8 = 0x0c;
/// `br_if`
pub const BR_IF: u8 = 0x0d;
/// `return`
pub const RETURN: u8 = 0x0f;
/// `call`
pub const CALL: u8 = 0x10;
/// `drop`
pub const DROP: u8 = 0x1a;
/// `local.get`
pub const LOCAL_GET: u8 = 0x20;
/// `local.set`
pub const LOCAL_SET: u8 = 0x21;
/// `local.tee`
pub const LOCAL_TEE: u8 = 0x22;
/// `global.get`
pub const GLOBAL_GET: u8 = 0x23;
/// `global.set`
pub const GLOBAL_SET: u8 = 0x24;
/// `i32.load`
pub const I32_LOAD: u8 = 0x28;
/// `i64.load`
pub const I64_LOAD: u8 = 0x29;
/// `f64.load`
pub const F64_LOAD: u8 = 0x2b;
/// `i32.load8_u`
pub const I32_LOAD8_U: u8 = 0x2d;
/// `i32.store`
pub const I32_STORE: u8 = 0x36;
/// `i64.store`
pub const I64_STORE: u8 = 0x37;
/// `f64.store`
pub const F64_STORE: u8 = 0x39;
/// `i32.store8`
pub const I32_STORE8: u8 = 0x3a;
/// `memory.size`
pub const MEMORY_SIZE: u8 = 0x3f;
/// `memory.grow`
pub const MEMORY_GROW: u8 = 0x40;
/// The prefix byte for the multi-byte opcodes.
pub const PREFIX_FC: u8 = 0xfc;
/// `memory.copy`, as the sub-opcode after [`PREFIX_FC`].
pub const MEMORY_COPY: u32 = 10;
/// `i32.const`
pub const I32_CONST: u8 = 0x41;
/// `i64.const`
pub const I64_CONST: u8 = 0x42;
/// `f64.const`
pub const F64_CONST: u8 = 0x44;
/// `i32.eqz`
pub const I32_EQZ: u8 = 0x45;
/// `i32.eq`
pub const I32_EQ: u8 = 0x46;
/// `i32.ne`
pub const I32_NE: u8 = 0x47;
/// `i32.lt_s`
pub const I32_LT_S: u8 = 0x48;
/// `i32.lt_u`
pub const I32_LT_U: u8 = 0x49;
/// `i32.gt_s`
pub const I32_GT_S: u8 = 0x4a;
/// `i32.gt_u`
pub const I32_GT_U: u8 = 0x4b;
/// `i32.le_s`
pub const I32_LE_S: u8 = 0x4c;
/// `i32.ge_s`
pub const I32_GE_S: u8 = 0x4e;
/// `i32.ge_u`
pub const I32_GE_U: u8 = 0x4f;
/// `i64.clz`
pub const I64_CLZ: u8 = 0x79;
/// `i64.eqz`
pub const I64_EQZ: u8 = 0x50;
/// `i64.eq`
pub const I64_EQ: u8 = 0x51;
/// `i64.ne`
pub const I64_NE: u8 = 0x52;
/// `i64.lt_s`
pub const I64_LT_S: u8 = 0x53;
/// `i64.lt_u`
pub const I64_LT_U: u8 = 0x54;
/// `i64.gt_s`
pub const I64_GT_S: u8 = 0x55;
/// `i64.gt_u`
pub const I64_GT_U: u8 = 0x56;
/// `i64.le_s`
pub const I64_LE_S: u8 = 0x57;
/// `i64.le_u`
pub const I64_LE_U: u8 = 0x58;
/// `i64.ge_s`
pub const I64_GE_S: u8 = 0x59;
/// `i64.ge_u`
pub const I64_GE_U: u8 = 0x5a;
/// `f64.eq`
pub const F64_EQ: u8 = 0x61;
/// `f64.ne`
pub const F64_NE: u8 = 0x62;
/// `f64.lt`
pub const F64_LT: u8 = 0x63;
/// `f64.gt`
pub const F64_GT: u8 = 0x64;
/// `f64.le`
pub const F64_LE: u8 = 0x65;
/// `f64.ge`
pub const F64_GE: u8 = 0x66;
/// `i32.add`
pub const I32_ADD: u8 = 0x6a;
/// `i32.sub`
pub const I32_SUB: u8 = 0x6b;
/// `i32.mul`
pub const I32_MUL: u8 = 0x6c;
/// `i32.div_u`
pub const I32_DIV_U: u8 = 0x6e;
/// `i32.and`
pub const I32_AND: u8 = 0x71;
/// `i32.or`
pub const I32_OR: u8 = 0x72;
/// `i32.shl`
pub const I32_SHL: u8 = 0x74;
/// `i32.shr_u`
pub const I32_SHR_U: u8 = 0x76;
/// `i64.add`
pub const I64_ADD: u8 = 0x7c;
/// `i64.sub`
pub const I64_SUB: u8 = 0x7d;
/// `i64.mul`
pub const I64_MUL: u8 = 0x7e;
/// `i64.div_s`
pub const I64_DIV_S: u8 = 0x7f;
/// `i64.div_u`
pub const I64_DIV_U: u8 = 0x80;
/// `i64.rem_s`
pub const I64_REM_S: u8 = 0x81;
/// `i64.rem_u`
pub const I64_REM_U: u8 = 0x82;
/// `i64.and`
pub const I64_AND: u8 = 0x83;
/// `i64.or`
pub const I64_OR: u8 = 0x84;
/// `i64.shl`
pub const I64_SHL: u8 = 0x86;
/// `i64.shr_u`
pub const I64_SHR_U: u8 = 0x88;
/// `f64.abs`
pub const F64_ABS: u8 = 0x99;
/// `f64.ceil`
pub const F64_CEIL: u8 = 0x9b;
/// `i32.trunc_f64_s`
pub const I32_TRUNC_F64_S: u8 = 0xaa;
/// `f64.convert_i32_s`
pub const F64_CONVERT_I32_S: u8 = 0xb7;
/// `f64.neg`
pub const F64_NEG: u8 = 0x9a;
/// `f64.add`
pub const F64_ADD: u8 = 0xa0;
/// `f64.sub`
pub const F64_SUB: u8 = 0xa1;
/// `f64.mul`
pub const F64_MUL: u8 = 0xa2;
/// `f64.div`
pub const F64_DIV: u8 = 0xa3;
/// `i32.wrap_i64`
pub const I32_WRAP_I64: u8 = 0xa7;
/// `i64.extend_i32_u`
pub const I64_EXTEND_I32_U: u8 = 0xad;
/// `i64.reinterpret_f64`
pub const I64_REINTERPRET_F64: u8 = 0xbd;
