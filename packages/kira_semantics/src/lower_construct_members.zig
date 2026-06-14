const std = @import("std");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");

// The SwiftUI-style direct construct surface lowers a construct's top-level `@Required` and
// default members (`lower_construct_members`) into three model collections:
//
//   * `@Required let name: T`        -> a required field the concrete declaration must provide.
//   * `@Required function f(...) -> T` -> a required function (reuses the existing satisfaction
//                                          machinery in `lower_construct_requirements`).
//   * a bodied, non-required member  -> a default member (e.g. `let node: Node { body.node }`)
//                                       whose body may read required fields. Overriding every
//                                       default member that reads a required field discharges
//                                       that requirement (the terminal-`node` rule).
pub const DirectMembers = struct {
    required_fields: []model.RequiredField,
    required_functions: []model.RequiredFunction,
    default_members: []model.ConstructDefaultMember,
};

pub fn isAnnotation(annotation: syntax.ast.Annotation, name: []const u8) bool {
    return annotation.name.segments.len == 1 and std.mem.eql(u8, annotation.name.segments[0].text, name);
}

fn hasAnnotation(annotations: []const syntax.ast.Annotation, name: []const u8) bool {
    for (annotations) |annotation| {
        if (isAnnotation(annotation, name)) return true;
    }
    return false;
}

pub fn hasContentAnnotation(annotations: []const syntax.ast.Annotation) bool {
    return hasAnnotation(annotations, "Content");
}

pub fn collectConstructDirectMembers(
    ctx: *shared.Context,
    construct_decl: syntax.ast.ConstructDecl,
) !DirectMembers {
    var required_fields = std.array_list.Managed(model.RequiredField).init(ctx.allocator);
    var required_functions = std.array_list.Managed(model.RequiredFunction).init(ctx.allocator);
    var default_members = std.array_list.Managed(model.ConstructDefaultMember).init(ctx.allocator);

    // Pass 1: the set of required field names, used to resolve what a default member reads.
    var required_field_names = std.StringHashMapUnmanaged(void){};
    defer required_field_names.deinit(ctx.allocator);
    for (construct_decl.members) |member| {
        if (member == .field_decl and hasAnnotation(member.field_decl.annotations, "Required")) {
            try required_field_names.put(ctx.allocator, member.field_decl.name, {});
        }
    }

    for (construct_decl.members) |member| {
        switch (member) {
            .field_decl => |field| {
                if (hasAnnotation(field.annotations, "Required")) {
                    try required_fields.append(.{
                        .name = try ctx.allocator.dupe(u8, field.name),
                        .type_text = if (field.type_expr) |type_expr| try shared.typeTextFromSyntax(ctx, type_expr.*) else "",
                        .span = field.span,
                    });
                } else if (field.body != null) {
                    try default_members.append(.{
                        .name = try ctx.allocator.dupe(u8, field.name),
                        .is_field = true,
                        .references = try collectBlockReferences(ctx, field.body.?, &required_field_names),
                        .span = field.span,
                    });
                }
            },
            .function_decl => |function| {
                if (hasAnnotation(function.annotations, "Required")) {
                    try required_functions.append(try requiredFunctionSig(ctx, function));
                } else if (function.body != null) {
                    try default_members.append(.{
                        .name = try ctx.allocator.dupe(u8, function.name),
                        .is_field = false,
                        .references = try collectBlockReferences(ctx, function.body.?, &required_field_names),
                        .span = function.span,
                    });
                }
            },
            else => {},
        }
    }

    return .{
        .required_fields = try required_fields.toOwnedSlice(),
        .required_functions = try required_functions.toOwnedSlice(),
        .default_members = try default_members.toOwnedSlice(),
    };
}

fn requiredFunctionSig(ctx: *shared.Context, function: syntax.ast.FunctionDecl) !model.RequiredFunction {
    var param_types = std.array_list.Managed([]const u8).init(ctx.allocator);
    for (function.params) |param| {
        const text = if (param.type_expr) |type_expr| try shared.typeTextFromSyntax(ctx, type_expr.*) else "";
        try param_types.append(text);
    }
    const return_type = if (function.return_type) |type_expr|
        try shared.typeTextFromSyntax(ctx, type_expr.*)
    else
        "Void";
    return .{
        .name = try ctx.allocator.dupe(u8, function.name),
        .param_types = try param_types.toOwnedSlice(),
        .return_type = return_type,
        .span = function.span,
    };
}

// Collect the required-field names a default member's body reads, so an override of that member
// can discharge those requirements. Only identifiers that name a required field are recorded.
fn collectBlockReferences(
    ctx: *shared.Context,
    block: syntax.ast.Block,
    names: *const std.StringHashMapUnmanaged(void),
) ![]const []const u8 {
    var refs = std.StringArrayHashMapUnmanaged(void){};
    defer refs.deinit(ctx.allocator);
    for (block.statements) |statement| try walkStatement(ctx, statement, names, &refs);

    var out = std.array_list.Managed([]const u8).init(ctx.allocator);
    for (refs.keys()) |key| try out.append(key);
    return out.toOwnedSlice();
}

fn walkStatement(
    ctx: *shared.Context,
    statement: syntax.ast.Statement,
    names: *const std.StringHashMapUnmanaged(void),
    refs: *std.StringArrayHashMapUnmanaged(void),
) anyerror!void {
    switch (statement) {
        .let_stmt => |stmt| {
            if (stmt.value) |value| try walkExpr(ctx, value, names, refs);
        },
        .assign_stmt => |stmt| {
            try walkExpr(ctx, stmt.target, names, refs);
            try walkExpr(ctx, stmt.value, names, refs);
        },
        .expr_stmt => |stmt| try walkExpr(ctx, stmt.expr, names, refs),
        .return_stmt => |stmt| {
            if (stmt.value) |value| try walkExpr(ctx, value, names, refs);
        },
        .if_stmt => |stmt| {
            try walkExpr(ctx, stmt.condition, names, refs);
            for (stmt.then_block.statements) |inner| try walkStatement(ctx, inner, names, refs);
            if (stmt.else_block) |else_block| {
                for (else_block.statements) |inner| try walkStatement(ctx, inner, names, refs);
            }
        },
        .for_stmt => |stmt| {
            try walkExpr(ctx, stmt.iterator, names, refs);
            for (stmt.body.statements) |inner| try walkStatement(ctx, inner, names, refs);
        },
        .while_stmt => |stmt| {
            try walkExpr(ctx, stmt.condition, names, refs);
            for (stmt.body.statements) |inner| try walkStatement(ctx, inner, names, refs);
        },
        else => {},
    }
}

fn walkExpr(
    ctx: *shared.Context,
    expr: *const syntax.ast.Expr,
    names: *const std.StringHashMapUnmanaged(void),
    refs: *std.StringArrayHashMapUnmanaged(void),
) anyerror!void {
    switch (expr.*) {
        .identifier => |ident| {
            if (ident.name.segments.len >= 1) {
                const root = ident.name.segments[0].text;
                if (names.contains(root)) try refs.put(ctx.allocator, root, {});
            }
        },
        .member => |member| try walkExpr(ctx, member.object, names, refs),
        .index => |index| {
            try walkExpr(ctx, index.object, names, refs);
            try walkExpr(ctx, index.index, names, refs);
        },
        .unary => |unary| try walkExpr(ctx, unary.operand, names, refs),
        .ownership => |ownership| try walkExpr(ctx, ownership.operand, names, refs),
        .try_expr => |try_expr| try walkExpr(ctx, try_expr.operand, names, refs),
        .binary => |binary| {
            try walkExpr(ctx, binary.lhs, names, refs);
            try walkExpr(ctx, binary.rhs, names, refs);
        },
        .conditional => |conditional| {
            try walkExpr(ctx, conditional.condition, names, refs);
            try walkExpr(ctx, conditional.then_expr, names, refs);
            try walkExpr(ctx, conditional.else_expr, names, refs);
        },
        .array => |array| {
            for (array.elements) |element| try walkExpr(ctx, element, names, refs);
        },
        .call => |call| {
            try walkExpr(ctx, call.callee, names, refs);
            for (call.args) |arg| try walkExpr(ctx, arg.value, names, refs);
        },
        .struct_literal => |literal| {
            for (literal.fields) |field| try walkExpr(ctx, field.value, names, refs);
        },
        else => {},
    }
}
