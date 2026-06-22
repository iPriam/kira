const std = @import("std");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");

const ComptimeValue = union(enum) {
    integer: i64,
    float: f64,
    boolean: bool,
    string: []const u8,
};

const ComptimeBinding = struct {
    name: []const u8,
    value: ComptimeValue,
};

pub fn lowerComptimeCall(
    ctx: *shared.Context,
    node: syntax.ast.CallExpr,
    header: shared.FunctionHeader,
) !?model.Expr {
    const decl = header.comptime_decl orelse return null;
    if (node.trailing_builder != null or node.trailing_callback != null) return null;
    if (node.args.len != decl.params.len) return null;
    const body = decl.body orelse return null;
    if (body.statements.len != 1 or body.statements[0] != .return_stmt) return null;
    const return_expr = body.statements[0].return_stmt.value orelse return null;

    const bindings = try ctx.allocator.alloc(ComptimeBinding, decl.params.len);
    for (decl.params, 0..) |param, index| {
        bindings[index] = .{
            .name = param.name,
            .value = try evalComptimeExpr(ctx, node.args[index].value, bindings[0..index]) orelse return null,
        };
    }
    const value = try evalComptimeExpr(ctx, return_expr, bindings) orelse return null;
    return try comptimeValueToExpr(ctx, value, header.return_type, node.span);
}

fn evalComptimeExpr(ctx: *shared.Context, expr: *const syntax.ast.Expr, bindings: []const ComptimeBinding) !?ComptimeValue {
    return switch (expr.*) {
        .integer => |value| .{ .integer = value.value },
        .float => |value| .{ .float = value.value },
        .string => |value| .{ .string = try ctx.allocator.dupe(u8, value.value) },
        .bool => |value| .{ .boolean = value.value },
        .identifier => |value| blk: {
            if (value.name.segments.len != 1) break :blk null;
            const name = value.name.segments[0].text;
            for (bindings) |binding| {
                if (std.mem.eql(u8, binding.name, name)) break :blk binding.value;
            }
            break :blk null;
        },
        .unary => |value| blk: {
            const operand = try evalComptimeExpr(ctx, value.operand, bindings) orelse break :blk null;
            break :blk switch (value.op) {
                .negate => switch (operand) {
                    .integer => |int| ComptimeValue{ .integer = -int },
                    .float => |float| ComptimeValue{ .float = -float },
                    else => null,
                },
                .not => switch (operand) {
                    .boolean => |boolean| ComptimeValue{ .boolean = !boolean },
                    else => null,
                },
            };
        },
        .binary => |value| blk: {
            const lhs = try evalComptimeExpr(ctx, value.lhs, bindings) orelse break :blk null;
            const rhs = try evalComptimeExpr(ctx, value.rhs, bindings) orelse break :blk null;
            break :blk evalComptimeBinary(lhs, value.op, rhs);
        },
        else => null,
    };
}

fn evalComptimeBinary(lhs: ComptimeValue, op: syntax.ast.BinaryOp, rhs: ComptimeValue) ?ComptimeValue {
    return switch (op) {
        .add => switch (lhs) {
            .integer => |l| switch (rhs) {
                .integer => |r| .{ .integer = l + r },
                else => null,
            },
            .float => |l| switch (rhs) {
                .float => |r| .{ .float = l + r },
                else => null,
            },
            else => null,
        },
        .subtract => switch (lhs) {
            .integer => |l| switch (rhs) {
                .integer => |r| .{ .integer = l - r },
                else => null,
            },
            .float => |l| switch (rhs) {
                .float => |r| .{ .float = l - r },
                else => null,
            },
            else => null,
        },
        .multiply => switch (lhs) {
            .integer => |l| switch (rhs) {
                .integer => |r| .{ .integer = l * r },
                else => null,
            },
            .float => |l| switch (rhs) {
                .float => |r| .{ .float = l * r },
                else => null,
            },
            else => null,
        },
        .divide => switch (lhs) {
            .integer => |l| switch (rhs) {
                .integer => |r| if (r == 0) null else .{ .integer = @divTrunc(l, r) },
                else => null,
            },
            .float => |l| switch (rhs) {
                .float => |r| .{ .float = l / r },
                else => null,
            },
            else => null,
        },
        .modulo => switch (lhs) {
            .integer => |l| switch (rhs) {
                .integer => |r| if (r == 0) null else .{ .integer = @mod(l, r) },
                else => null,
            },
            else => null,
        },
        .equal => .{ .boolean = comptimeValuesEqual(lhs, rhs) },
        .not_equal => .{ .boolean = !comptimeValuesEqual(lhs, rhs) },
        .less, .less_equal, .greater, .greater_equal => evalComptimeOrdering(lhs, op, rhs),
        .logical_and => switch (lhs) {
            .boolean => |l| switch (rhs) {
                .boolean => |r| .{ .boolean = l and r },
                else => null,
            },
            else => null,
        },
        .logical_or => switch (lhs) {
            .boolean => |l| switch (rhs) {
                .boolean => |r| .{ .boolean = l or r },
                else => null,
            },
            else => null,
        },
    };
}

fn evalComptimeOrdering(lhs: ComptimeValue, op: syntax.ast.BinaryOp, rhs: ComptimeValue) ?ComptimeValue {
    return switch (lhs) {
        .integer => |l| switch (rhs) {
            .integer => |r| .{ .boolean = switch (op) {
                .less => l < r,
                .less_equal => l <= r,
                .greater => l > r,
                .greater_equal => l >= r,
                else => unreachable,
            } },
            else => null,
        },
        .float => |l| switch (rhs) {
            .float => |r| .{ .boolean = switch (op) {
                .less => l < r,
                .less_equal => l <= r,
                .greater => l > r,
                .greater_equal => l >= r,
                else => unreachable,
            } },
            else => null,
        },
        else => null,
    };
}

fn comptimeValuesEqual(lhs: ComptimeValue, rhs: ComptimeValue) bool {
    return switch (lhs) {
        .integer => |l| switch (rhs) {
            .integer => |r| l == r,
            else => false,
        },
        .float => |l| switch (rhs) {
            .float => |r| l == r,
            else => false,
        },
        .boolean => |l| switch (rhs) {
            .boolean => |r| l == r,
            else => false,
        },
        .string => |l| switch (rhs) {
            .string => |r| std.mem.eql(u8, l, r),
            else => false,
        },
    };
}

fn comptimeValueToExpr(
    ctx: *shared.Context,
    value: ComptimeValue,
    declared_type: model.ResolvedType,
    span: source_pkg.Span,
) !model.Expr {
    return switch (value) {
        .integer => |int| .{ .integer = .{ .value = int, .ty = if (declared_type.kind == .unknown) .{ .kind = .integer } else declared_type, .span = span } },
        .float => |float| .{ .float = .{ .value = float, .ty = if (declared_type.kind == .unknown) .{ .kind = .float } else declared_type, .span = span } },
        .boolean => |boolean| .{ .boolean = .{ .value = boolean, .ty = if (declared_type.kind == .unknown) .{ .kind = .boolean } else declared_type, .span = span } },
        .string => |string| .{ .string = .{ .value = try ctx.allocator.dupe(u8, string), .ty = if (declared_type.kind == .unknown) .{ .kind = .string } else declared_type, .span = span } },
    };
}
