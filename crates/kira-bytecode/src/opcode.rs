//! The KBC opcode set.
//!
//! Ported from kira-zig `kira_bytecode/src/instruction.zig` (`OpCode`,
//! `enum(u8)`). Discriminants are verified against the Zig compiler's actual
//! `@intFromEnum` values and are serialized into KBC containers.
//!
//! APPEND-ONLY: new serialized opcodes must be appended after the last
//! pre-existing serialized opcode (`string_index_of`), never inserted
//! mid-enum, so no persisted tag ever shifts. The VM-internal `Fused*` block
//! must remain the contiguous trailing block (see [`is_fused`]).

/// A KBC instruction opcode (Zig `OpCode`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpCode {
    /// Zig `const_int`.
    ConstInt = 0,
    /// Zig `const_float`.
    ConstFloat = 1,
    /// Zig `const_string`.
    ConstString = 2,
    /// Zig `const_bool`.
    ConstBool = 3,
    /// Zig `const_null_ptr`.
    ConstNullPtr = 4,
    /// Zig `const_function`.
    ConstFunction = 5,
    /// Zig `const_closure`.
    ConstClosure = 6,
    /// Zig `alloc_struct`.
    AllocStruct = 7,
    /// Zig `alloc_enum`.
    AllocEnum = 8,
    /// Zig `alloc_native_state`.
    AllocNativeState = 9,
    /// Zig `alloc_array`.
    AllocArray = 10,
    /// Zig `add`.
    Add = 11,
    /// Zig `subtract`.
    Subtract = 12,
    /// Zig `multiply`.
    Multiply = 13,
    /// Zig `divide`.
    Divide = 14,
    /// Zig `modulo`.
    Modulo = 15,
    /// Zig `compare`.
    Compare = 16,
    /// Zig `unary`.
    Unary = 17,
    /// Zig `store_local`.
    StoreLocal = 18,
    /// Zig `load_local`.
    LoadLocal = 19,
    /// Zig `local_ptr`.
    LocalPtr = 20,
    /// Zig `subobject_ptr`.
    SubobjectPtr = 21,
    /// Zig `field_ptr`.
    FieldPtr = 22,
    /// Zig `recover_native_state`.
    RecoverNativeState = 23,
    /// Zig `native_state_field_get`.
    NativeStateFieldGet = 24,
    /// Zig `native_state_field_set`.
    NativeStateFieldSet = 25,
    /// Zig `c_string_to_string`.
    CStringToString = 26,
    /// Zig `array_len`.
    ArrayLen = 27,
    /// Zig `string_len`.
    StringLen = 28,
    /// Zig `array_get`.
    ArrayGet = 29,
    /// Zig `array_set`.
    ArraySet = 30,
    /// Zig `array_append`.
    ArrayAppend = 31,
    /// Zig `enum_tag`.
    EnumTag = 32,
    /// Zig `enum_payload`.
    EnumPayload = 33,
    /// Zig `load_indirect`.
    LoadIndirect = 34,
    /// Zig `store_indirect`.
    StoreIndirect = 35,
    /// Zig `copy_indirect`.
    CopyIndirect = 36,
    /// Zig `branch`.
    Branch = 37,
    /// Zig `jump`.
    Jump = 38,
    /// Zig `label`.
    Label = 39,
    /// Zig `print`.
    Print = 40,
    /// Zig `call_runtime`.
    CallRuntime = 41,
    /// Zig `call_native`.
    CallNative = 42,
    /// Zig `call_virtual`.
    CallVirtual = 43,
    /// Zig `call_value`.
    CallValue = 44,
    /// Zig `ret`.
    Ret = 45,
    /// Zig `convert` — numeric Int<->Float cast. Appended after the last
    /// pre-existing serialized opcode so no earlier tag shifts; carried by KBC7.
    Convert = 46,
    /// Zig `bitwise` — bitwise/shift ops. Appended after `convert`; carried by KBC8.
    Bitwise = 47,
    /// Zig `free_native_state` — releases an `alloc_native_state` box
    /// (`nativeStateFree`). Appended after `bitwise`; carried by KBC9.
    FreeNativeState = 48,
    /// Zig `task_spawn` — async task spine (deferred execution): captures
    /// callee + eagerly-evaluated args without calling. Carried by KBCB.
    TaskSpawn = 49,
    /// Zig `task_spawn_ready` — wraps a pure value as a completed task. Carried by KBCB.
    TaskSpawnReady = 50,
    /// Zig `task_await` — first-drives the task and yields its result; joining
    /// a cancelled task or joining twice traps. Carried by KBCB.
    TaskAwait = 51,
    /// Zig `task_cancel` — sets the cooperative cancel flag. Carried by KBCB.
    TaskCancel = 52,
    /// Zig `task_detach` — drives and discards. Carried by KBCB.
    TaskDetach = 53,
    /// Zig `task_yield` — cooperative progress point: run the next queued task
    /// before the current body continues. Carried by KBCC.
    TaskYield = 54,
    /// Zig `frame_get` — task-frame slot read for state-machine (suspendable)
    /// task bodies. Carried by KBCC.
    FrameGet = 55,
    /// Zig `frame_set` — task-frame slot write. Carried by KBCC.
    FrameSet = 56,
    /// Zig `task_is_complete` — true when a task is no longer pending
    /// (park-until-complete join checks). Carried by KBCC.
    TaskIsComplete = 57,
    /// Zig `task_sleep` — park the current task for at least N milliseconds.
    /// Carried by KBCC.
    TaskSleep = 58,
    /// Zig `string_from_scalar` — `String(x)` scalar conversion. String ops
    /// remain appended after the complete pre-existing serialized range.
    StringFromScalar = 59,
    /// Zig `string_char_at`.
    StringCharAt = 60,
    /// Zig `string_substring`.
    StringSubstring = 61,
    /// Zig `string_index_of`.
    StringIndexOf = 62,
    // --- VM-internal fused instructions ------------------------------------
    // Produced exclusively by the VM's decode pass inside its private
    // per-function code copies. They never appear in compiler output or
    // serialized modules (serialize/deserialize reject them), so shifting
    // their tags is safe when a new serialized opcode is appended before them.
    /// Zig `fused_compare_branch`.
    FusedCompareBranch = 63,
    /// Zig `fused_compare_const_branch`.
    FusedCompareConstBranch = 64,
    /// Zig `fused_cmp_local_const_branch`.
    FusedCmpLocalConstBranch = 65,
    /// Zig `fused_arith_locals_store`.
    FusedArithLocalsStore = 66,
    /// Zig `fused_arith_local_const_store`.
    FusedArithLocalConstStore = 67,
    /// Zig `fused_arith_locals_ret`.
    FusedArithLocalsRet = 68,
    /// Zig `fused_array_bind_local`.
    FusedArrayBindLocal = 69,
    /// Zig `fused_array_field_load`.
    FusedArrayFieldLoad = 70,
}

/// True for the VM-internal fused superinstructions — produced only by the
/// VM's decode pass, never by the compiler or serializer (Zig `isFused`).
///
/// Single source of truth: a range check over the contiguous trailing block of
/// fused tags, so serialize/deserialize rejection and register-read analysis
/// gate on one predicate.
pub const fn is_fused(op: OpCode) -> bool {
    (op as u8) >= (OpCode::FusedCompareBranch as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the Zig "persisted opcode tags remain stable" intent, with
    /// discriminants verified against the Zig compiler's actual
    /// `@intFromEnum` output (the Zig test's literals for `ret`/`task_sleep`/
    /// `string_from_scalar` are off by one and evidently latent).
    #[test]
    fn persisted_opcode_tags_are_stable() {
        assert_eq!(OpCode::ArrayGet as u8, 29);
        assert_eq!(OpCode::Ret as u8, 45);
        assert_eq!(OpCode::TaskSleep as u8, 58);
        assert_eq!(OpCode::StringFromScalar as u8, 59);
        assert_eq!(OpCode::FusedArrayFieldLoad as u8, 70);
    }

    #[test]
    fn is_fused_classifies_the_trailing_block() {
        assert!(!is_fused(OpCode::StringIndexOf));
        assert!(is_fused(OpCode::FusedCompareBranch));
        assert!(is_fused(OpCode::FusedArrayFieldLoad));
    }
}
