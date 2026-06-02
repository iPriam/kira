const std = @import("std");
const syntax = @import("kira_ksl_syntax_model");
const shader_model = @import("kira_shader_model");
const shader_ir = @import("kira_shader_ir");

pub fn qualifiedKey(allocator: std.mem.Allocator, module_alias: ?[]const u8, name: []const u8) ![]const u8 {
    if (module_alias) |alias| return std.fmt.allocPrint(allocator, "{s}__{s}", .{ alias, name });
    return allocator.dupe(u8, name);
}

pub fn qualifiedNameText(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName) ![]const u8 {
    var buffer = std.array_list.Managed(u8).init(allocator);
    for (name.segments, 0..) |segment, index| {
        if (index != 0) try buffer.append('.');
        try buffer.appendSlice(segment.text);
    }
    return buffer.toOwnedSlice();
}

pub fn builtinType(name: []const u8) ?shader_model.Type {
    if (std.mem.eql(u8, name, "Bool")) return .{ .scalar = .bool };
    if (std.mem.eql(u8, name, "Int")) return .{ .scalar = .int };
    if (std.mem.eql(u8, name, "UInt")) return .{ .scalar = .uint };
    if (std.mem.eql(u8, name, "Float")) return .{ .scalar = .float };
    if (std.mem.eql(u8, name, "Float2")) return .{ .vector = .{ .scalar = .float, .width = 2 } };
    if (std.mem.eql(u8, name, "Float3")) return .{ .vector = .{ .scalar = .float, .width = 3 } };
    if (std.mem.eql(u8, name, "Float4")) return .{ .vector = .{ .scalar = .float, .width = 4 } };
    if (std.mem.eql(u8, name, "Int2")) return .{ .vector = .{ .scalar = .int, .width = 2 } };
    if (std.mem.eql(u8, name, "Int3")) return .{ .vector = .{ .scalar = .int, .width = 3 } };
    if (std.mem.eql(u8, name, "Int4")) return .{ .vector = .{ .scalar = .int, .width = 4 } };
    if (std.mem.eql(u8, name, "UInt2")) return .{ .vector = .{ .scalar = .uint, .width = 2 } };
    if (std.mem.eql(u8, name, "UInt3")) return .{ .vector = .{ .scalar = .uint, .width = 3 } };
    if (std.mem.eql(u8, name, "UInt4")) return .{ .vector = .{ .scalar = .uint, .width = 4 } };
    if (std.mem.eql(u8, name, "Float2x2")) return .{ .matrix = .{ .columns = 2, .rows = 2 } };
    if (std.mem.eql(u8, name, "Float3x3")) return .{ .matrix = .{ .columns = 3, .rows = 3 } };
    if (std.mem.eql(u8, name, "Float4x4")) return .{ .matrix = .{ .columns = 4, .rows = 4 } };
    if (std.mem.eql(u8, name, "Texture2d")) return .{ .texture = .texture_2d };
    if (std.mem.eql(u8, name, "TextureCube")) return .{ .texture = .texture_cube };
    if (std.mem.eql(u8, name, "DepthTexture2d")) return .{ .texture = .depth_2d };
    if (std.mem.eql(u8, name, "Sampler")) return .{ .sampler = .filtering };
    if (std.mem.eql(u8, name, "ComparisonSampler")) return .{ .sampler = .comparison };
    return null;
}

pub fn builtinFromName(name: []const u8) ?shader_model.Builtin {
    if (std.mem.eql(u8, name, "position")) return .position;
    if (std.mem.eql(u8, name, "vertex_index")) return .vertex_index;
    if (std.mem.eql(u8, name, "instance_index")) return .instance_index;
    if (std.mem.eql(u8, name, "front_facing")) return .front_facing;
    if (std.mem.eql(u8, name, "frag_coord")) return .frag_coord;
    if (std.mem.eql(u8, name, "thread_id")) return .thread_id;
    if (std.mem.eql(u8, name, "local_id")) return .local_id;
    if (std.mem.eql(u8, name, "group_id")) return .group_id;
    if (std.mem.eql(u8, name, "local_index")) return .local_index;
    return null;
}

pub fn annotationNameText(allocator: std.mem.Allocator, expr: *const syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .identifier => |value| qualifiedNameText(allocator, value.name),
        .member => |value| blk: {
            const object_text = try annotationNameText(allocator, value.object);
            break :blk try std.fmt.allocPrint(allocator, "{s}.{s}", .{ object_text, value.name });
        },
        else => allocator.dupe(u8, ""),
    };
}

pub fn stageKindToModel(kind: syntax.ast.StageKind) shader_model.Stage {
    return switch (kind) {
        .vertex => .vertex,
        .fragment => .fragment,
        .compute => .compute,
    };
}

pub const StageBindingsResult = struct {
    groups: []const shader_ir.GroupDecl,
    resources: []const shader_ir.ResourceDecl,
};

pub fn groupClassRank(class: shader_model.module.GroupClass) u32 {
    return switch (class) {
        .frame => 0,
        .pass => 1,
        .material => 2,
        .object => 3,
        .draw => 4,
        .dispatch => 5,
        .custom => 6,
    };
}

pub fn findField(fields: []const shader_ir.FieldDecl, name: []const u8) ?shader_ir.FieldDecl {
    for (fields) |field_decl| {
        if (std.mem.eql(u8, field_decl.name, name)) return field_decl;
    }
    return null;
}

pub fn typeEql(lhs: shader_model.Type, rhs: shader_model.Type) bool {
    if (std.meta.activeTag(lhs) != std.meta.activeTag(rhs)) return false;
    return switch (lhs) {
        .void => rhs == .void,
        .scalar => lhs.scalar == rhs.scalar,
        .vector => lhs.vector.scalar == rhs.vector.scalar and lhs.vector.width == rhs.vector.width,
        .matrix => lhs.matrix.columns == rhs.matrix.columns and lhs.matrix.rows == rhs.matrix.rows,
        .struct_ref => std.mem.eql(u8, lhs.struct_ref, rhs.struct_ref),
        .texture => lhs.texture == rhs.texture,
        .sampler => lhs.sampler == rhs.sampler,
        .runtime_array => rhs == .runtime_array and typeEql(lhs.runtime_array.*, rhs.runtime_array.*),
    };
}

pub fn typeName(allocator: std.mem.Allocator, ty: shader_model.Type) ![]const u8 {
    return switch (ty) {
        .void => allocator.dupe(u8, "Void"),
        .scalar => |scalar| allocator.dupe(u8, switch (scalar) {
            .bool => "Bool",
            .int => "Int",
            .uint => "UInt",
            .float => "Float",
        }),
        .vector => |vector| std.fmt.allocPrint(allocator, "{s}{d}", .{
            switch (vector.scalar) {
                .bool => "Bool",
                .int => "Int",
                .uint => "UInt",
                .float => "Float",
            },
            vector.width,
        }),
        .matrix => |matrix| std.fmt.allocPrint(allocator, "Float{d}x{d}", .{ matrix.columns, matrix.rows }),
        .struct_ref => allocator.dupe(u8, ty.struct_ref),
        .texture => |texture| allocator.dupe(u8, switch (texture) {
            .texture_2d => "Texture2d",
            .texture_cube => "TextureCube",
            .depth_2d => "DepthTexture2d",
        }),
        .sampler => |sampler| allocator.dupe(u8, switch (sampler) {
            .filtering => "Sampler",
            .comparison => "ComparisonSampler",
        }),
        .runtime_array => |element| std.fmt.allocPrint(allocator, "[{s}]", .{try typeName(allocator, element.*)}),
    };
}

pub fn constValueText(allocator: std.mem.Allocator, value: shader_ir.ConstValue) ![]const u8 {
    return switch (value) {
        .bool => |bool_value| allocator.dupe(u8, if (bool_value) "true" else "false"),
        .int => |int_value| std.fmt.allocPrint(allocator, "{d}", .{int_value}),
        .uint => |uint_value| std.fmt.allocPrint(allocator, "{d}", .{uint_value}),
        .float => |float_value| std.fmt.allocPrint(allocator, "{d}", .{float_value}),
    };
}

pub fn reflectedLayout(allocator: std.mem.Allocator, class: []const u8, layout: shader_ir.StructLayout) !shader_model.ReflectedLayout {
    var fields = std.array_list.Managed(shader_model.ReflectedLayoutField).init(allocator);
    for (layout.fields) |field_layout| {
        try fields.append(.{
            .name = field_layout.name,
            .offset = field_layout.offset,
            .alignment = field_layout.alignment,
            .size = field_layout.size,
            .stride = field_layout.stride,
        });
    }
    return .{
        .class = class,
        .alignment = layout.alignment,
        .size = layout.size,
        .fields = try fields.toOwnedSlice(),
    };
}

pub fn resourceDeclVisibility(allocator: std.mem.Allocator, stages: []const shader_ir.StageDecl, resource_name: []const u8) ![]const shader_model.Stage {
    _ = resource_name;
    var items = std.array_list.Managed(shader_model.Stage).init(allocator);
    for (stages) |stage_decl| try items.append(stage_decl.kind);
    return items.toOwnedSlice();
}

pub fn intrinsicFromName(name: []const u8) ?shader_ir.Intrinsic {
    if (std.mem.eql(u8, name, "mul")) return .mul;
    if (std.mem.eql(u8, name, "normalize")) return .normalize;
    if (std.mem.eql(u8, name, "dot")) return .dot;
    if (std.mem.eql(u8, name, "sample")) return .sample;
    if (std.mem.eql(u8, name, "length")) return .length;
    if (std.mem.eql(u8, name, "pow")) return .pow;
    if (std.mem.eql(u8, name, "sin")) return .sin;
    if (std.mem.eql(u8, name, "atan2")) return .atan2;
    if (std.mem.eql(u8, name, "smoothstep")) return .smoothstep;
    return null;
}

pub fn inferBinaryType(
    analyzer: anytype,
    binary_expr: syntax.ast.BinaryExpr,
    left_ty: shader_model.Type,
    right_ty: shader_model.Type,
) !shader_model.Type {
    const comparison = switch (binary_expr.op) {
        .less, .less_equal, .greater, .greater_equal, .equal, .not_equal => true,
        else => false,
    };
    if (comparison) {
        if (!typeEql(left_ty, right_ty)) {
            try analyzer.emitDiagnostic("KSL022", "comparison type mismatch", binary_expr.span, "Compare values of the same type.");
            return error.DiagnosticsEmitted;
        }
        return .{ .scalar = .bool };
    }
    if (typeEql(left_ty, right_ty)) return left_ty;
    if (isVectorType(left_ty) and right_ty == .scalar and vectorScalar(left_ty) == right_ty.scalar) return left_ty;
    if (isVectorType(right_ty) and left_ty == .scalar and vectorScalar(right_ty) == left_ty.scalar) return right_ty;
    try analyzer.emitDiagnostic("KSL023", "binary operator type mismatch", binary_expr.span, "Make both sides use compatible numeric types.");
    return error.DiagnosticsEmitted;
}

pub fn isVectorType(ty: shader_model.Type) bool {
    return ty == .vector;
}

pub fn vectorScalar(ty: shader_model.Type) ?shader_model.ScalarType {
    return if (ty == .vector) ty.vector.scalar else null;
}

pub fn isIntegerLike(ty: shader_model.Type) bool {
    return switch (ty) {
        .scalar => ty.scalar == .int or ty.scalar == .uint,
        .vector => ty.vector.scalar == .int or ty.vector.scalar == .uint,
        else => false,
    };
}

pub fn alignForward(value: u32, alignment: u32) u32 {
    const remainder = value % alignment;
    return if (remainder == 0) value else value + (alignment - remainder);
}
