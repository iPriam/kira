const std = @import("std");
const ir = @import("ir.zig");
const model = @import("kira_semantics_model");

pub fn lowerResolvedTypeSlice(allocator: std.mem.Allocator, program: model.Program, types: []const model.ResolvedType) ![]ir.ValueType {
    const lowered = try allocator.alloc(ir.ValueType, types.len);
    for (types, 0..) |ty, index| lowered[index] = try lowerResolvedType(program, ty);
    return lowered;
}

pub fn lowerResolvedType(program: model.Program, ty: model.ResolvedType) !ir.ValueType {
    return switch (ty.kind) {
        .void => .{ .kind = .void },
        .integer => .{ .kind = .integer, .name = ty.name },
        .float => .{ .kind = .float, .name = ty.name },
        .string => .{ .kind = .string },
        .boolean => .{ .kind = .boolean, .name = ty.name },
        .construct_any => .{ .kind = .construct_any, .name = ty.name, .construct_constraint = if (ty.construct_constraint) |constraint| .{ .construct_name = constraint.construct_name } else null, .existential = ty.existential },
        .array => .{ .kind = .array, .name = ty.name },
        .raw_ptr, .c_string, .callback, .native_state, .native_state_view => .{ .kind = .raw_ptr, .name = ty.name },
        .enum_instance => .{ .kind = .enum_instance, .name = ty.name },
        .named => if (ty.name) |name| lowerNamedType(program, name) else return error.UnsupportedType,
        .ffi_struct, .unknown => return error.UnsupportedType,
    };
}

pub fn lowerNamedType(program: model.Program, name: []const u8) anyerror!ir.ValueType {
    for (program.types) |type_decl| {
        if (!std.mem.eql(u8, type_decl.name, name)) continue;
        if (type_decl.ffi) |ffi_info| {
            return switch (ffi_info) {
                .pointer, .callback => .{ .kind = .raw_ptr, .name = name },
                .alias => |value| lowerResolvedType(program, value.target),
                .ffi_struct => .{ .kind = .ffi_struct, .name = name },
                .array => .{ .kind = .raw_ptr, .name = name },
            };
        }
        return .{ .kind = .ffi_struct, .name = name };
    }
    for (program.enums) |enum_decl| {
        if (std.mem.eql(u8, enum_decl.name, name)) return .{ .kind = .enum_instance, .name = name };
    }
    if (std.mem.endsWith(u8, name, "_ptr")) return .{ .kind = .raw_ptr, .name = name };
    return error.UnsupportedType;
}

pub fn lowerExecutableCompareOperandType(program: model.Program, ty: model.ResolvedType, op: model.hir.BinaryOp) !ir.ValueType {
    const lowered = try lowerResolvedType(program, ty);
    return switch (lowered.kind) {
        .integer => lowered,
        .float => lowered,
        .boolean => switch (op) {
            .equal, .not_equal => lowered,
            else => error.UnsupportedExecutableFeature,
        },
        .raw_ptr, .ffi_struct, .enum_instance => switch (op) {
            .equal, .not_equal => lowered,
            else => error.UnsupportedExecutableFeature,
        },
        // String content equality; ordering (`<`/`>`) is not defined.
        .string => switch (op) {
            .equal, .not_equal => lowered,
            else => error.UnsupportedExecutableFeature,
        },
        else => error.UnsupportedExecutableFeature,
    };
}

pub fn lowerExecutableIntegerType(program: model.Program, ty: model.ResolvedType) !ir.ValueType {
    const lowered = try lowerResolvedType(program, ty);
    if (lowered.kind != .integer) return error.UnsupportedExecutableFeature;
    return lowered;
}

pub fn lowerExecutableNumericType(program: model.Program, ty: model.ResolvedType) !ir.ValueType {
    const lowered = try lowerResolvedType(program, ty);
    return switch (lowered.kind) {
        .integer, .float => lowered,
        else => error.UnsupportedExecutableFeature,
    };
}

pub fn lowerExecutableBooleanType(program: model.Program, ty: model.ResolvedType) !ir.ValueType {
    const lowered = try lowerResolvedType(program, ty);
    if (lowered.kind != .boolean) return error.UnsupportedExecutableFeature;
    return lowered;
}

pub fn valueTypesEqual(lhs: ir.ValueType, rhs: ir.ValueType) bool {
    if (lhs.kind != rhs.kind) return false;
    if (lhs.construct_constraint) |constraint| {
        const rhs_constraint = rhs.construct_constraint orelse return false;
        if (!std.mem.eql(u8, constraint.construct_name, rhs_constraint.construct_name)) return false;
    } else if (rhs.construct_constraint != null) {
        return false;
    }
    if (lhs.name == null and rhs.name == null) return true;
    if (lhs.name == null or rhs.name == null) return false;
    return std.mem.eql(u8, lhs.name.?, rhs.name.?);
}

pub fn findTypeDeclByName(program: model.Program, name: []const u8) ?model.TypeDecl {
    for (program.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
    }
    return null;
}

pub fn resolveConstructFieldIndex(
    type_decl: model.TypeDecl,
    filled: []bool,
    next_index: *usize,
    field_init: model.hir.ConstructFieldInit,
) !usize {
    if (field_init.field_index) |field_index| return field_index;
    if (field_init.field_name) |field_name| {
        return fieldIndexByName(type_decl, field_name) orelse return error.UnsupportedExecutableFeature;
    }

    while (next_index.* < filled.len and filled[next_index.*]) next_index.* += 1;
    if (next_index.* >= filled.len) return error.UnsupportedExecutableFeature;
    const resolved = next_index.*;
    next_index.* += 1;
    return resolved;
}

pub fn fieldIndexByName(type_decl: model.TypeDecl, field_name: []const u8) ?usize {
    for (type_decl.fields, 0..) |field_decl, index| {
        if (std.mem.eql(u8, field_decl.name, field_name)) return index;
    }
    return null;
}

pub fn nativeStateTypeId(type_name: []const u8) u64 {
    var hash: u64 = 14695981039346656037;
    for (type_name) |byte| {
        hash ^= @as(u64, byte);
        hash *%= 1099511628211;
    }
    return hash & 0x7fff_ffff_ffff_ffff;
}
