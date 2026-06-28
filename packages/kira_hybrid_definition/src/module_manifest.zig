const std = @import("std");
const runtime_abi = @import("kira_runtime_abi");

pub const TypeRef = struct {
    kind: Kind,
    name: ?[]const u8 = null,
    construct_constraint: ?ConstructConstraint = null,

    pub const ConstructConstraint = struct {
        construct_name: []const u8,
    };

    pub const Kind = enum(u8) {
        void,
        integer,
        float,
        string,
        boolean,
        construct_any,
        array,
        raw_ptr,
        ffi_struct,
        enum_instance,
    };
};

pub const OwnershipMode = enum(u8) {
    owned,
    borrow_read,
    borrow_mut,
    move,
    copy,
};

pub const FunctionManifest = struct {
    id: u32,
    name: []const u8,
    execution: runtime_abi.FunctionExecution,
    param_types: []const TypeRef = &.{},
    param_ownership: []const OwnershipMode = &.{},
    return_type: TypeRef = .{ .kind = .void },
    return_ownership: OwnershipMode = .owned,
    exported_name: ?[]const u8 = null,
};

pub const HybridModuleManifest = struct {
    module_name: []const u8,
    bytecode_path: []const u8,
    native_library_path: []const u8,
    entry_function_id: u32,
    entry_execution: runtime_abi.FunctionExecution,
    functions: []const FunctionManifest,

    pub fn writeToFile(self: HybridModuleManifest, path: []const u8) !void {
        const file = try std.Io.Dir.cwd().createFile(std.Options.debug_io, path, .{ .truncate = true });
        defer file.close(std.Options.debug_io);
        var buffer: [4096]u8 = undefined;
        var writer = file.writer(std.Options.debug_io, &buffer);
        defer writer.interface.flush() catch {};

        try writer.interface.writeAll("KHM2");
        try writeString(&writer.interface, self.module_name);
        try writeString(&writer.interface, self.bytecode_path);
        try writeString(&writer.interface, self.native_library_path);
        try writer.interface.writeInt(u32, self.entry_function_id, .little);
        try writer.interface.writeByte(@intFromEnum(self.entry_execution));
        try writer.interface.writeInt(u32, @as(u32, @intCast(self.functions.len)), .little);
        for (self.functions) |function_decl| {
            try writer.interface.writeInt(u32, function_decl.id, .little);
            try writer.interface.writeByte(@intFromEnum(function_decl.execution));
            try writeString(&writer.interface, function_decl.name);
            try writer.interface.writeInt(u32, @as(u32, @intCast(function_decl.param_types.len)), .little);
            for (function_decl.param_types) |param_type| try writeTypeRef(&writer.interface, param_type);
            try writeOwnershipModes(&writer.interface, function_decl.param_ownership);
            try writeTypeRef(&writer.interface, function_decl.return_type);
            try writer.interface.writeByte(@intFromEnum(function_decl.return_ownership));
            try writeString(&writer.interface, function_decl.exported_name orelse "");
        }
    }

    pub fn readFromFile(allocator: std.mem.Allocator, path: []const u8) !HybridModuleManifest {
        // Our own build artifact; a large program's manifest can exceed 1 MiB.
        const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(256 * 1024 * 1024));
        var reader_state = std.Io.Reader.fixed(bytes);
        const reader = &reader_state;

        var magic: [4]u8 = undefined;
        try reader.readSliceAll(&magic);
        const format: enum { khm1, khm2 } = if (std.mem.eql(u8, &magic, "KHM1"))
            .khm1
        else if (std.mem.eql(u8, &magic, "KHM2"))
            .khm2
        else
            return error.InvalidHybridManifest;

        const module_name = try readString(allocator, reader);
        const bytecode_path = try readString(allocator, reader);
        const native_library_path = try readString(allocator, reader);
        const entry_function_id = try reader.takeInt(u32, .little);
        const entry_execution: runtime_abi.FunctionExecution = @enumFromInt(try reader.takeByte());
        const function_count = try reader.takeInt(u32, .little);
        var functions = std.array_list.Managed(FunctionManifest).init(allocator);
        for (0..function_count) |_| {
            const function_id = try reader.takeInt(u32, .little);
            const execution: runtime_abi.FunctionExecution = @enumFromInt(try reader.takeByte());
            const name = try readString(allocator, reader);
            const param_count = try reader.takeInt(u32, .little);
            const param_types = try allocator.alloc(TypeRef, param_count);
            for (param_types) |*param_type| param_type.* = try readTypeRef(allocator, reader);
            const param_ownership = switch (format) {
                .khm1 => try defaultOwnershipModes(allocator, param_count, .owned),
                .khm2 => try readOwnershipModes(allocator, reader),
            };
            const return_type = try readTypeRef(allocator, reader);
            const return_ownership = switch (format) {
                .khm1 => OwnershipMode.owned,
                .khm2 => @as(OwnershipMode, @enumFromInt(try reader.takeByte())),
            };
            const exported_name = try readString(allocator, reader);
            try functions.append(.{
                .id = function_id,
                .name = name,
                .execution = execution,
                .param_types = param_types,
                .param_ownership = param_ownership,
                .return_type = return_type,
                .return_ownership = return_ownership,
                .exported_name = if (exported_name.len == 0) null else exported_name,
            });
        }

        return .{
            .module_name = module_name,
            .bytecode_path = bytecode_path,
            .native_library_path = native_library_path,
            .entry_function_id = entry_function_id,
            .entry_execution = entry_execution,
            .functions = try functions.toOwnedSlice(),
        };
    }
};

fn writeString(writer: anytype, value: []const u8) !void {
    try writer.writeInt(u32, @as(u32, @intCast(value.len)), .little);
    try writer.writeAll(value);
}

fn readString(allocator: std.mem.Allocator, reader: anytype) ![]const u8 {
    const length = try reader.takeInt(u32, .little);
    const buffer = try allocator.alloc(u8, length);
    try reader.readSliceAll(buffer);
    return buffer;
}

fn writeTypeRef(writer: anytype, value: TypeRef) !void {
    try writer.writeByte(@intFromEnum(value.kind));
    try writeString(writer, value.name orelse "");
    try writeString(writer, if (value.construct_constraint) |constraint| constraint.construct_name else "");
}

fn readTypeRef(allocator: std.mem.Allocator, reader: anytype) !TypeRef {
    const kind: TypeRef.Kind = @enumFromInt(try reader.takeByte());
    const name = try readString(allocator, reader);
    const constraint_name = try readString(allocator, reader);
    return .{
        .kind = kind,
        .name = if (name.len == 0) null else name,
        .construct_constraint = if (constraint_name.len == 0) null else .{ .construct_name = constraint_name },
    };
}

fn writeOwnershipModes(writer: anytype, values: []const OwnershipMode) !void {
    try writer.writeInt(u32, @as(u32, @intCast(values.len)), .little);
    for (values) |value| try writer.writeByte(@intFromEnum(value));
}

fn readOwnershipModes(allocator: std.mem.Allocator, reader: anytype) ![]const OwnershipMode {
    const count = try reader.takeInt(u32, .little);
    const values = try allocator.alloc(OwnershipMode, count);
    for (values) |*value| value.* = @enumFromInt(try reader.takeByte());
    return values;
}

fn defaultOwnershipModes(allocator: std.mem.Allocator, count: u32, mode: OwnershipMode) ![]const OwnershipMode {
    const values = try allocator.alloc(OwnershipMode, count);
    @memset(values, mode);
    return values;
}
