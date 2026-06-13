const std = @import("std");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");

pub fn isCallableValueExpr(expr: *syntax.ast.Expr, scope: *model.Scope) bool {
    return switch (expr.*) {
        .identifier => |node| scope.get(node.name.segments[0].text) != null,
        .member, .index => true,
        else => false,
    };
}

pub fn flattenCalleeName(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .identifier => |node| allocator.dupe(u8, node.name.segments[0].text),
        .member => flattenMemberExprPath(allocator, expr),
        else => allocator.dupe(u8, "<expr>"),
    };
}

pub fn flattenMemberExpr(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) !struct { root: []const u8, path: []const u8 } {
    const path = try flattenMemberExprPath(allocator, expr);
    const root_end = std.mem.indexOfScalar(u8, path, '.') orelse path.len;
    return .{
        .root = try allocator.dupe(u8, path[0..root_end]),
        .path = path,
    };
}

pub fn flattenMemberExprPath(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .member => |node| blk: {
            const left = try flattenMemberExprPath(allocator, node.object);
            break :blk try std.fmt.allocPrint(allocator, "{s}.{s}", .{ left, node.member });
        },
        .identifier => |node| allocator.dupe(u8, node.name.segments[0].text),
        else => allocator.dupe(u8, "<expr>"),
    };
}

pub fn qualifiedNameRootMatches(name: syntax.ast.QualifiedName, expected_name: []const u8) bool {
    return name.segments.len > 1 and std.mem.eql(u8, name.segments[0].text, expected_name);
}

pub fn enumMemberMatches(node: syntax.ast.MemberExpr, enum_name: []const u8) bool {
    return switch (node.object.*) {
        .identifier => |value| std.mem.eql(u8, value.name.segments[value.name.segments.len - 1].text, enum_name),
        else => false,
    };
}
