const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const backend_api = @import("kira_backend_api");
const build_options = @import("kira_llvm_build_options");
const llvm = @import("llvm_c.zig");
const runtime_symbols = @import("runtime_symbols.zig");
const enum_ops = @import("backend_text_ir_enum_ops.zig");
const parent = @import("backend.zig");
const collectCallValueDispatchers = parent.collectCallValueDispatchers;
const freeStringList = parent.freeStringList;
const freeSymbolNames = parent.freeSymbolNames;
const shouldLowerFunction = parent.shouldLowerFunction;
const functionSymbolName = parent.functionSymbolName;
const buildTextExternDecl = parent.buildTextExternDecl;
const countStringConstants = parent.countStringConstants;
const appendTypeDefinitions = parent.appendTypeDefinitions;
const buildCallValueDispatcher = parent.buildCallValueDispatcher;
const buildHybridBridgeWrapper = parent.buildHybridBridgeWrapper;
const buildMonomorphizationPlan = parent.buildMonomorphizationPlan;
const llvmValueTypeText = parent.llvmValueTypeText;
const llvmLocalStorageTypeText = parent.llvmLocalStorageTypeText;
const writeLlvmSymbol = parent.writeLlvmSymbol;
const inferRegisterTypes = parent.inferRegisterTypes;
const functionById = parent.functionById;
const findTypeDecl = parent.findTypeDecl;
const typeRefName = parent.typeRefName;
const appendStringGlobals = parent.appendStringGlobals;
const writePrintInstruction = parent.writePrintInstruction;
const writeCallInstruction = parent.writeCallInstruction;
const writeIndirectCallInstruction = parent.writeIndirectCallInstruction;
const writeLlvmStringLiteral = parent.writeLlvmStringLiteral;
const writeLlvmEscapedBytes = parent.writeLlvmEscapedBytes;
const writeLlvmFloatLiteral = parent.writeLlvmFloatLiteral;
const llvmComparePredicate = parent.llvmComparePredicate;
const llvmCompareValueTypeText = parent.llvmCompareValueTypeText;
const llvmIndirectLoadTypeText = parent.llvmIndirectLoadTypeText;
const llvmFieldStoreValuePrefix = parent.llvmFieldStoreValuePrefix;
const fieldIndex = parent.fieldIndex;
const fieldType = parent.fieldType;
const isPointerLikeValueType = parent.isPointerLikeValueType;
const bridgeTagValue = parent.bridgeTagValue;
const integerAbiTypeName = parent.integerAbiTypeName;
const floatAbiTypeName = parent.floatAbiTypeName;
const writeLlvmLabelName = parent.writeLlvmLabelName;
pub fn buildTextLlvmIr(
    allocator: std.mem.Allocator,
    request: backend_api.CompileRequest,
    triple: []const u8,
) ![]u8 {
    const dispatchers = try collectCallValueDispatchers(allocator, request.program.*);
    defer allocator.free(dispatchers);
    var globals = std.array_list.Managed([]const u8).init(allocator);
    defer freeStringList(allocator, &globals);
    const plan = try buildMonomorphizationPlan(allocator, request);
    defer {
        var owned = plan;
        owned.deinit();
    }
    var symbol_names = std.AutoHashMapUnmanaged(u32, []const u8){};
    defer freeSymbolNames(allocator, &symbol_names);
    for (request.program.functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
        const name = try functionSymbolName(allocator, function_decl, request.mode);
        try symbol_names.put(allocator, function_decl.id, name);
    }
    var function_bodies = std.array_list.Managed([]const u8).init(allocator);
    defer freeStringList(allocator, &function_bodies);
    var function_decls = std.array_list.Managed([]const u8).init(allocator);
    defer freeStringList(allocator, &function_decls);
    var string_counter: usize = 0;
    for (request.program.functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode)) continue;
        if (function_decl.is_extern) try function_decls.append(try buildTextExternDecl(allocator, request, &symbol_names, function_decl));
    }
    for (request.program.functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode) or function_decl.is_extern) continue;
        const register_types = try inferRegisterTypes(allocator, request.program.*, function_decl);
        defer allocator.free(register_types);
        const base_variant = parent.FunctionVariant{ .function_id = function_decl.id, .param_types = function_decl.param_types, .param_ownership = function_decl.param_ownership, .local_types = function_decl.local_types, .return_type = function_decl.return_type, .return_ownership = function_decl.return_ownership, .register_types = register_types, .symbol_name = symbol_names.get(function_decl.id) orelse return error.MissingFunctionDeclaration };
        const body = try buildTextFunctionBody(allocator, request, &plan, &symbol_names, &globals, function_decl, base_variant, string_counter);
        string_counter += countStringConstants(function_decl);
        try function_bodies.append(body);
    }
    for (plan.variants) |variant| {
        const function_decl = functionById(request.program.*, variant.function_id) orelse return error.UnknownFunction;
        const body = try buildTextFunctionBody(allocator, request, &plan, &symbol_names, &globals, function_decl, variant, string_counter);
        string_counter += countStringConstants(function_decl);
        try function_bodies.append(body);
    }
    if (request.mode == .llvm_native) {
        const entry_decl = request.program.functions[request.program.entry_index];
        if (!shouldLowerFunction(entry_decl.execution, request.mode)) return error.RuntimeEntrypointInNativeBuild;
        try function_bodies.append(try parent.buildTextMainBody(allocator, symbol_names.get(entry_decl.id) orelse return error.MissingFunctionDeclaration));
    }
    var output: std.Io.Writer.Allocating = .init(allocator);
    errdefer output.deinit();
    var writer = &output.writer;
    try writer.print("; ModuleID = \"{s}\"\n", .{request.module_name});
    try writer.print("source_filename = \"{s}\"\n", .{request.module_name});
    try writer.print("target triple = \"{s}\"\n\n", .{triple});
    try appendTypeDefinitions(allocator, writer, request.program);
    try writer.writeAll("%kira.string = type { ptr, i64 }\n\n");
    try writer.writeAll("%kira.bridge.value = type { i8, [7 x i8], i64, i64 }\n\n");
    try writer.writeAll("@kira_bool_true_data = private unnamed_addr constant [5 x i8] c\"true\\00\"\n");
    try writer.writeAll("@kira_bool_true = private unnamed_addr constant %kira.string { ptr getelementptr inbounds ([5 x i8], ptr @kira_bool_true_data, i64 0, i64 0), i64 4 }\n");
    try writer.writeAll("@kira_bool_false_data = private unnamed_addr constant [6 x i8] c\"false\\00\"\n");
    try writer.writeAll("@kira_bool_false = private unnamed_addr constant %kira.string { ptr getelementptr inbounds ([6 x i8], ptr @kira_bool_false_data, i64 0, i64 0), i64 5 }\n\n");
    try writer.writeAll("declare void @\"kira_native_write_i64\"(i64)\n");
    try writer.writeAll("declare void @\"kira_native_write_f64\"(double)\n");
    try writer.writeAll("declare void @\"kira_native_write_string\"(ptr, i64)\n");
    try writer.writeAll("declare void @\"kira_native_write_ptr\"(i64)\n");
    try writer.writeAll("declare void @\"kira_native_write_newline\"()\n");
    try writer.writeAll("declare void @\"kira_native_print_i64\"(i64)\n");
    try writer.writeAll("declare void @\"kira_native_print_f64\"(double)\n");
    try writer.writeAll("declare void @\"kira_native_print_string\"(ptr, i64)\n");
    try writer.writeAll("declare ptr @\"kira_array_alloc\"(i64)\n");
    try writer.writeAll("declare i64 @\"kira_array_len\"(ptr)\n");
    try writer.writeAll("declare void @\"kira_array_store\"(ptr, i64, ptr)\n");
    try writer.writeAll("declare void @\"kira_array_append\"(ptr, ptr)\n");
    try writer.writeAll("declare void @\"kira_array_load\"(ptr, i64, ptr)\n");
    try writer.writeAll("declare void @\"kira_array_release\"(ptr, ptr)\n");
    try writer.writeAll("declare ptr @\"kira_native_state_alloc\"(i64, i64)\n");
    try writer.writeAll("declare ptr @\"kira_native_state_payload\"(ptr)\n");
    try writer.writeAll("declare ptr @\"kira_native_state_recover\"(ptr, i64)\n");
    try writer.writeAll("declare i64 @strlen(ptr)\n");
    try writer.writeAll("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)\n");
    try writer.writeAll("declare ptr @malloc(i64)\n");
    try writer.writeAll("declare void @free(ptr)\n");
    if (request.mode == .hybrid) {
        try writer.writeAll("declare void @\"kira_hybrid_call_runtime\"(i32, ptr, i32, ptr)\n");
    }
    try appendReleaseDefinitions(allocator, writer, request.program);
    for (function_decls.items) |decl| {
        try writer.writeAll(decl);
        try writer.writeByte('\n');
    }
    try writer.writeByte('\n');
    if (function_bodies.items.len > 0) try writer.writeByte('\n');
    for (globals.items) |global_def| {
        try writer.writeAll(global_def);
        try writer.writeByte('\n');
    }
    if (globals.items.len > 0 and function_bodies.items.len > 0) try writer.writeByte('\n');
    for (function_bodies.items) |body| {
        try writer.writeAll(body);
        try writer.writeByte('\n');
    }
    for (dispatchers) |dispatcher| {
        try writer.writeAll(try buildCallValueDispatcher(allocator, request, &symbol_names, dispatcher));
        try writer.writeByte('\n');
    }
    if (request.mode == .hybrid) for (request.program.functions) |function_decl| {
        if (!shouldLowerFunction(function_decl.execution, request.mode) or function_decl.is_extern) continue;
        try writer.writeAll(try buildHybridBridgeWrapper(allocator, &symbol_names, function_decl));
        try writer.writeByte('\n');
    };
    return output.toOwnedSlice();
}
pub fn buildTextFunctionBody(
    allocator: std.mem.Allocator,
    request: backend_api.CompileRequest,
    plan: *const parent.MonomorphizationPlan,
    symbol_names: *const std.AutoHashMapUnmanaged(u32, []const u8),
    globals: *std.array_list.Managed([]const u8),
    function_decl: ir.Function,
    variant: parent.FunctionVariant,
    string_counter: usize,
) ![]const u8 {
    var body: std.Io.Writer.Allocating = .init(allocator);
    errdefer body.deinit();
    var writer = &body.writer;
    const function_name = variant.symbol_name;
    try writer.writeAll("define ");
    if (request.mode == .hybrid and builtin.os.tag == .windows) {
        try writer.writeAll("dllexport ");
    }
    try writer.writeAll(llvmValueTypeText(variant.return_type));
    try writer.writeByte(' ');
    try writeLlvmSymbol(writer, function_name);
    try writer.writeByte('(');
    for (variant.param_types, 0..) |param_type, index| {
        if (index != 0) try writer.writeAll(", ");
        try writer.writeAll(llvmValueTypeText(param_type));
        try writer.writeAll(" %arg");
        try writer.print("{d}", .{index});
    }
    try writer.writeAll(") {\nentry:\n");
    const register_types = variant.register_types;
    const string_state = try allocator.alloc(usize, 1);
    defer allocator.free(string_state);
    string_state[0] = string_counter;
    var temp_counter: usize = 0;
    var block_terminated = false;
    var owned_values = std.array_list.Managed(OwnedValue).init(allocator);
    defer owned_values.deinit();
    const register_owned_ptr = try allocator.alloc(bool, register_types.len);
    defer allocator.free(register_owned_ptr);
    @memset(register_owned_ptr, false);
    const register_escaped = try allocator.alloc(bool, register_types.len);
    defer allocator.free(register_escaped);
    @memset(register_escaped, false);
    const register_local_ptr = try allocator.alloc(?usize, register_types.len);
    defer allocator.free(register_local_ptr);
    @memset(register_local_ptr, null);
    const local_owns_contents = try allocator.alloc(bool, variant.local_types.len);
    defer allocator.free(local_owns_contents);
    @memset(local_owns_contents, false);
    const local_owned_values = try allocator.alloc(bool, variant.local_types.len);
    defer allocator.free(local_owned_values);
    @memset(local_owned_values, false);
    for (variant.local_types, 0..) |local_type, index| {
        const storage_type = try llvmLocalStorageTypeText(allocator, request.program, local_type);
        try writer.writeAll("  %local");
        try writer.print("{d}", .{index});
        try writer.writeAll(" = alloca ");
        try writer.writeAll(storage_type);
        try writer.writeAll("\n");
        if (local_type.kind == .ffi_struct) {
            const struct_type_name = typeRefName(local_type.name orelse return error.UnsupportedExecutableFeature);
            try writer.print("  %local.storage.{d} = alloca {s}\n", .{ index, struct_type_name });
            try writer.print("  store {s} zeroinitializer, ptr %local.storage.{d}\n", .{ struct_type_name, index });
            try writer.print("  %local.storage.int.{d} = ptrtoint ptr %local.storage.{d} to i64\n", .{ index, index });
            try writer.print("  store i64 %local.storage.int.{d}, ptr %local{d}\n", .{ index, index });
        }
    }
    for (register_types, 0..) |register_type, index| {
        if (register_type.kind != .ffi_struct) continue;
        try writer.print("  %cleanup.heap.slot.{d} = alloca ptr\n", .{index});
        try writer.print("  store ptr null, ptr %cleanup.heap.slot.{d}\n", .{index});
    }
    for (variant.param_types, 0..) |param_type, index| {
        try writer.writeAll("  store ");
        try writer.writeAll(llvmValueTypeText(param_type));
        try writer.writeAll(" %arg");
        try writer.print("{d}", .{index});
        try writer.writeAll(", ptr %local");
        try writer.print("{d}\n", .{index});
    }
    for (function_decl.instructions) |instruction| {
        switch (instruction) {
            .const_int => |value| {
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = add i64 0, ");
                try writer.print("{d}\n", .{value.value});
            },
            .const_float => |value| {
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = fadd double 0.0, ");
                try writeLlvmFloatLiteral(writer, value.value);
                try writer.writeAll("\n");
            },
            .const_string => |value| {
                const string_index = string_state[0];
                string_state[0] += 1;
                try appendStringGlobals(allocator, globals, string_index, value.value);
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = load %kira.string, ptr @kira_str_");
                try writer.print("{d}", .{string_index});
                try writer.writeAll("\n");
            },
            .const_bool => |value| {
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = add i1 0, ");
                try writer.writeAll(if (value.value) "1\n" else "0\n");
            },
            .const_null_ptr => |value| {
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = add i64 0, 0\n");
            },
            .alloc_struct => |value| {
                const struct_type_name = typeRefName(value.type_name);
                try writer.print("  %alloc.size.ptr.{d} = getelementptr inbounds {s}, ptr null, i32 1\n", .{ value.dst, struct_type_name });
                try writer.print("  %alloc.size.{d} = ptrtoint ptr %alloc.size.ptr.{d} to i64\n", .{ value.dst, value.dst });
                try writer.print("  %alloc.empty.{d} = icmp eq i64 %alloc.size.{d}, 0\n", .{ value.dst, value.dst });
                try writer.print("  %alloc.bytes.{d} = select i1 %alloc.empty.{d}, i64 1, i64 %alloc.size.{d}\n", .{ value.dst, value.dst, value.dst });
                try writer.print("  %alloc.ptr.{d} = call ptr @malloc(i64 %alloc.bytes.{d})\n", .{ value.dst, value.dst });
                try writer.print("  store {s} zeroinitializer, ptr %alloc.ptr.{d}\n", .{ struct_type_name, value.dst });
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = ptrtoint ptr %alloc.ptr.");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" to i64\n");
                try writer.print("  store ptr %alloc.ptr.{d}, ptr %cleanup.heap.slot.{d}\n", .{ value.dst, value.dst });
                try owned_values.append(.{ .reg = value.dst, .ty = register_types[value.dst], .kind = .heap_ptr });
                register_owned_ptr[value.dst] = true;
            },
            .alloc_enum => |value| try enum_ops.emitAllocEnum(writer, register_types, value),
            .alloc_native_state => |value| {
                const type_decl = findTypeDecl(request.program, value.type_name) orelse return error.UnsupportedExecutableFeature;
                const struct_type_name = typeRefName(value.type_name);
                try writer.print("  %native.state.size.ptr.{d} = getelementptr inbounds [{d} x %kira.bridge.value], ptr null, i32 1\n", .{
                    value.dst,
                    type_decl.fields.len,
                });
                try writer.print("  %native.state.size.{d} = ptrtoint ptr %native.state.size.ptr.{d} to i64\n", .{ value.dst, value.dst });
                try writer.print("  %native.state.box.{d} = call ptr @\"kira_native_state_alloc\"(i64 {d}, i64 %native.state.size.{d})\n", .{
                    value.dst,
                    value.type_id,
                    value.dst,
                });
                try writer.print("  %native.state.payload.{d} = call ptr @\"kira_native_state_payload\"(ptr %native.state.box.{d})\n", .{ value.dst, value.dst });
                try writer.print("  %native.state.src.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.src });
                for (type_decl.fields, 0..) |field_decl, index| {
                    const field_abi = try llvmIndirectLoadTypeText(allocator, request.program, field_decl.ty);
                    try writer.print("  %native.state.src.field.ptr.{d}.{d} = getelementptr inbounds {s}, ptr %native.state.src.{d}, i32 0, i32 {d}\n", .{
                        value.dst, index, struct_type_name, value.dst, index,
                    });
                    try writer.print("  %native.state.slot.ptr.{d}.{d} = getelementptr inbounds %kira.bridge.value, ptr %native.state.payload.{d}, i64 {d}\n", .{
                        value.dst, index, value.dst, index,
                    });
                    try writer.print("  %native.state.pack.{d}.{d}.0 = insertvalue %kira.bridge.value zeroinitializer, i8 {d}, 0\n", .{
                        value.dst, index, bridgeTagValue(field_decl.ty),
                    });
                    switch (field_decl.ty.kind) {
                        .integer => {
                            try writer.print("  %native.state.load.int.{d}.{d} = load {s}, ptr %native.state.src.field.ptr.{d}.{d}\n", .{
                                value.dst, index, field_abi, value.dst, index,
                            });
                            if (std.mem.eql(u8, field_abi, "i64")) {
                                try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.int.{d}.{d}, 2\n", .{
                                    value.dst, index, value.dst, index, value.dst, index,
                                });
                            } else {
                                try writer.print("  %native.state.load.int.ext.{d}.{d} = sext {s} %native.state.load.int.{d}.{d} to i64\n", .{
                                    value.dst, index, field_abi, value.dst, index,
                                });
                                try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.int.ext.{d}.{d}, 2\n", .{
                                    value.dst, index, value.dst, index, value.dst, index,
                                });
                            }
                        },
                        .float => {
                            try writer.print("  %native.state.load.float.{d}.{d} = load {s}, ptr %native.state.src.field.ptr.{d}.{d}\n", .{
                                value.dst, index, field_abi, value.dst, index,
                            });
                            if (std.mem.eql(u8, field_abi, "float")) {
                                try writer.print("  %native.state.load.float64.{d}.{d} = fpext float %native.state.load.float.{d}.{d} to double\n", .{
                                    value.dst, index, value.dst, index,
                                });
                                try writer.print("  %native.state.load.float.bits.{d}.{d} = bitcast double %native.state.load.float64.{d}.{d} to i64\n", .{
                                    value.dst, index, value.dst, index,
                                });
                            } else {
                                try writer.print("  %native.state.load.float.bits.{d}.{d} = bitcast double %native.state.load.float.{d}.{d} to i64\n", .{
                                    value.dst, index, value.dst, index,
                                });
                            }
                            try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.float.bits.{d}.{d}, 2\n", .{
                                value.dst, index, value.dst, index, value.dst, index,
                            });
                        },
                        .boolean => {
                            try writer.print("  %native.state.load.bool.{d}.{d} = load i8, ptr %native.state.src.field.ptr.{d}.{d}\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.bool.word.{d}.{d} = zext i8 %native.state.load.bool.{d}.{d} to i64\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.bool.word.{d}.{d}, 2\n", .{
                                value.dst, index, value.dst, index, value.dst, index,
                            });
                        },
                        .construct_any, .raw_ptr, .array, .enum_instance => {
                            try writer.print("  %native.state.load.ptr.{d}.{d} = load ptr, ptr %native.state.src.field.ptr.{d}.{d}\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.ptrint.{d}.{d} = ptrtoint ptr %native.state.load.ptr.{d}.{d} to i64\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.ptrint.{d}.{d}, 2\n", .{
                                value.dst, index, value.dst, index, value.dst, index,
                            });
                        },
                        .string => {
                            try writer.print("  %native.state.load.str.{d}.{d} = load %kira.string, ptr %native.state.src.field.ptr.{d}.{d}\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.str.ptr.{d}.{d} = extractvalue %kira.string %native.state.load.str.{d}.{d}, 0\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.str.ptrint.{d}.{d} = ptrtoint ptr %native.state.load.str.ptr.{d}.{d} to i64\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.str.len.{d}.{d} = extractvalue %kira.string %native.state.load.str.{d}.{d}, 1\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.pack.{d}.{d}.1 = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.str.ptrint.{d}.{d}, 2\n", .{
                                value.dst, index, value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.1, i64 %native.state.load.str.len.{d}.{d}, 3\n", .{
                                value.dst, index, value.dst, index, value.dst, index,
                            });
                        },
                        .ffi_struct => {
                            const field_struct_name = typeRefName(field_decl.ty.name orelse return error.UnsupportedExecutableFeature);
                            try writer.print("  %native.state.load.struct.{d}.{d} = load {s}, ptr %native.state.src.field.ptr.{d}.{d}\n", .{
                                value.dst, index, field_struct_name, value.dst, index,
                            });
                            try writer.print("  %native.state.load.struct.size.ptr.{d}.{d} = getelementptr inbounds {s}, ptr null, i32 1\n", .{
                                value.dst, index, field_struct_name,
                            });
                            try writer.print("  %native.state.load.struct.size.{d}.{d} = ptrtoint ptr %native.state.load.struct.size.ptr.{d}.{d} to i64\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.struct.copy.{d}.{d} = call ptr @malloc(i64 %native.state.load.struct.size.{d}.{d})\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  store {s} %native.state.load.struct.{d}.{d}, ptr %native.state.load.struct.copy.{d}.{d}\n", .{
                                field_struct_name, value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.load.struct.ptrint.{d}.{d} = ptrtoint ptr %native.state.load.struct.copy.{d}.{d} to i64\n", .{
                                value.dst, index, value.dst, index,
                            });
                            try writer.print("  %native.state.pack.{d}.{d} = insertvalue %kira.bridge.value %native.state.pack.{d}.{d}.0, i64 %native.state.load.struct.ptrint.{d}.{d}, 2\n", .{
                                value.dst, index, value.dst, index, value.dst, index,
                            });
                        },
                        .void => return error.UnsupportedExecutableFeature,
                    }
                    try writer.print("  store %kira.bridge.value %native.state.pack.{d}.{d}, ptr %native.state.slot.ptr.{d}.{d}\n", .{
                        value.dst, index, value.dst, index,
                    });
                }
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = ptrtoint ptr %native.state.box.");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" to i64\n");
            },
            .alloc_array => |value| {
                try writer.print("  %alloc.array.ptr.{d} = call ptr @\"kira_array_alloc\"(i64 %r{d})\n", .{ value.dst, value.len });
                try writer.print("  %r{d} = ptrtoint ptr %alloc.array.ptr.{d} to i64\n", .{ value.dst, value.dst });
                try owned_values.append(.{ .reg = value.dst, .ty = register_types[value.dst], .kind = .array });
                register_owned_ptr[value.dst] = true;
            },
            .const_function => |value| {
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                switch (value.representation) {
                    .callable_value => {
                        try writer.writeAll(" = add i64 0, ");
                        try writer.print("{d}\n", .{value.function_id});
                    },
                    .native_callback => {
                        const callee_decl = functionById(request.program.*, value.function_id) orelse return error.UnknownFunction;
                        const callee_name = symbol_names.get(callee_decl.id) orelse return error.MissingFunctionDeclaration;
                        try writer.writeAll(" = ptrtoint ptr ");
                        try writeLlvmSymbol(writer, callee_name);
                        try writer.writeAll(" to i64\n");
                    },
                }
            },
            .const_closure => |value| {
                try writer.print("  %closure.size.ptr.{d} = getelementptr inbounds [{d} x %kira.bridge.value], ptr null, i32 1\n", .{ value.dst, value.captures.len });
                try writer.print("  %closure.captures.size.{d} = ptrtoint ptr %closure.size.ptr.{d} to i64\n", .{ value.dst, value.dst });
                try writer.print("  %closure.size.{d} = add i64 16, %closure.captures.size.{d}\n", .{ value.dst, value.dst });
                try writer.print("  %closure.ptr.{d} = call ptr @malloc(i64 %closure.size.{d})\n", .{ value.dst, value.dst });
                try writer.print("  store i64 {d}, ptr %closure.ptr.{d}\n", .{ value.function_id, value.dst });
                try writer.print("  %closure.count.ptr.{d} = getelementptr inbounds i64, ptr %closure.ptr.{d}, i64 1\n", .{ value.dst, value.dst });
                try writer.print("  store i64 {d}, ptr %closure.count.ptr.{d}\n", .{ value.captures.len, value.dst });
                try writer.print("  %closure.slots.{d} = getelementptr inbounds i8, ptr %closure.ptr.{d}, i64 16\n", .{ value.dst, value.dst });
                for (value.captures, 0..) |capture_reg, index| {
                    const capture_ty = register_types[capture_reg];
                    try writer.print("  %closure.slot.{d}.{d} = getelementptr inbounds %kira.bridge.value, ptr %closure.slots.{d}, i64 {d}\n", .{ value.dst, index, value.dst, index });
                    try writer.print("  %closure.pack.{d}.{d}.0 = insertvalue %kira.bridge.value zeroinitializer, i8 {d}, 0\n", .{ value.dst, index, bridgeTagValue(capture_ty) });
                    switch (capture_ty.kind) {
                        .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => {
                            try writer.print("  %closure.pack.{d}.{d} = insertvalue %kira.bridge.value %closure.pack.{d}.{d}.0, i64 %r{d}, 2\n", .{ value.dst, index, value.dst, index, capture_reg });
                        },
                        .boolean => {
                            try writer.print("  %closure.bool.{d}.{d} = zext i1 %r{d} to i64\n", .{ value.dst, index, capture_reg });
                            try writer.print("  %closure.pack.{d}.{d} = insertvalue %kira.bridge.value %closure.pack.{d}.{d}.0, i64 %closure.bool.{d}.{d}, 2\n", .{ value.dst, index, value.dst, index, value.dst, index });
                        },
                        .float => {
                            if (capture_ty.name != null and std.mem.eql(u8, capture_ty.name.?, "F32")) {
                                try writer.print("  %closure.float.ext.{d}.{d} = fpext float %r{d} to double\n", .{ value.dst, index, capture_reg });
                                try writer.print("  %closure.float.bits.{d}.{d} = bitcast double %closure.float.ext.{d}.{d} to i64\n", .{ value.dst, index, value.dst, index });
                            } else {
                                try writer.print("  %closure.float.bits.{d}.{d} = bitcast double %r{d} to i64\n", .{ value.dst, index, capture_reg });
                            }
                            try writer.print("  %closure.pack.{d}.{d} = insertvalue %kira.bridge.value %closure.pack.{d}.{d}.0, i64 %closure.float.bits.{d}.{d}, 2\n", .{ value.dst, index, value.dst, index, value.dst, index });
                        },
                        .string => {
                            try writer.print("  %closure.str.ptr.{d}.{d} = extractvalue %kira.string %r{d}, 0\n", .{ value.dst, index, capture_reg });
                            try writer.print("  %closure.str.ptrint.{d}.{d} = ptrtoint ptr %closure.str.ptr.{d}.{d} to i64\n", .{ value.dst, index, value.dst, index });
                            try writer.print("  %closure.str.len.{d}.{d} = extractvalue %kira.string %r{d}, 1\n", .{ value.dst, index, capture_reg });
                            try writer.print("  %closure.pack.{d}.{d}.1 = insertvalue %kira.bridge.value %closure.pack.{d}.{d}.0, i64 %closure.str.ptrint.{d}.{d}, 2\n", .{ value.dst, index, value.dst, index, value.dst, index });
                            try writer.print("  %closure.pack.{d}.{d} = insertvalue %kira.bridge.value %closure.pack.{d}.{d}.1, i64 %closure.str.len.{d}.{d}, 3\n", .{ value.dst, index, value.dst, index, value.dst, index });
                        },
                        .void => return error.UnsupportedExecutableFeature,
                    }
                    try writer.print("  store %kira.bridge.value %closure.pack.{d}.{d}, ptr %closure.slot.{d}.{d}\n", .{ value.dst, index, value.dst, index });
                }
                try writer.print("  %r{d} = ptrtoint ptr %closure.ptr.{d} to i64\n", .{ value.dst, value.dst });
                try owned_values.append(.{ .reg = value.dst, .ty = register_types[value.dst], .kind = .raw_heap });
                register_owned_ptr[value.dst] = true;
            },
            .add => |value| {
                const arithmetic_type = register_types[value.lhs];
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(if (arithmetic_type.kind == .float) " = fadd " else " = add ");
                try writer.writeAll(llvmValueTypeText(arithmetic_type));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.lhs});
                try writer.writeAll(", %r");
                try writer.print("{d}\n", .{value.rhs});
            },
            .subtract => |value| {
                const arithmetic_type = register_types[value.lhs];
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(if (arithmetic_type.kind == .float) " = fsub " else " = sub ");
                try writer.writeAll(llvmValueTypeText(arithmetic_type));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.lhs});
                try writer.writeAll(", %r");
                try writer.print("{d}\n", .{value.rhs});
            },
            .multiply => |value| {
                const arithmetic_type = register_types[value.lhs];
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(if (arithmetic_type.kind == .float) " = fmul " else " = mul ");
                try writer.writeAll(llvmValueTypeText(arithmetic_type));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.lhs});
                try writer.writeAll(", %r");
                try writer.print("{d}\n", .{value.rhs});
            },
            .divide => |value| {
                const arithmetic_type = register_types[value.lhs];
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(if (arithmetic_type.kind == .float) " = fdiv " else " = sdiv ");
                try writer.writeAll(llvmValueTypeText(arithmetic_type));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.lhs});
                try writer.writeAll(", %r");
                try writer.print("{d}\n", .{value.rhs});
            },
            .modulo => |value| {
                const arithmetic_type = register_types[value.lhs];
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(if (arithmetic_type.kind == .float) " = frem " else " = srem ");
                try writer.writeAll(llvmValueTypeText(arithmetic_type));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.lhs});
                try writer.writeAll(", %r");
                try writer.print("{d}\n", .{value.rhs});
            },
            .compare => |value| {
                const operand_type = register_types[value.lhs];
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(if (operand_type.kind == .float) " = fcmp " else " = icmp ");
                try writer.writeAll(try llvmComparePredicate(operand_type, value.op));
                try writer.writeByte(' ');
                try writer.writeAll(llvmCompareValueTypeText(operand_type));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.lhs});
                try writer.writeAll(", %r");
                try writer.print("{d}\n", .{value.rhs});
            },
            .unary => |value| switch (value.op) {
                .negate => {
                    const unary_type = register_types[value.src];
                    try writer.writeAll("  %r");
                    try writer.print("{d}", .{value.dst});
                    if (unary_type.kind == .float) {
                        try writer.writeAll(" = fsub double 0.0, %r");
                        try writer.print("{d}\n", .{value.src});
                    } else {
                        try writer.writeAll(" = sub i64 0, %r");
                        try writer.print("{d}\n", .{value.src});
                    }
                },
                .not => {
                    try writer.writeAll("  %r");
                    try writer.print("{d}", .{value.dst});
                    try writer.writeAll(" = xor i1 %r");
                    try writer.print("{d}", .{value.src});
                    try writer.writeAll(", true\n");
                },
            },
            .store_local => |value| {
                try writer.writeAll("  store ");
                try writer.writeAll(llvmValueTypeText(register_types[value.src]));
                try writer.writeAll(" %r");
                try writer.print("{d}", .{value.src});
                try writer.writeAll(", ptr %local");
                try writer.print("{d}\n", .{value.local});
                if (register_owned_ptr[value.src]) {
                    switch (variant.local_types[value.local].kind) {
                        .array, .raw_ptr, .string => {
                            local_owned_values[value.local] = true;
                            register_escaped[value.src] = true;
                        },
                        else => {},
                    }
                }
                if (register_local_ptr[value.src]) |source_local| {
                    switch (variant.local_types[value.local].kind) {
                        .array, .raw_ptr, .string => switch (variant.local_types[source_local].kind) {
                            .array, .raw_ptr, .string => {
                                if (local_owned_values[source_local]) {
                                    local_owned_values[source_local] = false;
                                    local_owned_values[value.local] = true;
                                }
                            },
                            else => {},
                        },
                        else => {},
                    }
                }
            },
            .load_local => |value| {
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                switch (function_decl.local_types[value.local].kind) {
                    .ffi_struct => {
                        try writer.writeAll(" = load i64, ptr %local");
                        try writer.print("{d}\n", .{value.local});
                        register_local_ptr[value.dst] = value.local;
                    },
                    .array, .raw_ptr, .string => {
                        try writer.writeAll(" = load ");
                        try writer.writeAll(llvmValueTypeText(function_decl.local_types[value.local]));
                        try writer.writeAll(", ptr %local");
                        try writer.print("{d}\n", .{value.local});
                        register_local_ptr[value.dst] = value.local;
                    },
                    else => {
                        try writer.writeAll(" = load ");
                        try writer.writeAll(llvmValueTypeText(function_decl.local_types[value.local]));
                        try writer.writeAll(", ptr %local");
                        try writer.print("{d}\n", .{value.local});
                    },
                }
            },
            .local_ptr => |value| {
                try writer.print("  %r{d} = ptrtoint ptr %local{d} to i64\n", .{ value.dst, value.local });
                register_local_ptr[value.dst] = value.local;
            },
            .subobject_ptr => |value| {
                const base_type_name = register_types[value.base].name orelse return error.UnsupportedExecutableFeature;
                const struct_type_name = typeRefName(base_type_name);
                try writer.print("  %subobject.base.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.base });
                try writer.print("  %subobject.ptr.{d} = getelementptr inbounds {s}, ptr %subobject.base.{d}, i32 0, i32 {d}\n", .{ value.dst, struct_type_name, value.dst, value.offset });
                try writer.print("  %r{d} = ptrtoint ptr %subobject.ptr.{d} to i64\n", .{ value.dst, value.dst });
            },
            .field_ptr => |value| {
                const struct_type_name = typeRefName(value.base_type_name);
                try writer.print("  %field.base.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.base });
                try writer.writeAll("  %field.ptr.");
                try writer.print("{d}", .{value.dst});
                try writer.print(" = getelementptr inbounds {s}, ptr %field.base.{d}, i32 0, i32 {d}\n", .{ struct_type_name, value.dst, value.field_index });
                try writer.writeAll("  %r");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" = ptrtoint ptr %field.ptr.");
                try writer.print("{d}", .{value.dst});
                try writer.writeAll(" to i64\n");
            },
            .recover_native_state => |value| {
                try writer.print("  %native.recover.state.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.state });
                try writer.print("  %native.recover.payload.{d} = call ptr @\"kira_native_state_recover\"(ptr %native.recover.state.{d}, i64 {d})\n", .{
                    value.dst,
                    value.dst,
                    value.type_id,
                });
                try writer.print("  %r{d} = ptrtoint ptr %native.recover.payload.{d} to i64\n", .{ value.dst, value.dst });
            },
            .native_state_field_get => |value| {
                const temp_index = temp_counter;
                temp_counter += 1;
                try writer.print("  %native.state.get.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.state });
                try writer.print("  %native.state.get.slot.{d} = getelementptr inbounds %kira.bridge.value, ptr %native.state.get.ptr.{d}, i64 {d}\n", .{
                    value.dst,
                    value.dst,
                    value.field_index,
                });
                try writer.print("  %native.state.get.val.{d} = load %kira.bridge.value, ptr %native.state.get.slot.{d}\n", .{ temp_index, value.dst });
                switch (value.field_ty.kind) {
                    .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => {
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = extractvalue %kira.bridge.value %native.state.get.val.{d}, 2\n", .{temp_index});
                    },
                    .float => {
                        try writer.print("  %native.state.get.bits.{d} = extractvalue %kira.bridge.value %native.state.get.val.{d}, 2\n", .{ temp_index, temp_index });
                        if (value.field_ty.name != null and std.mem.eql(u8, value.field_ty.name.?, "F32")) {
                            try writer.print("  %native.state.get.float64.{d} = bitcast i64 %native.state.get.bits.{d} to double\n", .{ temp_index, temp_index });
                            try writer.writeAll("  %r");
                            try writer.print("{d}", .{value.dst});
                            try writer.print(" = fptrunc double %native.state.get.float64.{d} to float\n", .{temp_index});
                        } else {
                            try writer.writeAll("  %r");
                            try writer.print("{d}", .{value.dst});
                            try writer.print(" = bitcast i64 %native.state.get.bits.{d} to double\n", .{temp_index});
                        }
                    },
                    .boolean => {
                        try writer.print("  %native.state.get.word.{d} = extractvalue %kira.bridge.value %native.state.get.val.{d}, 2\n", .{ temp_index, temp_index });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = trunc i64 %native.state.get.word.{d} to i1\n", .{temp_index});
                    },
                    .string => {
                        try writer.print("  %native.state.get.ptrint.{d} = extractvalue %kira.bridge.value %native.state.get.val.{d}, 2\n", .{ temp_index, temp_index });
                        try writer.print("  %native.state.get.len.{d} = extractvalue %kira.bridge.value %native.state.get.val.{d}, 3\n", .{ temp_index, temp_index });
                        try writer.print("  %native.state.get.ptrcast.{d} = inttoptr i64 %native.state.get.ptrint.{d} to ptr\n", .{ temp_index, temp_index });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(".0 = insertvalue %kira.string zeroinitializer, ptr %native.state.get.ptrcast.{d}, 0\n", .{temp_index});
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = insertvalue %kira.string %r{d}.0, i64 %native.state.get.len.{d}, 1\n", .{ value.dst, temp_index });
                    },
                    .void => return error.UnsupportedExecutableFeature,
                }
            },
            .native_state_field_set => |value| {
                const temp_index = temp_counter;
                temp_counter += 1;
                try writer.print("  %native.state.set.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src, value.state });
                try writer.print("  %native.state.set.slot.{d} = getelementptr inbounds %kira.bridge.value, ptr %native.state.set.ptr.{d}, i64 {d}\n", .{
                    value.src,
                    value.src,
                    value.field_index,
                });
                try writer.print("  %native.state.set.pack.{d}.0 = insertvalue %kira.bridge.value zeroinitializer, i8 {d}, 0\n", .{
                    temp_index,
                    bridgeTagValue(register_types[value.src]),
                });
                switch (register_types[value.src].kind) {
                    .integer, .construct_any, .raw_ptr, .array, .enum_instance => {
                        try writer.print("  %native.state.set.pack.{d} = insertvalue %kira.bridge.value %native.state.set.pack.{d}.0, i64 %r{d}, 2\n", .{
                            temp_index, temp_index, value.src,
                        });
                    },
                    .ffi_struct => {
                        const field_struct_name = typeRefName(register_types[value.src].name orelse return error.UnsupportedExecutableFeature);
                        try writer.print("  %native.state.set.struct.src.{d} = inttoptr i64 %r{d} to ptr\n", .{ temp_index, value.src });
                        try writer.print("  %native.state.set.struct.value.{d} = load {s}, ptr %native.state.set.struct.src.{d}\n", .{
                            temp_index, field_struct_name, temp_index,
                        });
                        try writer.print("  %native.state.set.struct.size.ptr.{d} = getelementptr inbounds {s}, ptr null, i32 1\n", .{
                            temp_index, field_struct_name,
                        });
                        try writer.print("  %native.state.set.struct.size.{d} = ptrtoint ptr %native.state.set.struct.size.ptr.{d} to i64\n", .{
                            temp_index, temp_index,
                        });
                        try writer.print("  %native.state.set.struct.copy.{d} = call ptr @malloc(i64 %native.state.set.struct.size.{d})\n", .{
                            temp_index, temp_index,
                        });
                        try writer.print("  store {s} %native.state.set.struct.value.{d}, ptr %native.state.set.struct.copy.{d}\n", .{
                            field_struct_name, temp_index, temp_index,
                        });
                        try writer.print("  %native.state.set.struct.ptrint.{d} = ptrtoint ptr %native.state.set.struct.copy.{d} to i64\n", .{
                            temp_index, temp_index,
                        });
                        try writer.print("  %native.state.set.pack.{d} = insertvalue %kira.bridge.value %native.state.set.pack.{d}.0, i64 %native.state.set.struct.ptrint.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .float => {
                        if (register_types[value.src].name != null and std.mem.eql(u8, register_types[value.src].name.?, "F32")) {
                            try writer.print("  %native.state.set.float.ext.{d} = fpext float %r{d} to double\n", .{ temp_index, value.src });
                            try writer.print("  %native.state.set.float.bits.{d} = bitcast double %native.state.set.float.ext.{d} to i64\n", .{ temp_index, temp_index });
                        } else {
                            try writer.print("  %native.state.set.float.bits.{d} = bitcast double %r{d} to i64\n", .{ temp_index, value.src });
                        }
                        try writer.print("  %native.state.set.pack.{d} = insertvalue %kira.bridge.value %native.state.set.pack.{d}.0, i64 %native.state.set.float.bits.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .boolean => {
                        try writer.print("  %native.state.set.bool.{d} = zext i1 %r{d} to i64\n", .{ temp_index, value.src });
                        try writer.print("  %native.state.set.pack.{d} = insertvalue %kira.bridge.value %native.state.set.pack.{d}.0, i64 %native.state.set.bool.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .string => {
                        try writer.print("  %native.state.set.str.ptr.{d} = extractvalue %kira.string %r{d}, 0\n", .{ temp_index, value.src });
                        try writer.print("  %native.state.set.str.ptrint.{d} = ptrtoint ptr %native.state.set.str.ptr.{d} to i64\n", .{ temp_index, temp_index });
                        try writer.print("  %native.state.set.str.len.{d} = extractvalue %kira.string %r{d}, 1\n", .{ temp_index, value.src });
                        try writer.print("  %native.state.set.pack.{d}.1 = insertvalue %kira.bridge.value %native.state.set.pack.{d}.0, i64 %native.state.set.str.ptrint.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                        try writer.print("  %native.state.set.pack.{d} = insertvalue %kira.bridge.value %native.state.set.pack.{d}.1, i64 %native.state.set.str.len.{d}, 3\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .void => return error.UnsupportedExecutableFeature,
                }
                try writer.print("  store %kira.bridge.value %native.state.set.pack.{d}, ptr %native.state.set.slot.{d}\n", .{
                    temp_index,
                    value.src,
                });
                if (register_local_ptr[value.src]) |source_local| {
                    switch (variant.local_types[source_local].kind) {
                        .ffi_struct => local_owns_contents[source_local] = false,
                        .array, .raw_ptr, .string => local_owned_values[source_local] = false,
                        else => {},
                    }
                }
            },
            .c_string_to_string => |value| {
                try writer.print("  %cstring.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.src });
                try writer.print("  %cstring.len.{d} = call i64 @strlen(ptr %cstring.ptr.{d})\n", .{ value.dst, value.dst });
                try writer.print("  %cstring.copy.{d} = call ptr @malloc(i64 %cstring.len.{d})\n", .{ value.dst, value.dst });
                try writer.print("  call void @llvm.memcpy.p0.p0.i64(ptr %cstring.copy.{d}, ptr %cstring.ptr.{d}, i64 %cstring.len.{d}, i1 false)\n", .{ value.dst, value.dst, value.dst });
                try writer.print("  %r{d}.0 = insertvalue %kira.string zeroinitializer, ptr %cstring.copy.{d}, 0\n", .{ value.dst, value.dst });
                try writer.print("  %r{d} = insertvalue %kira.string %r{d}.0, i64 %cstring.len.{d}, 1\n", .{ value.dst, value.dst, value.dst });
            },
            .array_len => |value| {
                try writer.print("  %array.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.array });
                try writer.print("  %r{d} = call i64 @\"kira_array_len\"(ptr %array.ptr.{d})\n", .{ value.dst, value.dst });
            },
            .array_get => |value| {
                const temp_index = temp_counter;
                temp_counter += 1;
                try writer.print("  %array.get.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.array });
                try writer.print("  %array.get.val.ptr.{d} = alloca %kira.bridge.value\n", .{temp_index});
                try writer.print("  call void @\"kira_array_load\"(ptr %array.get.ptr.{d}, i64 %r{d}, ptr %array.get.val.ptr.{d})\n", .{
                    value.dst, value.index, temp_index,
                });
                try writer.print("  %array.get.val.{d} = load %kira.bridge.value, ptr %array.get.val.ptr.{d}\n", .{ temp_index, temp_index });
                switch (value.ty.kind) {
                    .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => {
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = extractvalue %kira.bridge.value %array.get.val.{d}, 2\n", .{temp_index});
                    },
                    .float => {
                        try writer.print("  %array.get.bits.{d} = extractvalue %kira.bridge.value %array.get.val.{d}, 2\n", .{ temp_index, temp_index });
                        if (value.ty.name != null and std.mem.eql(u8, value.ty.name.?, "F32")) {
                            try writer.print("  %array.get.float64.{d} = bitcast i64 %array.get.bits.{d} to double\n", .{ temp_index, temp_index });
                            try writer.writeAll("  %r");
                            try writer.print("{d}", .{value.dst});
                            try writer.print(" = fptrunc double %array.get.float64.{d} to float\n", .{temp_index});
                        } else {
                            try writer.writeAll("  %r");
                            try writer.print("{d}", .{value.dst});
                            try writer.print(" = bitcast i64 %array.get.bits.{d} to double\n", .{temp_index});
                        }
                    },
                    .boolean => {
                        try writer.print("  %array.get.word.{d} = extractvalue %kira.bridge.value %array.get.val.{d}, 2\n", .{ temp_index, temp_index });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = trunc i64 %array.get.word.{d} to i1\n", .{temp_index});
                    },
                    .string => {
                        try writer.print("  %array.get.ptrint.{d} = extractvalue %kira.bridge.value %array.get.val.{d}, 2\n", .{ temp_index, temp_index });
                        try writer.print("  %array.get.len.{d} = extractvalue %kira.bridge.value %array.get.val.{d}, 3\n", .{ temp_index, temp_index });
                        try writer.print("  %array.get.ptrcast.{d} = inttoptr i64 %array.get.ptrint.{d} to ptr\n", .{ temp_index, temp_index });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(".0 = insertvalue %kira.string zeroinitializer, ptr %array.get.ptrcast.{d}, 0\n", .{temp_index});
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = insertvalue %kira.string %r{d}.0, i64 %array.get.len.{d}, 1\n", .{ value.dst, temp_index });
                    },
                    .void => return error.UnsupportedExecutableFeature,
                }
            },
            .array_set => |value| {
                const temp_index = temp_counter;
                temp_counter += 1;
                try writer.print("  %array.set.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src, value.array });
                try writer.print("  %array.set.pack.{d}.0 = insertvalue %kira.bridge.value zeroinitializer, i8 {d}, 0\n", .{
                    temp_index, bridgeTagValue(register_types[value.src]),
                });
                switch (register_types[value.src].kind) {
                    .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => {
                        try writer.print("  %array.set.pack.{d} = insertvalue %kira.bridge.value %array.set.pack.{d}.0, i64 %r{d}, 2\n", .{
                            temp_index, temp_index, value.src,
                        });
                    },
                    .float => {
                        if (register_types[value.src].name != null and std.mem.eql(u8, register_types[value.src].name.?, "F32")) {
                            try writer.print("  %array.set.float.ext.{d} = fpext float %r{d} to double\n", .{ temp_index, value.src });
                            try writer.print("  %array.set.float.bits.{d} = bitcast double %array.set.float.ext.{d} to i64\n", .{ temp_index, temp_index });
                        } else {
                            try writer.print("  %array.set.float.bits.{d} = bitcast double %r{d} to i64\n", .{ temp_index, value.src });
                        }
                        try writer.print("  %array.set.pack.{d} = insertvalue %kira.bridge.value %array.set.pack.{d}.0, i64 %array.set.float.bits.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .boolean => {
                        try writer.print("  %array.set.bool.{d} = zext i1 %r{d} to i64\n", .{ temp_index, value.src });
                        try writer.print("  %array.set.pack.{d} = insertvalue %kira.bridge.value %array.set.pack.{d}.0, i64 %array.set.bool.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .string => {
                        try writer.print("  %array.set.str.ptr.{d} = extractvalue %kira.string %r{d}, 0\n", .{ temp_index, value.src });
                        try writer.print("  %array.set.str.ptrint.{d} = ptrtoint ptr %array.set.str.ptr.{d} to i64\n", .{ temp_index, temp_index });
                        try writer.print("  %array.set.str.len.{d} = extractvalue %kira.string %r{d}, 1\n", .{ temp_index, value.src });
                        try writer.print("  %array.set.pack.{d}.1 = insertvalue %kira.bridge.value %array.set.pack.{d}.0, i64 %array.set.str.ptrint.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                        try writer.print("  %array.set.pack.{d} = insertvalue %kira.bridge.value %array.set.pack.{d}.1, i64 %array.set.str.len.{d}, 3\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .void => return error.UnsupportedExecutableFeature,
                }
                try writer.print("  %array.set.pack.ptr.{d} = alloca %kira.bridge.value\n", .{temp_index});
                try writer.print("  store %kira.bridge.value %array.set.pack.{d}, ptr %array.set.pack.ptr.{d}\n", .{ temp_index, temp_index });
                try writer.print("  call void @\"kira_array_store\"(ptr %array.set.ptr.{d}, i64 %r{d}, ptr %array.set.pack.ptr.{d})\n", .{
                    value.src, value.index, temp_index,
                });
                if (register_owned_ptr[value.src]) register_escaped[value.src] = true;
                if (register_local_ptr[value.src]) |source_local| {
                    switch (variant.local_types[source_local].kind) {
                        .ffi_struct => local_owns_contents[source_local] = false,
                        .array, .raw_ptr, .string => local_owned_values[source_local] = false,
                        else => {},
                    }
                }
            },
            .array_append => |value| {
                const temp_index = temp_counter;
                temp_counter += 1;
                try writer.print("  %array.append.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src, value.array });
                try writer.print("  %array.append.pack.{d}.0 = insertvalue %kira.bridge.value zeroinitializer, i8 {d}, 0\n", .{
                    temp_index, bridgeTagValue(register_types[value.src]),
                });
                switch (register_types[value.src].kind) {
                    .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => {
                        try writer.print("  %array.append.pack.{d} = insertvalue %kira.bridge.value %array.append.pack.{d}.0, i64 %r{d}, 2\n", .{
                            temp_index, temp_index, value.src,
                        });
                    },
                    .float => {
                        if (register_types[value.src].name != null and std.mem.eql(u8, register_types[value.src].name.?, "F32")) {
                            try writer.print("  %array.append.float.ext.{d} = fpext float %r{d} to double\n", .{ temp_index, value.src });
                            try writer.print("  %array.append.float.bits.{d} = bitcast double %array.append.float.ext.{d} to i64\n", .{ temp_index, temp_index });
                        } else {
                            try writer.print("  %array.append.float.bits.{d} = bitcast double %r{d} to i64\n", .{ temp_index, value.src });
                        }
                        try writer.print("  %array.append.pack.{d} = insertvalue %kira.bridge.value %array.append.pack.{d}.0, i64 %array.append.float.bits.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .boolean => {
                        try writer.print("  %array.append.bool.{d} = zext i1 %r{d} to i64\n", .{ temp_index, value.src });
                        try writer.print("  %array.append.pack.{d} = insertvalue %kira.bridge.value %array.append.pack.{d}.0, i64 %array.append.bool.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .string => {
                        try writer.print("  %array.append.str.ptr.{d} = extractvalue %kira.string %r{d}, 0\n", .{ temp_index, value.src });
                        try writer.print("  %array.append.str.ptrint.{d} = ptrtoint ptr %array.append.str.ptr.{d} to i64\n", .{ temp_index, temp_index });
                        try writer.print("  %array.append.str.len.{d} = extractvalue %kira.string %r{d}, 1\n", .{ temp_index, value.src });
                        try writer.print("  %array.append.pack.{d}.1 = insertvalue %kira.bridge.value %array.append.pack.{d}.0, i64 %array.append.str.ptrint.{d}, 2\n", .{
                            temp_index, temp_index, temp_index,
                        });
                        try writer.print("  %array.append.pack.{d} = insertvalue %kira.bridge.value %array.append.pack.{d}.1, i64 %array.append.str.len.{d}, 3\n", .{
                            temp_index, temp_index, temp_index,
                        });
                    },
                    .void => return error.UnsupportedExecutableFeature,
                }
                try writer.print("  %array.append.pack.ptr.{d} = alloca %kira.bridge.value\n", .{temp_index});
                try writer.print("  store %kira.bridge.value %array.append.pack.{d}, ptr %array.append.pack.ptr.{d}\n", .{ temp_index, temp_index });
                try writer.print("  call void @\"kira_array_append\"(ptr %array.append.ptr.{d}, ptr %array.append.pack.ptr.{d})\n", .{
                    value.src, temp_index,
                });
                if (register_owned_ptr[value.src]) register_escaped[value.src] = true;
                if (register_local_ptr[value.src]) |source_local| {
                    switch (variant.local_types[source_local].kind) {
                        .ffi_struct => local_owns_contents[source_local] = false,
                        .array, .raw_ptr, .string => local_owned_values[source_local] = false,
                        else => {},
                    }
                }
            },
            .enum_tag => |value| try enum_ops.emitEnumTag(writer, value),
            .enum_payload => |value| try enum_ops.emitEnumPayload(writer, value),
            .load_indirect => |value| {
                const abi_type = try llvmIndirectLoadTypeText(allocator, request.program, value.ty);
                try writer.print("  %load.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst, value.ptr });
                switch (value.ty.kind) {
                    .integer => {
                        try writer.print("  %load.raw.{d} = load {s}, ptr %load.ptr.{d}\n", .{ value.dst, abi_type, value.dst });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        if (std.mem.eql(u8, abi_type, "i8") or std.mem.eql(u8, abi_type, "i16") or std.mem.eql(u8, abi_type, "i32")) {
                            try writer.print(" = sext {s} %load.raw.{d} to i64\n", .{ abi_type, value.dst });
                        } else {
                            try writer.print(" = load i64, ptr %load.ptr.{d}\n", .{value.dst});
                        }
                    },
                    .boolean => {
                        try writer.print("  %load.raw.{d} = load i8, ptr %load.ptr.{d}\n", .{ value.dst, value.dst });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = trunc i8 %load.raw.{d} to i1\n", .{value.dst});
                    },
                    .construct_any, .raw_ptr, .enum_instance => {
                        try writer.print("  %load.rawptr.{d} = load ptr, ptr %load.ptr.{d}\n", .{ value.dst, value.dst });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = ptrtoint ptr %load.rawptr.{d} to i64\n", .{value.dst});
                    },
                    .array => {
                        try writer.print("  %load.arrayptr.{d} = load ptr, ptr %load.ptr.{d}\n", .{ value.dst, value.dst });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = ptrtoint ptr %load.arrayptr.{d} to i64\n", .{value.dst});
                    },
                    .float => {
                        try writer.print("  %load.raw.float.{d} = load {s}, ptr %load.ptr.{d}\n", .{ value.dst, abi_type, value.dst });
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        if (std.mem.eql(u8, abi_type, "float")) {
                            try writer.print(" = fpext float %load.raw.float.{d} to double\n", .{value.dst});
                        } else {
                            try writer.print(" = fadd double %load.raw.float.{d}, 0.0\n", .{value.dst});
                        }
                    },
                    .string => {
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = load {s}, ptr %load.ptr.{d}\n", .{ abi_type, value.dst });
                    },
                    .ffi_struct => {
                        try writer.writeAll("  %r");
                        try writer.print("{d}", .{value.dst});
                        try writer.print(" = add i64 %r{d}, 0\n", .{value.ptr});
                    },
                    else => return error.UnsupportedExecutableFeature,
                }
            },
            .store_indirect => |value| {
                const abi_type = try llvmIndirectLoadTypeText(allocator, request.program, value.ty);
                try writer.print("  %store.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src, value.ptr });
                switch (value.ty.kind) {
                    .integer => {
                        if (std.mem.eql(u8, abi_type, "i64")) {
                            try writer.print("  store i64 %r{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                        } else {
                            try writer.print("  %store.cast.{d} = trunc i64 %r{d} to {s}\n", .{ value.src, value.src, abi_type });
                            try writer.print("  store {s} %store.cast.{d}, ptr %store.ptr.{d}\n", .{ abi_type, value.src, value.src });
                        }
                    },
                    .boolean => {
                        try writer.print("  %store.bool.{d} = zext i1 %r{d} to i8\n", .{ value.src, value.src });
                        try writer.print("  store i8 %store.bool.{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                    },
                    .construct_any, .raw_ptr, .enum_instance => {
                        if (value.ty.name != null and std.mem.eql(u8, value.ty.name.?, "CString") and register_types[value.src].kind == .string) {
                            try writer.print("  %store.cstr.{d} = extractvalue %kira.string %r{d}, 0\n", .{ value.src, value.src });
                            try writer.print("  store ptr %store.cstr.{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                        } else {
                            try writer.print("  %store.rawptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src, value.src });
                            try writer.print("  store ptr %store.rawptr.{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                        }
                    },
                    .array => {
                        try writer.print("  %store.arrayptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src, value.src });
                        try writer.print("  store ptr %store.arrayptr.{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                    },
                    .float => {
                        if (std.mem.eql(u8, abi_type, "float")) {
                            try writer.print("  %store.float.{d} = fptrunc double %r{d} to float\n", .{ value.src, value.src });
                            try writer.print("  store float %store.float.{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                        } else {
                            try writer.print("  store double %r{d}, ptr %store.ptr.{d}\n", .{ value.src, value.src });
                        }
                    },
                    .string => {
                        try writer.print("  store {s} %r{d}, ptr %store.ptr.{d}\n", .{ abi_type, value.src, value.src });
                    },
                    else => return error.UnsupportedExecutableFeature,
                }
                if (register_owned_ptr[value.src]) register_escaped[value.src] = true;
                if (register_local_ptr[value.src]) |source_local| {
                    switch (variant.local_types[source_local].kind) {
                        .ffi_struct => local_owns_contents[source_local] = false,
                        .array, .raw_ptr, .string => local_owned_values[source_local] = false,
                        else => {},
                    }
                }
            },
            .copy_indirect => |value| {
                const struct_type_name = typeRefName(value.type_name);
                try writer.print("  %copy.dst.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.dst_ptr, value.dst_ptr });
                try writer.print("  %copy.src.{d} = inttoptr i64 %r{d} to ptr\n", .{ value.src_ptr, value.src_ptr });
                try writer.print("  %copy.val.{d} = load {s}, ptr %copy.src.{d}\n", .{ value.dst_ptr, struct_type_name, value.src_ptr });
                try writer.print("  store {s} %copy.val.{d}, ptr %copy.dst.{d}\n", .{ struct_type_name, value.dst_ptr, value.dst_ptr });
                if (register_local_ptr[value.dst_ptr]) |local_index| {
                    if (register_owned_ptr[value.src_ptr]) local_owns_contents[local_index] = true;
                    if (register_local_ptr[value.src_ptr]) |source_local| {
                        if (local_owns_contents[source_local]) {
                            local_owns_contents[source_local] = false;
                            local_owns_contents[local_index] = true;
                        }
                    }
                }
                if (register_local_ptr[value.src_ptr]) |source_local| {
                    if (local_owns_contents[source_local]) local_owns_contents[source_local] = false;
                }
            },
            .branch => |value| {
                try writer.writeAll("  br i1 %r");
                try writer.print("{d}", .{value.condition});
                try writer.writeAll(", label %");
                try writeLlvmLabelName(writer, value.true_label);
                try writer.writeAll(", label %");
                try writeLlvmLabelName(writer, value.false_label);
                try writer.writeAll("\n");
                block_terminated = true;
            },
            .jump => |value| {
                try writer.writeAll("  br label %");
                try writeLlvmLabelName(writer, value.label);
                try writer.writeAll("\n");
                block_terminated = true;
            },
            .label => |value| {
                if (!block_terminated) {
                    try writer.writeAll("  br label %");
                    try writeLlvmLabelName(writer, value.id);
                    try writer.writeAll("\n");
                }
                try writeLlvmLabelName(writer, value.id);
                try writer.writeAll(":\n");
                block_terminated = false;
            },
            .print => |value| {
                try writePrintInstruction(
                    allocator,
                    writer,
                    request.program,
                    globals,
                    register_types[value.src],
                    value.src,
                    &string_state[0],
                    &temp_counter,
                );
            },
            .call => |value| {
                try writeCallInstruction(writer, request, plan, symbol_names, request.program, register_types, value, &temp_counter);
                if (value.dst) |dst| {
                    const callee_decl = functionById(request.program.*, value.callee) orelse return error.UnknownFunction;
                    const owns_return = !callee_decl.is_extern and try functionReturnOwnsAllocation(allocator, request.program, value.callee, 0);
                    if (owns_return and register_types[dst].kind == .array) {
                        try owned_values.append(.{ .reg = dst, .ty = register_types[dst], .kind = .array });
                        register_owned_ptr[dst] = true;
                    }
                    if (owns_return and register_types[dst].kind == .ffi_struct) {
                        try writer.print("  %cleanup.call.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ dst, dst });
                        try writer.print("  store ptr %cleanup.call.ptr.{d}, ptr %cleanup.heap.slot.{d}\n", .{ dst, dst });
                        try owned_values.append(.{ .reg = dst, .ty = register_types[dst], .kind = .heap_ptr });
                        register_owned_ptr[dst] = true;
                    }
                }
            },
            .call_virtual => |value| {
                const type_decl = findTypeDecl(request.program, value.static_type_name) orelse return error.UnknownType;
                var resolved_callee: ?u32 = null;
                for (type_decl.methods) |method_decl| {
                    if (std.mem.eql(u8, method_decl.name, value.method_name)) {
                        resolved_callee = method_decl.function_id;
                        break;
                    }
                }
                const callee = resolved_callee orelse return error.UnknownFunction;
                const args = try allocator.alloc(u32, value.args.len + 1);
                defer allocator.free(args);
                args[0] = value.receiver;
                @memcpy(args[1..], value.args);
                try writeCallInstruction(writer, request, plan, symbol_names, request.program, register_types, .{
                    .callee = callee,
                    .args = args,
                    .dst = value.dst,
                }, &temp_counter);
                if (value.dst) |dst| {
                    const callee_decl = functionById(request.program.*, callee) orelse return error.UnknownFunction;
                    const owns_return = !callee_decl.is_extern and try functionReturnOwnsAllocation(allocator, request.program, callee, 0);
                    if (owns_return and register_types[dst].kind == .array) {
                        try owned_values.append(.{ .reg = dst, .ty = register_types[dst], .kind = .array });
                        register_owned_ptr[dst] = true;
                    }
                    if (owns_return and register_types[dst].kind == .ffi_struct) {
                        try writer.print("  %cleanup.call.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ dst, dst });
                        try writer.print("  store ptr %cleanup.call.ptr.{d}, ptr %cleanup.heap.slot.{d}\n", .{ dst, dst });
                        try owned_values.append(.{ .reg = dst, .ty = register_types[dst], .kind = .heap_ptr });
                        register_owned_ptr[dst] = true;
                    }
                }
            },
            .call_value => |value| {
                try writeIndirectCallInstruction(writer, request, symbol_names, request.program, register_types, value);
            },
            .ret => |value| {
                try emitFunctionCleanup(allocator, writer, request.program, variant.local_types, local_owns_contents, local_owned_values, owned_values.items, register_escaped, register_local_ptr, value.src, &temp_counter);
                if (value.src) |src| {
                    try writer.writeAll("  ret ");
                    try writer.writeAll(llvmValueTypeText(function_decl.return_type));
                    try writer.writeAll(" %r");
                    try writer.print("{d}", .{src});
                    try writer.writeAll("\n");
                } else {
                    try writer.writeAll("  ret void\n");
                }
                block_terminated = true;
            },
        }
    }
    if (!block_terminated) {
        try emitFunctionCleanup(allocator, writer, request.program, variant.local_types, local_owns_contents, local_owned_values, owned_values.items, register_escaped, register_local_ptr, null, &temp_counter);
        try writer.writeAll("  ret void\n");
    }
    try writer.writeAll("}\n");
    return body.toOwnedSlice();
}

const OwnedValue = struct {
    reg: u32,
    ty: ir.ValueType,
    kind: Kind,

    const Kind = enum {
        array,
        heap_ptr,
        raw_heap,
    };
};

fn functionReturnOwnsAllocation(
    allocator: std.mem.Allocator,
    program: *const ir.Program,
    function_id: u32,
    depth: usize,
) !bool {
    if (depth > 64) return false;
    const function_decl = functionById(program.*, function_id) orelse return error.UnknownFunction;
    if (function_decl.is_extern) return false;

    const owned = try allocator.alloc(bool, function_decl.register_count);
    defer allocator.free(owned);
    @memset(owned, false);

    var saw_value_return = false;
    for (function_decl.instructions) |instruction| {
        switch (instruction) {
            .alloc_struct => |value| owned[value.dst] = true,
            .alloc_array => |value| owned[value.dst] = true,
            .call => |value| if (value.dst) |dst| {
                const callee_decl = functionById(program.*, value.callee) orelse return error.UnknownFunction;
                owned[dst] = !callee_decl.is_extern and try functionReturnOwnsAllocation(allocator, program, value.callee, depth + 1);
            },
            .call_virtual => |value| if (value.dst) |dst| {
                const type_decl = findTypeDecl(program, value.static_type_name) orelse return error.UnknownType;
                var resolved_callee: ?u32 = null;
                for (type_decl.methods) |method_decl| {
                    if (std.mem.eql(u8, method_decl.name, value.method_name)) {
                        resolved_callee = method_decl.function_id;
                        break;
                    }
                }
                const callee = resolved_callee orelse return error.UnknownFunction;
                const callee_decl = functionById(program.*, callee) orelse return error.UnknownFunction;
                owned[dst] = !callee_decl.is_extern and try functionReturnOwnsAllocation(allocator, program, callee, depth + 1);
            },
            .ret => |value| {
                const src = value.src orelse return false;
                saw_value_return = true;
                if (src >= owned.len or !owned[src]) return false;
            },
            else => {},
        }
    }

    return saw_value_return;
}

fn emitFunctionCleanup(
    allocator: std.mem.Allocator,
    writer: anytype,
    program: *const ir.Program,
    local_types: []const ir.ValueType,
    local_owns_contents: []const bool,
    local_owned_values: []const bool,
    owned_values: []const OwnedValue,
    register_escaped: []const bool,
    register_local_ptr: []const ?usize,
    returned_reg: ?u32,
    cleanup_temp_counter: *usize,
) !void {
    for (owned_values) |owned| {
        if (returned_reg != null and returned_reg.? == owned.reg) continue;
        if (register_escaped[owned.reg]) continue;
        const cleanup_temp = cleanup_temp_counter.*;
        cleanup_temp_counter.* += 1;
        switch (owned.kind) {
            .array => {
                const release_fn = try arrayElementDestroySymbol(allocator, program, owned.ty);
                defer if (release_fn) |name| allocator.free(name);
                try writer.print("  %cleanup.array.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ cleanup_temp, owned.reg });
                try writer.writeAll("  call void @\"kira_array_release\"(ptr ");
                try writer.print("%cleanup.array.ptr.{d}, ptr ", .{cleanup_temp});
                if (release_fn) |name| {
                    try writeLlvmSymbol(writer, name);
                } else {
                    try writer.writeAll("null");
                }
                try writer.writeAll(")\n");
            },
            .heap_ptr => {
                try writer.print("  %cleanup.heap.ptr.{d} = load ptr, ptr %cleanup.heap.slot.{d}\n", .{ cleanup_temp, owned.reg });
                try writer.print("  call void @free(ptr %cleanup.heap.ptr.{d})\n", .{cleanup_temp});
            },
            .raw_heap => {
                try writer.print("  %cleanup.raw.ptr.{d} = inttoptr i64 %r{d} to ptr\n", .{ cleanup_temp, owned.reg });
                try writer.print("  call void @free(ptr %cleanup.raw.ptr.{d})\n", .{cleanup_temp});
            },
        }
    }

    const returned_local = if (returned_reg) |reg|
        if (reg < register_local_ptr.len) register_local_ptr[reg] else null
    else
        null;

    for (local_types, 0..) |local_type, index| {
        if (returned_local != null and returned_local.? == index) continue;
        switch (local_type.kind) {
            .ffi_struct => {
                if (!local_owns_contents[index]) continue;
                const release_name = try releaseContentsSymbolName(allocator, local_type.name orelse return error.UnsupportedExecutableFeature);
                defer allocator.free(release_name);
                try writer.writeAll("  call void ");
                try writeLlvmSymbol(writer, release_name);
                try writer.print("(ptr %local.storage.{d})\n", .{index});
            },
            .array => {
                if (!local_owned_values[index]) continue;
                const release_fn = try arrayElementDestroySymbol(allocator, program, local_type);
                defer if (release_fn) |name| allocator.free(name);
                const cleanup_temp = cleanup_temp_counter.*;
                cleanup_temp_counter.* += 1;
                try writer.print("  %cleanup.local.array.val.{d} = load i64, ptr %local{d}\n", .{ cleanup_temp, index });
                try writer.print("  %cleanup.local.array.ptr.{d} = inttoptr i64 %cleanup.local.array.val.{d} to ptr\n", .{ cleanup_temp, cleanup_temp });
                try writer.writeAll("  call void @\"kira_array_release\"(ptr ");
                try writer.print("%cleanup.local.array.ptr.{d}, ptr ", .{cleanup_temp});
                if (release_fn) |name| {
                    try writeLlvmSymbol(writer, name);
                } else {
                    try writer.writeAll("null");
                }
                try writer.writeAll(")\n");
            },
            else => {},
        }
    }
}

fn appendReleaseDefinitions(allocator: std.mem.Allocator, writer: anytype, program: *const ir.Program) !void {
    try writer.writeAll("define void @\"kira_destroy_raw_ptr\"(ptr %value) {\nentry:\n  call void @free(ptr %value)\n  ret void\n}\n");
    for (program.types) |type_decl| {
        if (type_decl.ffi) |ffi_info| {
            if (ffi_info != .ffi_struct) continue;
        }

        const contents_name = try releaseContentsSymbolName(allocator, type_decl.name);
        defer allocator.free(contents_name);
        const destroy_name = try destroySymbolName(allocator, type_decl.name);
        defer allocator.free(destroy_name);
        const struct_type_name = typeRefName(type_decl.name);

        try writer.writeAll("define void ");
        try writeLlvmSymbol(writer, contents_name);
        try writer.writeAll("(ptr %value) {\nentry:\n");
        for (type_decl.fields, 0..) |field, index| {
            switch (field.ty.kind) {
                .ffi_struct => {
                    const field_contents = try releaseContentsSymbolName(allocator, field.ty.name orelse return error.UnsupportedExecutableFeature);
                    defer allocator.free(field_contents);
                    try writer.print("  %release.field.{d} = getelementptr inbounds {s}, ptr %value, i32 0, i32 {d}\n", .{ index, struct_type_name, index });
                    try writer.writeAll("  call void ");
                    try writeLlvmSymbol(writer, field_contents);
                    try writer.print("(ptr %release.field.{d})\n", .{index});
                },
                .array => {
                    const release_fn = try arrayElementDestroySymbol(allocator, program, field.ty);
                    defer if (release_fn) |name| allocator.free(name);
                    try writer.print("  %release.array.field.{d} = getelementptr inbounds {s}, ptr %value, i32 0, i32 {d}\n", .{ index, struct_type_name, index });
                    try writer.print("  %release.array.{d} = load ptr, ptr %release.array.field.{d}\n", .{ index, index });
                    try writer.print("  call void @\"kira_array_release\"(ptr %release.array.{d}, ptr ", .{index});
                    if (release_fn) |name| {
                        try writeLlvmSymbol(writer, name);
                    } else {
                        try writer.writeAll("null");
                    }
                    try writer.writeAll(")\n");
                },
                else => {},
            }
        }
        try writer.writeAll("  ret void\n}\n");

        try writer.writeAll("define void ");
        try writeLlvmSymbol(writer, destroy_name);
        try writer.writeAll("(ptr %value) {\nentry:\n");
        try writer.writeAll("  %isnull = icmp eq ptr %value, null\n");
        try writer.writeAll("  br i1 %isnull, label %done, label %body\nbody:\n");
        try writer.writeAll("  call void ");
        try writeLlvmSymbol(writer, contents_name);
        try writer.writeAll("(ptr %value)\n");
        try writer.writeAll("  call void @free(ptr %value)\n");
        try writer.writeAll("  br label %done\ndone:\n  ret void\n}\n");
    }
    if (program.types.len > 0) try writer.writeByte('\n');
}

fn arrayElementDestroySymbol(allocator: std.mem.Allocator, program: *const ir.Program, array_ty: ir.ValueType) !?[]const u8 {
    const array_name = array_ty.name orelse return null;
    const type_decl = findTypeDecl(program, array_name) orelse return null;
    if (type_decl.ffi) |ffi_info| {
        return switch (ffi_info) {
            .ffi_struct => try destroySymbolName(allocator, type_decl.name),
            .array => |array_info| blk: {
                if (array_info.element.kind != .ffi_struct) break :blk null;
                break :blk try destroySymbolName(allocator, array_info.element.name orelse return error.UnsupportedExecutableFeature);
            },
            else => null,
        };
    }
    return try destroySymbolName(allocator, type_decl.name);
}

fn releaseContentsSymbolName(allocator: std.mem.Allocator, type_name: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "kira_release_contents_{s}", .{type_name});
}

fn destroySymbolName(allocator: std.mem.Allocator, type_name: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "kira_destroy_{s}", .{type_name});
}
