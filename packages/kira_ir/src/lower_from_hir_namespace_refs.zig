const std = @import("std");
const ir = @import("ir.zig");
const model = @import("kira_semantics_model");
const parent = @import("lower_from_hir.zig");

const Lowerer = parent.Lowerer;
const findTypeFieldDefaultExpr = parent.findTypeFieldDefaultExpr;
const findTypeDeclByName = parent.findTypeDeclByName;
const functionIdByName = parent.functionIdByName;

pub fn lowerNamespaceRefExpr(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    path: []const u8,
) !?u32 {
    const split = splitQualifiedPath(path) orelse return null;
    if (try lowerNamespaceEnumRef(lowerer, instructions, split.namespace_path, split.member_name)) |lowered| {
        return lowered;
    }
    if (try lowerNamespaceTypeFieldRef(lowerer, instructions, split.namespace_path, split.member_name)) |lowered| {
        return lowered;
    }
    if (try lowerNamespaceAccessorRef(lowerer, instructions, path)) |lowered| {
        return lowered;
    }
    return null;
}

const QualifiedPath = struct {
    namespace_path: []const u8,
    member_name: []const u8,
};

fn splitQualifiedPath(path: []const u8) ?QualifiedPath {
    const separator = std.mem.lastIndexOfScalar(u8, path, '.') orelse return null;
    if (separator == 0 or separator + 1 >= path.len) return null;
    return .{
        .namespace_path = path[0..separator],
        .member_name = path[separator + 1 ..],
    };
}

fn lowerNamespaceEnumRef(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    namespace_path: []const u8,
    member_name: []const u8,
) !?u32 {
    const enum_decl = resolveEnumDecl(lowerer.program, namespace_path) orelse return null;
    const variant_decl = findEnumVariant(enum_decl, member_name) orelse return null;

    const payload_src = if (variant_decl.payload_ty != null) blk: {
        const default_value = variant_decl.default_value orelse return error.UnsupportedExecutableFeature;
        break :blk try lowerer.lowerExpr(instructions, default_value);
    } else null;

    const dst = lowerer.freshRegister();
    try instructions.append(.{ .alloc_enum = .{
        .dst = dst,
        .enum_type_name = enum_decl.name,
        .discriminant = variant_decl.discriminant,
        .payload_src = payload_src,
    } });
    return dst;
}

fn lowerNamespaceTypeFieldRef(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    namespace_path: []const u8,
    member_name: []const u8,
) !?u32 {
    const type_decl = resolveTypeDecl(lowerer.program, namespace_path) orelse return null;
    for (type_decl.fields) |field_decl| {
        if (!std.mem.eql(u8, field_decl.name, member_name)) continue;
        const default_value = findTypeFieldDefaultExpr(lowerer.program, type_decl.name, member_name) orelse return error.UnsupportedExecutableFeature;
        return try lowerer.lowerExpr(instructions, default_value);
    }
    return null;
}

fn lowerNamespaceAccessorRef(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    path: []const u8,
) !?u32 {
    const function_id = functionIdByName(lowerer.program, path) orelse return null;
    const function_decl = findFunctionById(lowerer.program, function_id) orelse return error.UnsupportedExecutableFeature;
    if (function_decl.params.len != 0 or function_decl.return_type.kind == .void) return error.UnsupportedExecutableFeature;

    const dst = lowerer.freshRegister();
    try instructions.append(.{ .call = .{
        .callee = function_id,
        .args = &.{},
        .dst = dst,
    } });
    return dst;
}

fn resolveEnumDecl(program: model.Program, namespace_path: []const u8) ?model.EnumDecl {
    if (findEnumDeclByName(program, namespace_path)) |enum_decl| return enum_decl;
    return findEnumDeclByLeaf(program, qualifiedLeaf(namespace_path));
}

fn resolveTypeDecl(program: model.Program, namespace_path: []const u8) ?model.TypeDecl {
    if (findTypeDeclByName(program, namespace_path)) |type_decl| return type_decl;
    return findTypeDeclByLeaf(program, qualifiedLeaf(namespace_path));
}

fn findEnumDeclByName(program: model.Program, name: []const u8) ?model.EnumDecl {
    for (program.enums) |enum_decl| {
        if (std.mem.eql(u8, enum_decl.name, name)) return enum_decl;
    }
    return null;
}

fn findEnumVariant(enum_decl: model.EnumDecl, name: []const u8) ?model.EnumVariantHir {
    for (enum_decl.variants) |variant_decl| {
        if (std.mem.eql(u8, variant_decl.name, name)) return variant_decl;
    }
    return null;
}

fn findFunctionById(program: model.Program, function_id: u32) ?model.Function {
    for (program.functions) |function_decl| {
        if (function_decl.id == function_id) return function_decl;
    }
    return null;
}

fn findEnumDeclByLeaf(program: model.Program, leaf_name: []const u8) ?model.EnumDecl {
    var resolved: ?model.EnumDecl = null;
    for (program.enums) |enum_decl| {
        if (!std.mem.eql(u8, qualifiedLeaf(enum_decl.name), leaf_name)) continue;
        if (resolved != null) return null;
        resolved = enum_decl;
    }
    return resolved;
}

fn findTypeDeclByLeaf(program: model.Program, leaf_name: []const u8) ?model.TypeDecl {
    var resolved: ?model.TypeDecl = null;
    for (program.types) |type_decl| {
        if (!std.mem.eql(u8, qualifiedLeaf(type_decl.name), leaf_name)) continue;
        if (resolved != null) return null;
        resolved = type_decl;
    }
    return resolved;
}

fn qualifiedLeaf(path: []const u8) []const u8 {
    const separator = std.mem.lastIndexOfScalar(u8, path, '.') orelse return path;
    return path[separator + 1 ..];
}
