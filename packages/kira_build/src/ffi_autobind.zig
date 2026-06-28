const std = @import("std");
const native = @import("kira_native_lib_definition");
const fs_helpers = @import("ffi_autobind_fs.zig");
const autobind_cache = @import("ffi_autobind_cache.zig");
const clang_dump = @import("ffi_autobind_clang.zig");
const macros = @import("ffi_autobind_macros.zig");
const names = @import("ffi_autobind_names.zig");
const profiles = @import("ffi_autobind_profiles.zig");
const kira_types = @import("ffi_autobind_kira_types.zig");
const sdk = @import("ffi_autobind_sdk.zig");
const dynamic_runtime = @import("ffi_autobind_dynamic_runtime.zig");

const sanitizeIdentifier = names.sanitizeIdentifier;
const fieldTypeName = kira_types.fieldTypeName;
const kiraTypeName = kira_types.kiraTypeName;
const parseCType = kira_types.parseCType;
const parseInlineCallbackFromQualType = kira_types.parseInlineCallbackFromQualType;
const resolveRecord = kira_types.resolveRecord;
const syntheticFieldCallbackName = kira_types.syntheticFieldCallbackName;
const typedefResolvesToEnumAlias = kira_types.typedefResolvesToEnumAlias;
const typedefResolvesToPrimitiveAlias = kira_types.typedefResolvesToPrimitiveAlias;
const typedefResolvesToSelfRecordOrEnum = kira_types.typedefResolvesToSelfRecordOrEnum;

var ready_bindings_mutex: std.atomic.Mutex = .unlocked;
var ready_bindings: std.StringHashMapUnmanaged([]const u8) = .empty;
var binding_mode: BindingMode = .ensure;

pub const BindingMode = enum {
    ensure,
    skip,
};

pub fn setBindingMode(mode: BindingMode) void {
    binding_mode = mode;
}

pub const CParam = sdk.CParam;
pub const CFunction = sdk.CFunction;
pub const CField = sdk.CField;
pub const CEnumItem = sdk.CEnumItem;
pub const CEnum = sdk.CEnum;
pub const CRecord = sdk.CRecord;
pub const CTypedef = sdk.CTypedef;
pub const ArrayTypeInfo = sdk.ArrayTypeInfo;
pub const AstIndex = sdk.AstIndex;

pub fn ensureGeneratedBindings(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) !void {
    const autobinding = library.autobinding orelse return;
    if (binding_mode == .skip) return;
    const cache_key = try autobind_cache.cacheKey(allocator, library, autobinding);
    defer allocator.free(cache_key);
    if (bindingReady(autobinding.output_path, cache_key)) return;
    if (try autobind_cache.bindingsAreCurrent(allocator, autobinding.output_path, cache_key)) {
        try markBindingReady(autobinding.output_path, cache_key);
        return;
    }

    var index = AstIndex{};
    if (profiles.astDumpFilters(autobinding.bindings.profile)) |filters| {
        for (filters) |filter| {
            const ast_json = try clang_dump.dumpAst(allocator, library, autobinding.headers, filter);
            defer allocator.free(ast_json);
            try sdk.clang_ast.buildAstIndexInto(allocator, ast_json, autobinding.headers, &index);
        }
    } else {
        const ast_json = try clang_dump.dumpAst(allocator, library, autobinding.headers, null);
        defer allocator.free(ast_json);
        try sdk.clang_ast.buildAstIndexInto(allocator, ast_json, autobinding.headers, &index);
    }
    try macros.collectConstants(allocator, autobinding.headers, &index.macros);
    const rendered = try renderBindings(allocator, library, autobinding.bindings, index);
    defer allocator.free(rendered);

    const maybe_dir = std.fs.path.dirname(autobinding.output_path) orelse ".";
    try fs_helpers.makePath(maybe_dir);
    if (std.fs.path.isAbsolute(autobinding.output_path)) {
        const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, autobinding.output_path, .{ .truncate = true });
        defer file.close(std.Options.debug_io);
        try file.writeStreamingAll(std.Options.debug_io, rendered);
    } else {
        try std.Io.Dir.cwd().writeFile(std.Options.debug_io, .{
            .sub_path = autobinding.output_path,
            .data = rendered,
        });
    }
    try autobind_cache.writeKey(autobinding.output_path, cache_key);
    try markBindingReady(autobinding.output_path, cache_key);
}

fn bindingReady(output_path: []const u8, cache_key: []const u8) bool {
    lockMutex(&ready_bindings_mutex);
    defer ready_bindings_mutex.unlock();
    const existing = ready_bindings.get(output_path) orelse return false;
    return std.mem.eql(u8, existing, cache_key);
}

fn markBindingReady(output_path: []const u8, cache_key: []const u8) !void {
    lockMutex(&ready_bindings_mutex);
    defer ready_bindings_mutex.unlock();
    const allocator = std.heap.page_allocator;
    const owned_key = try allocator.dupe(u8, cache_key);
    errdefer allocator.free(owned_key);

    if (ready_bindings.getEntry(output_path)) |entry| {
        allocator.free(entry.value_ptr.*);
        entry.value_ptr.* = owned_key;
        return;
    }

    try ready_bindings.put(
        allocator,
        try allocator.dupe(u8, output_path),
        owned_key,
    );
}

fn lockMutex(mutex: *std.atomic.Mutex) void {
    while (!mutex.tryLock()) {
        std.Thread.yield() catch std.atomic.spinLoopHint();
    }
}

pub fn renderBindings(
    allocator: std.mem.Allocator,
    library: native.ResolvedNativeLibrary,
    spec: native.AutobindingBindings,
    index: AstIndex,
) ![]u8 {
    var required_structs = std.StringHashMapUnmanaged(void){};
    var required_callbacks = std.StringHashMapUnmanaged(void){};
    var required_pointers = std.StringHashMapUnmanaged([]const u8){};
    var required_aliases = std.StringHashMapUnmanaged(void){};
    var required_enums = std.StringHashMapUnmanaged(void){};
    var required_arrays = std.StringHashMapUnmanaged(ArrayTypeInfo){};
    var required_inline_callbacks = std.StringHashMapUnmanaged(CTypedef){};

    var function_names = std.array_list.Managed([]const u8).init(allocator);
    const profile_selection = profiles.selection(spec.profile);
    try appendProfileSelection(allocator, profile_selection, &function_names, &required_structs, &required_callbacks, &index);
    if (spec.mode == .all_public) {
        var function_iter = index.functions.iterator();
        while (function_iter.next()) |entry| try function_names.append(entry.key_ptr.*);

        var struct_iter_all = index.records.iterator();
        while (struct_iter_all.next()) |entry| try required_structs.put(allocator, entry.key_ptr.*, {});

        var typedef_iter_all = index.typedefs.iterator();
        while (typedef_iter_all.next()) |entry| {
            switch (entry.value_ptr.kind) {
                .callback => try required_callbacks.put(allocator, entry.key_ptr.*, {}),
                .array, .alias => try required_aliases.put(allocator, entry.key_ptr.*, {}),
            }
        }

        var enum_iter_all = index.enums.iterator();
        while (enum_iter_all.next()) |entry| try required_enums.put(allocator, entry.key_ptr.*, {});
    } else {
        for (spec.structs) |name| try required_structs.put(allocator, name, {});
        for (spec.callbacks) |name| try required_callbacks.put(allocator, name, {});

        for (spec.functions) |name| {
            const function_decl = index.functions.get(name) orelse return error.MissingAutobindFunction;
            try function_names.append(function_decl.name);
            try collectTypeDependencies(allocator, function_decl.return_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
            for (function_decl.params) |param| {
                try collectTypeDependencies(allocator, param.qual_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
            }
        }
    }

    for (function_names.items) |name| {
        const function_decl = index.functions.get(name) orelse continue;
        try collectTypeDependencies(allocator, function_decl.return_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
        for (function_decl.params) |param| {
            try collectTypeDependencies(allocator, param.qual_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
        }
    }

    try collectSelectedTypeDependencies(allocator, required_structs, required_aliases, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);

    const callback_names = try sortedMapKeys(allocator, required_callbacks);
    defer allocator.free(callback_names);
    for (callback_names) |name| {
        const typedef_decl = index.typedefs.get(name) orelse continue;
        for (typedef_decl.callback_params) |param| {
            try collectTypeDependencies(allocator, param, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
        }
        if (typedef_decl.callback_result) |result_type| {
            try collectTypeDependencies(allocator, result_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
        }
    }

    const array_names = try sortedMapKeys(allocator, required_arrays);
    defer allocator.free(array_names);
    for (array_names) |name| {
        const array_info = required_arrays.get(name) orelse continue;
        try collectTypeDependencies(allocator, array_info.element_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
    }

    const inline_callback_struct_names = try sortedMapKeys(allocator, required_structs);
    defer allocator.free(inline_callback_struct_names);
    for (inline_callback_struct_names) |name| {
        const record = resolveRecord(name, &index) orelse continue;
        for (record.fields) |field| {
            if (try parseInlineCallbackFromQualType(allocator, try syntheticFieldCallbackName(allocator, name, field.name), field.qual_type)) |callback_decl| {
                try required_inline_callbacks.put(allocator, callback_decl.name, callback_decl);
                for (callback_decl.callback_params) |param| {
                    try collectTypeDependencies(allocator, param, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
                }
                if (callback_decl.callback_result) |result_type| {
                    try collectTypeDependencies(allocator, result_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
                }
            }
        }
    }

    const sorted_aliases = try sortedMapKeys(allocator, required_aliases);
    const sorted_enums = try sortedMapKeys(allocator, required_enums);
    const sorted_callbacks = try sortedMapKeys(allocator, required_callbacks);
    const sorted_inline_callbacks = try sortedMapKeys(allocator, required_inline_callbacks);
    const sorted_arrays = try sortedMapKeys(allocator, required_arrays);
    const sorted_structs = try sortedMapKeys(allocator, required_structs);
    const sorted_pointers = try sortedMapKeys(allocator, required_pointers);
    sortStrings(function_names.items);
    const unique_function_names = uniqueSortedStrings(function_names.items);

    var output: std.Io.Writer.Allocating = .init(allocator);
    errdefer output.deinit();
    var writer = &output.writer;

    try writer.print("// generated by kira FFI autobinding for {s}\n\n", .{library.name});
    if (profiles.dynamicLoaderName(spec.profile)) |loader_name| {
        try dynamic_runtime.writeBindings(writer, loader_name);
    }

    for (sorted_aliases) |name| {
        const typedef_decl = index.typedefs.get(name) orelse return error.MissingAutobindType;
        if (typedefResolvesToSelfRecordOrEnum(name, typedef_decl, &index)) continue;
        if (typedefResolvesToPrimitiveAlias(typedef_decl)) continue;
        if (typedefResolvesToEnumAlias(typedef_decl, &index)) continue;
        try writeAliasType(allocator, writer, typedef_decl);
    }

    for (sorted_enums) |name| {
        const enum_decl = index.enums.get(name) orelse return error.MissingAutobindType;
        try writeEnumConstantsType(writer, enum_decl);
    }

    for (sorted_callbacks) |name| {
        const typedef_decl = index.typedefs.get(name) orelse return error.MissingAutobindCallback;
        try writeCallbackType(allocator, writer, typedef_decl);
    }

    for (sorted_inline_callbacks) |name| {
        const callback_decl = required_inline_callbacks.get(name) orelse continue;
        try writeCallbackType(allocator, writer, callback_decl);
    }

    for (sorted_arrays) |name| {
        const array_info = required_arrays.get(name) orelse continue;
        try writeSyntheticArrayType(allocator, writer, array_info);
    }

    for (sorted_structs) |name| {
        if (resolveRecord(name, &index) == null) continue;
        try writeStructType(allocator, writer, name, &required_inline_callbacks, &index);
    }

    for (sorted_pointers) |name| {
        const target_name = required_pointers.get(name) orelse continue;
        try writer.print("@FFI.Pointer {{ target: {s}; ownership: borrowed; }}\n", .{target_name});
        try writer.print("struct {s} {{}}\n\n", .{name});
    }

    if (spec.mode == .all_public and index.macros.count() > 0) {
        const macro_names = try sortedMapKeys(allocator, index.macros);
        try writeMacroConstantsType(writer, library.name, macro_names, &index);
    }

    var emitted_functions = std.StringHashMapUnmanaged(void){};
    for (unique_function_names) |name| {
        const function_decl = index.functions.get(name) orelse return error.MissingAutobindFunction;
        const emitted_name = sanitizeIdentifier(function_decl.name);
        if (emitted_functions.contains(emitted_name)) continue;
        try emitted_functions.put(allocator, emitted_name, {});
        try writeFunctionDecl(allocator, writer, library.name, function_decl, &index);
    }

    return output.toOwnedSlice();
}

fn appendProfileSelection(
    allocator: std.mem.Allocator,
    selection: profiles.ProfileSelection,
    function_names: *std.array_list.Managed([]const u8),
    required_structs: *std.StringHashMapUnmanaged(void),
    required_callbacks: *std.StringHashMapUnmanaged(void),
    index: *const AstIndex,
) !void {
    for (selection.functions) |name| {
        if (index.functions.contains(name)) try function_names.append(name);
    }
    for (selection.structs) |name| {
        if (resolveRecord(name, index) != null or index.typedefs.contains(name)) {
            try required_structs.put(allocator, name, {});
        }
    }
    for (selection.callbacks) |name| {
        if (index.typedefs.contains(name)) try required_callbacks.put(allocator, name, {});
    }
}

fn writeAliasType(allocator: std.mem.Allocator, writer: anytype, typedef_decl: CTypedef) !void {
    switch (typedef_decl.kind) {
        .callback => return writeCallbackType(allocator, writer, typedef_decl),
        .array => {
            try writer.print("@FFI.Array {{ element: {s}; count: {d}; }}\n", .{
                try kiraTypeName(allocator, typedef_decl.array_element_type orelse return error.InvalidAutobindingDecl, null),
                typedef_decl.array_count,
            });
            try writer.print("struct {s} {{}}\n\n", .{typedef_decl.name});
        },
        .alias => {
            try writer.print("@FFI.Alias {{ target: {s}; }}\n", .{try kiraTypeName(allocator, typedef_decl.qual_type, null)});
            try writer.print("struct {s} {{}}\n\n", .{typedef_decl.name});
        },
    }
}

fn writeSyntheticArrayType(allocator: std.mem.Allocator, writer: anytype, array_info: ArrayTypeInfo) !void {
    try writer.print("@FFI.Array {{ element: {s}; count: {d}; }}\n", .{
        try kiraTypeName(allocator, array_info.element_type, null),
        array_info.count,
    });
    try writer.print("struct {s} {{}}\n\n", .{array_info.name});
}

fn writeEnumConstantsType(writer: anytype, enum_decl: CEnum) !void {
    if (enum_decl.items.len == 0) return;
    try writer.print("struct {s}Constants {{\n", .{sanitizeIdentifier(enum_decl.name)});
    for (enum_decl.items) |item| {
        try writer.print("    let {s}: I64 = {d}\n", .{ sanitizeIdentifier(item.name), item.value });
    }
    try writer.writeAll("}\n\n");
}

fn writeCallbackType(allocator: std.mem.Allocator, writer: anytype, typedef_decl: CTypedef) !void {
    try writer.print("@FFI.Callback {{ abi: c; params: [", .{});
    for (typedef_decl.callback_params, 0..) |param, index| {
        if (index != 0) try writer.writeAll(", ");
        try writer.writeAll(try kiraTypeName(allocator, param, null));
    }
    try writer.writeAll("]; result: ");
    try writer.writeAll(try kiraTypeName(allocator, typedef_decl.callback_result orelse "void", null));
    try writer.writeAll("; }\n");
    try writer.print("struct {s} {{}}\n\n", .{typedef_decl.name});
}

fn writeStructType(
    allocator: std.mem.Allocator,
    writer: anytype,
    name: []const u8,
    inline_callbacks: *const std.StringHashMapUnmanaged(CTypedef),
    index: *const AstIndex,
) !void {
    const record = resolveRecord(name, index) orelse return error.MissingAutobindStruct;
    try writer.writeAll("@FFI.Struct { layout: c; }\n");
    try writer.print("struct {s} {{\n", .{name});
    for (record.fields) |field| {
        const type_name = try fieldTypeName(allocator, name, field, inline_callbacks, index);
        try writer.print("    var {s}: {s}\n", .{ sanitizeIdentifier(field.name), type_name });
    }
    try writer.writeAll("}\n\n");
}

fn writeMacroConstantsType(writer: anytype, library_name: []const u8, macro_names: []const []const u8, index: *const AstIndex) !void {
    if (macro_names.len == 0) return;
    try writer.print("struct {s}Constants {{\n", .{sanitizeIdentifier(library_name)});
    for (macro_names) |name| {
        const macro = index.macros.get(name) orelse continue;
        const ty = if (std.mem.startsWith(u8, macro.value, "-")) "I64" else "U64";
        try writer.print("    let {s}: {s} = {s}\n", .{ sanitizeIdentifier(macro.name), ty, macro.value });
    }
    try writer.writeAll("}\n\n");
}

fn writeFunctionDecl(allocator: std.mem.Allocator, writer: anytype, library_name: []const u8, function_decl: CFunction, index: *const AstIndex) !void {
    try writer.print("@FFI.Extern {{ library: {s}; symbol: {s}; abi: c; }}\n", .{ library_name, function_decl.name });
    try writer.print("function {s}(", .{function_decl.name});
    for (function_decl.params, 0..) |param, param_index| {
        if (param_index != 0) try writer.writeAll(", ");
        const type_name = try kiraTypeName(allocator, param.qual_type, index);
        try writer.print("{s}: {s}", .{ sanitizeIdentifier(param.name), type_name });
    }
    const result_type = try kiraTypeName(allocator, function_decl.return_type, index);
    try writer.print("): {s};\n\n", .{result_type});
}

fn collectTypeDependencies(
    allocator: std.mem.Allocator,
    qual_type: []const u8,
    required_structs: *std.StringHashMapUnmanaged(void),
    required_callbacks: *std.StringHashMapUnmanaged(void),
    required_pointers: *std.StringHashMapUnmanaged([]const u8),
    required_aliases: *std.StringHashMapUnmanaged(void),
    required_enums: *std.StringHashMapUnmanaged(void),
    required_arrays: *std.StringHashMapUnmanaged(ArrayTypeInfo),
    index: *const AstIndex,
) !void {
    const parsed = try parseCType(allocator, qual_type, index);
    switch (parsed) {
        .plain => {},
        .struct_name => |name| try required_structs.put(allocator, name, {}),
        .callback_name => |name| try required_callbacks.put(allocator, name, {}),
        .alias_name => |name| try required_aliases.put(allocator, name, {}),
        .enum_name => |name| try required_enums.put(allocator, name, {}),
        .array_name => |value| try required_arrays.put(allocator, value.name, value),
        .pointer_to_named => |value| {
            if (index.enums.contains(value.target_name)) {
                try required_enums.put(allocator, value.target_name, {});
            } else if (index.typedefs.contains(value.target_name) and resolveRecord(value.target_name, index) == null and index.typedefs.get(value.target_name).?.kind != .callback) {
                try required_aliases.put(allocator, value.target_name, {});
            } else {
                try required_structs.put(allocator, value.target_name, {});
            }
            try required_pointers.put(allocator, value.pointer_name, value.target_name);
        },
    }
}

fn collectSelectedTypeDependencies(
    allocator: std.mem.Allocator,
    selected_structs: std.StringHashMapUnmanaged(void),
    selected_aliases: std.StringHashMapUnmanaged(void),
    required_structs: *std.StringHashMapUnmanaged(void),
    required_callbacks: *std.StringHashMapUnmanaged(void),
    required_pointers: *std.StringHashMapUnmanaged([]const u8),
    required_aliases: *std.StringHashMapUnmanaged(void),
    required_enums: *std.StringHashMapUnmanaged(void),
    required_arrays: *std.StringHashMapUnmanaged(ArrayTypeInfo),
    index: *const AstIndex,
) !void {
    const struct_names = try sortedMapKeys(allocator, selected_structs);
    defer allocator.free(struct_names);
    for (struct_names) |name| {
        const record = resolveRecord(name, index) orelse continue;
        for (record.fields) |field| {
            try collectTypeDependencies(allocator, field.qual_type, required_structs, required_callbacks, required_pointers, required_aliases, required_enums, required_arrays, index);
        }
    }

    const alias_names = try sortedMapKeys(allocator, selected_aliases);
    defer allocator.free(alias_names);
    for (alias_names) |name| {
        const typedef_decl = index.typedefs.get(name) orelse continue;
        switch (typedef_decl.kind) {
            .alias => try collectTypeDependencies(allocator, typedef_decl.qual_type, required_structs, required_callbacks, required_pointers, required_aliases, required_enums, required_arrays, index),
            .array => try collectTypeDependencies(allocator, typedef_decl.array_element_type orelse continue, required_structs, required_callbacks, required_pointers, required_aliases, required_enums, required_arrays, index),
            .callback => {
                for (typedef_decl.callback_params) |param| {
                    try collectTypeDependencies(allocator, param, required_structs, required_callbacks, required_pointers, required_aliases, required_enums, required_arrays, index);
                }
                if (typedef_decl.callback_result) |result_type| {
                    try collectTypeDependencies(allocator, result_type, required_structs, required_callbacks, required_pointers, required_aliases, required_enums, required_arrays, index);
                }
            },
        }
    }
}

fn sortStrings(values: [][]const u8) void {
    std.mem.sort([]const u8, values, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.order(u8, lhs, rhs) == .lt;
        }
    }.lessThan);
}

fn uniqueSortedStrings(values: [][]const u8) [][]const u8 {
    if (values.len <= 1) return values;

    var write_index: usize = 1;
    var previous = values[0];
    for (values[1..]) |value| {
        if (std.mem.eql(u8, previous, value)) continue;
        values[write_index] = value;
        write_index += 1;
        previous = value;
    }
    return values[0..write_index];
}

fn sortedMapKeys(allocator: std.mem.Allocator, map: anytype) ![]const []const u8 {
    var keys = std.array_list.Managed([]const u8).init(allocator);
    var iter = map.iterator();
    while (iter.next()) |entry| try keys.append(entry.key_ptr.*);
    sortStrings(keys.items);
    return keys.toOwnedSlice();
}
