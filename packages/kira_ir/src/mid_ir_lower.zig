const std = @import("std");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");
const source_pkg = @import("kira_source");
const lower_hir = @import("lower_from_hir.zig");

pub const LowerOptions = struct {
    include_tests: bool = false,
};

const Context = struct {
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

fn lowerParams(allocator: std.mem.Allocator, params: []const model.Parameter) ![]const mid.Parameter {
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

fn lowerLocals(
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

fn lowerBlock(ctx: *Context, statements: []const model.Statement) anyerror!mid.Block {
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

fn lowerValue(ctx: *Context, expr: *model.Expr) anyerror!mid.Value {
    return switch (expr.*) {
        .integer => |node| .{ .integer = .{ .ty = node.ty, .span = node.span } },
        .float => |node| .{ .float = .{ .ty = node.ty, .span = node.span } },
        .string => |node| .{ .string = .{ .ty = node.ty, .span = node.span } },
        .boolean => |node| .{ .boolean = .{ .ty = node.ty, .span = node.span } },
        .null_ptr => |node| .{ .null_ptr = .{ .ty = node.ty, .span = node.span } },
        .function_ref => |node| .{ .function_ref = .{
            .function_id = node.function_id,
            .name = node.name,
            .ty = node.ty,
            .span = node.span,
        } },
        .local => |node| .{ .place = .{ .place = .{
            .root = if (lookupLocal(ctx.program, node.local_id).is_capture) .{ .capture = node.local_id } else .{ .local = node.local_id },
            .ty = node.ty,
            .span = node.span,
        }, .ownership = node.ownership } },
        .field => |node| blk: {
            if (try lowerPlace(ctx, expr)) |place| break :blk .{ .place = .{ .place = place } };
            break :blk .{ .opaque_member = .{
                .object = try allocValue(ctx, try lowerValue(ctx, node.object)),
                .field_name = node.field_name,
                .ty = node.ty,
                .temp_id = nextTempId(ctx),
                .span = node.span,
            } };
        },
        .index => |node| blk: {
            if (try lowerPlace(ctx, expr)) |place| break :blk .{ .place = .{ .place = place } };
            break :blk .{ .opaque_index = .{
                .object = try allocValue(ctx, try lowerValue(ctx, node.object)),
                .index = try allocValue(ctx, try lowerValue(ctx, node.index)),
                .ty = node.ty,
                .temp_id = nextTempId(ctx),
                .span = node.span,
            } };
        },
        .parent_view => blk: {
            if (try lowerPlace(ctx, expr)) |place| break :blk .{ .place = .{ .place = place } };
            return error.UnsupportedExecutableFeature;
        },
        .namespace_ref => |node| .{ .namespace_ref = .{
            .path = node.path,
            .ty = node.ty,
            .span = node.span,
        } },
        .call => |node| .{ .call = .{
            .callee_name = node.callee_name,
            .function_id = node.function_id,
            .args = try lowerValueSlice(ctx, node.args),
            .param_ownership = try lookupCallOwnership(ctx.allocator, ctx.program, node.function_id, node.callee_name),
            .return_ownership = lookupCallReturnOwnership(ctx.program, node.function_id),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .virtual_call => |node| .{ .virtual_call = .{
            .receiver = try allocValue(ctx, try lowerValue(ctx, node.receiver)),
            .receiver_ownership = blk: {
                const ownership = lookupVirtualCallOwnership(ctx.program, node.static_type_name, node.method_name);
                break :blk if (ownership.len != 0) ownership[0] else model.OwnershipMode.borrow_read;
            },
            .static_type_name = node.static_type_name,
            .method_name = node.method_name,
            .args = try lowerValueSlice(ctx, node.args),
            .param_ownership = blk: {
                const ownership = lookupVirtualCallOwnership(ctx.program, node.static_type_name, node.method_name);
                break :blk if (ownership.len > 1) ownership[1..] else &.{};
            },
            .return_ownership = lookupVirtualCallReturnOwnership(ctx.program, node.static_type_name, node.method_name),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .callback => |node| try lowerCallbackValue(ctx, node),
        .call_value => |node| .{ .call_value = .{
            .callee = try allocValue(ctx, try lowerValue(ctx, node.callee)),
            .args = try lowerValueSlice(ctx, node.args),
            .param_ownership = node.param_ownership,
            .return_ownership = .owned,
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .construct => |node| .{ .construct = .{
            .type_name = node.type_name,
            .fields = try lowerConstructFields(ctx, node.fields),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .construct_enum_variant => |node| .{ .construct_enum_variant = .{
            .enum_name = node.enum_name,
            .variant_name = node.variant_name,
            .payload = if (node.payload) |payload| try allocValue(ctx, try lowerValue(ctx, payload)) else null,
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .array => |node| .{ .array = .{
            .elements = try lowerValueSlice(ctx, node.elements),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .builder_array => |node| .{ .builder_array = .{
            .builder = try lowerBuilderBlock(ctx, node.builder),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .binary => |node| .{ .binary = .{
            .lhs = try allocValue(ctx, try lowerValue(ctx, node.lhs)),
            .rhs = try allocValue(ctx, try lowerValue(ctx, node.rhs)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .unary => |node| .{ .unary = .{
            .operand = try allocValue(ctx, try lowerValue(ctx, node.operand)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .conditional => |node| .{ .conditional = .{
            .condition = try allocValue(ctx, try lowerValue(ctx, node.condition)),
            .then_value = try allocValue(ctx, try lowerValue(ctx, node.then_expr)),
            .else_value = try allocValue(ctx, try lowerValue(ctx, node.else_expr)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .native_state => |node| .{ .native_state = .{
            .inner = try allocValue(ctx, try lowerValue(ctx, node.value)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .native_user_data => |node| .{ .native_user_data = .{
            .inner = try allocValue(ctx, try lowerValue(ctx, node.state)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .native_recover => |node| .{ .native_recover = .{
            .inner = try allocValue(ctx, try lowerValue(ctx, node.value)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .c_string_to_string => |node| .{ .c_string_to_string = .{
            .inner = try allocValue(ctx, try lowerValue(ctx, node.value)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .array_len => |node| .{ .array_len = .{
            .inner = try allocValue(ctx, try lowerValue(ctx, node.object)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
        .string_len => |node| .{ .string_len = .{
            .inner = try allocValue(ctx, try lowerValue(ctx, node.object)),
            .ty = node.ty,
            .temp_id = nextTempId(ctx),
            .span = node.span,
        } },
    };
}

fn lowerCallbackValue(ctx: *Context, node: model.hir.CallbackExpr) anyerror!mid.Value {
    const function_id = ctx.next_callback_id;
    ctx.next_callback_id += 1;
    const function_name = try std.fmt.allocPrint(ctx.allocator, "callback_{d}", .{function_id});
    const function_decl = mid.Function{
        .id = function_id,
        .name = function_name,
        .execution = .inherited,
        .is_extern = false,
        .params = try lowerParams(ctx.allocator, node.params),
        .locals = try lowerLocals(ctx.allocator, node.params, node.locals),
        .captures = try lowerCaptures(ctx.allocator, node.captures),
        .return_type = node.return_type,
        .return_ownership = .owned,
        .body = try lowerBlock(ctx, node.body),
        .span = node.span,
    };
    try ctx.functions.append(function_decl);
    return .{ .callback = .{
        .function_id = function_id,
        .captures = function_decl.captures,
        .ty = node.ty,
        .temp_id = nextTempId(ctx),
        .span = node.span,
    } };
}

fn lowerCaptures(allocator: std.mem.Allocator, captures: []const model.Capture) ![]const mid.Capture {
    const lowered = try allocator.alloc(mid.Capture, captures.len);
    for (captures, 0..) |capture, index| {
        lowered[index] = .{
            .local_id = capture.local_id,
            .source_local_id = capture.source_local_id,
            .by_ref = capture.by_ref,
            .ownership = capture.ownership,
            .name = capture.name,
            .ty = capture.ty,
            .span = capture.span,
        };
    }
    return lowered;
}

fn lowerConstructFields(ctx: *Context, fields: []const model.ConstructFieldInit) anyerror![]mid.ConstructFieldInit {
    const lowered = try ctx.allocator.alloc(mid.ConstructFieldInit, fields.len);
    for (fields, 0..) |field, index| {
        lowered[index] = .{
            .field_name = field.field_name,
            .field_index = field.field_index,
            .value = try lowerValue(ctx, field.value),
            .span = field.span,
        };
    }
    return lowered;
}

fn lowerBuilderBlock(ctx: *Context, builder: model.BuilderBlock) anyerror!mid.BuilderBlock {
    var items = std.array_list.Managed(mid.BuilderItem).init(ctx.allocator);
    defer items.deinit();
    for (builder.items) |item| {
        try items.append(switch (item) {
            .expr => |value| .{ .expr = .{
                .value = try lowerValue(ctx, value.expr),
                .span = value.span,
            } },
            .if_item => |value| .{ .if_item = .{
                .condition = try lowerValue(ctx, value.condition),
                .then_block = try lowerBuilderBlock(ctx, value.then_block),
                .else_block = if (value.else_block) |else_block| try lowerBuilderBlock(ctx, else_block) else null,
                .span = value.span,
            } },
            .for_item => |value| .{ .for_item = .{
                .binding = lookupLocal(ctx.program, value.binding_local_id),
                .iterator = try lowerValue(ctx, value.iterator),
                .body = try lowerBuilderBlock(ctx, value.body),
                .span = value.span,
            } },
            .switch_item => |value| .{ .switch_item = .{
                .subject = try lowerValue(ctx, value.subject),
                .cases = try lowerBuilderSwitchCases(ctx, value.cases),
                .default_block = if (value.default_block) |default_block| try lowerBuilderBlock(ctx, default_block) else null,
                .span = value.span,
            } },
        });
    }
    return .{
        .items = try items.toOwnedSlice(),
        .span = builder.span,
    };
}

fn lowerBuilderSwitchCases(ctx: *Context, cases: []const model.BuilderSwitchCase) anyerror![]mid.BuilderSwitchCase {
    const lowered = try ctx.allocator.alloc(mid.BuilderSwitchCase, cases.len);
    for (cases, 0..) |case_node, index| {
        lowered[index] = .{
            .pattern = try lowerValue(ctx, case_node.pattern),
            .body = try lowerBuilderBlock(ctx, case_node.body),
            .span = case_node.span,
        };
    }
    return lowered;
}

fn lowerValueSlice(ctx: *Context, values: []const *model.Expr) anyerror![]mid.Value {
    const lowered = try ctx.allocator.alloc(mid.Value, values.len);
    for (values, 0..) |value, index| lowered[index] = try lowerValue(ctx, value);
    return lowered;
}

fn allocValue(ctx: *Context, value: mid.Value) anyerror!*mid.Value {
    const ptr = try ctx.allocator.create(mid.Value);
    ptr.* = value;
    return ptr;
}

fn lowerPlaceOrOpaque(ctx: *Context, expr: *model.Expr) anyerror!mid.Place {
    return (try lowerPlace(ctx, expr)) orelse error.UnsupportedExecutableFeature;
}

fn lowerPlace(ctx: *Context, expr: *model.Expr) anyerror!?mid.Place {
    return switch (expr.*) {
        .local => |node| .{
            .root = if (lookupLocal(ctx.program, node.local_id).is_capture) .{ .capture = node.local_id } else .{ .local = node.local_id },
            .ty = node.ty,
            .span = node.span,
        },
        .field => |node| blk: {
            const base = (try lowerPlace(ctx, node.object)) orelse return null;
            var projections = std.array_list.Managed(mid.Projection).init(ctx.allocator);
            defer projections.deinit();
            try projections.appendSlice(base.projections);
            try projections.append(.{ .field = .{
                .container_type_name = node.container_type_name,
                .field_name = node.field_name,
                .field_index = node.field_index,
                .ty = node.ty,
                .span = node.span,
            } });
            break :blk .{
                .root = base.root,
                .projections = try projections.toOwnedSlice(),
                .ty = node.ty,
                .span = node.span,
            };
        },
        .index => |node| blk: {
            const base = (try lowerPlace(ctx, node.object)) orelse return null;
            var projections = std.array_list.Managed(mid.Projection).init(ctx.allocator);
            defer projections.deinit();
            try projections.appendSlice(base.projections);
            const dynamic_value = if (node.index.* == .integer) null else try allocValue(ctx, try lowerValue(ctx, node.index));
            try projections.append(.{ .index = .{
                .index = if (node.index.* == .integer) node.index.integer.value else null,
                .dynamic_index = dynamic_value,
                .ty = node.ty,
                .span = node.span,
            } });
            break :blk .{
                .root = base.root,
                .projections = try projections.toOwnedSlice(),
                .ty = node.ty,
                .span = node.span,
            };
        },
        .parent_view => |node| blk: {
            const base = (try lowerPlace(ctx, node.object)) orelse return null;
            var projections = std.array_list.Managed(mid.Projection).init(ctx.allocator);
            defer projections.deinit();
            try projections.appendSlice(base.projections);
            try projections.append(.{ .parent_view = .{
                .offset = node.offset,
                .ty = node.ty,
                .span = node.span,
            } });
            break :blk .{
                .root = base.root,
                .projections = try projections.toOwnedSlice(),
                .ty = node.ty,
                .span = node.span,
            };
        },
        else => null,
    };
}

fn lookupLocal(program: model.Program, local_id: u32) mid.Local {
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

fn lookupCallOwnership(
    allocator: std.mem.Allocator,
    program: model.Program,
    function_id: ?u32,
    callee_name: []const u8,
) ![]const model.OwnershipMode {
    if (builtinCallOwnership(callee_name)) |ownership| return ownership;
    const id = function_id orelse return &.{};
    for (program.functions) |function_decl| {
        if (function_decl.id != id) continue;
        const lowered = function_decl.params;
        if (lowered.len == 0) return &.{};
        var modes = std.array_list.Managed(model.OwnershipMode).init(allocator);
        defer modes.deinit();
        for (lowered) |param| modes.append(param.ownership) catch return &.{};
        return modes.toOwnedSlice() catch &.{};
    }
    return &.{};
}

fn builtinCallOwnership(callee_name: []const u8) ?[]const model.OwnershipMode {
    if (std.mem.eql(u8, callee_name, "array.append")) return &.{ .borrow_mut, .owned };
    if (std.mem.eql(u8, callee_name, "print")) return &.{.borrow_read};
    return null;
}

fn lookupCallReturnOwnership(program: model.Program, function_id: ?u32) model.OwnershipMode {
    const id = function_id orelse return .owned;
    for (program.functions) |function_decl| {
        if (function_decl.id == id) return function_decl.return_ownership;
    }
    return .owned;
}

fn lookupVirtualCallOwnership(program: model.Program, static_type_name: []const u8, method_name: []const u8) []const model.OwnershipMode {
    const function_name = fullMethodName(std.heap.page_allocator, static_type_name, method_name) catch return &.{};
    defer std.heap.page_allocator.free(function_name);
    if (lower_hir.functionIdByName(program, function_name)) |id| return lookupCallOwnership(std.heap.page_allocator, program, id, function_name) catch &.{};
    return &.{};
}

fn lookupVirtualCallReturnOwnership(program: model.Program, static_type_name: []const u8, method_name: []const u8) model.OwnershipMode {
    const function_name = fullMethodName(std.heap.page_allocator, static_type_name, method_name) catch return .owned;
    defer std.heap.page_allocator.free(function_name);
    if (lower_hir.functionIdByName(program, function_name)) |id| return lookupCallReturnOwnership(program, id);
    return .owned;
}

fn fullMethodName(allocator: std.mem.Allocator, static_type_name: []const u8, method_name: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}.{s}", .{ static_type_name, method_name });
}

fn nextTempId(ctx: *Context) u32 {
    defer ctx.next_temp_id += 1;
    return ctx.next_temp_id;
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
