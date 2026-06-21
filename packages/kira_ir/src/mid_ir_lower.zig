const std = @import("std");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");
const source_pkg = @import("kira_source");
const lower_hir = @import("lower_from_hir.zig");
const value_lowering = @import("mid_ir_lower_value.zig");

// Value/place/expression lowering lives in `mid_ir_lower_value.zig`; alias the
// entry points it exposes so the statement lowering can call them unqualified.
const lowerValue = value_lowering.lowerValue;
const lowerPlaceOrOpaque = value_lowering.lowerPlaceOrOpaque;

pub const LowerOptions = struct {
    include_tests: bool = false,
};

pub const Context = struct {
    allocator: std.mem.Allocator,
    program: model.Program,
    next_callback_id: u32,
    next_temp_id: u32 = 0,
    functions: std.array_list.Managed(mid.Function),
};

pub fn lowerProgram(allocator: std.mem.Allocator, program: model.Program, options: LowerOptions) !mid.Program {
    var reachable = std.AutoHashMapUnmanaged(u32, void){};
    defer reachable.deinit(allocator);
    try lower_hir.markReachableFunction(allocator, program, &reachable, program.functions[program.entry_index].id);
    if (options.include_tests) {
        for (program.tests) |test_case| {
            if (lower_hir.functionIdByName(program, test_case.test_function)) |id| {
                try lower_hir.markReachableFunction(allocator, program, &reachable, id);
            }
            if (lower_hir.functionIdByName(program, test_case.expect_function)) |id| {
                try lower_hir.markReachableFunction(allocator, program, &reachable, id);
            }
        }
    }

    var ctx = Context{
        .allocator = allocator,
        .program = program,
        .next_callback_id = nextCallbackId(program),
        .functions = std.array_list.Managed(mid.Function).init(allocator),
    };
    defer ctx.functions.deinit();

    var entry_index: ?usize = null;
    for (program.functions) |function_decl| {
        if (!reachable.contains(function_decl.id)) continue;
        if (function_decl.id == program.functions[program.entry_index].id) {
            entry_index = ctx.functions.items.len;
        }
        try ctx.functions.append(try lowerFunction(&ctx, function_decl));
    }

    return .{
        .source_program = program,
        .functions = try ctx.functions.toOwnedSlice(),
        .entry_index = entry_index orelse 0,
    };
}

fn nextCallbackId(program: model.Program) u32 {
    var next_id: u32 = 0;
    for (program.functions) |function_decl| next_id = @max(next_id, function_decl.id + 1);
    return next_id;
}

fn lowerFunction(ctx: *Context, function_decl: model.Function) !mid.Function {
    return .{
        .id = function_decl.id,
        .name = function_decl.name,
        .execution = function_decl.execution,
        .is_extern = function_decl.is_extern,
        .params = try lowerParams(ctx.allocator, function_decl.params),
        .locals = try lowerLocals(ctx.allocator, function_decl.params, function_decl.locals),
        .return_type = function_decl.return_type,
        .return_ownership = function_decl.return_ownership,
        .body = try lowerBlock(ctx, function_decl.body),
        .span = function_decl.span,
    };
}

pub fn lowerParams(allocator: std.mem.Allocator, params: []const model.Parameter) ![]const mid.Parameter {
    const lowered = try allocator.alloc(mid.Parameter, params.len);
    for (params, 0..) |param, index| {
        lowered[index] = .{
            .id = param.id,
            .name = param.name,
            .ty = param.ty,
            .ownership = param.ownership,
            .span = param.span,
        };
    }
    return lowered;
}

pub fn lowerLocals(
    allocator: std.mem.Allocator,
    params: []const model.Parameter,
    locals: []const model.LocalSymbol,
) ![]const mid.Local {
    var param_ids = std.AutoHashMapUnmanaged(u32, void){};
    defer param_ids.deinit(allocator);
    for (params) |param| try param_ids.put(allocator, param.id, {});

    const lowered = try allocator.alloc(mid.Local, locals.len);
    for (locals, 0..) |local, index| {
        lowered[index] = .{
            .id = local.id,
            .name = local.name,
            .ty = local.ty,
            .ownership = local.ownership,
            .is_parameter = param_ids.contains(local.id),
            .is_capture = local.is_capture,
            .span = local.span,
        };
    }
    return lowered;
}

pub fn lowerBlock(ctx: *Context, statements: []const model.Statement) anyerror!mid.Block {
    var lowered = std.array_list.Managed(mid.Statement).init(ctx.allocator);
    defer lowered.deinit();
    for (statements) |statement| {
        try lowered.append(try lowerStatement(ctx, statement));
    }
    return .{
        .statements = try lowered.toOwnedSlice(),
        .span = blockSpan(statements),
    };
}

fn lowerStatement(ctx: *Context, statement: model.Statement) anyerror!mid.Statement {
    return switch (statement) {
        .let_stmt => |node| .{ .let_stmt = .{
            .local = lookupLocal(ctx.program, node.local_id),
            .value = if (node.value) |value| try lowerValue(ctx, value) else null,
            .is_reborrow = node.is_reborrow,
            .span = node.span,
        } },
        .assign_stmt => |node| .{ .assign_stmt = .{
            .target = try lowerPlaceOrOpaque(ctx, node.target),
            .value = try lowerValue(ctx, node.value),
            .span = node.span,
        } },
        .expr_stmt => |node| .{ .expr_stmt = .{
            .value = try lowerValue(ctx, node.expr),
            .span = node.span,
        } },
        .if_stmt => |node| .{ .if_stmt = .{
            .condition = try lowerValue(ctx, node.condition),
            .then_block = try lowerBlock(ctx, node.then_body),
            .else_block = if (node.else_body) |else_body| try lowerBlock(ctx, else_body) else null,
            .span = node.span,
        } },
        .for_stmt => |node| .{ .for_stmt = .{
            .binding = lookupLocal(ctx.program, node.binding_local_id),
            .iterator = try lowerValue(ctx, node.iterator),
            .body = try lowerBlock(ctx, node.body),
            .span = node.span,
        } },
        .while_stmt => |node| .{ .while_stmt = .{
            .condition = try lowerValue(ctx, node.condition),
            .body = try lowerBlock(ctx, node.body),
            .span = node.span,
        } },
        .break_stmt => |node| .{ .break_stmt = .{ .span = node.span } },
        .continue_stmt => |node| .{ .continue_stmt = .{ .span = node.span } },
        .match_stmt => |node| .{ .match_stmt = .{
            .subject = try lowerValue(ctx, node.subject),
            .arms = try lowerMatchArms(ctx, node.arms),
            .span = node.span,
        } },
        .switch_stmt => |node| .{ .switch_stmt = .{
            .subject = try lowerValue(ctx, node.subject),
            .cases = try lowerSwitchCases(ctx, node.cases),
            .default_block = if (node.default_body) |default_body| try lowerBlock(ctx, default_body) else null,
            .span = node.span,
        } },
        .return_stmt => |node| .{ .return_stmt = .{
            .return_place = .{
                .root = .return_slot,
                .ty = if (node.value) |value| model.hir.exprType(value.*) else .{ .kind = .void },
                .span = node.span,
            },
            .value = if (node.value) |value| try lowerValue(ctx, value) else null,
            .span = node.span,
        } },
    };
}

fn lowerMatchArms(ctx: *Context, arms: []const model.MatchArm) ![]mid.MatchArm {
    const lowered = try ctx.allocator.alloc(mid.MatchArm, arms.len);
    for (arms, 0..) |arm, index| {
        var bound_locals = std.array_list.Managed(mid.Local).init(ctx.allocator);
        defer bound_locals.deinit();
        try collectMatchPatternLocals(ctx, arm.pattern, &bound_locals);
        lowered[index] = .{
            .bound_locals = try bound_locals.toOwnedSlice(),
            .guard = if (arm.guard) |guard| try lowerValue(ctx, guard) else null,
            .body = try lowerBlock(ctx, arm.body),
            .span = arm.span,
        };
    }
    return lowered;
}

fn collectMatchPatternLocals(
    ctx: *Context,
    pattern: model.MatchPattern,
    out: *std.array_list.Managed(mid.Local),
) !void {
    switch (pattern) {
        .binding => |binding| try appendUniqueLocal(out, lookupLocal(ctx.program, binding.local_id)),
        .variant => |variant| {
            if (variant.as_binding_local_id) |local_id| {
                try appendUniqueLocal(out, lookupLocal(ctx.program, local_id));
            }
            if (variant.inner) |inner| try collectMatchPatternLocals(ctx, inner.*, out);
        },
    }
}

fn appendUniqueLocal(out: *std.array_list.Managed(mid.Local), local: mid.Local) !void {
    for (out.items) |existing| {
        if (existing.id == local.id) return;
    }
    try out.append(local);
}

fn lowerSwitchCases(ctx: *Context, cases: []const model.SwitchCase) ![]mid.SwitchCase {
    const lowered = try ctx.allocator.alloc(mid.SwitchCase, cases.len);
    for (cases, 0..) |case_node, index| {
        lowered[index] = .{
            .pattern = try lowerValue(ctx, case_node.pattern),
            .body = try lowerBlock(ctx, case_node.body),
            .span = case_node.span,
        };
    }
    return lowered;
}

pub fn lookupLocal(program: model.Program, local_id: u32) mid.Local {
    for (program.functions) |function_decl| {
        for (function_decl.locals) |local| {
            if (local.id == local_id) {
                return .{
                    .id = local.id,
                    .name = local.name,
                    .ty = local.ty,
                    .ownership = local.ownership,
                    .is_parameter = local.is_param,
                    .is_capture = local.is_capture,
                    .span = local.span,
                };
            }
        }
    }
    return .{
        .id = local_id,
        .name = "",
        .ty = .{ .kind = .unknown },
        .ownership = .owned,
        .span = .{ .start = 0, .end = 0 },
    };
}

fn blockSpan(statements: []const model.Statement) source_pkg.Span {
    if (statements.len == 0) return .{ .start = 0, .end = 0 };
    return .{
        .start = statementSpan(statements[0]).start,
        .end = statementSpan(statements[statements.len - 1]).end,
    };
}

fn statementSpan(statement: model.Statement) source_pkg.Span {
    return switch (statement) {
        .let_stmt => |node| node.span,
        .assign_stmt => |node| node.span,
        .expr_stmt => |node| node.span,
        .if_stmt => |node| node.span,
        .for_stmt => |node| node.span,
        .while_stmt => |node| node.span,
        .break_stmt => |node| node.span,
        .continue_stmt => |node| node.span,
        .match_stmt => |node| node.span,
        .switch_stmt => |node| node.span,
        .return_stmt => |node| node.span,
    };
}
