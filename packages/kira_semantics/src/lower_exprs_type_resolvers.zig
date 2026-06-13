const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");

pub fn resolveSyntaxExprType(ctx: *shared.Context, expr: *syntax.ast.Expr, span: source_pkg.Span) anyerror!model.ResolvedType {
    return switch (expr.*) {
        .integer => .{ .kind = .integer },
        .float => .{ .kind = .float },
        .string => .{ .kind = .string },
        .bool => .{ .kind = .boolean },
        .array => |node| try resolveSyntaxArrayLiteralType(ctx, node.elements, node.span),
        .callback => .{ .kind = .unknown },
        .struct_literal => |node| try shared.typeFromSyntax(ctx, .{ .named = node.type_name }),
        .native_state => |node| blk: {
            const value_ty = try resolveSyntaxExprType(ctx, node.value, span);
            break :blk if (value_ty.kind == .named and value_ty.name != null)
                .{ .kind = .native_state, .name = value_ty.name }
            else
                .{ .kind = .unknown };
        },
        .native_user_data => .{ .kind = .raw_ptr, .name = "RawPtr" },
        .native_recover => |node| blk: {
            const recovered_ty = try shared.typeFromSyntaxChecked(ctx, node.state_type.*);
            break :blk if (recovered_ty.kind == .named and recovered_ty.name != null)
                .{ .kind = .native_state_view, .name = recovered_ty.name }
            else
                .{ .kind = .unknown };
        },
        .ownership => |node| try resolveSyntaxExprType(ctx, node.operand, span),
        .unary => |node| blk: {
            const operand_ty = try resolveSyntaxExprType(ctx, node.operand, span);
            break :blk switch (node.op) {
                .negate => if (operand_ty.kind == .integer or operand_ty.kind == .float) operand_ty else model.ResolvedType{ .kind = .unknown },
                .not => .{ .kind = .boolean },
            };
        },
        .binary => |node| try resolveSyntaxBinaryExprType(ctx, node, span),
        .call => |node| {
            if (node.callee.* == .identifier) {
                const name = node.callee.identifier.name.segments[0].text;
                if (ctx.type_headers) |headers| {
                    if (headers.get(name)) |_| {
                        return .{ .kind = .named, .name = try ctx.allocator.dupe(u8, name) };
                    }
                }
                if (node.args.len > 0) {
                    for (node.args) |arg| {
                        if (arg.label != null) return .{ .kind = .named, .name = try ctx.allocator.dupe(u8, name) };
                    }
                }
            }
            return .{ .kind = .unknown };
        },
        .member => |node| blk: {
            if (node.object.* == .identifier and ctx.enum_headers != null) {
                const enum_name = node.object.identifier.name.segments[node.object.identifier.name.segments.len - 1].text;
                if (ctx.enum_headers.?.get(enum_name)) |enum_decl| {
                    for (enum_decl.variants) |variant_decl| {
                        if (std.mem.eql(u8, variant_decl.name, node.member) and variant_decl.payload_ty == null) {
                            break :blk .{ .kind = .enum_instance, .name = try ctx.allocator.dupe(u8, enum_name) };
                        }
                    }
                }
            }
            break :blk .{ .kind = .unknown };
        },
        .index => |node| blk: {
            const object_ty = try resolveSyntaxExprType(ctx, node.object, span);
            break :blk try resolveIndexElementType(ctx, object_ty, node.span);
        },
        else => .{ .kind = .unknown },
    };
}

fn resolveSyntaxBinaryExprType(ctx: *shared.Context, node: syntax.ast.BinaryExpr, span: source_pkg.Span) !model.ResolvedType {
    const lhs_ty = try resolveSyntaxExprType(ctx, node.lhs, span);
    const rhs_ty = try resolveSyntaxExprType(ctx, node.rhs, span);
    return switch (node.op) {
        .add, .subtract, .multiply, .divide, .modulo => blk: {
            if (lhs_ty.kind == .float or rhs_ty.kind == .float) break :blk .{ .kind = .float };
            if (lhs_ty.kind == .integer and rhs_ty.kind == .integer) break :blk .{ .kind = .integer };
            break :blk .{ .kind = .unknown };
        },
        .equal, .not_equal, .less, .less_equal, .greater, .greater_equal, .logical_and, .logical_or => .{ .kind = .boolean },
    };
}

pub fn resolveBinaryType(ctx: *shared.Context, op: syntax.ast.BinaryOp, lhs: *model.Expr, rhs: *model.Expr, span: source_pkg.Span) !model.ResolvedType {
    const lhs_ty = model.hir.exprType(lhs.*);
    const rhs_ty = model.hir.exprType(rhs.*);
    return switch (op) {
        .add, .subtract, .multiply, .divide, .modulo => blk: {
            if (lhs_ty.eql(rhs_ty) and (lhs_ty.kind == .integer or lhs_ty.kind == .float)) break :blk lhs_ty;
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM013",
                .title = "invalid binary operator types",
                .message = "Arithmetic operators require both operands to use the same numeric type.",
                .labels = &.{diagnostics.primaryLabel(span, "operands do not use compatible numeric types")},
                .help = "Make both operands integers or both operands floats, or add an explicit type declaration where coercion is allowed.",
            });
            return error.DiagnosticsEmitted;
        },
        .equal, .not_equal, .less, .less_equal, .greater, .greater_equal => blk: {
            if (lhs_ty.eql(rhs_ty)) break :blk .{ .kind = .boolean };
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM028",
                .title = "comparison requires matching types",
                .message = "Comparison operators require both sides to have the same type.",
                .labels = &.{diagnostics.primaryLabel(span, "comparison uses incompatible operand types")},
                .help = "Make both operands the same type before comparing them.",
            });
            return error.DiagnosticsEmitted;
        },
        .logical_and, .logical_or => blk: {
            if (lhs_ty.kind == .boolean and rhs_ty.kind == .boolean) break :blk .{ .kind = .boolean };
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM034",
                .title = "logical operators require booleans",
                .message = "Logical `&&` and `||` require both operands to be boolean values.",
                .labels = &.{diagnostics.primaryLabel(span, "logical operands are not both booleans")},
                .help = "Ensure both sides resolve to `Bool` before using a logical operator.",
            });
            return error.DiagnosticsEmitted;
        },
    };
}

pub fn resolveConditionalType(
    ctx: *shared.Context,
    then_ty: model.ResolvedType,
    else_ty: model.ResolvedType,
    span: source_pkg.Span,
) !model.ResolvedType {
    if (then_ty.eql(else_ty)) return then_ty;
    if (shared.canAssignInContext(ctx, then_ty, else_ty) and !shared.canAssignInContext(ctx, else_ty, then_ty)) return then_ty;
    if (shared.canAssignInContext(ctx, else_ty, then_ty) and !shared.canAssignInContext(ctx, then_ty, else_ty)) return else_ty;
    if (shared.commonClassType(ctx, then_ty, else_ty)) |common_ty| return common_ty;

    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM033",
        .title = "conditional branches require compatible types",
        .message = "Both branches of a conditional expression must resolve to the same type or a compatible coercion target.",
        .labels = &.{diagnostics.primaryLabel(span, "conditional branches do not agree on a result type")},
        .help = "Make both branches the same type, or use an explicit coercion target that both branches can satisfy.",
    });
    return error.DiagnosticsEmitted;
}

pub fn resolveSyntaxArrayLiteralType(ctx: *shared.Context, elements: []const *syntax.ast.Expr, span: source_pkg.Span) anyerror!model.ResolvedType {
    if (elements.len == 0) return .{ .kind = .array };

    var element_ty = try resolveSyntaxExprType(ctx, elements[0], span);
    for (elements[1..]) |element| {
        const next_ty = try resolveSyntaxExprType(ctx, element, span);
        if (element_ty.eql(next_ty)) continue;
        if (shared.commonClassType(ctx, element_ty, next_ty)) |common_ty| {
            element_ty = common_ty;
            continue;
        }
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM050",
            .title = "array literal requires a consistent element type",
            .message = "All elements in an executable array literal must resolve to the same type.",
            .labels = &.{diagnostics.primaryLabel(span, "array literal mixes incompatible element types")},
            .help = "Make every array element the same type, or split the values into separate arrays.",
        });
        return error.DiagnosticsEmitted;
    }

    return .{
        .kind = .array,
        .name = try shared.typeTextFromResolved(ctx.allocator, element_ty),
    };
}

pub fn resolveArrayElementType(ctx: *shared.Context, array_ty: model.ResolvedType, span: source_pkg.Span) !model.ResolvedType {
    return resolveElementType(ctx, array_ty, span, .for_loop);
}

pub fn resolveIndexElementType(ctx: *shared.Context, array_ty: model.ResolvedType, span: source_pkg.Span) !model.ResolvedType {
    return resolveElementType(ctx, array_ty, span, .index);
}

const ElementUse = enum {
    for_loop,
    index,
};

fn resolveElementType(ctx: *shared.Context, array_ty: model.ResolvedType, span: source_pkg.Span, use: ElementUse) !model.ResolvedType {
    if (array_ty.kind == .named) {
        if (shared.namedTypeInfo(ctx, array_ty)) |info| {
            switch (info) {
                .array => |value| return value.element,
                .alias => |value| return resolveElementType(ctx, value.target, span, use),
                else => {},
            }
        }
    }

    if (array_ty.kind != .array) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM051",
            .title = switch (use) {
                .for_loop => "for loop requires an array iterator",
                .index => "indexing requires an array value",
            },
            .message = switch (use) {
                .for_loop => "Executable `for` loops currently iterate over array values.",
                .index => "Index expressions can only select elements from array values.",
            },
            .labels = &.{diagnostics.primaryLabel(span, switch (use) {
                .for_loop => "iterator is not an array value",
                .index => "indexed value is not an array",
            })},
            .help = switch (use) {
                .for_loop => "Use an array value in the `for ... in ...` position.",
                .index => "Use `value[index]` only when `value` has an array or fixed-array FFI type.",
            },
        });
        return error.DiagnosticsEmitted;
    }
    if (array_ty.name == null) return .{ .kind = .unknown };
    return try shared.resolvedTypeFromText(array_ty.name.?);
}
