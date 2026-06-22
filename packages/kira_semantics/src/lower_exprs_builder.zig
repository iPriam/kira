const std = @import("std");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const parent = @import("lower_exprs.zig");
const scope_flow = @import("lower_exprs_scope_flow.zig");

const lowerExpr = parent.lowerExpr;
const resolveArrayElementType = parent.resolveArrayElementType;

// Unwrap a content expression's trailing modifier chain down to the base widget construction.
// A modifier call is a `.call` whose callee is a `.member` access (`Base(..).modifier(..)`);
// the base widget is reached once the callee is no longer a member access (e.g. `Text(..)`).
pub fn stripContentModifiers(expr: *syntax.ast.Expr) *syntax.ast.Expr {
    var current = expr;
    while (current.* == .call and current.call.callee.* == .member) {
        current = current.call.callee.member.object;
    }
    return current;
}

pub fn lowerBuilderBlock(
    ctx: *shared.Context,
    builder: syntax.ast.BuilderBlock,
    imports: []const model.Import,
    scope: ?*model.Scope,
) anyerror!model.BuilderBlock {
    var empty_scope = model.Scope{};
    defer empty_scope.deinit(ctx.allocator);
    const active_scope = if (scope) |actual| actual else &empty_scope;

    var items = std.array_list.Managed(model.BuilderItem).init(ctx.allocator);
    for (builder.items) |item| {
        switch (item) {
            .expr => |value| try items.append(.{
                .expr = .{
                    // Content items may be modifier chains (`Text(..).font(..)`). Modifiers are
                    // `extend` declarations that are validated but not yet lowered to runtime, so the
                    // runtime content array carries the base widget, matching ordinary widget content.
                    .expr = try lowerExpr(ctx, stripContentModifiers(value.expr), imports, active_scope, null),
                    .span = value.span,
                },
            }),
            .if_item => |value| try items.append(.{ .if_item = .{
                .condition = try lowerExpr(ctx, value.condition, imports, active_scope, null),
                .then_block = try lowerBuilderBlock(ctx, value.then_block, imports, active_scope),
                .else_block = if (value.else_block) |else_block| try lowerBuilderBlock(ctx, else_block, imports, active_scope) else null,
                .span = value.span,
            } }),
            .for_item => |value| blk: {
                const iterator = try lowerExpr(ctx, value.iterator, imports, active_scope, null);
                var binding_local_id: u32 = 0;
                var binding_ty: model.ResolvedType = .{ .kind = .unknown };
                var loop_scope_ptr = active_scope;
                var loop_scope_storage = model.Scope{};
                var has_loop_scope = false;
                defer if (has_loop_scope) loop_scope_storage.deinit(ctx.allocator);

                if (ctx.active_locals != null and ctx.active_next_local_id != null) {
                    binding_ty = try resolveArrayElementType(ctx, model.hir.exprType(iterator.*), value.span);
                    binding_local_id = ctx.active_next_local_id.?.*;
                    ctx.active_next_local_id.?.* += 1;
                    try ctx.active_locals.?.append(.{
                        .id = binding_local_id,
                        .name = try ctx.allocator.dupe(u8, value.binding_name),
                        .ty = binding_ty,
                        .ownership = .owned,
                        .span = value.span,
                    });
                    loop_scope_storage = try scope_flow.cloneScope(ctx.allocator, active_scope.*);
                    has_loop_scope = true;
                    try loop_scope_storage.put(ctx.allocator, value.binding_name, .{
                        .id = binding_local_id,
                        .ty = binding_ty,
                        .storage = .immutable,
                        .initialized = true,
                        .decl_span = value.span,
                    });
                    loop_scope_ptr = &loop_scope_storage;
                }

                try items.append(.{ .for_item = .{
                    .binding_name = try ctx.allocator.dupe(u8, value.binding_name),
                    .binding_local_id = binding_local_id,
                    .binding_ty = binding_ty,
                    .iterator = iterator,
                    .body = try lowerBuilderBlock(ctx, value.body, imports, loop_scope_ptr),
                    .span = value.span,
                } });
                break :blk;
            },
            .switch_item => |value| blk: {
                var cases = std.array_list.Managed(model.BuilderSwitchCase).init(ctx.allocator);
                for (value.cases) |case_node| {
                    try cases.append(.{
                        .pattern = try lowerExpr(ctx, case_node.pattern, imports, active_scope, null),
                        .body = try lowerBuilderBlock(ctx, case_node.body, imports, active_scope),
                        .span = case_node.span,
                    });
                }
                try items.append(.{ .switch_item = .{
                    .subject = try lowerExpr(ctx, value.subject, imports, active_scope, null),
                    .cases = try cases.toOwnedSlice(),
                    .default_block = if (value.default_block) |default_block| try lowerBuilderBlock(ctx, default_block, imports, active_scope) else null,
                    .span = value.span,
                } });
                break :blk;
            },
        }
    }

    return .{
        .items = try items.toOwnedSlice(),
        .span = builder.span,
    };
}
