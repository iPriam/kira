const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const parent = @import("lower_exprs.zig");
const types = @import("lower_exprs_types.zig");
const lowerExpr = parent.lowerExpr;
const lowerImplicitSelfFieldExpr = parent.lowerImplicitSelfFieldExpr;
const exprSpan = types.exprSpan;

pub fn lowerAssignmentTarget(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    return switch (expr.*) {
        .identifier => |node| blk: {
            const name = node.name.segments[0].text;
            if (try shared.resolveLocalOrCapture(ctx, scope.*, name, node.span)) |resolution| {
                const binding = resolution.binding;
                const target = try ctx.allocator.create(model.Expr);
                target.* = .{ .local = .{
                    .local_id = binding.id,
                    .name = try ctx.allocator.dupe(u8, name),
                    .ty = binding.ty,
                    .storage = binding.storage,
                    .span = node.span,
                } };
                if (binding.storage == .immutable) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM050",
                        .title = "cannot assign to immutable binding",
                        .message = "This assignment targets a `let` binding, which is immutable.",
                        .labels = &.{
                            diagnostics.primaryLabel(exprSpan(expr.*), "immutable binding cannot appear on the left side of '='"),
                        },
                        .help = "Use `var` for mutable bindings, or assign to a mutable field instead.",
                    });
                    break :blk error.DiagnosticsEmitted;
                }
                break :blk target;
            }

            if (try lowerImplicitSelfFieldExpr(ctx, scope, name, node.span)) |field_expr| {
                const target = try ctx.allocator.create(model.Expr);
                target.* = field_expr;
                if (field_expr.field.storage == .immutable) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM050",
                        .title = "cannot assign to immutable binding",
                        .message = "This assignment targets a `let` field, which is immutable.",
                        .labels = &.{
                            diagnostics.primaryLabel(exprSpan(expr.*), "immutable field cannot appear on the left side of '='"),
                        },
                        .help = "Declare the field with `var` if mutation is intended.",
                    });
                    break :blk error.DiagnosticsEmitted;
                }
                break :blk target;
            }

            {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM012",
                    .title = "unknown local name",
                    .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not find a local binding named '{s}'.", .{name}),
                    .labels = &.{diagnostics.primaryLabel(exprSpan(expr.*), "unknown local name")},
                    .help = "Declare the value before assigning to it.",
                });
                return error.DiagnosticsEmitted;
            }
        },
        .member, .index => blk: {
            const target = try lowerExpr(ctx, expr, imports, scope, function_headers);
            switch (target.*) {
                .field => |node| {
                    if (node.storage == .immutable) {
                        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                            .severity = .@"error",
                            .code = "KSEM050",
                            .title = "cannot assign to immutable binding",
                            .message = "This assignment targets a `let` field, which is immutable.",
                            .labels = &.{
                                diagnostics.primaryLabel(exprSpan(expr.*), "immutable field cannot appear on the left side of '='"),
                            },
                            .help = "Declare the field with `var` if mutation is intended.",
                        });
                        break :blk error.DiagnosticsEmitted;
                    }
                },
                .index => {},
                else => {},
            }
            break :blk target;
        },
        else => blk: {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM046",
                .title = "invalid assignment target",
                .message = "Assignments can only target locals or fields.",
                .labels = &.{
                    diagnostics.primaryLabel(exprSpan(expr.*), "this expression cannot appear on the left side of '='"),
                },
                .help = "Assign to a local name or a field reference.",
            });
            break :blk error.DiagnosticsEmitted;
        },
    };
}
