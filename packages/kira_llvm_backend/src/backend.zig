const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const backend_api = @import("kira_backend_api");
const llvm = @import("llvm_c.zig");
const toolchain = @import("toolchain.zig");
const linker = @import("link.zig");
pub const emscripten = @import("emscripten.zig");
const runtime_symbols = @import("runtime_symbols.zig");
const backend_utils = @import("backend_utils.zig");
const monomorphization = @import("backend_monomorphization.zig");
const backend_capi = @import("backend_capi.zig");

pub const MonomorphizationPlan = monomorphization.Plan;
pub const FunctionVariant = monomorphization.FunctionVariant;
pub const buildMonomorphizationPlan = monomorphization.buildPlan;

pub const writePrintInstruction = backend_utils.writePrintInstruction;
pub const appendStringGlobals = backend_utils.appendStringGlobals;
pub const writeLlvmSymbol = backend_utils.writeLlvmSymbol;
pub const writeLlvmStringLiteral = backend_utils.writeLlvmStringLiteral;
pub const writeLlvmEscapedBytes = backend_utils.writeLlvmEscapedBytes;
pub const writeLlvmFloatLiteral = backend_utils.writeLlvmFloatLiteral;
pub const hexDigit = backend_utils.hexDigit;
pub const appendTypeDefinitions = backend_utils.appendTypeDefinitions;
pub const typeRefName = backend_utils.typeRefName;
pub const findTypeDecl = backend_utils.findTypeDecl;
pub const llvmFieldAbiTypeText = backend_utils.llvmFieldAbiTypeText;
pub const integerAbiTypeName = backend_utils.integerAbiTypeName;
pub const floatAbiTypeName = backend_utils.floatAbiTypeName;
pub const llvmValueTypeText = backend_utils.llvmValueTypeText;
pub const llvmCompareValueTypeText = backend_utils.llvmCompareValueTypeText;
pub const llvmComparePredicate = backend_utils.llvmComparePredicate;
pub const llvmCallTypeText = backend_utils.llvmCallTypeText;
pub const externReturnUsesSRet = backend_utils.externReturnUsesSRet;
pub const externReturnAbiTypeText = backend_utils.externReturnAbiTypeText;
pub const externParamAbiTypeText = backend_utils.externParamAbiTypeText;
pub const llvmLocalStorageTypeText = backend_utils.llvmLocalStorageTypeText;
pub const isPointerLikeValueType = backend_utils.isPointerLikeValueType;
pub const fieldIndex = backend_utils.fieldIndex;
pub const fieldType = backend_utils.fieldType;
pub const llvmIndirectLoadTypeText = backend_utils.llvmIndirectLoadTypeText;
pub const llvmFieldStoreValuePrefix = backend_utils.llvmFieldStoreValuePrefix;
pub const writeLlvmLabelName = backend_utils.writeLlvmLabelName;
pub const countStringConstants = backend_utils.countStringConstants;
pub const freeStringList = backend_utils.freeStringList;
pub const freeSymbolNames = backend_utils.freeSymbolNames;
pub const writeTextFile = backend_utils.writeTextFile;
pub const emitObjectFileViaClang = backend_utils.emitObjectFileViaClang;
pub const inferRegisterTypes = backend_utils.inferRegisterTypes;
pub const functionExecutionById = backend_utils.functionExecutionById;
pub const functionById = backend_utils.functionById;
pub const bridgeTagValue = backend_utils.bridgeTagValue;
pub const resolveExecution = backend_utils.resolveExecution;
pub const requiresTextIrFallback = backend_utils.requiresTextIrFallback;
pub const functionDeclNeedsTextIrFallback = backend_utils.functionDeclNeedsTextIrFallback;
pub const hostTargetTriple = backend_utils.hostTargetTriple;
pub const targetTriple = @import("backend_platform_utils.zig").targetTriple;
pub const ensureParentDir = backend_utils.ensureParentDir;
pub const allocPrintZ = backend_utils.allocPrintZ;
pub const inheritedProcessEnviron = backend_utils.inheritedProcessEnviron;

pub fn compile(allocator: std.mem.Allocator, request: backend_api.CompileRequest) !backend_api.CompileResult {
    if (request.mode != .llvm_native and request.mode != .hybrid) return error.UnsupportedBackendMode;

    const triple = try targetTriple(allocator, request.target_selector);
    defer allocator.free(triple);
    // The LLVM C-API backend is the sole native/hybrid codegen path (the textual-IR writer
    // has been retired). It is at full corpus parity (1047/0 across vm/llvm/hybrid) and,
    // with owned-value drop on by default, is leak-equivalent-or-better than the writer was.
    return compileViaCApi(allocator, request, triple);
}

fn compileViaCApi(
    allocator: std.mem.Allocator,
    request: backend_api.CompileRequest,
    triple: []const u8,
) !backend_api.CompileResult {
    try ensureParentDir(request.emit.object_path);
    if (request.emit.executable_path) |path| try ensureParentDir(path);
    if (request.emit.shared_library_path) |path| try ensureParentDir(path);

    const llvm_toolchain = try toolchain.Toolchain.discover(allocator);
    var api = try llvm.Api.open(llvm_toolchain);
    defer api.close();

    const lowered = try backend_capi.buildModule(allocator, &api, request, triple);
    // A module must be disposed before its owning context. defer runs LIFO, so
    // register the context disposal first (runs last) and the module second.
    defer api.LLVMContextDispose(lowered.context);
    defer api.LLVMDisposeModule(lowered.module_ref);

    try emitObjectFileViaClang(allocator, &api, lowered.module_ref, request.emit.object_path, request.target_selector);

    var artifacts = std.array_list.Managed(backend_api.Artifact).init(allocator);
    try artifacts.append(.{
        .kind = .native_object,
        .path = try allocator.dupe(u8, request.emit.object_path),
    });

    if (request.emit.executable_path) |executable_path| {
        const bridge_object = try linker.buildRuntimeHelpersObject(allocator, request.emit.object_path, false, request.target_selector);
        const dynamic_ffi_object = try linker.buildDynamicFfiHelpersObject(allocator, request.emit.object_path, false, request.target_selector);
        try linker.linkExecutable(allocator, executable_path, &.{ request.emit.object_path, bridge_object, dynamic_ffi_object }, request.resolved_native_libraries, request.target_selector);
        try artifacts.append(.{
            .kind = .executable,
            .path = try allocator.dupe(u8, executable_path),
        });
    }

    // Hybrid builds emit a shared library that the VM loads to call kira_native_impl_*.
    if (request.emit.shared_library_path) |library_path| {
        const bridge_object = try linker.buildRuntimeHelpersObject(allocator, request.emit.object_path, true, request.target_selector);
        const dynamic_ffi_object = try linker.buildDynamicFfiHelpersObject(allocator, request.emit.object_path, true, request.target_selector);
        try linker.linkSharedLibrary(allocator, library_path, &.{ request.emit.object_path, bridge_object, dynamic_ffi_object }, request.resolved_native_libraries, request.target_selector);
        try artifacts.append(.{
            .kind = .native_library,
            .path = try allocator.dupe(u8, library_path),
        });
    }

    return .{ .artifacts = try artifacts.toOwnedSlice() };
}

pub fn validate(allocator: std.mem.Allocator, request: backend_api.CompileRequest) !void {
    if (request.mode != .llvm_native and request.mode != .hybrid) return error.UnsupportedBackendMode;

    const triple = try targetTriple(allocator, request.target_selector);
    defer allocator.free(triple);

    // Validate by lowering through the C-API backend and verifying the LLVM module, then
    // discarding it without emitting an object. This is the same lowering the build path
    // uses, so a program that validates here will also compile. (Requires the managed LLVM
    // — the only native codegen path now that the text-IR writer is retired.)
    const llvm_toolchain = try toolchain.Toolchain.discover(allocator);
    var api = try llvm.Api.open(llvm_toolchain);
    defer api.close();
    const lowered = try backend_capi.buildModule(allocator, &api, request, triple);
    api.LLVMDisposeModule(lowered.module_ref);
    api.LLVMContextDispose(lowered.context);
}

const TargetMachineInfo = struct {
    machine: llvm.c.LLVMTargetMachineRef,
    cpu_name: [*c]u8,
    cpu_features: [*c]u8,
};

fn createTargetMachine(allocator: std.mem.Allocator, api: *const llvm.Api, triple: []const u8) !TargetMachineInfo {
    const triple_z = try allocator.dupeZ(u8, triple);
    var target_ref: llvm.c.LLVMTargetRef = undefined;
    var target_error: [*c]u8 = null;
    if (api.LLVMGetTargetFromTriple(triple_z.ptr, &target_ref, &target_error) != 0) {
        defer if (target_error != null) api.LLVMDisposeMessage(target_error);
        return error.TargetLookupFailed;
    }

    const cpu_name = api.LLVMGetHostCPUName();
    const cpu_features = api.LLVMGetHostCPUFeatures();

    const machine = api.LLVMCreateTargetMachine(
        target_ref,
        triple_z.ptr,
        cpu_name,
        cpu_features,
        llvm.c.LLVMCodeGenLevelDefault,
        llvm.c.LLVMRelocDefault,
        llvm.c.LLVMCodeModelDefault,
    ) orelse return error.TargetMachineCreationFailed;

    return .{
        .machine = machine,
        .cpu_name = cpu_name,
        .cpu_features = cpu_features,
    };
}

const LoweredModule = struct {
    context: llvm.c.LLVMContextRef,
    module_ref: llvm.c.LLVMModuleRef,
};

const Types = struct {
    api: *const llvm.Api,
    context: llvm.c.LLVMContextRef,
    bool_ty: llvm.c.LLVMTypeRef,
    i8: llvm.c.LLVMTypeRef,
    i32: llvm.c.LLVMTypeRef,
    i64: llvm.c.LLVMTypeRef,
    usize_ty: llvm.c.LLVMTypeRef,
    void_ty: llvm.c.LLVMTypeRef,
    ptr_ty: llvm.c.LLVMTypeRef,
    string_ty: llvm.c.LLVMTypeRef,

    fn init(api: *const llvm.Api, context: llvm.c.LLVMContextRef) Types {
        const ptr_ty = api.LLVMPointerTypeInContext(context, 0);
        const usize_ty = api.LLVMInt64TypeInContext(context);
        var string_fields = [_]llvm.c.LLVMTypeRef{ ptr_ty, usize_ty };
        return .{
            .api = api,
            .context = context,
            .bool_ty = api.LLVMInt1TypeInContext(context),
            .i8 = api.LLVMInt8TypeInContext(context),
            .i32 = api.LLVMInt32TypeInContext(context),
            .i64 = api.LLVMInt64TypeInContext(context),
            .usize_ty = usize_ty,
            .void_ty = api.LLVMVoidTypeInContext(context),
            .ptr_ty = ptr_ty,
            .string_ty = api.LLVMStructTypeInContext(context, &string_fields, string_fields.len, 0),
        };
    }

    fn llvmType(self: Types, value_type: ir.ValueType) llvm.c.LLVMTypeRef {
        return switch (value_type.kind) {
            .void => self.void_ty,
            .integer => self.i64,
            .float => self.api.LLVMFloatTypeInContext(self.context),
            .string => self.string_ty,
            .boolean => self.bool_ty,
            .construct_any, .array, .raw_ptr, .ffi_struct => self.usize_ty,
        };
    }

    fn voidFunctionType(self: Types) llvm.c.LLVMTypeRef {
        return self.api.LLVMFunctionType(self.void_ty, null, 0, 0);
    }
};

const RuntimeDecls = struct {
    print_i64_ty: llvm.c.LLVMTypeRef,
    print_i64_fn: llvm.c.LLVMValueRef,
    print_string_ty: llvm.c.LLVMTypeRef,
    print_string_fn: llvm.c.LLVMValueRef,
    call_runtime_ty: ?llvm.c.LLVMTypeRef,
    call_runtime_fn: ?llvm.c.LLVMValueRef,
};

fn declareRuntime(api: *const llvm.Api, module_ref: llvm.c.LLVMModuleRef, types: Types, mode: backend_api.BackendMode) RuntimeDecls {
    var int_args = [_]llvm.c.LLVMTypeRef{types.i64};
    const print_i64_ty = api.LLVMFunctionType(types.void_ty, &int_args, int_args.len, 0);

    var string_args = [_]llvm.c.LLVMTypeRef{ types.ptr_ty, types.usize_ty };
    const print_string_ty = api.LLVMFunctionType(types.void_ty, &string_args, string_args.len, 0);

    var call_runtime_args = [_]llvm.c.LLVMTypeRef{types.i32};
    const call_runtime_ty = if (mode == .hybrid) api.LLVMFunctionType(types.void_ty, &call_runtime_args, call_runtime_args.len, 0) else null;

    return .{
        .print_i64_ty = print_i64_ty,
        .print_i64_fn = api.LLVMAddFunction(module_ref, runtime_symbols.print_i64, print_i64_ty),
        .print_string_ty = print_string_ty,
        .print_string_fn = api.LLVMAddFunction(module_ref, runtime_symbols.print_string, print_string_ty),
        .call_runtime_ty = call_runtime_ty,
        .call_runtime_fn = if (call_runtime_ty) |value| api.LLVMAddFunction(module_ref, runtime_symbols.call_runtime, value) else null,
    };
}

fn lowerProgram(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    target_machine: TargetMachineInfo,
    request: backend_api.CompileRequest,
    triple: []const u8,
) !LoweredModule {
    _ = target_machine;
    const context = api.LLVMContextCreate();
    const module_name = try allocator.dupeZ(u8, request.module_name);
    const module_ref = api.LLVMModuleCreateWithNameInContext(module_name.ptr, context);
    api.LLVMSetTarget(module_ref, try allocator.dupeZ(u8, triple));

    const builder = api.LLVMCreateBuilderInContext(context);
    defer api.LLVMDisposeBuilder(builder);

    const types = Types.init(api, context);
    const runtime_decls = declareRuntime(api, module_ref, types, request.mode);

    var functions = std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef){};
    defer functions.deinit(allocator);

    for (request.program.functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
        const function_value = try declareFunction(allocator, api, module_ref, function_decl, request.mode, types);
        try functions.put(allocator, function_decl.id, function_value);
    }

    for (request.program.functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
        const function_value = functions.get(function_decl.id) orelse return error.MissingFunctionDeclaration;
        try lowerFunction(allocator, api, builder, module_ref, types, runtime_decls, request, &functions, function_decl, function_value);
    }

    if (request.mode == .llvm_native) {
        const entry_decl = request.program.functions[request.program.entry_index];
        if (!shouldLowerFunction(entry_decl.execution, request.mode)) return error.RuntimeEntrypointInNativeBuild;
        const entry_function = functions.get(entry_decl.id) orelse return error.MissingFunctionDeclaration;
        try buildHostMain(api, builder, module_ref, types, entry_function);
    }

    try verifyModule(api, module_ref);
    return .{ .context = context, .module_ref = module_ref };
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

fn declareFunction(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    module_ref: llvm.c.LLVMModuleRef,
    function_decl: ir.Function,
    mode: backend_api.BackendMode,
    types: Types,
) !llvm.c.LLVMValueRef {
    const function_ty = types.voidFunctionType();
    const name = try functionSymbolName(allocator, function_decl, mode);
    const function_value = api.LLVMAddFunction(module_ref, name.ptr, function_ty);
    if (mode == .hybrid and builtin.os.tag == .windows) {
        api.LLVMSetDLLStorageClass(function_value, llvm.c.LLVMDLLExportStorageClass);
    }
    return function_value;
}

pub fn functionSymbolName(allocator: std.mem.Allocator, function_decl: ir.Function, mode: backend_api.BackendMode) ![:0]u8 {
    if (function_decl.is_extern) {
        if (function_decl.foreign) |foreign| {
            return allocator.dupeZ(u8, foreign.symbol_name);
        }
    }
    return switch (mode) {
        .llvm_native => allocPrintZ(allocator, "kira_fn_{d}_{s}", .{ function_decl.id, function_decl.name }),
        .hybrid => allocPrintZ(allocator, "kira_native_impl_{d}", .{function_decl.id}),
        .vm_bytecode => unreachable,
    };
}

fn lowerFunction(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    module_ref: llvm.c.LLVMModuleRef,
    types: Types,
    runtime_decls: RuntimeDecls,
    request: backend_api.CompileRequest,
    functions: *const std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef),
    function_decl: ir.Function,
    function_value: llvm.c.LLVMValueRef,
) !void {
    const entry_block = api.LLVMAppendBasicBlockInContext(types.context, function_value, "entry");
    api.LLVMPositionBuilderAtEnd(builder, entry_block);

    const register_types = try inferRegisterTypes(allocator, request.program.*, function_decl);
    const register_values = try allocator.alloc(llvm.c.LLVMValueRef, function_decl.register_count);
    const locals = try allocator.alloc(llvm.c.LLVMValueRef, function_decl.local_count);

    for (function_decl.local_types, 0..) |local_type, index| {
        locals[index] = api.LLVMBuildAlloca(builder, types.llvmType(local_type), "local");
    }

    var string_counter: usize = 0;
    for (function_decl.instructions) |instruction| {
        switch (instruction) {
            .const_int => |value| register_values[value.dst] = api.LLVMConstInt(types.i64, @bitCast(@as(u64, @intCast(value.value))), 1),
            .const_string => |value| {
                register_values[value.dst] = try buildStringConstant(allocator, api, module_ref, types, value.value, string_counter);
                string_counter += 1;
            },
            .const_bool => |value| register_values[value.dst] = api.LLVMConstInt(types.bool_ty, if (value.value) 1 else 0, 0),
            .const_null_ptr => |value| register_values[value.dst] = api.LLVMConstInt(types.usize_ty, 0, 0),
            .alloc_struct => return error.UnsupportedExecutableFeature,
            .alloc_array => return error.UnsupportedExecutableFeature,
            .const_function => return error.UnsupportedExecutableFeature,
            .const_closure => return error.UnsupportedExecutableFeature,
            .add => |value| register_values[value.dst] = api.LLVMBuildAdd(builder, register_values[value.lhs], register_values[value.rhs], "add"),
            .subtract => |value| register_values[value.dst] = api.LLVMBuildSub(builder, register_values[value.lhs], register_values[value.rhs], "sub"),
            .multiply => |value| register_values[value.dst] = api.LLVMBuildMul(builder, register_values[value.lhs], register_values[value.rhs], "mul"),
            .divide => |value| register_values[value.dst] = api.LLVMBuildSDiv(builder, register_values[value.lhs], register_values[value.rhs], "div"),
            .modulo => |value| register_values[value.dst] = api.LLVMBuildSRem(builder, register_values[value.lhs], register_values[value.rhs], "mod"),
            .unary => |value| {
                register_values[value.dst] = switch (value.op) {
                    .negate => api.LLVMBuildNeg(builder, register_values[value.src], "neg"),
                    .not => api.LLVMBuildNot(builder, register_values[value.src], "not"),
                };
            },
            .compare, .branch, .jump, .label => return error.UnsupportedExecutableFeature,
            .store_local => |value| _ = api.LLVMBuildStore(builder, register_values[value.src], locals[value.local]),
            .load_local => |value| register_values[value.dst] = api.LLVMBuildLoad2(builder, types.llvmType(function_decl.local_types[value.local]), locals[value.local], "load"),
            .local_ptr => return error.UnsupportedExecutableFeature,
            .subobject_ptr, .field_ptr, .c_string_to_string, .array_len, .array_get, .array_set, .array_append, .load_indirect, .store_indirect, .copy_indirect => return error.UnsupportedExecutableFeature,
            .print => |value| try lowerPrint(api, builder, runtime_decls, register_types[value.src], register_values[value.src]),
            .call => |value| {
                if (value.args.len != 0 or value.dst != null) return error.UnsupportedExecutableFeature;
                try lowerCall(api, builder, types, runtime_decls, request.mode, request.program, functions, value.callee);
            },
            .call_value => return error.UnsupportedExecutableFeature,
            .ret => |value| {
                if (value.src != null) return error.UnsupportedExecutableFeature;
                _ = api.LLVMBuildRetVoid(builder);
                return;
            },
        }
    }

    _ = api.LLVMBuildRetVoid(builder);
}

fn lowerCall(
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    types: Types,
    runtime_decls: RuntimeDecls,
    mode: backend_api.BackendMode,
    program: *const ir.Program,
    functions: *const std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef),
    callee_id: u32,
) !void {
    const callee_execution = functionExecutionById(program.*, callee_id) orelse return error.UnknownFunction;
    switch (resolveExecution(callee_execution, mode)) {
        .native => {
            const callee_fn = functions.get(callee_id) orelse return error.MissingFunctionDeclaration;
            const fn_ty = types.voidFunctionType();
            _ = api.LLVMBuildCall2(builder, fn_ty, callee_fn, null, 0, "");
        },
        .runtime => {
            if (mode != .hybrid) return error.RuntimeCallInNativeBuild;
            var args = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(types.i32, callee_id, 0)};
            _ = api.LLVMBuildCall2(builder, runtime_decls.call_runtime_ty.?, runtime_decls.call_runtime_fn.?, &args, args.len, "");
        },
        .inherited => unreachable,
    }
}

fn buildStringConstant(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    module_ref: llvm.c.LLVMModuleRef,
    types: Types,
    value: []const u8,
    index: usize,
) !llvm.c.LLVMValueRef {
    const global_name = try allocPrintZ(allocator, ".kira.str.{d}", .{index});
    const array_ty = api.LLVMArrayType2(types.i8, value.len + 1);
    const global = api.LLVMAddGlobal(module_ref, array_ty, global_name.ptr);
    api.LLVMSetLinkage(global, llvm.c.LLVMPrivateLinkage);
    api.LLVMSetGlobalConstant(global, 1);
    api.LLVMSetInitializer(global, api.LLVMConstStringInContext2(types.context, value.ptr, value.len, 0));

    const zero = api.LLVMConstInt(types.i32, 0, 0);
    var indices = [_]llvm.c.LLVMValueRef{ zero, zero };
    const data_ptr = api.LLVMConstInBoundsGEP2(array_ty, global, &indices, indices.len);
    const length = api.LLVMConstInt(types.usize_ty, value.len, 0);
    var fields = [_]llvm.c.LLVMValueRef{ data_ptr, length };
    return api.LLVMConstNamedStruct(types.string_ty, &fields, fields.len);
}

fn lowerPrint(
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    runtime_decls: RuntimeDecls,
    value_type: ir.ValueType,
    value_ref: llvm.c.LLVMValueRef,
) !void {
    switch (value_type.kind) {
        .integer => {
            var args = [_]llvm.c.LLVMValueRef{value_ref};
            _ = api.LLVMBuildCall2(builder, runtime_decls.print_i64_ty, runtime_decls.print_i64_fn, &args, args.len, "");
        },
        .string => {
            const data_ptr = api.LLVMBuildExtractValue(builder, value_ref, 0, "str.ptr");
            const length = api.LLVMBuildExtractValue(builder, value_ref, 1, "str.len");
            var args = [_]llvm.c.LLVMValueRef{ data_ptr, length };
            _ = api.LLVMBuildCall2(builder, runtime_decls.print_string_ty, runtime_decls.print_string_fn, &args, args.len, "");
        },
        .void, .float, .boolean, .construct_any, .array, .raw_ptr, .ffi_struct => return error.UnsupportedExecutableFeature,
    }
}

fn buildHostMain(
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    module_ref: llvm.c.LLVMModuleRef,
    types: Types,
    entry_function: llvm.c.LLVMValueRef,
) !void {
    const main_ty = api.LLVMFunctionType(types.i32, null, 0, 0);
    const main_fn = api.LLVMAddFunction(module_ref, "main", main_ty);
    const entry_block = api.LLVMAppendBasicBlockInContext(types.context, main_fn, "entry");
    api.LLVMPositionBuilderAtEnd(builder, entry_block);

    const entry_fn_ty = types.voidFunctionType();
    _ = api.LLVMBuildCall2(builder, entry_fn_ty, entry_function, null, 0, "");
    _ = api.LLVMBuildRet(builder, api.LLVMConstInt(types.i32, 0, 0));
}

fn verifyModule(api: *const llvm.Api, module_ref: llvm.c.LLVMModuleRef) !void {
    var error_message: [*c]u8 = null;
    if (api.LLVMVerifyModule(module_ref, llvm.c.LLVMReturnStatusAction, &error_message) != 0) {
        defer if (error_message != null) api.LLVMDisposeMessage(error_message);
        return error.InvalidLlvmModule;
    }
}

pub const CallValueDispatcher = struct {
    hash: u64,
    param_types: []const ir.ValueType,
    return_type: ir.ValueType,
};

pub fn collectCallValueDispatchers(allocator: std.mem.Allocator, program: ir.Program) ![]CallValueDispatcher {
    var dispatchers = std.array_list.Managed(CallValueDispatcher).init(allocator);
    for (program.functions) |function_decl| {
        for (function_decl.instructions) |instruction| {
            if (instruction != .call_value) continue;
            const call_inst = instruction.call_value;
            const hash = hashCallValueSignature(call_inst.param_types, call_inst.return_type);
            var found = false;
            for (dispatchers.items) |existing| {
                if (existing.hash != hash) continue;
                if (!sameCallValueSignature(existing.param_types, existing.return_type, call_inst.param_types, call_inst.return_type)) {
                    return error.UnsupportedExecutableFeature;
                }
                found = true;
                break;
            }
            if (!found) {
                try dispatchers.append(.{
                    .hash = hash,
                    .param_types = call_inst.param_types,
                    .return_type = call_inst.return_type,
                });
            }
        }
    }
    return dispatchers.toOwnedSlice();
}

pub fn hashCallValueSignature(param_types: []const ir.ValueType, return_type: ir.ValueType) u64 {
    var hasher = std.hash.Wyhash.init(0);
    for (param_types) |param_type| hashValueType(&hasher, param_type);
    hasher.update(&.{0xff});
    hashValueType(&hasher, return_type);
    return hasher.final();
}

pub fn hashValueType(hasher: *std.hash.Wyhash, value_type: ir.ValueType) void {
    hasher.update(&.{@intFromEnum(value_type.kind)});
    if (value_type.name) |name| {
        hasher.update(&.{1});
        hasher.update(name);
    } else {
        hasher.update(&.{0});
    }
}

pub fn sameCallValueSignature(
    lhs_params: []const ir.ValueType,
    lhs_return: ir.ValueType,
    rhs_params: []const ir.ValueType,
    rhs_return: ir.ValueType,
) bool {
    if (lhs_params.len != rhs_params.len) return false;
    for (lhs_params, 0..) |lhs, index| {
        const rhs = rhs_params[index];
        if (!sameValueType(lhs, rhs)) return false;
    }
    return sameValueType(lhs_return, rhs_return);
}

pub fn sameValueType(lhs: ir.ValueType, rhs: ir.ValueType) bool {
    if (lhs.kind != rhs.kind) return false;
    if (lhs.name == null or rhs.name == null) return lhs.name == null and rhs.name == null;
    return std.mem.eql(u8, lhs.name.?, rhs.name.?);
}
