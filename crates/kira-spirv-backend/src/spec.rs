//! The numbers the SPIR-V specification assigns.
//!
//! Only the ones this backend emits are here, each named as the specification
//! names it. A magic constant in the emitter would be unreadable and unauditable
//! against the spec; a named one can be checked line by line.

/// The word every SPIR-V module starts with.
pub(crate) const MAGIC: u32 = 0x0723_0203;

/// The version this backend emits: SPIR-V 1.3, which Vulkan 1.1 consumes.
///
/// 1.3 rather than 1.0 for one reason: it has the `StorageBuffer` storage
/// class. Under 1.0 a storage buffer is a `Uniform` variable whose struct is
/// decorated `BufferBlock`, a spelling deprecated ever since — and this
/// workspace binds storage buffers everywhere.
pub(crate) const VERSION: u32 = 0x0001_0300;

/// The generator word. The high half is the tool, the low half its version.
///
/// Zero is the registry's "unknown generator", which is the honest entry until
/// this backend is registered with Khronos.
pub(crate) const GENERATOR: u32 = 0;

/// Opcodes, in the specification's numbering.
pub(crate) mod op {
    pub(crate) const NAME: u16 = 5;
    pub(crate) const MEMBER_NAME: u16 = 6;
    pub(crate) const EXT_INST_IMPORT: u16 = 11;
    pub(crate) const EXT_INST: u16 = 12;
    pub(crate) const MEMORY_MODEL: u16 = 14;
    pub(crate) const ENTRY_POINT: u16 = 15;
    pub(crate) const EXECUTION_MODE: u16 = 16;
    pub(crate) const CAPABILITY: u16 = 17;
    pub(crate) const TYPE_VOID: u16 = 19;
    pub(crate) const TYPE_BOOL: u16 = 20;
    pub(crate) const TYPE_INT: u16 = 21;
    pub(crate) const TYPE_FLOAT: u16 = 22;
    pub(crate) const TYPE_VECTOR: u16 = 23;
    pub(crate) const TYPE_MATRIX: u16 = 24;
    pub(crate) const TYPE_IMAGE: u16 = 25;
    pub(crate) const TYPE_SAMPLER: u16 = 26;
    pub(crate) const TYPE_SAMPLED_IMAGE: u16 = 27;
    pub(crate) const TYPE_RUNTIME_ARRAY: u16 = 29;
    pub(crate) const TYPE_STRUCT: u16 = 30;
    pub(crate) const TYPE_POINTER: u16 = 32;
    pub(crate) const TYPE_FUNCTION: u16 = 33;
    pub(crate) const CONSTANT_TRUE: u16 = 41;
    pub(crate) const CONSTANT_FALSE: u16 = 42;
    pub(crate) const CONSTANT: u16 = 43;
    pub(crate) const FUNCTION: u16 = 54;
    pub(crate) const FUNCTION_PARAMETER: u16 = 55;
    pub(crate) const FUNCTION_END: u16 = 56;
    pub(crate) const FUNCTION_CALL: u16 = 57;
    pub(crate) const VARIABLE: u16 = 59;
    pub(crate) const LOAD: u16 = 61;
    pub(crate) const STORE: u16 = 62;
    pub(crate) const ACCESS_CHAIN: u16 = 65;
    pub(crate) const ARRAY_LENGTH: u16 = 68;
    pub(crate) const DECORATE: u16 = 71;
    pub(crate) const MEMBER_DECORATE: u16 = 72;
    pub(crate) const VECTOR_SHUFFLE: u16 = 79;
    pub(crate) const COMPOSITE_CONSTRUCT: u16 = 80;
    pub(crate) const COMPOSITE_EXTRACT: u16 = 81;
    pub(crate) const SAMPLED_IMAGE: u16 = 86;
    pub(crate) const IMAGE_SAMPLE_IMPLICIT_LOD: u16 = 87;
    pub(crate) const IMAGE_FETCH: u16 = 95;
    pub(crate) const CONVERT_F_TO_U: u16 = 109;
    pub(crate) const CONVERT_F_TO_S: u16 = 110;
    pub(crate) const CONVERT_S_TO_F: u16 = 111;
    pub(crate) const CONVERT_U_TO_F: u16 = 112;
    pub(crate) const BITCAST: u16 = 124;
    pub(crate) const S_NEGATE: u16 = 126;
    pub(crate) const F_NEGATE: u16 = 127;
    pub(crate) const I_ADD: u16 = 128;
    pub(crate) const F_ADD: u16 = 129;
    pub(crate) const I_SUB: u16 = 130;
    pub(crate) const F_SUB: u16 = 131;
    pub(crate) const I_MUL: u16 = 132;
    pub(crate) const F_MUL: u16 = 133;
    pub(crate) const U_DIV: u16 = 134;
    pub(crate) const S_DIV: u16 = 135;
    pub(crate) const F_DIV: u16 = 136;
    pub(crate) const U_MOD: u16 = 137;
    pub(crate) const S_REM: u16 = 138;
    pub(crate) const F_REM: u16 = 140;
    pub(crate) const VECTOR_TIMES_SCALAR: u16 = 142;
    pub(crate) const MATRIX_TIMES_VECTOR: u16 = 145;
    pub(crate) const MATRIX_TIMES_MATRIX: u16 = 146;
    pub(crate) const DOT: u16 = 148;
    pub(crate) const LOGICAL_OR: u16 = 166;
    pub(crate) const LOGICAL_AND: u16 = 167;
    pub(crate) const LOGICAL_NOT: u16 = 168;
    pub(crate) const I_EQUAL: u16 = 170;
    pub(crate) const I_NOT_EQUAL: u16 = 171;
    pub(crate) const U_GREATER_THAN: u16 = 172;
    pub(crate) const S_GREATER_THAN: u16 = 173;
    pub(crate) const U_GREATER_THAN_EQUAL: u16 = 174;
    pub(crate) const S_GREATER_THAN_EQUAL: u16 = 175;
    pub(crate) const U_LESS_THAN: u16 = 176;
    pub(crate) const S_LESS_THAN: u16 = 177;
    pub(crate) const U_LESS_THAN_EQUAL: u16 = 178;
    pub(crate) const S_LESS_THAN_EQUAL: u16 = 179;
    pub(crate) const F_ORD_EQUAL: u16 = 180;
    pub(crate) const F_ORD_NOT_EQUAL: u16 = 182;
    pub(crate) const F_ORD_LESS_THAN: u16 = 184;
    pub(crate) const F_ORD_GREATER_THAN: u16 = 186;
    pub(crate) const F_ORD_LESS_THAN_EQUAL: u16 = 188;
    pub(crate) const F_ORD_GREATER_THAN_EQUAL: u16 = 190;
    pub(crate) const SHIFT_RIGHT_LOGICAL: u16 = 194;
    pub(crate) const SHIFT_RIGHT_ARITHMETIC: u16 = 195;
    pub(crate) const SHIFT_LEFT_LOGICAL: u16 = 196;
    pub(crate) const BITWISE_OR: u16 = 197;
    pub(crate) const BITWISE_XOR: u16 = 198;
    pub(crate) const BITWISE_AND: u16 = 199;
    pub(crate) const ATOMIC_I_ADD: u16 = 234;
    pub(crate) const LOOP_MERGE: u16 = 246;
    pub(crate) const SELECTION_MERGE: u16 = 247;
    pub(crate) const LABEL: u16 = 248;
    pub(crate) const BRANCH: u16 = 249;
    pub(crate) const BRANCH_CONDITIONAL: u16 = 250;
    pub(crate) const RETURN: u16 = 253;
    pub(crate) const RETURN_VALUE: u16 = 254;
}

/// `OpCapability` operands.
pub(crate) mod capability {
    pub(crate) const SHADER: u32 = 1;
}

/// `OpMemoryModel` operands.
pub(crate) mod memory_model {
    pub(crate) const LOGICAL: u32 = 0;
    pub(crate) const GLSL450: u32 = 1;
}

/// `OpEntryPoint` execution models.
pub(crate) mod execution_model {
    pub(crate) const VERTEX: u32 = 0;
    pub(crate) const FRAGMENT: u32 = 4;
    pub(crate) const GL_COMPUTE: u32 = 5;
}

/// `OpExecutionMode` operands.
pub(crate) mod execution_mode {
    pub(crate) const ORIGIN_UPPER_LEFT: u32 = 7;
    pub(crate) const LOCAL_SIZE: u32 = 17;
}

/// Storage classes.
pub(crate) mod storage_class {
    pub(crate) const UNIFORM_CONSTANT: u32 = 0;
    pub(crate) const INPUT: u32 = 1;
    pub(crate) const UNIFORM: u32 = 2;
    pub(crate) const OUTPUT: u32 = 3;
    pub(crate) const FUNCTION: u32 = 7;
    pub(crate) const STORAGE_BUFFER: u32 = 12;
}

/// Decorations.
pub(crate) mod decoration {
    pub(crate) const BLOCK: u32 = 2;
    pub(crate) const COL_MAJOR: u32 = 5;
    pub(crate) const ARRAY_STRIDE: u32 = 6;
    pub(crate) const MATRIX_STRIDE: u32 = 7;
    pub(crate) const BUILT_IN: u32 = 11;
    pub(crate) const NO_PERSPECTIVE: u32 = 13;
    pub(crate) const FLAT: u32 = 14;
    pub(crate) const NON_WRITABLE: u32 = 24;
    pub(crate) const LOCATION: u32 = 30;
    pub(crate) const BINDING: u32 = 33;
    pub(crate) const DESCRIPTOR_SET: u32 = 34;
    pub(crate) const OFFSET: u32 = 35;
}

/// `BuiltIn` decoration operands.
pub(crate) mod built_in {
    pub(crate) const POSITION: u32 = 0;
    pub(crate) const FRONT_FACING: u32 = 17;
    pub(crate) const FRAG_COORD: u32 = 15;
    pub(crate) const WORKGROUP_ID: u32 = 26;
    pub(crate) const LOCAL_INVOCATION_ID: u32 = 27;
    pub(crate) const GLOBAL_INVOCATION_ID: u32 = 28;
    pub(crate) const LOCAL_INVOCATION_INDEX: u32 = 29;
    pub(crate) const VERTEX_INDEX: u32 = 42;
    pub(crate) const INSTANCE_INDEX: u32 = 43;
}

/// `OpTypeImage` dimensionality.
pub(crate) mod dim {
    pub(crate) const TWO_D: u32 = 1;
    pub(crate) const CUBE: u32 = 3;
}

/// Memory scopes and semantics, for the atomics.
pub(crate) mod scope {
    /// Every invocation in the device, which is what a storage-buffer atomic
    /// across workgroups needs.
    pub(crate) const DEVICE: u32 = 1;
    /// Relaxed: the corpus's atomics are counters, and none of them publishes
    /// data another invocation then reads through a different address.
    pub(crate) const RELAXED: u32 = 0;
}

/// The instruction numbers in the `GLSL.std.450` extended set.
pub(crate) mod glsl {
    pub(crate) const NAME: &str = "GLSL.std.450";
    pub(crate) const F_ABS: u32 = 4;
    pub(crate) const S_ABS: u32 = 5;
    pub(crate) const FLOOR: u32 = 8;
    pub(crate) const CEIL: u32 = 9;
    pub(crate) const FRACT: u32 = 10;
    pub(crate) const SIN: u32 = 13;
    pub(crate) const COS: u32 = 14;
    pub(crate) const TAN: u32 = 15;
    pub(crate) const ATAN2: u32 = 25;
    pub(crate) const POW: u32 = 26;
    pub(crate) const EXP: u32 = 27;
    pub(crate) const LOG: u32 = 28;
    pub(crate) const SQRT: u32 = 31;
    pub(crate) const F_MIN: u32 = 37;
    pub(crate) const U_MIN: u32 = 38;
    pub(crate) const S_MIN: u32 = 39;
    pub(crate) const F_MAX: u32 = 40;
    pub(crate) const U_MAX: u32 = 41;
    pub(crate) const S_MAX: u32 = 42;
    pub(crate) const F_CLAMP: u32 = 43;
    pub(crate) const U_CLAMP: u32 = 44;
    pub(crate) const S_CLAMP: u32 = 45;
    pub(crate) const F_MIX: u32 = 46;
    pub(crate) const STEP: u32 = 48;
    pub(crate) const SMOOTH_STEP: u32 = 49;
    pub(crate) const LENGTH: u32 = 66;
    pub(crate) const CROSS: u32 = 68;
    pub(crate) const NORMALIZE: u32 = 69;
}
