//! Value and place lowering from HIR into the Mid IR: expressions, places,
//! builder blocks, callbacks, and the per-call ownership lookups. The driver
//! (program/function/statement lowering) lives in `mid_ir_lower.zig`.
const std = @import("std");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");
const source_pkg = @import("kira_source");
const lower_hir = @import("lower_from_hir.zig");
const lower = @import("mid_ir_lower.zig");

const Context = lower.Context;
const lookupLocal = lower.lookupLocal;
const lowerParams = lower.lowerParams;
const lowerLocals = lower.lowerLocals;
const lowerBlock = lower.lowerBlock;

pub fn lowerValue(ctx: *Context, expr: *model.Expr) anyerror!mid.Value {
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
        .cast => |node| .{ .cast = .{
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

pub fn lowerPlaceOrOpaque(ctx: *Context, expr: *model.Expr) anyerror!mid.Place {
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

test {
    std.testing.refAllDecls(@This());
}
