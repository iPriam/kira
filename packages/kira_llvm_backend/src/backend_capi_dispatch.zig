// call_value dispatch for the LLVM C-API backend. A function value is an i64:
// a small value is a direct function id; otherwise the high bit is set and the
// rest is a heap closure pointer { i64 fn_id, i64 count, [N x bridge] }. Each
// distinct call signature gets one dispatcher that switches on the id and, for
// closures, appends the unpacked captures as trailing arguments.
const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const backend_api = @import("kira_backend_api");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const capi = @import("backend_capi.zig");

const resolveExecution = utils.resolveExecution;
const allocPrintZ = utils.allocPrintZ;

// A function value is carried as an i64: a small value (<= 0xFFFFFFFF) is a direct
// function id; otherwise the high bit is set and the rest is a pointer to a heap
// closure { i64 fn_id, i64 count, [N x bridge] }. Each distinct call signature gets
// one dispatcher function that switches on the id and (for closures) appends the
// unpacked captures as trailing arguments.

pub const DispatcherDecl = struct {
    fn_ty: llvm.c.LLVMTypeRef,
    fn_value: llvm.c.LLVMValueRef,
};

pub const DispatcherSig = struct {
    hash: u64,
    param_types: []const ir.ValueType,
    return_type: ir.ValueType,
};

fn sameValueType(lhs: ir.ValueType, rhs: ir.ValueType) bool {
    if (lhs.kind != rhs.kind) return false;
    if (lhs.name == null or rhs.name == null) return lhs.name == null and rhs.name == null;
    return std.mem.eql(u8, lhs.name.?, rhs.name.?);
}

fn sameSignature(lp: []const ir.ValueType, lr: ir.ValueType, rp: []const ir.ValueType, rr: ir.ValueType) bool {
    if (lp.len != rp.len) return false;
    for (lp, 0..) |l, i| if (!sameValueType(l, rp[i])) return false;
    return sameValueType(lr, rr);
}

fn closureSignature(disp_params: []const ir.ValueType, disp_return: ir.ValueType, fn_params: []const ir.ValueType, fn_return: ir.ValueType) bool {
    if (!sameValueType(disp_return, fn_return)) return false;
    if (fn_params.len <= disp_params.len) return false;
    for (disp_params, 0..) |pt, i| if (!sameValueType(pt, fn_params[i])) return false;
    return true;
}

pub fn hashCallValueSignature(param_types: []const ir.ValueType, return_type: ir.ValueType) u64 {
    var hasher = std.hash.Wyhash.init(0);
    for (param_types) |pt| hashValueType(&hasher, pt);
    hasher.update(&.{0xff});
    hashValueType(&hasher, return_type);
    return hasher.final();
}

fn hashValueType(hasher: *std.hash.Wyhash, value_type: ir.ValueType) void {
    hasher.update(&.{@intFromEnum(value_type.kind)});
    if (value_type.name) |name| {
        hasher.update(&.{1});
        hasher.update(name);
    } else {
        hasher.update(&.{0});
    }
}

pub fn dispatcherSymbolName(allocator: std.mem.Allocator, hash: u64) ![:0]u8 {
    return allocPrintZ(allocator, "kira_capi_dispatch_{x}", .{hash});
}

// Collect the distinct call_value signatures the program needs dispatchers for.
pub fn collectCallValueDispatchers(allocator: std.mem.Allocator, program: ir.Program) ![]DispatcherSig {
    var out = std.array_list.Managed(DispatcherSig).init(allocator);
    for (program.functions) |function_decl| {
        for (function_decl.instructions) |instruction| {
            if (instruction != .call_value) continue;
            const cv = instruction.call_value;
            const hash = hashCallValueSignature(cv.param_types, cv.return_type);
            var found = false;
            for (out.items) |existing| {
                if (existing.hash == hash) {
                    found = true;
                    break;
                }
            }
            if (!found) try out.append(.{ .hash = hash, .param_types = cv.param_types, .return_type = cv.return_type });
        }
    }
    return out.toOwnedSlice();
}

pub fn unpackBridgeValue(api: *const llvm.Api, b: llvm.c.LLVMBuilderRef, types: capi.Types, value_type: ir.ValueType, bv: llvm.c.LLVMValueRef) !llvm.c.LLVMValueRef {
    const payload = api.LLVMBuildExtractValue(b, bv, 2, "bv.get.payload");
    return switch (value_type.kind) {
        .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => payload,
        .float => blk: {
            const d = api.LLVMBuildBitCast(b, payload, types.double_ty, "bv.get.double");
            break :blk if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
                api.LLVMBuildFPTrunc(b, d, types.float_ty, "bv.get.f32")
            else
                d;
        },
        .boolean => api.LLVMBuildTrunc(b, payload, types.bool_ty, "bv.get.bool"),
        .string => blk: {
            const extra = api.LLVMBuildExtractValue(b, bv, 3, "bv.get.extra");
            const sp = api.LLVMBuildIntToPtr(b, payload, types.ptr_ty, "bv.get.strptr");
            var s = api.LLVMConstNull(types.string_ty);
            s = api.LLVMBuildInsertValue(b, s, sp, 0, "bv.get.str0");
            s = api.LLVMBuildInsertValue(b, s, extra, 1, "bv.get.str1");
            break :blk s;
        },
        .void => error.UnsupportedExecutableFeature,
    };
}

const DispatchCtx = struct {
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    types: capi.Types,
    request: backend_api.CompileRequest,
    functions: *const std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef),
    runtime_decls: capi.RuntimeDecls,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    sig: DispatcherSig,
    fn_value: llvm.c.LLVMValueRef,
};

// A function value's callee can target either a native function or, in hybrid mode,
// a VM-resident (.runtime) function dispatched through kira_hybrid_call_runtime.
fn dispatchable(ctx: DispatchCtx, fd: ir.Function) bool {
    const e = resolveExecution(fd.execution, ctx.request.mode);
    return e == .native or (ctx.request.mode == .hybrid and e == .runtime);
}

fn dispatchReturn(ctx: DispatchCtx, value: ?llvm.c.LLVMValueRef) void {
    const api = ctx.api;
    if (ctx.sig.return_type.kind == .void) {
        _ = api.LLVMBuildRetVoid(ctx.builder);
    } else {
        _ = api.LLVMBuildRet(ctx.builder, value.?);
    }
}

// Emit one dispatch case. `capture_bridges` are the raw bridge values loaded from a
// closure's slots (empty for a direct function-id call). Native callees take the
// captures as unpacked trailing args; runtime callees take leading args + captures
// (already bridge values) through kira_hybrid_call_runtime.
fn emitCallee(ctx: DispatchCtx, fd: ir.Function, leading: []const llvm.c.LLVMValueRef, capture_bridges: []const llvm.c.LLVMValueRef) !void {
    const api = ctx.api;
    const b = ctx.builder;
    if (resolveExecution(fd.execution, ctx.request.mode) == .native) {
        const callee = ctx.functions.get(fd.id) orelse return error.MissingFunctionDeclaration;
        const callee_ty = try ctx.types.functionType(ctx.allocator, fd);
        const args = try ctx.allocator.alloc(llvm.c.LLVMValueRef, leading.len + capture_bridges.len);
        defer ctx.allocator.free(args);
        @memcpy(args[0..leading.len], leading);
        for (capture_bridges, 0..) |bv, i| {
            args[leading.len + i] = try unpackBridgeValue(api, b, ctx.types, fd.param_types[leading.len + i], bv);
        }
        const result = api.LLVMBuildCall2(b, callee_ty, callee, args.ptr, @intCast(args.len), "");
        dispatchReturn(ctx, if (ctx.sig.return_type.kind == .void) null else result);
        return;
    }
    // Runtime callee (hybrid): pack leading args + captures into a bridge array and
    // call the VM. fd.param_types.len = leading.len + capture count.
    const rt = ctx.runtime_decls.call_runtime orelse return error.RuntimeCallInNativeBuild;
    const param_count = fd.param_types.len;
    const arr_ty = api.LLVMArrayType2(ctx.types.bridge_ty, param_count);
    const arr = api.LLVMBuildAlloca(b, arr_ty, "disp.rt.args");
    for (leading, 0..) |value, i| {
        var idx = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(ctx.types.i32, 0, 0), api.LLVMConstInt(ctx.types.i32, @intCast(i), 0) };
        const slot = api.LLVMBuildInBoundsGEP2(b, arr_ty, arr, &idx, idx.len, "disp.rt.slot");
        const bv = try packBridgeValue(api, b, ctx.types, ctx.struct_types, ctx.runtime_decls.malloc, fd.param_types[i], value, false);
        _ = api.LLVMBuildStore(b, bv, slot);
    }
    for (capture_bridges, 0..) |bv, j| {
        var idx = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(ctx.types.i32, 0, 0), api.LLVMConstInt(ctx.types.i32, @intCast(leading.len + j), 0) };
        const slot = api.LLVMBuildInBoundsGEP2(b, arr_ty, arr, &idx, idx.len, "disp.rt.slot");
        _ = api.LLVMBuildStore(b, bv, slot);
    }
    const result_slot = api.LLVMBuildAlloca(b, ctx.types.bridge_ty, "disp.rt.result");
    var rt_args = [_]llvm.c.LLVMValueRef{
        api.LLVMConstInt(ctx.types.i32, fd.id, 0),
        arr,
        api.LLVMConstInt(ctx.types.i32, @intCast(param_count), 0),
        result_slot,
    };
    _ = api.LLVMBuildCall2(b, rt.ty, rt.fn_value, &rt_args, rt_args.len, "");
    if (ctx.sig.return_type.kind == .void) {
        _ = api.LLVMBuildRetVoid(b);
    } else {
        const out_bv = api.LLVMBuildLoad2(b, ctx.types.bridge_ty, result_slot, "disp.rt.bv");
        dispatchReturn(ctx, try unpackBridgeValue(api, b, ctx.types, ctx.sig.return_type, out_bv));
    }
}

pub fn buildDispatcher(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    types: capi.Types,
    request: backend_api.CompileRequest,
    functions: *const std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef),
    runtime_decls: capi.RuntimeDecls,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    sig: DispatcherSig,
    fn_value: llvm.c.LLVMValueRef,
) !void {
    const ctx = DispatchCtx{ .allocator = allocator, .api = api, .builder = builder, .types = types, .request = request, .functions = functions, .runtime_decls = runtime_decls, .struct_types = struct_types, .sig = sig, .fn_value = fn_value };
    const ctxp = types.context;

    const function_id = api.LLVMGetParam(fn_value, 0);
    // Dispatcher params after function_id are the call arguments.
    const leading = try allocator.alloc(llvm.c.LLVMValueRef, sig.param_types.len);
    defer allocator.free(leading);
    for (sig.param_types, 0..) |_, i| leading[i] = api.LLVMGetParam(fn_value, @intCast(i + 1));

    const entry = api.LLVMAppendBasicBlockInContext(ctxp, fn_value, "entry");
    const direct = api.LLVMAppendBasicBlockInContext(ctxp, fn_value, "direct");
    const closure = api.LLVMAppendBasicBlockInContext(ctxp, fn_value, "closure");
    const default_block = api.LLVMAppendBasicBlockInContext(ctxp, fn_value, "default");

    api.LLVMPositionBuilderAtEnd(builder, entry);
    const is_direct = api.LLVMBuildICmp(builder, llvm.c.LLVMIntULE, function_id, api.LLVMConstInt(types.i64, 0xFFFFFFFF, 0), "is_direct");
    _ = api.LLVMBuildCondBr(builder, is_direct, direct, closure);

    // Direct path: switch on the function id (native or runtime callee).
    api.LLVMPositionBuilderAtEnd(builder, direct);
    var direct_count: u32 = 0;
    for (request.program.functions) |fd| {
        if (fd.is_extern or !dispatchable(ctx, fd)) continue;
        if (!sameSignature(sig.param_types, sig.return_type, fd.param_types, fd.return_type)) continue;
        direct_count += 1;
    }
    const direct_switch = api.LLVMBuildSwitch(builder, function_id, default_block, direct_count);
    for (request.program.functions) |fd| {
        if (fd.is_extern or !dispatchable(ctx, fd)) continue;
        if (!sameSignature(sig.param_types, sig.return_type, fd.param_types, fd.return_type)) continue;
        const case_block = api.LLVMAppendBasicBlockInContext(ctxp, fn_value, "dcase");
        api.LLVMAddCase(direct_switch, api.LLVMConstInt(types.i64, fd.id, 0), case_block);
        api.LLVMPositionBuilderAtEnd(builder, case_block);
        try emitCallee(ctx, fd, leading, &.{});
    }

    // Closure path: load the function id + captures from the heap closure.
    api.LLVMPositionBuilderAtEnd(builder, closure);
    const raw = api.LLVMBuildAnd(builder, function_id, api.LLVMConstInt(types.i64, 0x7FFFFFFFFFFFFFFF, 0), "closure.raw");
    const closure_ptr = api.LLVMBuildIntToPtr(builder, raw, types.ptr_ty, "closure.ptr");
    const closure_id = api.LLVMBuildLoad2(builder, types.i64, closure_ptr, "closure.id");
    var slots_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(types.i64, 16, 0)};
    const slots = api.LLVMBuildInBoundsGEP2(builder, types.i8, closure_ptr, &slots_idx, slots_idx.len, "closure.slots");

    var closure_count: u32 = 0;
    for (request.program.functions) |fd| {
        if (fd.is_extern or !dispatchable(ctx, fd)) continue;
        if (!closureSignature(sig.param_types, sig.return_type, fd.param_types, fd.return_type)) continue;
        closure_count += 1;
    }
    const closure_switch = api.LLVMBuildSwitch(builder, closure_id, default_block, closure_count);
    for (request.program.functions) |fd| {
        if (fd.is_extern or !dispatchable(ctx, fd)) continue;
        if (!closureSignature(sig.param_types, sig.return_type, fd.param_types, fd.return_type)) continue;
        const case_block = api.LLVMAppendBasicBlockInContext(ctxp, fn_value, "ccase");
        api.LLVMAddCase(closure_switch, api.LLVMConstInt(types.i64, fd.id, 0), case_block);
        api.LLVMPositionBuilderAtEnd(builder, case_block);
        // Load the capture bridge values (the params beyond the leading args).
        const capture_count = fd.param_types.len - sig.param_types.len;
        const capture_bridges = try allocator.alloc(llvm.c.LLVMValueRef, capture_count);
        defer allocator.free(capture_bridges);
        for (0..capture_count) |ci| {
            var slot_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(types.i64, ci, 0)};
            const slot = api.LLVMBuildInBoundsGEP2(builder, types.bridge_ty, slots, &slot_idx, slot_idx.len, "closure.slot");
            capture_bridges[ci] = api.LLVMBuildLoad2(builder, types.bridge_ty, slot, "closure.bv");
        }
        try emitCallee(ctx, fd, leading, capture_bridges);
    }

    // Default: no match — unreachable (the front end guarantees a valid callee).
    api.LLVMPositionBuilderAtEnd(builder, default_block);
    _ = api.LLVMBuildUnreachable(builder);
}

// Pack a register value into a %kira.bridge.value. box_struct=true heap-copies an
// ffi_struct (array/native-state elements own a copy); false stores the pointer
// (closure captures, native-call results). Mirrors FunctionCodegen.packBridgeBoxed.
pub fn packBridgeValue(
    api: *const llvm.Api,
    b: llvm.c.LLVMBuilderRef,
    types: capi.Types,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    malloc_decl: capi.RuntimeDecls.Decl,
    value_type: ir.ValueType,
    value: llvm.c.LLVMValueRef,
    box_struct: bool,
) !llvm.c.LLVMValueRef {
    const tagValue = @import("backend_utils.zig").bridgeTagValue;
    var bv = api.LLVMConstNull(types.bridge_ty);
    bv = api.LLVMBuildInsertValue(b, bv, api.LLVMConstInt(types.i8, tagValue(value_type), 0), 0, "bv.tag");
    switch (value_type.kind) {
        .integer, .construct_any, .raw_ptr, .array, .enum_instance => {
            bv = api.LLVMBuildInsertValue(b, bv, value, 2, "bv.payload");
        },
        .ffi_struct => {
            if (!box_struct) {
                bv = api.LLVMBuildInsertValue(b, bv, value, 2, "bv.payload");
            } else {
                const struct_ty = struct_types.get(value_type.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
                const src = api.LLVMBuildIntToPtr(b, value, types.ptr_ty, "bv.struct.src");
                const loaded = api.LLVMBuildLoad2(b, struct_ty, src, "bv.struct.val");
                var margs = [_]llvm.c.LLVMValueRef{api.LLVMSizeOf(struct_ty)};
                const copy = api.LLVMBuildCall2(b, malloc_decl.ty, malloc_decl.fn_value, &margs, margs.len, "bv.struct.copy");
                _ = api.LLVMBuildStore(b, loaded, copy);
                bv = api.LLVMBuildInsertValue(b, bv, api.LLVMBuildPtrToInt(b, copy, types.i64, "bv.struct.int"), 2, "bv.payload");
            }
        },
        .float => {
            const as_double = if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
                api.LLVMBuildFPExt(b, value, types.double_ty, "bv.fpext")
            else
                value;
            bv = api.LLVMBuildInsertValue(b, bv, api.LLVMBuildBitCast(b, as_double, types.i64, "bv.fbits"), 2, "bv.payload");
        },
        .boolean => bv = api.LLVMBuildInsertValue(b, bv, api.LLVMBuildZExt(b, value, types.i64, "bv.bool"), 2, "bv.payload"),
        .string => {
            const sp = api.LLVMBuildExtractValue(b, value, 0, "bv.str.ptr");
            bv = api.LLVMBuildInsertValue(b, bv, api.LLVMBuildPtrToInt(b, sp, types.i64, "bv.str.ptrint"), 2, "bv.payload");
            bv = api.LLVMBuildInsertValue(b, bv, api.LLVMBuildExtractValue(b, value, 1, "bv.str.len"), 3, "bv.extra");
        },
        .void => {},
    }
    return bv;
}

// Hybrid mode: build the `kira_native_fn_{id}(ptr args, i32 arg_count, ptr out)`
// trampoline the VM calls. It unpacks bridge-value args, calls kira_native_impl_{id},
// and packs the result back into the out slot. Mirrors backend_utils.buildHybridBridgeWrapper.
pub fn buildHybridTrampoline(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    module_ref: llvm.c.LLVMModuleRef,
    types: capi.Types,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    malloc_decl: capi.RuntimeDecls.Decl,
    function_decl: ir.Function,
    impl_fn: llvm.c.LLVMValueRef,
    impl_ty: llvm.c.LLVMTypeRef,
) !void {
    var params = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.i32, types.ptr_ty };
    const fn_ty = api.LLVMFunctionType(types.void_ty, &params, params.len, 0);
    const name = try allocPrintZ(allocator, "kira_native_fn_{d}", .{function_decl.id});
    defer allocator.free(name);
    const fn_value = api.LLVMAddFunction(module_ref, name.ptr, fn_ty);
    if (builtin.os.tag == .windows) {
        api.LLVMSetDLLStorageClass(fn_value, llvm.c.LLVMDLLExportStorageClass);
    }
    const entry = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "entry");
    api.LLVMPositionBuilderAtEnd(builder, entry);

    const args_ptr = api.LLVMGetParam(fn_value, 0);
    const out_ptr = api.LLVMGetParam(fn_value, 2);

    const call_args = try allocator.alloc(llvm.c.LLVMValueRef, function_decl.param_types.len);
    defer allocator.free(call_args);
    for (function_decl.param_types, 0..) |param_type, index| {
        var idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(types.i64, @intCast(index), 0)};
        const slot = api.LLVMBuildInBoundsGEP2(builder, types.bridge_ty, args_ptr, &idx, idx.len, "tramp.slot");
        const bv = api.LLVMBuildLoad2(builder, types.bridge_ty, slot, "tramp.bv");
        call_args[index] = try unpackBridgeValue(api, builder, types, param_type, bv);
    }
    const result = api.LLVMBuildCall2(builder, impl_ty, impl_fn, call_args.ptr, @intCast(call_args.len), "");
    // Pack the result (or a void bridge) into the out slot. Native results store the
    // struct pointer directly (box_struct=false), matching the writer.
    const out_bv = try packBridgeValue(api, builder, types, struct_types, malloc_decl, function_decl.return_type, result, false);
    _ = api.LLVMBuildStore(builder, out_bv, out_ptr);
    _ = api.LLVMBuildRetVoid(builder);
}
