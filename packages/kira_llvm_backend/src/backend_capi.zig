// LLVM C-API backend.
//
// This is the successor to the textual-IR "writer" backend. Instead of emitting
// LLVM IR as formatted strings (where SSA register numbers, basic-block
// dominance, and ownership cleanup all have to be tracked by hand and are easy
// to get subtly wrong), this backend drives the LLVM C API directly: it holds
// real `LLVMValueRef`/`LLVMBasicBlockRef` handles, lets LLVM manage SSA, and
// runs the verifier on the in-memory module before emission.
//
// Coverage today is the scalar + control-flow + direct-call core (the compute
// surface of the language). Aggregates (structs, arrays, enums, closures,
// native state, FFI) are not lowered here yet and report a precise
// `error.UnsupportedExecutableFeature` so the caller can fall back to the text
// backend during the migration. The goal is to grow this to full parity and
// then retire the writer.

const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const backend_api = @import("kira_backend_api");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const runtime_symbols = @import("runtime_symbols.zig");
const codegen = @import("backend_capi_codegen.zig");
const dispatch = @import("backend_capi_dispatch.zig");
const drop = @import("backend_capi_drop.zig");
const ffi = @import("backend_capi_ffi.zig");

const functionExecutionById = utils.functionExecutionById;
const functionById = utils.functionById;
const resolveExecution = utils.resolveExecution;
const inferRegisterTypes = utils.inferRegisterTypes;
const allocPrintZ = utils.allocPrintZ;

pub const Lowered = struct {
    context: llvm.c.LLVMContextRef,
    module_ref: llvm.c.LLVMModuleRef,
};

fn dropEnabled() bool {
    // Owned-value drop elaboration is now ON by default (the C-API backend is the default
    // codegen path and must free owned values like the retired text writer did). Set
    // KIRA_CAPI_DROP=0 to opt out during the transition.
    const raw = std.c.getenv("KIRA_CAPI_DROP") orelse return true;
    const value = std.mem.span(raw);
    return value.len != 0 and value[0] != '0';
}

pub fn shouldLowerFunction(execution: runtime_abi.FunctionExecution, mode: backend_api.BackendMode) bool {
    return switch (mode) {
        .llvm_native => switch (execution) {
            .runtime => false,
            .inherited, .native => true,
        },
        .hybrid => execution == .native,
        .vm_bytecode => false,
    };
}

fn functionSymbolName(allocator: std.mem.Allocator, function_decl: ir.Function, mode: backend_api.BackendMode) ![:0]u8 {
    if (function_decl.is_extern) {
        if (function_decl.foreign) |foreign| return allocator.dupeZ(u8, foreign.symbol_name);
    }
    return switch (mode) {
        .llvm_native => allocPrintZ(allocator, "kira_fn_{d}_{s}", .{ function_decl.id, function_decl.name }),
        .hybrid => allocPrintZ(allocator, "kira_native_impl_{d}", .{function_decl.id}),
        .vm_bytecode => unreachable,
    };
}

pub const Types = struct {
    api: *const llvm.Api,
    context: llvm.c.LLVMContextRef,
    bool_ty: llvm.c.LLVMTypeRef,
    i8: llvm.c.LLVMTypeRef,
    i16: llvm.c.LLVMTypeRef,
    i32: llvm.c.LLVMTypeRef,
    i64: llvm.c.LLVMTypeRef,
    double_ty: llvm.c.LLVMTypeRef,
    float_ty: llvm.c.LLVMTypeRef,
    void_ty: llvm.c.LLVMTypeRef,
    ptr_ty: llvm.c.LLVMTypeRef,
    string_ty: llvm.c.LLVMTypeRef,
    bridge_ty: llvm.c.LLVMTypeRef,

    fn init(api: *const llvm.Api, context: llvm.c.LLVMContextRef) Types {
        const ptr_ty = api.LLVMPointerTypeInContext(context, 0);
        const i64_ty = api.LLVMInt64TypeInContext(context);
        const i8_ty = api.LLVMInt8TypeInContext(context);
        var string_fields = [_]llvm.c.LLVMTypeRef{ ptr_ty, i64_ty };
        // Matches the runtime KiraBridgeValue: { tag:i8, pad:[7 x i8], payload:i64, extra:i64 }.
        var bridge_fields = [_]llvm.c.LLVMTypeRef{ i8_ty, api.LLVMArrayType2(i8_ty, 7), i64_ty, i64_ty };
        return .{
            .api = api,
            .context = context,
            .bool_ty = api.LLVMInt1TypeInContext(context),
            .i8 = i8_ty,
            .i16 = api.LLVMInt16TypeInContext(context),
            .i32 = api.LLVMInt32TypeInContext(context),
            .i64 = i64_ty,
            .double_ty = api.LLVMDoubleTypeInContext(context),
            .float_ty = api.LLVMFloatTypeInContext(context),
            .void_ty = api.LLVMVoidTypeInContext(context),
            .ptr_ty = ptr_ty,
            .string_ty = api.LLVMStructTypeInContext(context, &string_fields, string_fields.len, 0),
            .bridge_ty = api.LLVMStructTypeInContext(context, &bridge_fields, bridge_fields.len, 0),
        };
    }

    // Mirrors backend_utils.llvmValueTypeText: Kira `Float` is 64-bit (double);
    // only the F32 named float is 32-bit. Pointers and aggregates are carried as
    // i64 in the register ABI, matching the text backend so cross-backend calls
    // and the runtime helpers stay binary-compatible.
    pub fn llvmType(self: Types, value_type: ir.ValueType) llvm.c.LLVMTypeRef {
        return switch (value_type.kind) {
            .void => self.void_ty,
            .integer => self.i64,
            .float => if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32")) self.float_ty else self.double_ty,
            .string => self.string_ty,
            .boolean => self.bool_ty,
            .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => self.i64,
        };
    }

    pub fn functionType(self: Types, allocator: std.mem.Allocator, function_decl: ir.Function) !llvm.c.LLVMTypeRef {
        const params = try allocator.alloc(llvm.c.LLVMTypeRef, function_decl.param_types.len);
        defer allocator.free(params);
        for (function_decl.param_types, 0..) |param_type, index| params[index] = self.llvmType(param_type);
        const ret = self.llvmType(function_decl.return_type);
        return self.api.LLVMFunctionType(ret, params.ptr, @intCast(params.len), 0);
    }
};

pub const RuntimeDecls = struct {
    print_i64: Decl,
    print_f64: Decl,
    print_string: Decl,
    call_runtime: ?Decl,
    malloc: Decl,
    free: Decl,
    strlen: Decl,
    memcpy: Decl,
    array_alloc: Decl,
    array_len: Decl,
    array_load: Decl,
    array_store: Decl,
    array_store_release: Decl,
    array_append: Decl,
    array_release: Decl,
    array_clone: Decl,
    state_alloc: Decl,
    state_payload: Decl,
    state_recover: Decl,
    struct_alloc: Decl,
    struct_type_id: Decl,
    struct_free: Decl,
    // No-newline writers for composing aggregate (struct/enum/array) output.
    write_i64: Decl,
    write_f64: Decl,
    write_string: Decl,
    write_ptr: Decl,
    write_newline: Decl,
    bool_true: llvm.c.LLVMValueRef,
    bool_false: llvm.c.LLVMValueRef,

    pub const Decl = struct { ty: llvm.c.LLVMTypeRef, fn_value: llvm.c.LLVMValueRef };

    fn declare(api: *const llvm.Api, module_ref: llvm.c.LLVMModuleRef, types: Types, mode: backend_api.BackendMode) RuntimeDecls {
        var i64_args = [_]llvm.c.LLVMTypeRef{types.i64};
        const print_i64_ty = api.LLVMFunctionType(types.void_ty, &i64_args, i64_args.len, 0);
        var f64_args = [_]llvm.c.LLVMTypeRef{types.double_ty};
        const print_f64_ty = api.LLVMFunctionType(types.void_ty, &f64_args, f64_args.len, 0);
        var str_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.i64 };
        const print_string_ty = api.LLVMFunctionType(types.void_ty, &str_args, str_args.len, 0);
        // kira_hybrid_call_runtime(i32 function_id, ptr args, i32 arg_count, ptr result)
        var rt_args = [_]llvm.c.LLVMTypeRef{ types.i32, types.ptr_ty, types.i32, types.ptr_ty };
        const call_runtime_ty = if (mode == .hybrid) api.LLVMFunctionType(types.void_ty, &rt_args, rt_args.len, 0) else null;
        var malloc_args = [_]llvm.c.LLVMTypeRef{types.i64};
        const malloc_ty = api.LLVMFunctionType(types.ptr_ty, &malloc_args, malloc_args.len, 0);
        var free_args = [_]llvm.c.LLVMTypeRef{types.ptr_ty};
        const free_ty = api.LLVMFunctionType(types.void_ty, &free_args, free_args.len, 0);
        var strlen_args = [_]llvm.c.LLVMTypeRef{types.ptr_ty};
        const strlen_ty = api.LLVMFunctionType(types.i64, &strlen_args, strlen_args.len, 0);
        var memcpy_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.ptr_ty, types.i64 };
        const memcpy_ty = api.LLVMFunctionType(types.ptr_ty, &memcpy_args, memcpy_args.len, 0);
        var alloc_args = [_]llvm.c.LLVMTypeRef{types.i64};
        const array_alloc_ty = api.LLVMFunctionType(types.ptr_ty, &alloc_args, alloc_args.len, 0);
        var len_args = [_]llvm.c.LLVMTypeRef{types.ptr_ty};
        const array_len_ty = api.LLVMFunctionType(types.i64, &len_args, len_args.len, 0);
        var load_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.i64, types.ptr_ty };
        const array_load_ty = api.LLVMFunctionType(types.void_ty, &load_args, load_args.len, 0);
        var store_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.i64, types.ptr_ty };
        const array_store_ty = api.LLVMFunctionType(types.void_ty, &store_args, store_args.len, 0);
        var store_release_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.i64, types.ptr_ty, types.ptr_ty };
        const array_store_release_ty = api.LLVMFunctionType(types.void_ty, &store_release_args, store_release_args.len, 0);
        var append_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.ptr_ty };
        const array_append_ty = api.LLVMFunctionType(types.void_ty, &append_args, append_args.len, 0);
        var release_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.ptr_ty };
        const array_release_ty = api.LLVMFunctionType(types.void_ty, &release_args, release_args.len, 0);
        var clone_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.ptr_ty };
        const array_clone_ty = api.LLVMFunctionType(types.ptr_ty, &clone_args, clone_args.len, 0);
        var state_alloc_args = [_]llvm.c.LLVMTypeRef{ types.i64, types.i64 };
        const state_alloc_ty = api.LLVMFunctionType(types.ptr_ty, &state_alloc_args, state_alloc_args.len, 0);
        var state_payload_args = [_]llvm.c.LLVMTypeRef{types.ptr_ty};
        const state_payload_ty = api.LLVMFunctionType(types.ptr_ty, &state_payload_args, state_payload_args.len, 0);
        var state_recover_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.i64 };
        const state_recover_ty = api.LLVMFunctionType(types.ptr_ty, &state_recover_args, state_recover_args.len, 0);
        var struct_alloc_args = [_]llvm.c.LLVMTypeRef{ types.i64, types.i64 };
        const struct_alloc_ty = api.LLVMFunctionType(types.ptr_ty, &struct_alloc_args, struct_alloc_args.len, 0);
        var struct_type_id_args = [_]llvm.c.LLVMTypeRef{types.ptr_ty};
        const struct_type_id_ty = api.LLVMFunctionType(types.i64, &struct_type_id_args, struct_type_id_args.len, 0);
        const struct_free_ty = api.LLVMFunctionType(types.void_ty, &struct_type_id_args, struct_type_id_args.len, 0);
        const write_newline_ty = api.LLVMFunctionType(types.void_ty, null, 0, 0);

        return .{
            .print_i64 = .{ .ty = print_i64_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.print_i64, print_i64_ty) },
            .print_f64 = .{ .ty = print_f64_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.print_f64, print_f64_ty) },
            .print_string = .{ .ty = print_string_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.print_string, print_string_ty) },
            .call_runtime = if (call_runtime_ty) |ty| .{ .ty = ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.call_runtime, ty) } else null,
            .malloc = .{ .ty = malloc_ty, .fn_value = api.LLVMAddFunction(module_ref, "malloc", malloc_ty) },
            .free = .{ .ty = free_ty, .fn_value = api.LLVMAddFunction(module_ref, "free", free_ty) },
            .strlen = .{ .ty = strlen_ty, .fn_value = api.LLVMAddFunction(module_ref, "strlen", strlen_ty) },
            .memcpy = .{ .ty = memcpy_ty, .fn_value = api.LLVMAddFunction(module_ref, "memcpy", memcpy_ty) },
            .array_alloc = .{ .ty = array_alloc_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.array_alloc, array_alloc_ty) },
            .array_len = .{ .ty = array_len_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.array_len, array_len_ty) },
            .array_load = .{ .ty = array_load_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.array_load, array_load_ty) },
            .array_store = .{ .ty = array_store_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.array_store, array_store_ty) },
            .array_store_release = .{ .ty = array_store_release_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_array_store_release", array_store_release_ty) },
            .array_append = .{ .ty = array_append_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_array_append", array_append_ty) },
            .array_release = .{ .ty = array_release_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_array_release", array_release_ty) },
            .array_clone = .{ .ty = array_clone_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_array_clone", array_clone_ty) },
            .state_alloc = .{ .ty = state_alloc_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.native_state_alloc, state_alloc_ty) },
            .state_payload = .{ .ty = state_payload_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.native_state_payload, state_payload_ty) },
            .state_recover = .{ .ty = state_recover_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.native_state_recover, state_recover_ty) },
            .struct_alloc = .{ .ty = struct_alloc_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.struct_alloc, struct_alloc_ty) },
            .struct_type_id = .{ .ty = struct_type_id_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.struct_type_id, struct_type_id_ty) },
            .struct_free = .{ .ty = struct_free_ty, .fn_value = api.LLVMAddFunction(module_ref, runtime_symbols.struct_free, struct_free_ty) },
            .write_i64 = .{ .ty = print_i64_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_native_write_i64", print_i64_ty) },
            .write_f64 = .{ .ty = print_f64_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_native_write_f64", print_f64_ty) },
            .write_string = .{ .ty = print_string_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_native_write_string", print_string_ty) },
            .write_ptr = .{ .ty = print_i64_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_native_write_ptr", print_i64_ty) },
            .write_newline = .{ .ty = write_newline_ty, .fn_value = api.LLVMAddFunction(module_ref, "kira_native_write_newline", write_newline_ty) },
            .bool_true = boolGlobal(api, module_ref, types, "true", "kira.capi.bool.true"),
            .bool_false = boolGlobal(api, module_ref, types, "false", "kira.capi.bool.false"),
        };
    }
};

fn boolGlobal(api: *const llvm.Api, module_ref: llvm.c.LLVMModuleRef, types: Types, text: []const u8, name: [:0]const u8) llvm.c.LLVMValueRef {
    const array_ty = api.LLVMArrayType2(types.i8, text.len + 1);
    const global = api.LLVMAddGlobal(module_ref, array_ty, name.ptr);
    api.LLVMSetLinkage(global, llvm.c.LLVMPrivateLinkage);
    api.LLVMSetGlobalConstant(global, 1);
    api.LLVMSetInitializer(global, api.LLVMConstStringInContext2(types.context, text.ptr, text.len, 0));
    const zero = api.LLVMConstInt(types.i32, 0, 0);
    var indices = [_]llvm.c.LLVMValueRef{ zero, zero };
    const data_ptr = api.LLVMConstInBoundsGEP2(array_ty, global, &indices, indices.len);
    var fields = [_]llvm.c.LLVMValueRef{ data_ptr, api.LLVMConstInt(types.i64, text.len, 0) };
    return api.LLVMConstNamedStruct(types.string_ty, &fields, fields.len);
}

pub fn buildModule(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    request: backend_api.CompileRequest,
    triple: []const u8,
) !Lowered {
    const context = api.LLVMContextCreate();
    const module_name = try allocator.dupeZ(u8, request.module_name);
    defer allocator.free(module_name);
    const module_ref = api.LLVMModuleCreateWithNameInContext(module_name.ptr, context);
    const triple_z = try allocator.dupeZ(u8, triple);
    defer allocator.free(triple_z);
    api.LLVMSetTarget(module_ref, triple_z.ptr);

    const builder = api.LLVMCreateBuilderInContext(context);
    defer api.LLVMDisposeBuilder(builder);

    const types = Types.init(api, context);
    const runtime_decls = RuntimeDecls.declare(api, module_ref, types, request.mode);

    var struct_types = std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef){};
    defer struct_types.deinit(allocator);
    try buildStructTypes(allocator, api, types, request.program.programPtr(), &struct_types);

    // The per-type destructor/clone helpers are always generated: the clone helpers
    // (kira_clone_contents_<T>) implement affine deep-copy in copy_indirect, which is the
    // DEFAULT value semantics for pure-Kira value structs regardless of drop. Owned-value
    // FREEING (the cleanup-slot driver, scope-exit drops) stays opt-in behind
    // KIRA_CAPI_DROP while it is validated; with drop off, the destroy helpers are simply
    // never called, so generating them is free of behavior change or double-free risk.
    const drop_enabled = dropEnabled();
    var dtors: drop.Destructors = try drop.build(allocator, api, module_ref, types, &struct_types, request.program.programPtr(), runtime_decls);
    defer dtors.deinit(allocator);

    // Declare one dispatcher function per distinct call_value signature; bodies are
    // generated after the concrete functions are declared.
    const dispatcher_sigs = try dispatch.collectCallValueDispatchers(allocator, request.program.programPtr().*);
    defer allocator.free(dispatcher_sigs);
    var dispatchers = std.AutoHashMapUnmanaged(u64, dispatch.DispatcherDecl){};
    defer dispatchers.deinit(allocator);
    for (dispatcher_sigs) |sig| {
        const params = try allocator.alloc(llvm.c.LLVMTypeRef, sig.param_types.len + 1);
        defer allocator.free(params);
        params[0] = types.i64;
        for (sig.param_types, 0..) |pt, i| params[i + 1] = types.llvmType(pt);
        const fn_ty = api.LLVMFunctionType(types.llvmType(sig.return_type), params.ptr, @intCast(params.len), 0);
        const name = try dispatch.dispatcherSymbolName(allocator, sig.hash);
        defer allocator.free(name);
        const fn_value = api.LLVMAddFunction(module_ref, name.ptr, fn_ty);
        try dispatchers.put(allocator, sig.hash, .{ .fn_ty = fn_ty, .fn_value = fn_value });
    }

    var functions = std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef){};
    defer functions.deinit(allocator);

    for (request.program.programPtr().functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
        // Extern functions are real C symbols: declare them with their C ABI signature so
        // call sites (lowered through backend_capi_ffi) match the declaration.
        const function_ty = if (function_decl.is_extern)
            try ffi.externFunctionType(allocator, api, types, &struct_types, request.program.programPtr(), function_decl)
        else
            try types.functionType(allocator, function_decl);
        const name = try functionSymbolName(allocator, function_decl, request.mode);
        defer allocator.free(name);
        const function_value = api.LLVMAddFunction(module_ref, name.ptr, function_ty);
        if (request.mode == .hybrid and builtin.os.tag == .windows) {
            api.LLVMSetDLLStorageClass(function_value, llvm.c.LLVMDLLExportStorageClass);
        }
        try functions.put(allocator, function_decl.id, function_value);
    }

    for (request.program.programPtr().functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
        if (function_decl.is_extern) continue;
        const function_value = functions.get(function_decl.id) orelse return error.MissingFunctionDeclaration;
        var fc = codegen.FunctionCodegen{
            .allocator = allocator,
            .api = api,
            .builder = builder,
            .module_ref = module_ref,
            .types = types,
            .runtime_decls = runtime_decls,
            .struct_types = &struct_types,
            .dispatchers = &dispatchers,
            .dtors = &dtors,
            .drop_enabled = drop_enabled,
            .request = request,
            .functions = &functions,
            .function_decl = function_decl,
            .function_value = function_value,
        };
        try fc.lower();
    }

    // Generate the dispatcher bodies now that all concrete functions exist.
    for (dispatcher_sigs) |sig| {
        const decl = dispatchers.get(sig.hash).?;
        try dispatch.buildDispatcher(allocator, api, builder, types, request, &functions, runtime_decls, &struct_types, sig, decl.fn_value);
    }

    // Hybrid mode: emit the kira_native_fn_{id} trampoline the VM calls for each
    // native function, wrapping the kira_native_impl_{id} body.
    if (request.mode == .hybrid) {
        for (request.program.programPtr().functions) |function_decl| {
            if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
            if (function_decl.is_extern) continue;
            const impl_fn = functions.get(function_decl.id) orelse return error.MissingFunctionDeclaration;
            const impl_ty = try types.functionType(allocator, function_decl);
            try dispatch.buildHybridTrampoline(allocator, api, builder, module_ref, types, &struct_types, runtime_decls.malloc, function_decl, impl_fn, impl_ty);
        }
    }

    if (request.mode == .llvm_native) {
        const entry_decl = request.program.programPtr().functions[request.program.programPtr().entry_index];
        if (!shouldLowerFunction(entry_decl.execution, request.mode)) return error.RuntimeEntrypointInNativeBuild;
        const entry_value = functions.get(entry_decl.id) orelse return error.MissingFunctionDeclaration;
        try buildHostMain(allocator, api, builder, module_ref, types, entry_decl, entry_value);
    }

    if (std.c.getenv("KIRA_CAPI_DUMP") != null) {
        const text = api.LLVMPrintModuleToString(module_ref);
        defer api.LLVMDisposeMessage(text);
        std.debug.print("{s}\n", .{std.mem.span(text)});
    }
    try verifyModule(api, module_ref);
    return .{ .context = context, .module_ref = module_ref };
}

// Build a named LLVM struct type for every Kira struct/class declaration, mirroring
// backend_utils.appendTypeDefinitions. Two passes (create-named then set-body) so
// that fields referencing other (or the same) struct types resolve.
fn buildStructTypes(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    types: Types,
    program: *const ir.Program,
    out: *std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
) !void {
    for (program.types) |type_decl| {
        if (type_decl.ffi) |ffi_info| {
            if (ffi_info != .ffi_struct) continue;
        }
        const name_z = try allocPrintZ(allocator, "t.{s}", .{type_decl.name});
        defer allocator.free(name_z);
        const struct_ty = api.LLVMStructCreateNamed(types.context, name_z.ptr);
        try out.put(allocator, type_decl.name, struct_ty);
    }
    for (program.types) |type_decl| {
        if (type_decl.ffi) |ffi_info| {
            if (ffi_info != .ffi_struct) continue;
        }
        const struct_ty = out.get(type_decl.name).?;
        if (type_decl.fields.len == 0) {
            var one = [_]llvm.c.LLVMTypeRef{types.i8};
            api.LLVMStructSetBody(struct_ty, &one, one.len, 0);
            continue;
        }
        const elements = try allocator.alloc(llvm.c.LLVMTypeRef, type_decl.fields.len);
        defer allocator.free(elements);
        for (type_decl.fields, 0..) |field_decl, index| {
            elements[index] = try fieldStorageType(types, out.*, program, field_decl.ty);
        }
        api.LLVMStructSetBody(struct_ty, elements.ptr, @intCast(elements.len), 0);
    }
}

// In-struct storage type for a field, mirroring backend_utils.llvmFieldAbiTypeText:
// bool is stored as i8 (i1 in registers), arrays/pointers/constructs as a raw ptr,
// nested ffi_struct fields are stored inline by value, strings as %kira.string.
pub fn fieldStorageType(
    types: Types,
    struct_types: std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    program: *const ir.Program,
    value_type: ir.ValueType,
) !llvm.c.LLVMTypeRef {
    return switch (value_type.kind) {
        .void => error.UnsupportedExecutableFeature,
        .string => types.string_ty,
        .boolean => types.i8,
        .integer => intStorageType(types, value_type.name),
        .float => if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32")) types.float_ty else types.double_ty,
        .array => types.ptr_ty,
        .construct_any, .raw_ptr, .enum_instance => blk: {
            // An inline fixed FFI array (`@FFI.Array`) is laid out as `[count x element]`
            // and an FFI alias forwards to its target, mirroring the text backend's
            // llvmFieldAbiTypeText so the two backends agree on C struct layout.
            if (value_type.name) |name| {
                if (utils.findTypeDecl(program, name)) |type_decl| {
                    if (type_decl.ffi) |ffi_info| {
                        switch (ffi_info) {
                            .array => |info| {
                                const element_ty = try fieldStorageType(types, struct_types, program, info.element);
                                break :blk types.api.LLVMArrayType2(element_ty, info.count);
                            },
                            .alias => |info| break :blk try fieldStorageType(types, struct_types, program, info.target),
                            else => {},
                        }
                    }
                }
            }
            break :blk types.ptr_ty;
        },
        .ffi_struct => struct_types.get(value_type.name orelse return error.UnsupportedExecutableFeature) orelse error.UnsupportedExecutableFeature,
    };
}

fn intStorageType(types: Types, name: ?[]const u8) llvm.c.LLVMTypeRef {
    const n = name orelse return types.i64;
    if (std.mem.eql(u8, n, "I8") or std.mem.eql(u8, n, "U8")) return types.i8;
    if (std.mem.eql(u8, n, "I16") or std.mem.eql(u8, n, "U16")) return types.i16;
    if (std.mem.eql(u8, n, "I32") or std.mem.eql(u8, n, "U32")) return types.i32;
    return types.i64;
}

fn buildHostMain(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    module_ref: llvm.c.LLVMModuleRef,
    types: Types,
    entry_decl: ir.Function,
    entry_value: llvm.c.LLVMValueRef,
) !void {
    const main_ty = api.LLVMFunctionType(types.i32, null, 0, 0);
    const main_fn = api.LLVMAddFunction(module_ref, "main", main_ty);
    const entry_block = api.LLVMAppendBasicBlockInContext(types.context, main_fn, "entry");
    api.LLVMPositionBuilderAtEnd(builder, entry_block);
    const entry_fn_ty = try Types.functionType(types, allocator, entry_decl);
    _ = api.LLVMBuildCall2(builder, entry_fn_ty, entry_value, null, 0, "");
    _ = api.LLVMBuildRet(builder, api.LLVMConstInt(types.i32, 0, 0));
}

fn verifyModule(api: *const llvm.Api, module_ref: llvm.c.LLVMModuleRef) !void {
    var error_message: [*c]u8 = null;
    if (api.LLVMVerifyModule(module_ref, llvm.c.LLVMReturnStatusAction, &error_message) != 0) {
        defer if (error_message != null) api.LLVMDisposeMessage(error_message);
        if (error_message != null) std.debug.print("kira capi backend: invalid module:\n{s}\n", .{std.mem.span(error_message)});
        return error.InvalidLlvmModule;
    }
}
