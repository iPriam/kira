const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const exprs = @import("lower_exprs.zig");

pub fn lowerField(
    ctx: *shared.Context,
    field_decl: syntax.ast.FieldDecl,
    construct_model: ?model.Construct,
) !model.Field {
    try shared.validateAnnotationPlacement(ctx, field_decl.annotations, .field_decl, construct_model);
    const field_type = try exprs.resolveValueType(ctx, field_decl.type_expr, field_decl.value, field_decl.span);
    return .{
        .name = try ctx.allocator.dupe(u8, field_decl.name),
        .owner_type_name = "",
        .storage = @enumFromInt(@intFromEnum(field_decl.storage)),
        .slot_index = 0,
        .ty = field_type,
        .explicit_type = field_decl.type_expr != null,
        .default_value = if (field_decl.value) |value| try lowerFieldDefaultExprExpected(ctx, value, field_type, ctx.function_headers) else null,
        .annotations = try shared.lowerAnnotations(ctx, field_decl.annotations),
        .span = field_decl.span,
    };
}

pub fn lowerFieldDefaultExpr(ctx: *shared.Context, expr: *syntax.ast.Expr) !*model.Expr {
    return lowerFieldDefaultExprExpected(ctx, expr, .{ .kind = .unknown }, null);
}

pub fn lowerFieldDefaultExprExpected(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    if (expected_type.kind == .enum_instance) {
        if (try lowerFieldDefaultEnumVariantExpr(ctx, expr, expected_type, function_headers)) |enum_expr| {
            return enum_expr;
        }
    }

    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = switch (expr.*) {
        .integer => |node| .{ .integer = .{
            .value = node.value,
            .ty = .{ .kind = .integer },
            .span = node.span,
        } },
        .float => |node| .{ .float = .{
            .value = node.value,
            .ty = .{ .kind = .float },
            .span = node.span,
        } },
        .string => |node| .{ .string = .{
            .value = try ctx.allocator.dupe(u8, node.value),
            .ty = .{ .kind = .string },
            .span = node.span,
        } },
        .bool => |node| .{ .boolean = .{
            .value = node.value,
            .ty = .{ .kind = .boolean },
            .span = node.span,
        } },
        .array => |node| blk: {
            var elements = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            const element_type = try fieldDefaultArrayElementType(ctx, expected_type);
            for (node.elements) |element| try elements.append(try lowerFieldDefaultExprExpected(ctx, element, element_type, function_headers));
            break :blk .{ .array = .{
                .elements = try elements.toOwnedSlice(),
                .ty = if (expected_type.kind == .array) expected_type else .{ .kind = .array },
                .span = node.span,
            } };
        },
        .struct_literal => |node| blk: {
            var fields = std.array_list.Managed(model.ConstructFieldInit).init(ctx.allocator);
            for (node.fields) |field| {
                try fields.append(.{
                    .field_name = try ctx.allocator.dupe(u8, field.name),
                    .field_index = null,
                    .value = try lowerFieldDefaultExprExpected(ctx, field.value, .{ .kind = .unknown }, function_headers),
                    .span = field.span,
                });
            }
            break :blk .{ .construct = .{
                .type_name = try shared.qualifiedNameLeaf(ctx.allocator, node.type_name),
                .fields = try fields.toOwnedSlice(),
                .fill_mode = .defaults,
                .ty = .{ .kind = .named, .name = try shared.qualifiedNameLeaf(ctx.allocator, node.type_name) },
                .span = node.span,
            } };
        },
        .call => |node| blk: {
            const callee_name = switch (node.callee.*) {
                .identifier => |value| try shared.qualifiedNameText(ctx.allocator, value.name),
                .member => try flattenDefaultCalleeName(ctx.allocator, node.callee),
                else => {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM049",
                        .title = "unsupported field default value",
                        .message = "Field default constructor calls currently require a named type call target.",
                        .labels = &.{diagnostics.primaryLabel(defaultExprSpan(expr.*), "unsupported field default call target")},
                        .help = "Use a named type constructor such as `Point(x: 0.0, y: 0.0)`.",
                    });
                    return error.DiagnosticsEmitted;
                },
            };
            var fields = std.array_list.Managed(model.ConstructFieldInit).init(ctx.allocator);
            for (node.args) |arg| {
                try fields.append(.{
                    .field_name = if (arg.label) |label| try ctx.allocator.dupe(u8, label) else null,
                    .field_index = null,
                    .value = try lowerFieldDefaultExprExpected(ctx, arg.value, .{ .kind = .unknown }, function_headers),
                    .span = arg.span,
                });
            }
            break :blk .{ .construct = .{
                .type_name = try ctx.allocator.dupe(u8, qualifiedLeafText(callee_name)),
                .fields = try fields.toOwnedSlice(),
                .fill_mode = .defaults,
                .ty = .{ .kind = .named, .name = try ctx.allocator.dupe(u8, qualifiedLeafText(callee_name)) },
                .span = node.span,
            } };
        },
        .member => |node| .{ .namespace_ref = .{
            .root = switch (node.object.*) {
                .identifier => |value| try shared.qualifiedNameLeaf(ctx.allocator, value.name),
                else => "",
            },
            .path = switch (node.object.*) {
                .identifier => |value| try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ value.name.segments[value.name.segments.len - 1].text, node.member }),
                else => try ctx.allocator.dupe(u8, node.member),
            },
            .ty = .{ .kind = .unknown },
            .span = node.span,
        } },
        .identifier => |node| blk: {
            if (expected_type.kind == .callback) {
                if (function_headers) |headers| {
                    const name = try shared.qualifiedNameText(ctx.allocator, node.name);
                    if (headers.get(name)) |header| {
                        break :blk .{ .function_ref = .{
                            .representation = .callable_value,
                            .function_id = header.id,
                            .name = name,
                            .ty = expected_type,
                            .span = node.span,
                        } };
                    }
                }
            }
            try diagnostics.Emitter.init(ctx.allocator, ctx.diagnostics).err(.{
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Field defaults can only use a bare function name when the field has an explicit function type.",
                .span = defaultExprSpan(expr.*),
                .label = "unsupported field default value",
                .help = "Add an explicit function type to the field or use a literal/constructor default.",
            });
            return error.DiagnosticsEmitted;
        },
        .callback => {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Field defaults do not support callback literals.",
                .labels = &.{diagnostics.primaryLabel(defaultExprSpan(expr.*), "unsupported field default value")},
                .help = "Use a literal, constructor, or named constant for the field default.",
            });
            return error.DiagnosticsEmitted;
        },
        .unary => |node| blk: {
            const operand = try lowerFieldDefaultExprExpected(ctx, node.operand, .{ .kind = .unknown }, function_headers);
            break :blk .{ .unary = .{
                .operand = operand,
                .op = @enumFromInt(@intFromEnum(node.op)),
                .ty = model.hir.exprType(operand.*),
                .span = node.span,
            } };
        },
        else => {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Field default values in the executable pipeline currently require a literal or simple unary literal.",
                .labels = &.{diagnostics.primaryLabel(defaultExprSpan(expr.*), "unsupported field default value")},
                .help = "Use a literal default such as `7`, `true`, `1.5`, or `-1`.",
            });
            return error.DiagnosticsEmitted;
        },
    };
    return lowered;
}

fn lowerFieldDefaultEnumVariantExpr(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) anyerror!?*model.Expr {
    const enum_name = expected_type.name orelse return null;
    const enum_decl = findEnumDeclForDefault(ctx, enum_name) orelse return null;
    const VariantTarget = struct {
        name: []const u8,
        payload: ?*syntax.ast.Expr,
        span: source_pkg.Span,
    };
    const target: VariantTarget = switch (expr.*) {
        .identifier => |node| blk: {
            if (node.name.segments.len < 2 or !std.mem.eql(u8, node.name.segments[0].text, enum_name)) return null;
            break :blk .{
                .name = node.name.segments[node.name.segments.len - 1].text,
                .payload = null,
                .span = node.span,
            };
        },
        .member => |node| .{
            .name = node.member,
            .payload = null,
            .span = node.span,
        },
        .call => |node| blk: {
            const variant_name = switch (node.callee.*) {
                .member => |member| member.member,
                .identifier => |callee| name_blk: {
                    if (callee.name.segments.len < 2 or !std.mem.eql(u8, callee.name.segments[0].text, enum_name)) return null;
                    break :name_blk callee.name.segments[callee.name.segments.len - 1].text;
                },
                else => return null,
            };
            break :blk .{
                .name = variant_name,
                .payload = if (node.args.len == 1) node.args[0].value else null,
                .span = node.span,
            };
        },
        else => return null,
    };
    const variant = findEnumVariantForDefault(enum_decl, target.name) orelse return null;
    const payload = if (target.payload) |payload_expr|
        if (variant.payload_ty) |payload_ty|
            try lowerFieldDefaultExprExpected(ctx, payload_expr, payload_ty, function_headers)
        else {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM105",
                .title = "match pattern payload is invalid",
                .message = "This enum variant does not accept a payload value.",
                .labels = &.{diagnostics.primaryLabel(target.span, "payload value is not valid for this enum variant")},
                .help = "Remove the argument from this enum constructor call.",
            });
            return error.DiagnosticsEmitted;
        }
    else if (variant.payload_ty != null)
        variant.default_value
    else
        null;
    if (variant.payload_ty != null and payload == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM105",
            .title = "match pattern payload is invalid",
            .message = "This enum variant requires an associated payload value.",
            .labels = &.{diagnostics.primaryLabel(target.span, "missing enum payload value")},
            .help = "Pass the payload argument or add a default value on the enum variant declaration.",
        });
        return error.DiagnosticsEmitted;
    }
    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = .{ .construct_enum_variant = .{
        .enum_name = try ctx.allocator.dupe(u8, enum_decl.name),
        .variant_name = try ctx.allocator.dupe(u8, variant.name),
        .discriminant = variant.discriminant,
        .payload = payload,
        .ty = .{ .kind = .enum_instance, .name = try ctx.allocator.dupe(u8, enum_decl.name) },
        .span = target.span,
    } };
    return lowered;
}

fn findEnumDeclForDefault(ctx: *shared.Context, enum_name: []const u8) ?model.EnumDecl {
    if (ctx.concrete_enums) |concrete_enums| {
        if (concrete_enums.get(enum_name)) |enum_decl| return enum_decl;
    }
    if (ctx.enum_headers) |enum_headers| {
        if (enum_headers.get(enum_name)) |enum_decl| return enum_decl;
    }
    return null;
}

fn findEnumVariantForDefault(enum_decl: model.EnumDecl, variant_name: []const u8) ?model.EnumVariantHir {
    for (enum_decl.variants) |variant| {
        if (std.mem.eql(u8, variant.name, variant_name)) return variant;
    }
    return null;
}

fn fieldDefaultArrayElementType(ctx: *shared.Context, expected_type: model.ResolvedType) !model.ResolvedType {
    if (expected_type.kind != .array or expected_type.name == null) return .{ .kind = .unknown };
    const element_name = expected_type.name.?;
    if (ctx.enum_headers) |enum_headers| {
        if (enum_headers.get(element_name) != null) return .{ .kind = .enum_instance, .name = element_name };
    }
    if (ctx.concrete_enums) |concrete_enums| {
        if (concrete_enums.get(element_name) != null) return .{ .kind = .enum_instance, .name = element_name };
    }
    return try shared.resolvedTypeFromText(element_name);
}

fn defaultExprSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        .integer => |node| node.span,
        .float => |node| node.span,
        .string => |node| node.span,
        .bool => |node| node.span,
        .identifier => |node| node.span,
        .array => |node| node.span,
        .builder_array => |node| node.span,
        .callback => |node| node.span,
        .struct_literal => |node| node.span,
        .native_state => |node| node.span,
        .native_user_data => |node| node.span,
        .native_recover => |node| node.span,
        .ownership => |node| node.span,
        .unary => |node| node.span,
        .binary => |node| node.span,
        .conditional => |node| node.span,
        .quote => |node| node.span,
        .member => |node| node.span,
        .index => |node| node.span,
        .call => |node| node.span,
        .try_expr => |node| node.span,
    };
}

fn flattenDefaultCalleeName(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .identifier => |value| shared.qualifiedNameText(allocator, value.name),
        .member => |value| blk: {
            const left = try flattenDefaultCalleeName(allocator, value.object);
            break :blk try std.fmt.allocPrint(allocator, "{s}.{s}", .{ left, value.member });
        },
        else => allocator.dupe(u8, "<expr>"),
    };
}

fn qualifiedLeafText(name: []const u8) []const u8 {
    const index = std.mem.lastIndexOfScalar(u8, name, '.') orelse return name;
    return name[index + 1 ..];
}
