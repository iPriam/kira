const std = @import("std");
const native = @import("kira_native_lib_definition");
const fs_helpers = @import("ffi_autobind_fs.zig");
const autobind_cache = @import("ffi_autobind_cache.zig");
const clang_dump = @import("ffi_autobind_clang.zig");
const json_helpers = @import("ffi_autobind_json.zig");
const macros = @import("ffi_autobind_macros.zig");
const names = @import("ffi_autobind_names.zig");
const profiles = @import("ffi_autobind_profiles.zig");
const type_text = @import("ffi_autobind_type_text.zig");
const dynamic_runtime = @import("ffi_autobind_dynamic_runtime.zig");

const objectString = json_helpers.objectString;
const objectBool = json_helpers.objectBool;
const objectQualType = json_helpers.objectQualType;
const cloneStrings = json_helpers.cloneStrings;
const sanitizeIdentifier = names.sanitizeIdentifier;
const cleanCType = type_text.cleanCType;
const isUnsupportedAnonymousAggregate = type_text.isUnsupportedAnonymousAggregate;
const trimPointerTarget = type_text.trimPointerTarget;

pub const CParam = struct {
    name: []const u8,
    qual_type: []const u8,
};

pub const CFunction = struct {
    name: []const u8,
    return_type: []const u8,
    params: []const CParam,
};

pub const CField = struct {
    name: []const u8,
    qual_type: []const u8,
};

pub const CEnumItem = struct {
    name: []const u8,
    value: i64,
};

pub const CEnum = struct {
    name: []const u8,
    items: []const CEnumItem,
};

pub const CRecord = struct {
    name: []const u8,
    fields: []const CField,
};

pub const CTypedef = struct {
    name: []const u8,
    qual_type: []const u8,
    kind: Kind,
    callback_params: []const []const u8 = &.{},
    callback_result: ?[]const u8 = null,
    array_element_type: ?[]const u8 = null,
    array_count: usize = 0,

    const Kind = enum {
        alias,
        array,
        callback,
    };
};

const ArrayTypeInfo = struct {
    name: []const u8,
    element_type: []const u8,
    count: usize,
};

pub const AstIndex = struct {
    functions: std.StringHashMapUnmanaged(CFunction) = .{},
    enums: std.StringHashMapUnmanaged(CEnum) = .{},
    records: std.StringHashMapUnmanaged(CRecord) = .{},
    typedefs: std.StringHashMapUnmanaged(CTypedef) = .{},
    macros: std.StringHashMapUnmanaged(macros.CMacro) = .{},
};

pub fn ensureGeneratedBindings(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) !void {
    const autobinding = library.autobinding orelse return;
    const cache_key = try autobind_cache.cacheKey(allocator, library, autobinding);
    defer allocator.free(cache_key);
    if (try autobind_cache.bindingsAreCurrent(allocator, autobinding.output_path, cache_key)) return;

    var index = AstIndex{};
    if (profiles.astDumpFilters(autobinding.bindings.profile)) |filters| {
        for (filters) |filter| {
            const ast_json = try clang_dump.dumpAst(allocator, library, autobinding.headers, filter);
            defer allocator.free(ast_json);
            try buildAstIndexInto(allocator, ast_json, autobinding.headers, &index);
        }
    } else {
        const ast_json = try clang_dump.dumpAst(allocator, library, autobinding.headers, null);
        defer allocator.free(ast_json);
        try buildAstIndexInto(allocator, ast_json, autobinding.headers, &index);
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
}

fn buildAstIndex(allocator: std.mem.Allocator, ast_json: []const u8, headers: []const []const u8) !AstIndex {
    var index = AstIndex{};
    try buildAstIndexInto(allocator, ast_json, headers, &index);
    return index;
}

fn buildAstIndexInto(allocator: std.mem.Allocator, ast_json: []const u8, headers: []const []const u8, index: *AstIndex) !void {
    if (std.mem.trim(u8, ast_json, " \t\r\n").len == 0) return;
    var parsed = std.json.parseFromSlice(std.json.Value, allocator, ast_json, .{
        .ignore_unknown_fields = true,
    }) catch |err| switch (err) {
        error.SyntaxError => return buildFilteredAstIndexInto(allocator, ast_json, headers, index),
        else => return err,
    };
    defer parsed.deinit();

    const normalized_headers = try normalizePaths(allocator, headers);

    try walkNode(allocator, parsed.value, normalized_headers, index);
}

fn buildFilteredAstIndexInto(allocator: std.mem.Allocator, ast_json: []const u8, headers: []const []const u8, index: *AstIndex) !void {
    const normalized_headers = try normalizePaths(allocator, headers);
    var start: ?usize = null;
    var depth: usize = 0;
    var in_string = false;
    var escaped = false;
    var saw_document = false;

    for (ast_json, 0..) |ch, offset| {
        if (start == null) {
            if (std.ascii.isWhitespace(ch)) continue;
            if (ch != '{') return error.SyntaxError;
            start = offset;
            depth = 1;
            continue;
        }

        if (in_string) {
            if (escaped) {
                escaped = false;
            } else if (ch == '\\') {
                escaped = true;
            } else if (ch == '"') {
                in_string = false;
            }
            continue;
        }

        if (ch == '"') {
            in_string = true;
        } else if (ch == '{') {
            depth += 1;
        } else if (ch == '}') {
            depth -= 1;
            if (depth == 0) {
                const slice = ast_json[start.? .. offset + 1];
                var parsed = try std.json.parseFromSlice(std.json.Value, allocator, slice, .{
                    .ignore_unknown_fields = true,
                });
                defer parsed.deinit();
                try walkNode(allocator, parsed.value, normalized_headers, index);
                start = null;
                saw_document = true;
            }
        }
    }

    if (start != null or !saw_document) return error.SyntaxError;
}

fn walkNode(
    allocator: std.mem.Allocator,
    node: std.json.Value,
    headers: []const []const u8,
    index: *AstIndex,
) !void {
    if (node != .object) return;
    const object = node.object;
    const kind = objectString(object, "kind") orelse "";

    if (isHeaderNode(object, headers)) {
        if (std.mem.eql(u8, kind, "FunctionDecl")) {
            if (objectString(object, "name")) |name| {
                try index.functions.put(allocator, try allocator.dupe(u8, name), try extractFunctionDecl(allocator, object));
            }
        } else if (std.mem.eql(u8, kind, "EnumDecl")) {
            if (objectString(object, "name")) |name| {
                try index.enums.put(allocator, try allocator.dupe(u8, name), try extractEnumDecl(allocator, object));
            }
        } else if (std.mem.eql(u8, kind, "RecordDecl")) {
            if (objectString(object, "name")) |name| {
                if (objectBool(object, "completeDefinition")) {
                    try index.records.put(allocator, try allocator.dupe(u8, name), try extractRecordDecl(allocator, object));
                }
            }
        } else if (std.mem.eql(u8, kind, "TypedefDecl")) {
            if (objectString(object, "name")) |name| {
                try index.typedefs.put(allocator, try allocator.dupe(u8, name), try extractTypedefDecl(allocator, object));
            }
        }
    }

    if (object.get("inner")) |inner| {
        if (inner == .array) {
            for (inner.array.items) |child| try walkNode(allocator, child, headers, index);
        }
    }
}

fn isHeaderNode(object: std.json.ObjectMap, headers: []const []const u8) bool {
    const loc = object.get("loc") orelse return false;
    if (loc != .object) return false;
    const file = objectString(loc.object, "file") orelse {
        if (objectBool(object, "isImplicit")) return false;
        if (objectString(object, "name")) |name| {
            return !std.mem.startsWith(u8, name, "__");
        }
        return false;
    };
    const normalized_file = normalizePath(std.heap.page_allocator, file) catch file;
    defer if (normalized_file.ptr != file.ptr) std.heap.page_allocator.free(normalized_file);
    for (headers) |header| {
        if (std.mem.eql(u8, normalized_file, header)) return true;
    }
    return false;
}

fn normalizePaths(allocator: std.mem.Allocator, values: []const []const u8) ![]const []const u8 {
    var list = std.array_list.Managed([]const u8).init(allocator);
    for (values) |value| {
        try list.append(try normalizePath(allocator, value));
    }
    return list.toOwnedSlice();
}

fn normalizePath(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    return std.fs.path.resolve(allocator, &.{path});
}

fn extractFunctionDecl(allocator: std.mem.Allocator, object: std.json.ObjectMap) !CFunction {
    var params = std.array_list.Managed(CParam).init(allocator);
    if (object.get("inner")) |inner| {
        if (inner == .array) {
            for (inner.array.items, 0..) |child, index| {
                if (child != .object) continue;
                if (!std.mem.eql(u8, objectString(child.object, "kind") orelse "", "ParmVarDecl")) continue;
                try params.append(.{
                    .name = try namedOrIndexed(allocator, objectString(child.object, "name"), "arg", index),
                    .qual_type = try allocator.dupe(u8, objectQualType(child.object) orelse return error.InvalidAutobindingDecl),
                });
            }
        }
    }

    return .{
        .name = try allocator.dupe(u8, objectString(object, "name") orelse return error.InvalidAutobindingDecl),
        .return_type = try allocator.dupe(u8, functionResultType(object) orelse return error.InvalidAutobindingDecl),
        .params = try params.toOwnedSlice(),
    };
}

fn extractRecordDecl(allocator: std.mem.Allocator, object: std.json.ObjectMap) !CRecord {
    var fields = std.array_list.Managed(CField).init(allocator);
    if (object.get("inner")) |inner| {
        if (inner == .array) {
            for (inner.array.items, 0..) |child, index| {
                if (child != .object) continue;
                if (!std.mem.eql(u8, objectString(child.object, "kind") orelse "", "FieldDecl")) continue;
                try fields.append(.{
                    .name = try namedOrIndexed(allocator, objectString(child.object, "name"), "field", index),
                    .qual_type = try allocator.dupe(u8, objectQualType(child.object) orelse return error.InvalidAutobindingDecl),
                });
            }
        }
    }

    return .{
        .name = try allocator.dupe(u8, objectString(object, "name") orelse return error.InvalidAutobindingDecl),
        .fields = try fields.toOwnedSlice(),
    };
}

fn namedOrIndexed(allocator: std.mem.Allocator, maybe_name: ?[]const u8, prefix: []const u8, index: usize) ![]const u8 {
    if (maybe_name) |name| {
        if (name.len > 0) return allocator.dupe(u8, name);
    }
    return std.fmt.allocPrint(allocator, "{s}{d}", .{ prefix, index });
}

fn extractEnumDecl(allocator: std.mem.Allocator, object: std.json.ObjectMap) !CEnum {
    var items = std.array_list.Managed(CEnumItem).init(allocator);
    var next_value: i64 = 0;
    if (object.get("inner")) |inner| {
        if (inner == .array) {
            for (inner.array.items) |child| {
                if (child != .object) continue;
                if (!std.mem.eql(u8, objectString(child.object, "kind") orelse "", "EnumConstantDecl")) continue;
                const value = findIntegerValue(child) orelse next_value;
                try items.append(.{
                    .name = try allocator.dupe(u8, objectString(child.object, "name") orelse return error.InvalidAutobindingDecl),
                    .value = value,
                });
                next_value = value + 1;
            }
        }
    }
    return .{
        .name = try allocator.dupe(u8, objectString(object, "name") orelse return error.InvalidAutobindingDecl),
        .items = try items.toOwnedSlice(),
    };
}

fn extractTypedefDecl(allocator: std.mem.Allocator, object: std.json.ObjectMap) !CTypedef {
    var result = CTypedef{
        .name = try allocator.dupe(u8, objectString(object, "name") orelse return error.InvalidAutobindingDecl),
        .qual_type = try allocator.dupe(u8, objectQualType(object) orelse return error.InvalidAutobindingDecl),
        .kind = .alias,
    };

    if (result.qual_type.len > 0 and std.mem.indexOf(u8, result.qual_type, "(*)") != null) {
        result.kind = .callback;
        if (findFunctionProto(object)) |proto| {
            result.callback_result = try allocator.dupe(u8, proto.result_type);
            result.callback_params = try cloneStrings(allocator, proto.params);
        }
    } else if (try parseArrayType(allocator, result.qual_type)) |array_info| {
        result.kind = .array;
        result.array_element_type = array_info.element_type;
        result.array_count = array_info.count;
    }

    return result;
}

const FunctionProto = struct {
    result_type: []const u8,
    params: []const []const u8,
};

fn findFunctionProto(object: std.json.ObjectMap) ?FunctionProto {
    const inner = object.get("inner") orelse return null;
    return findFunctionProtoInValue(inner);
}

fn findFunctionProtoInValue(value: std.json.Value) ?FunctionProto {
    if (value == .object) {
        const kind = objectString(value.object, "kind") orelse "";
        if (std.mem.eql(u8, kind, "FunctionProtoType")) {
            var params = std.array_list.Managed([]const u8).init(std.heap.page_allocator);
            if (value.object.get("inner")) |inner| {
                if (inner == .array and inner.array.items.len > 0) {
                    const result_type = objectQualType(inner.array.items[0].object) orelse return null;
                    for (inner.array.items[1..]) |child| {
                        if (child != .object) continue;
                        const qual_type = objectQualType(child.object) orelse continue;
                        params.append(qual_type) catch return null;
                    }
                    return .{
                        .result_type = result_type,
                        .params = params.toOwnedSlice() catch return null,
                    };
                }
            }
        }
        if (value.object.get("inner")) |inner| return findFunctionProtoInValue(inner);
        return null;
    }
    if (value == .array) {
        for (value.array.items) |child| {
            if (findFunctionProtoInValue(child)) |proto| return proto;
        }
    }
    return null;
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
    var callback_dep_iter = required_callbacks.iterator();
    while (callback_dep_iter.next()) |entry| {
        const typedef_decl = index.typedefs.get(entry.key_ptr.*) orelse continue;
        for (typedef_decl.callback_params) |param| {
            try collectTypeDependencies(allocator, param, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
        }
        if (typedef_decl.callback_result) |result_type| {
            try collectTypeDependencies(allocator, result_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
        }
    }
    var array_dep_iter = required_arrays.iterator();
    while (array_dep_iter.next()) |entry| {
        try collectTypeDependencies(allocator, entry.value_ptr.element_type, &required_structs, &required_callbacks, &required_pointers, &required_aliases, &required_enums, &required_arrays, &index);
    }
    var inline_callback_struct_iter = required_structs.iterator();
    while (inline_callback_struct_iter.next()) |entry| {
        const name = entry.key_ptr.*;
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

    for (function_names.items) |name| {
        const function_decl = index.functions.get(name) orelse return error.MissingAutobindFunction;
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

fn fieldTypeName(
    allocator: std.mem.Allocator,
    owner_name: []const u8,
    field: CField,
    inline_callbacks: *const std.StringHashMapUnmanaged(CTypedef),
    index: *const AstIndex,
) ![]const u8 {
    const callback_name = try syntheticFieldCallbackName(allocator, owner_name, field.name);
    if (inline_callbacks.contains(callback_name)) return callback_name;
    return kiraTypeName(allocator, field.qual_type, index);
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
    var struct_iter = selected_structs.iterator();
    while (struct_iter.next()) |entry| {
        const record = resolveRecord(entry.key_ptr.*, index) orelse continue;
        for (record.fields) |field| {
            try collectTypeDependencies(allocator, field.qual_type, required_structs, required_callbacks, required_pointers, required_aliases, required_enums, required_arrays, index);
        }
    }

    var alias_iter = selected_aliases.iterator();
    while (alias_iter.next()) |entry| {
        const typedef_decl = index.typedefs.get(entry.key_ptr.*) orelse continue;
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

const ParsedType = union(enum) {
    plain,
    struct_name: []const u8,
    callback_name: []const u8,
    alias_name: []const u8,
    enum_name: []const u8,
    array_name: ArrayTypeInfo,
    pointer_to_named: struct {
        pointer_name: []const u8,
        target_name: []const u8,
    },
};

fn kiraTypeName(allocator: std.mem.Allocator, qual_type: []const u8, maybe_index: ?*const AstIndex) ![]const u8 {
    const text = cleanCType(qual_type);
    if (isUnsupportedAnonymousAggregate(text)) return allocator.dupe(u8, "RawPtr");
    if (primitiveKiraTypeName(text)) |name| return allocator.dupe(u8, name);
    if (std.mem.startsWith(u8, text, "enum ")) return allocator.dupe(u8, "U32");
    if (maybe_index) |index| {
        if (index.enums.contains(text)) return allocator.dupe(u8, "U32");
        if (index.typedefs.get(text)) |typedef_decl| {
            if (typedef_decl.kind == .alias) {
                const target = cleanCType(typedef_decl.qual_type);
                if (primitiveKiraTypeName(target)) |name| return allocator.dupe(u8, name);
                if (std.mem.startsWith(u8, target, "enum ")) return allocator.dupe(u8, "U32");
                if (index.enums.contains(target)) return allocator.dupe(u8, "U32");
            }
        }
    }
    if (try parseArrayType(allocator, text)) |array_info| {
        return allocator.dupe(u8, array_info.name);
    }
    if (std.mem.endsWith(u8, text, "*")) {
        const base = trimPointerTarget(text);
        if (std.mem.eql(u8, base, "void")) return allocator.dupe(u8, "RawPtr");
        return std.fmt.allocPrint(allocator, "{s}_ptr", .{base});
    }
    if (std.mem.startsWith(u8, text, "struct ")) return allocator.dupe(u8, text["struct ".len..]);
    return allocator.dupe(u8, text);
}

fn parseCType(allocator: std.mem.Allocator, qual_type: []const u8, index: *const AstIndex) !ParsedType {
    const text = cleanCType(qual_type);
    if (isUnsupportedAnonymousAggregate(text)) return .plain;
    if (isPrimitiveType(text)) return .plain;
    if (try parseArrayType(allocator, text)) |array_info| return .{ .array_name = array_info };
    if (std.mem.endsWith(u8, text, "*")) {
        const base = trimPointerTarget(text);
        if (std.mem.eql(u8, base, "void")) return .plain;
        return .{ .pointer_to_named = .{
            .pointer_name = try std.fmt.allocPrint(allocator, "{s}_ptr", .{base}),
            .target_name = try allocator.dupe(u8, base),
        } };
    }
    if (std.mem.startsWith(u8, text, "struct ")) {
        return .{ .struct_name = try allocator.dupe(u8, text["struct ".len..]) };
    }
    if (index.enums.contains(text)) return .{ .enum_name = try allocator.dupe(u8, text) };
    if (index.typedefs.get(text)) |typedef_decl| {
        return switch (typedef_decl.kind) {
            .callback => .{ .callback_name = try allocator.dupe(u8, text) },
            .array => .{ .alias_name = try allocator.dupe(u8, text) },
            .alias => {
                if (typedefResolvesToPrimitiveAlias(typedef_decl) or typedefResolvesToEnumAlias(typedef_decl, index)) return .plain;
                return .{ .alias_name = try allocator.dupe(u8, text) };
            },
        };
    }
    if (resolveRecord(text, index) != null) return .{ .struct_name = try allocator.dupe(u8, text) };
    return .plain;
}

fn resolveRecord(name: []const u8, index: *const AstIndex) ?CRecord {
    if (index.records.get(name)) |record| return record;
    if (index.typedefs.get(name)) |typedef_decl| {
        const target = trimStructPrefix(typedef_decl.qual_type);
        return index.records.get(target);
    }
    return null;
}

fn trimStructPrefix(text: []const u8) []const u8 {
    const trimmed = std.mem.trim(u8, text, " ");
    if (std.mem.startsWith(u8, trimmed, "struct ")) return trimmed["struct ".len..];
    return trimmed;
}

fn typedefResolvesToSelfRecordOrEnum(name: []const u8, typedef_decl: CTypedef, index: *const AstIndex) bool {
    if (resolveRecord(name, index) != null) {
        const target = trimStructPrefix(typedef_decl.qual_type);
        if (std.mem.eql(u8, target, name)) return true;
    }
    const trimmed = cleanCType(typedef_decl.qual_type);
    if (std.mem.startsWith(u8, trimmed, "enum ")) {
        const target = trimmed["enum ".len..];
        if (std.mem.eql(u8, target, name) and index.enums.contains(name)) return true;
    }
    return false;
}

fn typedefResolvesToPrimitiveAlias(typedef_decl: CTypedef) bool {
    if (typedef_decl.kind != .alias) return false;
    return primitiveKiraTypeName(cleanCType(typedef_decl.qual_type)) != null;
}

fn typedefResolvesToEnumAlias(typedef_decl: CTypedef, index: *const AstIndex) bool {
    if (typedef_decl.kind != .alias) return false;
    const target = cleanCType(typedef_decl.qual_type);
    if (std.mem.startsWith(u8, target, "enum ")) return true;
    return index.enums.contains(target);
}

fn isPrimitiveType(text: []const u8) bool {
    return primitiveKiraTypeName(text) != null;
}

fn functionResultType(object: std.json.ObjectMap) ?[]const u8 {
    const type_value = object.get("type") orelse return null;
    if (type_value != .object) return null;
    const qual_type = objectString(type_value.object, "qualType") orelse return null;
    const open = std.mem.indexOfScalar(u8, qual_type, '(') orelse return qual_type;
    return std.mem.trimEnd(u8, qual_type[0..open], " ");
}

fn extractEnumValue(object: std.json.ObjectMap) i64 {
    if (findIntegerValue(.{ .object = object })) |value| return value;
    return 0;
}

fn findIntegerValue(value: std.json.Value) ?i64 {
    switch (value) {
        .object => |object| {
            if (object.get("value")) |field| {
                switch (field) {
                    .string => return std.fmt.parseInt(i64, field.string, 0) catch null,
                    .integer => return @intCast(field.integer),
                    else => {},
                }
            }
            if (object.get("inner")) |inner| return findIntegerValue(inner);
            return null;
        },
        .array => |array| {
            for (array.items) |item| {
                if (findIntegerValue(item)) |found| return found;
            }
            return null;
        },
        .integer => |raw| return @intCast(raw),
        else => return null,
    }
}

fn primitiveKiraTypeName(text: []const u8) ?[]const u8 {
    if (std.mem.eql(u8, text, "void")) return "Void";
    if (std.mem.eql(u8, text, "char") or std.mem.eql(u8, text, "signed char") or std.mem.eql(u8, text, "int8_t")) return "I8";
    if (std.mem.eql(u8, text, "unsigned char") or std.mem.eql(u8, text, "uint8_t")) return "U8";
    if (std.mem.eql(u8, text, "short") or std.mem.eql(u8, text, "short int") or std.mem.eql(u8, text, "signed short") or std.mem.eql(u8, text, "int16_t")) return "I16";
    if (std.mem.eql(u8, text, "unsigned short") or std.mem.eql(u8, text, "unsigned short int") or std.mem.eql(u8, text, "uint16_t")) return "U16";
    if (std.mem.eql(u8, text, "int") or std.mem.eql(u8, text, "int32_t")) return "I32";
    if (std.mem.eql(u8, text, "unsigned int") or std.mem.eql(u8, text, "uint32_t")) return "U32";
    if (std.mem.eql(u8, text, "long")) return "I32";
    if (std.mem.eql(u8, text, "unsigned long")) return "U32";
    if (std.mem.eql(u8, text, "long long") or std.mem.eql(u8, text, "int64_t") or std.mem.eql(u8, text, "intptr_t") or std.mem.eql(u8, text, "ptrdiff_t")) return "I64";
    if (std.mem.eql(u8, text, "unsigned long long") or std.mem.eql(u8, text, "uint64_t") or std.mem.eql(u8, text, "uintptr_t") or std.mem.eql(u8, text, "size_t")) return "U64";
    if (std.mem.eql(u8, text, "float")) return "F32";
    if (std.mem.eql(u8, text, "double")) return "F64";
    if (std.mem.eql(u8, text, "_Bool") or std.mem.eql(u8, text, "bool")) return "CBool";
    if (std.mem.eql(u8, text, "const char *") or std.mem.eql(u8, text, "char *")) return "CString";
    if (std.mem.eql(u8, text, "const void *") or std.mem.eql(u8, text, "void *")) return "RawPtr";
    return null;
}

fn syntheticFieldCallbackName(allocator: std.mem.Allocator, owner_name: []const u8, field_name: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}_{s}_callback", .{ owner_name, sanitizeIdentifier(field_name) });
}

fn parseInlineCallbackFromQualType(
    allocator: std.mem.Allocator,
    callback_name: []const u8,
    qual_type: []const u8,
) !?CTypedef {
    const text = cleanCType(qual_type);
    const marker = std.mem.indexOf(u8, text, "(*)") orelse return null;
    const result_text = std.mem.trimEnd(u8, text[0..marker], " ");
    const params_start = std.mem.indexOfScalarPos(u8, text, marker + 3, '(') orelse return null;
    const params_end = std.mem.lastIndexOfScalar(u8, text, ')') orelse return null;
    if (params_end <= params_start) return null;
    const params_text = std.mem.trim(u8, text[params_start + 1 .. params_end], " ");

    var params = std.array_list.Managed([]const u8).init(allocator);
    if (!(std.mem.eql(u8, params_text, "void") or params_text.len == 0)) {
        var parts = std.mem.splitScalar(u8, params_text, ',');
        while (parts.next()) |part| {
            try params.append(try allocator.dupe(u8, std.mem.trim(u8, part, " ")));
        }
    }

    return .{
        .name = callback_name,
        .qual_type = try allocator.dupe(u8, text),
        .kind = .callback,
        .callback_params = try params.toOwnedSlice(),
        .callback_result = try allocator.dupe(u8, result_text),
    };
}

fn parseArrayType(allocator: std.mem.Allocator, text: []const u8) !?ArrayTypeInfo {
    const open = std.mem.lastIndexOfScalar(u8, text, '[') orelse return null;
    const close = std.mem.lastIndexOfScalar(u8, text, ']') orelse return null;
    if (close <= open) return null;
    const count_text = std.mem.trim(u8, text[open + 1 .. close], " ");
    const count = std.fmt.parseInt(usize, count_text, 10) catch return null;
    const element_text = std.mem.trim(u8, text[0..open], " ");
    const name = try syntheticArrayTypeName(allocator, element_text, count);
    return .{
        .name = name,
        .element_type = try allocator.dupe(u8, element_text),
        .count = count,
    };
}

fn syntheticArrayTypeName(allocator: std.mem.Allocator, element_text: []const u8, count: usize) ![]const u8 {
    const base_name = if (primitiveKiraTypeName(element_text)) |name|
        name
    else if (std.mem.endsWith(u8, element_text, "*"))
        try std.fmt.allocPrint(allocator, "{s}_ptr", .{trimPointerTarget(element_text)})
    else
        trimStructPrefix(element_text);
    return std.fmt.allocPrint(allocator, "{s}_array_{d}", .{ base_name, count });
}

fn sortStrings(values: [][]const u8) void {
    std.mem.sort([]const u8, values, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.order(u8, lhs, rhs) == .lt;
        }
    }.lessThan);
}

fn sortedMapKeys(allocator: std.mem.Allocator, map: anytype) ![]const []const u8 {
    var keys = std.array_list.Managed([]const u8).init(allocator);
    var iter = map.iterator();
    while (iter.next()) |entry| try keys.append(entry.key_ptr.*);
    sortStrings(keys.items);
    return keys.toOwnedSlice();
}
