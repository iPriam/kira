const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");

pub const NativeTypeLayout = struct {
    size: usize,
    alignment: usize,
};

pub fn fieldOffset(module: *const bytecode.Module, owner_type_name: []const u8, field_index: usize) anyerror!usize {
    const type_decl = findType(module, owner_type_name) orelse return error.RuntimeFailure;
    var offset: usize = 0;
    for (type_decl.fields, 0..) |field_decl, index| {
        const layout = try valueTypeLayout(module, field_decl.ty);
        offset = alignForward(offset, layout.alignment);
        if (index == field_index) return offset;
        offset += layout.size;
    }
    return error.RuntimeFailure;
}

pub fn valueTypeLayout(module: *const bytecode.Module, value_type: bytecode.TypeRef) anyerror!NativeTypeLayout {
    return switch (value_type.kind) {
        .void => .{ .size = 0, .alignment = 1 },
        .boolean => .{ .size = 1, .alignment = 1 },
        .integer => integerLayout(value_type.name),
        .float => if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
            .{ .size = 4, .alignment = 4 }
        else
            .{ .size = 8, .alignment = 8 },
        .string => .{ .size = @sizeOf(runtime_abi.BridgeString), .alignment = @alignOf(runtime_abi.BridgeString) },
        .construct_any, .array, .raw_ptr, .enum_instance => .{ .size = @sizeOf(usize), .alignment = @alignOf(usize) },
        .ffi_struct => try structLayout(module, value_type.name orelse return error.RuntimeFailure),
    };
}

pub fn structLayout(module: *const bytecode.Module, type_name: []const u8) anyerror!NativeTypeLayout {
    const type_decl = findType(module, type_name) orelse return error.RuntimeFailure;
    if (type_decl.fields.len == 0) {
        // Empty Kira types are emitted as `{ i8 }` in the native LLVM backend,
        // so they still occupy one byte and affect following field offsets.
        return .{ .size = 1, .alignment = 1 };
    }
    var offset: usize = 0;
    var max_alignment: usize = 1;
    for (type_decl.fields) |field_decl| {
        const field_layout = try valueTypeLayout(module, field_decl.ty);
        max_alignment = @max(max_alignment, field_layout.alignment);
        offset = alignForward(offset, field_layout.alignment);
        offset += field_layout.size;
    }
    return .{
        .size = alignForward(offset, max_alignment),
        .alignment = max_alignment,
    };
}

pub fn readInteger(name: ?[]const u8, address: usize) i64 {
    const value = name orelse return (@as(*const i64, @ptrFromInt(address))).*;
    if (std.mem.eql(u8, value, "U8")) return @as(i64, (@as(*const u8, @ptrFromInt(address))).*);
    if (std.mem.eql(u8, value, "U16")) return @as(i64, (@as(*const u16, @ptrFromInt(address))).*);
    if (std.mem.eql(u8, value, "U32")) return @as(i64, (@as(*const u32, @ptrFromInt(address))).*);
    if (std.mem.eql(u8, value, "I8")) return @as(*const i8, @ptrFromInt(address)).*;
    if (std.mem.eql(u8, value, "I16")) return @as(*const i16, @ptrFromInt(address)).*;
    if (std.mem.eql(u8, value, "I32")) return @as(*const i32, @ptrFromInt(address)).*;
    return (@as(*const i64, @ptrFromInt(address))).*;
}

pub fn readFloat(name: ?[]const u8, address: usize) f64 {
    if (name != null and std.mem.eql(u8, name.?, "F32")) {
        return @as(f64, (@as(*const f32, @ptrFromInt(address))).*);
    }
    return (@as(*const f64, @ptrFromInt(address))).*;
}

pub fn writeInteger(name: ?[]const u8, address: usize, value: runtime_abi.Value) anyerror!void {
    if (value != .integer) return error.RuntimeFailure;
    const raw = value.integer;
    const type_name = name orelse "I64";
    if (std.mem.eql(u8, type_name, "U8")) return writeInt(u8, address, raw);
    if (std.mem.eql(u8, type_name, "U16")) return writeInt(u16, address, raw);
    if (std.mem.eql(u8, type_name, "U32")) return writeInt(u32, address, raw);
    if (std.mem.eql(u8, type_name, "I8")) return writeInt(i8, address, raw);
    if (std.mem.eql(u8, type_name, "I16")) return writeInt(i16, address, raw);
    if (std.mem.eql(u8, type_name, "I32")) return writeInt(i32, address, raw);
    (@as(*i64, @ptrFromInt(address))).* = raw;
}

pub fn writeFloat(name: ?[]const u8, address: usize, value: runtime_abi.Value) anyerror!void {
    if (value != .float) return error.RuntimeFailure;
    if (name != null and std.mem.eql(u8, name.?, "F32")) {
        (@as(*f32, @ptrFromInt(address))).* = @floatCast(value.float);
        return;
    }
    (@as(*f64, @ptrFromInt(address))).* = value.float;
}

fn writeInt(comptime T: type, address: usize, value: i64) void {
    (@as(*T, @ptrFromInt(address))).* = @intCast(value);
}

fn integerLayout(name: ?[]const u8) NativeTypeLayout {
    const value = name orelse return .{ .size = 8, .alignment = 8 };
    if (std.mem.eql(u8, value, "I8") or std.mem.eql(u8, value, "U8")) return .{ .size = 1, .alignment = 1 };
    if (std.mem.eql(u8, value, "I16") or std.mem.eql(u8, value, "U16")) return .{ .size = 2, .alignment = 2 };
    if (std.mem.eql(u8, value, "I32") or std.mem.eql(u8, value, "U32")) return .{ .size = 4, .alignment = 4 };
    return .{ .size = 8, .alignment = 8 };
}

fn alignForward(value: usize, alignment: usize) usize {
    if (alignment <= 1) return value;
    return std.mem.alignForward(usize, value, alignment);
}

fn findType(module: *const bytecode.Module, name: []const u8) ?bytecode.TypeDecl {
    for (module.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
    }
    return null;
}

test "empty native types still occupy one byte and shift later fields" {
    const mode_ty = bytecode.TypeRef{ .kind = .enum_instance, .name = "Mode" };
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{
            .{ .name = "EmptyClass", .kind = .class, .fields = &.{} },
            .{ .name = "LayoutDescriptor", .fields = @constCast(&[_]bytecode.Field{
                .{ .name = "width", .ty = mode_ty },
                .{ .name = "height", .ty = mode_ty },
            }) },
            .{ .name = "Node", .fields = @constCast(&[_]bytecode.Field{
                .{ .name = "id", .ty = .{ .kind = .integer, .name = "I64" } },
                .{ .name = "view", .ty = .{ .kind = .ffi_struct, .name = "EmptyClass" } },
                .{ .name = "layout", .ty = .{ .kind = .ffi_struct, .name = "LayoutDescriptor" } },
            }) },
        }),
        .enums = @constCast(&[_]bytecode.EnumTypeDecl{.{
            .name = "Mode",
            .variants = @constCast(&[_]bytecode.EnumVariantDecl{.{ .name = "Fill", .discriminant = 0 }}),
        }}),
        .functions = &.{},
        .entry_function_id = null,
    };

    const empty_layout = try structLayout(&module, "EmptyClass");
    try std.testing.expectEqual(@as(usize, 1), empty_layout.size);
    try std.testing.expectEqual(@as(usize, 1), try fieldOffset(&module, "Node", 1));
    try std.testing.expectEqual(@as(usize, 16), try fieldOffset(&module, "Node", 2));
}
