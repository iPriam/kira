// FFI extern-call lowering for the LLVM C-API backend. An `extern` Kira function maps
// to a real C symbol whose ABI differs from Kira's register ABI: pointers/arrays/structs
// are not uniformly i64, a CString parameter is a NUL-terminated `char*`, small structs
// pass/return as integers, etc. This module declares extern functions with their C ABI
// signature and marshals arguments/returns at the call site, mirroring the text backend's
// extern handling in backend_text_ir_calls.zig so the two backends stay binary-compatible.
const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const capi = @import("backend_capi.zig");
const drop = @import("backend_capi_drop.zig");
const FunctionCodegen = @import("backend_capi_codegen.zig").FunctionCodegen;

const typeRefName = utils.typeRefName;

// A struct return uses an sret out-parameter only on Windows for structs larger than a
// register (matches externReturnUsesSRet). Elsewhere a small/medium struct comes back in
// integer registers, modeled as i{size*8}.
pub fn usesSret(program: *const ir.Program, value_type: ir.ValueType) bool {
    if (builtin.os.tag != .windows) return false;
    if (value_type.kind != .ffi_struct) return false;
    const size = utils.valueAbiSize(program, value_type) catch return false;
    return size > 8;
}

fn intAbiType(types: capi.Types, name: ?[]const u8) llvm.c.LLVMTypeRef {
    const value = name orelse "I64";
    if (std.mem.eql(u8, value, "I8") or std.mem.eql(u8, value, "U8")) return types.i8;
    if (std.mem.eql(u8, value, "I16") or std.mem.eql(u8, value, "U16")) return types.i16;
    if (std.mem.eql(u8, value, "I32") or std.mem.eql(u8, value, "U32")) return types.i32;
    return types.i64;
}

fn floatAbiType(types: capi.Types, name: ?[]const u8) llvm.c.LLVMTypeRef {
    if (name) |value| {
        if (std.mem.eql(u8, value, "F32")) return types.float_ty;
    }
    return types.double_ty;
}

fn structAbiInt(api: *const llvm.Api, types: capi.Types, size: usize) llvm.c.LLVMTypeRef {
    return api.LLVMIntTypeInContext(types.context, @intCast(@max(size, 1) * 8));
}

// LLVM type of an extern parameter under the C ABI.
pub fn paramAbiType(
    api: *const llvm.Api,
    types: capi.Types,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    program: *const ir.Program,
    value_type: ir.ValueType,
) !llvm.c.LLVMTypeRef {
    return switch (value_type.kind) {
        .construct_any, .array, .raw_ptr, .enum_instance => types.ptr_ty,
        .integer => intAbiType(types, value_type.name),
        .float => floatAbiType(types, value_type.name),
        .boolean => types.bool_ty,
        .string => types.string_ty,
        .ffi_struct => blk: {
            const size = try utils.valueAbiSize(program, value_type);
            if (size <= 8) break :blk structAbiInt(api, types, size);
            break :blk struct_types.get(value_type.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
        },
        .void => types.void_ty,
    };
}

// LLVM type of an extern return under the C ABI (sret returns are modeled as void here;
// the caller adds the sret pointer parameter).
pub fn returnAbiType(
    api: *const llvm.Api,
    types: capi.Types,
    program: *const ir.Program,
    value_type: ir.ValueType,
) !llvm.c.LLVMTypeRef {
    if (usesSret(program, value_type)) return types.void_ty;
    return switch (value_type.kind) {
        .void => types.void_ty,
        .construct_any, .array, .raw_ptr, .enum_instance => types.ptr_ty,
        .integer => intAbiType(types, value_type.name),
        .float => floatAbiType(types, value_type.name),
        .boolean => types.bool_ty,
        .string => types.string_ty,
        .ffi_struct => structAbiInt(api, types, try utils.valueAbiSize(program, value_type)),
    };
}

// LLVM function type for declaring an extern function with its C ABI. An sret return
// prepends a `ptr` out-parameter and makes the function nominally return void.
pub fn externFunctionType(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    types: capi.Types,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    program: *const ir.Program,
    function_decl: ir.Function,
) !llvm.c.LLVMTypeRef {
    const sret = usesSret(program, function_decl.return_type);
    const extra: usize = if (sret) 1 else 0;
    const params = try allocator.alloc(llvm.c.LLVMTypeRef, function_decl.param_types.len + extra);
    defer allocator.free(params);
    if (sret) params[0] = types.ptr_ty;
    for (function_decl.param_types, 0..) |param_type, index| {
        params[index + extra] = try paramAbiType(api, types, struct_types, program, param_type);
    }
    const ret = try returnAbiType(api, types, program, function_decl.return_type);
    return api.LLVMFunctionType(ret, params.ptr, @intCast(params.len), 0);
}

// Marshal a single Kira register value into the C ABI value for `param_type`. A transient
// CString buffer allocated for the call is appended to `cstr_temps` so the caller can free
// it after the call returns (the `const char*` is borrowed for the duration of the call).
fn marshalArg(fc: *FunctionCodegen, param_type: ir.ValueType, arg_reg: u32, cstr_temps: *std.array_list.Managed(llvm.c.LLVMValueRef)) !llvm.c.LLVMValueRef {
    const api = fc.api;
    const b = fc.builder;
    const arg = fc.registers[arg_reg];
    const arg_type = if (arg_reg < fc.register_types.len) fc.register_types[arg_reg] else param_type;
    switch (param_type.kind) {
        .construct_any, .array, .raw_ptr, .enum_instance => {
            // A Kira String passed to a CString parameter is copied into a fresh
            // NUL-terminated buffer (the callee expects char*, not {ptr,len}). The buffer
            // is a transient owned by this call site; record it so it is freed afterwards.
            if (arg_type.kind == .string and param_type.name != null and std.mem.eql(u8, param_type.name.?, "CString")) {
                const str_ptr = api.LLVMBuildExtractValue(b, arg, 0, "carg.str.ptr");
                const str_len = api.LLVMBuildExtractValue(b, arg, 1, "carg.str.len");
                const alloc_len = api.LLVMBuildAdd(b, str_len, api.LLVMConstInt(fc.types.i64, 1, 0), "carg.str.alloclen");
                var malloc_args = [_]llvm.c.LLVMValueRef{alloc_len};
                const buf = api.LLVMBuildCall2(b, fc.runtime_decls.malloc.ty, fc.runtime_decls.malloc.fn_value, &malloc_args, malloc_args.len, "carg.buf");
                var copy_args = [_]llvm.c.LLVMValueRef{ buf, str_ptr, str_len };
                _ = api.LLVMBuildCall2(b, fc.runtime_decls.memcpy.ty, fc.runtime_decls.memcpy.fn_value, &copy_args, copy_args.len, "carg.memcpy");
                var nul_idx = [_]llvm.c.LLVMValueRef{str_len};
                const nul_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i8, buf, &nul_idx, nul_idx.len, "carg.nul.slot");
                _ = api.LLVMBuildStore(b, api.LLVMConstInt(fc.types.i8, 0, 0), nul_slot);
                try cstr_temps.append(buf);
                return buf;
            }
            return api.LLVMBuildIntToPtr(b, arg, fc.types.ptr_ty, "carg.ptr");
        },
        .integer => {
            const abi = intAbiType(fc.types, param_type.name);
            if (abi == fc.types.i64) return arg;
            return api.LLVMBuildTrunc(b, arg, abi, "carg.itrunc");
        },
        .float => {
            const abi = floatAbiType(fc.types, param_type.name);
            if (abi == fc.types.double_ty) return arg;
            return api.LLVMBuildFPTrunc(b, arg, abi, "carg.ftrunc");
        },
        .ffi_struct => {
            const struct_ty = fc.struct_types.get(param_type.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
            const ptr = api.LLVMBuildIntToPtr(b, arg, fc.types.ptr_ty, "carg.struct.ptr");
            const size = try utils.valueAbiSize(fc.request.program.programPtr(), param_type);
            if (size <= 8) {
                const abi = structAbiInt(api, fc.types, size);
                return api.LLVMBuildLoad2(b, abi, ptr, "carg.struct.int");
            }
            return api.LLVMBuildLoad2(b, struct_ty, ptr, "carg.struct.val");
        },
        else => return arg,
    }
}

// Convert an extern call's C ABI result back into the Kira register representation and
// store it into `dst`.
fn storeResult(fc: *FunctionCodegen, dst: u32, ret_type: ir.ValueType, result: llvm.c.LLVMValueRef, sret_ptr: ?llvm.c.LLVMValueRef) !void {
    const api = fc.api;
    const b = fc.builder;
    if (sret_ptr) |ptr| {
        fc.registers[dst] = api.LLVMBuildPtrToInt(b, ptr, fc.types.i64, "cret.sret.int");
        return;
    }
    switch (ret_type.kind) {
        .construct_any, .array, .raw_ptr, .enum_instance => {
            fc.registers[dst] = api.LLVMBuildPtrToInt(b, result, fc.types.i64, "cret.ptrint");
        },
        .ffi_struct => {
            const struct_ty = fc.struct_types.get(ret_type.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
            const slot = fc.entryAlloca(struct_ty, "cret.struct.slot");
            _ = api.LLVMBuildStore(b, result, slot);
            fc.registers[dst] = api.LLVMBuildPtrToInt(b, slot, fc.types.i64, "cret.struct.int");
        },
        .integer => {
            const abi = intAbiType(fc.types, ret_type.name);
            fc.registers[dst] = if (abi == fc.types.i64) result else api.LLVMBuildSExt(b, result, fc.types.i64, "cret.sext");
        },
        .float => {
            const abi = floatAbiType(fc.types, ret_type.name);
            fc.registers[dst] = if (abi == fc.types.double_ty) result else api.LLVMBuildFPExt(b, result, fc.types.double_ty, "cret.fpext");
        },
        else => fc.registers[dst] = result,
    }
}

// Lower a direct call to an extern (C ABI) function: marshal arguments, emit the call
// with the extern signature, and convert the result. Owned extern results are not
// drop-tracked (the C library owns the storage); this is conservative — it can leak but
// never double-frees, matching the C-API drop model's "escape = stop tracking" rule.
pub fn lowerExternCall(fc: *FunctionCodegen, call: ir.Call, callee_decl: ir.Function) !void {
    const api = fc.api;
    const b = fc.builder;
    const callee_fn = fc.functions.get(call.callee) orelse return error.MissingFunctionDeclaration;
    const fn_ty = try externFunctionType(fc.allocator, api, fc.types, fc.struct_types, fc.request.program.programPtr(), callee_decl);

    const sret = usesSret(fc.request.program.programPtr(), callee_decl.return_type);
    const extra: usize = if (sret) 1 else 0;
    const args = try fc.allocator.alloc(llvm.c.LLVMValueRef, call.args.len + extra);
    defer fc.allocator.free(args);

    var sret_ptr: ?llvm.c.LLVMValueRef = null;
    if (sret) {
        const struct_ty = fc.struct_types.get(callee_decl.return_type.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
        const slot = fc.entryAlloca(struct_ty, "cret.sret.slot");
        sret_ptr = slot;
        args[0] = slot;
    }
    var cstr_temps = std.array_list.Managed(llvm.c.LLVMValueRef).init(fc.allocator);
    defer cstr_temps.deinit();
    for (call.args, 0..) |arg, index| {
        const param_type = if (index < callee_decl.param_types.len) callee_decl.param_types[index] else fc.register_types[arg];
        args[index + extra] = try marshalArg(fc, param_type, arg, &cstr_temps);
    }

    const result = api.LLVMBuildCall2(b, fn_ty, callee_fn, args.ptr, @intCast(args.len), "");

    // Free each transient String->CString buffer now that the call has consumed it. A C
    // `const char*` parameter is borrowed for the call's duration only; leaving these
    // unfreed leaks one buffer per extern string argument per call (every text-draw frame).
    for (cstr_temps.items) |buf| {
        var free_args = [_]llvm.c.LLVMValueRef{buf};
        _ = api.LLVMBuildCall2(b, fc.runtime_decls.free.ty, fc.runtime_decls.free.fn_value, &free_args, free_args.len, "");
    }

    // An owned aggregate passed to the extern escapes Kira's tracking (the C side may
    // retain it); stop tracking so we neither free-while-live nor double-free.
    if (fc.drop_enabled) {
        for (call.args) |arg| drop.onEscape(fc, arg);
    }

    if (call.dst) |dst| try storeResult(fc, dst, callee_decl.return_type, result, sret_ptr);
}
