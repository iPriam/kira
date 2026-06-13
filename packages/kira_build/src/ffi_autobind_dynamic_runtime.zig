//! Emits the Kira-runtime dynamic-FFI preamble into generated dynamic-loader
//! bindings. Every entry mirrors a `kira_runtime` export declared in
//! `foundation/NativeLibs/DynamicFfi/kira_dynamic_ffi.h`; the loader name
//! prefixes each function so generated modules stay self-contained.

const Binding = struct {
    symbol: []const u8,
    suffix: []const u8,
    signature: []const u8,
};

const runtime_bindings = [_]Binding{
    .{ .symbol = "kira_dynamic_host_platform_code", .suffix = "DynamicHostPlatformCode", .signature = "(): U32" },
    .{ .symbol = "kira_dynamic_library_open", .suffix = "DynamicLibraryOpen", .signature = "(name: CString): RawPtr" },
    .{ .symbol = "kira_dynamic_library_symbol", .suffix = "DynamicLibrarySymbol", .signature = "(library: RawPtr, name: CString): RawPtr" },
    .{ .symbol = "kira_dynamic_library_close", .suffix = "DynamicLibraryClose", .signature = "(library: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_null_ptr", .suffix = "DynamicNullPtr", .signature = "(): RawPtr" },
    .{ .symbol = "kira_dynamic_ptr_is_null", .suffix = "DynamicPtrIsNull", .signature = "(ptr: RawPtr): Bool" },
    .{ .symbol = "kira_dynamic_alloc", .suffix = "DynamicAlloc", .signature = "(size: U64): RawPtr" },
    .{ .symbol = "kira_dynamic_free", .suffix = "DynamicFree", .signature = "(ptr: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_read_u32", .suffix = "DynamicReadU32", .signature = "(ptr: RawPtr): U32" },
    .{ .symbol = "kira_dynamic_read_i32", .suffix = "DynamicReadI32", .signature = "(ptr: RawPtr): I32" },
    .{ .symbol = "kira_dynamic_read_ptr", .suffix = "DynamicReadPtr", .signature = "(ptr: RawPtr): RawPtr" },
    .{ .symbol = "kira_dynamic_read_u8_at", .suffix = "DynamicReadU8At", .signature = "(ptr: RawPtr, offset: U64): U8" },
    .{ .symbol = "kira_dynamic_read_u16_at", .suffix = "DynamicReadU16At", .signature = "(ptr: RawPtr, offset: U64): U16" },
    .{ .symbol = "kira_dynamic_read_u32_at", .suffix = "DynamicReadU32At", .signature = "(ptr: RawPtr, offset: U64): U32" },
    .{ .symbol = "kira_dynamic_read_i32_at", .suffix = "DynamicReadI32At", .signature = "(ptr: RawPtr, offset: U64): I32" },
    .{ .symbol = "kira_dynamic_read_u64_at", .suffix = "DynamicReadU64At", .signature = "(ptr: RawPtr, offset: U64): U64" },
    .{ .symbol = "kira_dynamic_read_i64_at", .suffix = "DynamicReadI64At", .signature = "(ptr: RawPtr, offset: U64): I64" },
    .{ .symbol = "kira_dynamic_read_ptr_at", .suffix = "DynamicReadPtrAt", .signature = "(ptr: RawPtr, offset: U64): RawPtr" },
    .{ .symbol = "kira_dynamic_read_f32_at", .suffix = "DynamicReadF32At", .signature = "(ptr: RawPtr, offset: U64): F32" },
    .{ .symbol = "kira_dynamic_read_f64_at", .suffix = "DynamicReadF64At", .signature = "(ptr: RawPtr, offset: U64): F64" },
    .{ .symbol = "kira_dynamic_write_u32", .suffix = "DynamicWriteU32", .signature = "(ptr: RawPtr, value: U32): Void" },
    .{ .symbol = "kira_dynamic_write_ptr", .suffix = "DynamicWritePtr", .signature = "(ptr: RawPtr, value: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_write_u8_at", .suffix = "DynamicWriteU8At", .signature = "(ptr: RawPtr, offset: U64, value: U8): Void" },
    .{ .symbol = "kira_dynamic_write_u16_at", .suffix = "DynamicWriteU16At", .signature = "(ptr: RawPtr, offset: U64, value: U16): Void" },
    .{ .symbol = "kira_dynamic_write_u32_at", .suffix = "DynamicWriteU32At", .signature = "(ptr: RawPtr, offset: U64, value: U32): Void" },
    .{ .symbol = "kira_dynamic_write_u64_at", .suffix = "DynamicWriteU64At", .signature = "(ptr: RawPtr, offset: U64, value: U64): Void" },
    .{ .symbol = "kira_dynamic_write_i64_at", .suffix = "DynamicWriteI64At", .signature = "(ptr: RawPtr, offset: U64, value: I64): Void" },
    .{ .symbol = "kira_dynamic_write_ptr_at", .suffix = "DynamicWritePtrAt", .signature = "(ptr: RawPtr, offset: U64, value: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_write_f32_at", .suffix = "DynamicWriteF32At", .signature = "(ptr: RawPtr, offset: U64, value: F32): Void" },
    .{ .symbol = "kira_dynamic_write_f64_at", .suffix = "DynamicWriteF64At", .signature = "(ptr: RawPtr, offset: U64, value: F64): Void" },
    .{ .symbol = "kira_dynamic_cstring_dup", .suffix = "DynamicCStringDup", .signature = "(text: CString): RawPtr" },
    .{ .symbol = "kira_dynamic_cstring_at", .suffix = "DynamicCStringAt", .signature = "(ptr: RawPtr, offset: U64): CString" },
    .{ .symbol = "kira_dynamic_ffi_call", .suffix = "DynamicFfiCall", .signature = "(functionPtr: RawPtr, resultType: U32, argTypes: RawPtr, argValues: RawPtr, argCount: U32, resultOut: RawPtr): I32" },
    .{ .symbol = "kira_dynamic_ffi_last_error_code", .suffix = "DynamicFfiLastErrorCode", .signature = "(): I32" },
    .{ .symbol = "kira_dynamic_ffi_call_i32_ptr", .suffix = "DynamicFfiCallI32Ptr", .signature = "(functionPtr: RawPtr, arg0: RawPtr): I32" },
    .{ .symbol = "kira_dynamic_ffi_call_i32_ptr_u32_ptr_ptr", .suffix = "DynamicFfiCallI32PtrU32PtrPtr", .signature = "(functionPtr: RawPtr, arg0: RawPtr, arg1: U32, arg2: RawPtr, arg3: RawPtr): I32" },
    .{ .symbol = "kira_dynamic_call_new", .suffix = "DynamicCallNew", .signature = "(maxArgs: U32): RawPtr" },
    .{ .symbol = "kira_dynamic_call_reset", .suffix = "DynamicCallReset", .signature = "(call: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_call_free", .suffix = "DynamicCallFree", .signature = "(call: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_call_arg_ptr", .suffix = "DynamicCallArgPtr", .signature = "(call: RawPtr, value: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_call_arg_i32", .suffix = "DynamicCallArgI32", .signature = "(call: RawPtr, value: I32): Void" },
    .{ .symbol = "kira_dynamic_call_arg_u32", .suffix = "DynamicCallArgU32", .signature = "(call: RawPtr, value: U32): Void" },
    .{ .symbol = "kira_dynamic_call_arg_i64", .suffix = "DynamicCallArgI64", .signature = "(call: RawPtr, value: I64): Void" },
    .{ .symbol = "kira_dynamic_call_arg_u64", .suffix = "DynamicCallArgU64", .signature = "(call: RawPtr, value: U64): Void" },
    .{ .symbol = "kira_dynamic_call_arg_f32", .suffix = "DynamicCallArgF32", .signature = "(call: RawPtr, value: F32): Void" },
    .{ .symbol = "kira_dynamic_call_arg_f64", .suffix = "DynamicCallArgF64", .signature = "(call: RawPtr, value: F64): Void" },
    .{ .symbol = "kira_dynamic_call_invoke_void", .suffix = "DynamicCallInvokeVoid", .signature = "(call: RawPtr, functionPtr: RawPtr): Void" },
    .{ .symbol = "kira_dynamic_call_invoke_i32", .suffix = "DynamicCallInvokeI32", .signature = "(call: RawPtr, functionPtr: RawPtr): I32" },
    .{ .symbol = "kira_dynamic_call_invoke_u32", .suffix = "DynamicCallInvokeU32", .signature = "(call: RawPtr, functionPtr: RawPtr): U32" },
    .{ .symbol = "kira_dynamic_call_invoke_i64", .suffix = "DynamicCallInvokeI64", .signature = "(call: RawPtr, functionPtr: RawPtr): I64" },
    .{ .symbol = "kira_dynamic_call_invoke_u64", .suffix = "DynamicCallInvokeU64", .signature = "(call: RawPtr, functionPtr: RawPtr): U64" },
    .{ .symbol = "kira_dynamic_call_invoke_ptr", .suffix = "DynamicCallInvokePtr", .signature = "(call: RawPtr, functionPtr: RawPtr): RawPtr" },
    .{ .symbol = "kira_dynamic_call_invoke_f32", .suffix = "DynamicCallInvokeF32", .signature = "(call: RawPtr, functionPtr: RawPtr): F32" },
    .{ .symbol = "kira_dynamic_call_invoke_f64", .suffix = "DynamicCallInvokeF64", .signature = "(call: RawPtr, functionPtr: RawPtr): F64" },
};

pub fn writeBindings(writer: anytype, loader_name: []const u8) !void {
    for (runtime_bindings) |binding| {
        try writer.print("@FFI.Extern {{ library: kira_runtime; symbol: {s}; abi: c; }}\n", .{binding.symbol});
        try writer.print("function {s}{s}{s};\n\n", .{ loader_name, binding.suffix, binding.signature });
    }
    try writer.writeAll("\n");
}
