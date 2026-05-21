const std = @import("std");
const model = @import("kira_semantics_model");

pub fn collectBoxedLocals(allocator: std.mem.Allocator, local_count: usize, body: []const model.Statement) ![]bool {
    const boxed = try allocator.alloc(bool, local_count);
    @memset(boxed, false);
    collectBoxedFromStatements(boxed, body);
    return boxed;
}

fn collectBoxedFromStatements(boxed: []bool, body: []const model.Statement) void {
    for (body) |statement| switch (statement) {
        .let_stmt => |node| if (node.value) |value| collectBoxedFromExpr(boxed, value),
        .assign_stmt => |node| {
            collectBoxedFromExpr(boxed, node.target);
            collectBoxedFromExpr(boxed, node.value);
        },
        .expr_stmt => |node| collectBoxedFromExpr(boxed, node.expr),
        .if_stmt => |node| {
            collectBoxedFromExpr(boxed, node.condition);
            collectBoxedFromStatements(boxed, node.then_body);
            if (node.else_body) |else_body| collectBoxedFromStatements(boxed, else_body);
        },
        .for_stmt => |node| {
            collectBoxedFromExpr(boxed, node.iterator);
            collectBoxedFromStatements(boxed, node.body);
        },
        .while_stmt => |node| {
            collectBoxedFromExpr(boxed, node.condition);
            collectBoxedFromStatements(boxed, node.body);
        },
        .switch_stmt => |node| {
            collectBoxedFromExpr(boxed, node.subject);
            for (node.cases) |case| {
                collectBoxedFromExpr(boxed, case.pattern);
                collectBoxedFromStatements(boxed, case.body);
            }
            if (node.default_body) |default_body| collectBoxedFromStatements(boxed, default_body);
        },
        .match_stmt => |node| {
            collectBoxedFromExpr(boxed, node.subject);
            for (node.arms) |arm| {
                if (arm.guard) |guard| collectBoxedFromExpr(boxed, guard);
                collectBoxedFromStatements(boxed, arm.body);
            }
        },
        .return_stmt => |node| if (node.value) |value| collectBoxedFromExpr(boxed, value),
        .break_stmt, .continue_stmt => {},
    };
}

fn collectBoxedFromExpr(boxed: []bool, expr: *const model.Expr) void {
    switch (expr.*) {
        .callback => |node| {
            for (node.captures) |capture| {
                if (capture.by_ref and capture.source_local_id < boxed.len) boxed[capture.source_local_id] = true;
            }
        },
        .binary => |node| {
            collectBoxedFromExpr(boxed, node.lhs);
            collectBoxedFromExpr(boxed, node.rhs);
        },
        .unary => |node| collectBoxedFromExpr(boxed, node.operand),
        .conditional => |node| {
            collectBoxedFromExpr(boxed, node.condition);
            collectBoxedFromExpr(boxed, node.then_expr);
            collectBoxedFromExpr(boxed, node.else_expr);
        },
        .construct => |node| for (node.fields) |field| collectBoxedFromExpr(boxed, field.value),
        .construct_enum_variant => |node| if (node.payload) |payload| collectBoxedFromExpr(boxed, payload),
        .call => |node| for (node.args) |arg| collectBoxedFromExpr(boxed, arg),
        .call_value => |node| {
            collectBoxedFromExpr(boxed, node.callee);
            for (node.args) |arg| collectBoxedFromExpr(boxed, arg);
        },
        .virtual_call => |node| {
            collectBoxedFromExpr(boxed, node.receiver);
            for (node.args) |arg| collectBoxedFromExpr(boxed, arg);
        },
        .array => |node| for (node.elements) |element| collectBoxedFromExpr(boxed, element),
        .index => |node| {
            collectBoxedFromExpr(boxed, node.object);
            collectBoxedFromExpr(boxed, node.index);
        },
        .array_len => |node| collectBoxedFromExpr(boxed, node.object),
        .string_len => |node| collectBoxedFromExpr(boxed, node.object),
        .field => |node| collectBoxedFromExpr(boxed, node.object),
        .parent_view => |node| collectBoxedFromExpr(boxed, node.object),
        .native_state => |node| collectBoxedFromExpr(boxed, node.value),
        .native_user_data => |node| collectBoxedFromExpr(boxed, node.state),
        .native_recover => |node| collectBoxedFromExpr(boxed, node.value),
        else => {},
    }
}
