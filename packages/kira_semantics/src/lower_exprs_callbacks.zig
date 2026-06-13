const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");
const names = @import("lower_exprs_names.zig");
const types = @import("lower_exprs_types.zig");
const flattenCalleeName = names.flattenCalleeName;
const exprSpan = types.exprSpan;

pub fn lowerCallbackArgument(
    ctx: *shared.Context,
    syntax_arg: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    callback_info: model.CallbackInfo,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    const name = try flattenCalleeName(ctx.allocator, syntax_arg);
    const header = function_headers.get(name) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM043",
            .title = "unknown callback target",
            .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not find a function named '{s}' for this callback argument.", .{name}),
            .labels = &.{
                diagnostics.primaryLabel(exprSpan(syntax_arg.*), "callback target is not a known function"),
            },
            .help = "Pass a named function that matches the callback signature.",
        });
        return error.DiagnosticsEmitted;
    };

    if (header.execution == .runtime) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM044",
            .title = "runtime callbacks are not supported here",
            .message = "Callbacks passed to native FFI must currently resolve to native or extern functions.",
            .labels = &.{
                diagnostics.primaryLabel(exprSpan(syntax_arg.*), "runtime function cannot be converted to a native callback"),
            },
            .help = "Mark the callback target with @Native or use an extern callback symbol.",
        });
        return error.DiagnosticsEmitted;
    }

    if (header.params.len != callback_info.params.len or !shared.canAssignExactly(header.return_type, callback_info.result)) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM045",
            .title = "invalid callback signature",
            .message = "The callback target does not match the required callback signature.",
            .labels = &.{
                diagnostics.primaryLabel(exprSpan(syntax_arg.*), "callback signature does not match"),
            },
            .help = "Match the callback parameter and result types exactly.",
        });
        return error.DiagnosticsEmitted;
    }
    for (header.params, 0..) |param_type, index| {
        if (!shared.canAssignExactly(param_type, callback_info.params[index])) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM045",
                .title = "invalid callback signature",
                .message = "The callback target does not match the required callback signature.",
                .labels = &.{
                    diagnostics.primaryLabel(exprSpan(syntax_arg.*), "callback signature does not match"),
                },
                .help = "Match the callback parameter and result types exactly.",
            });
            return error.DiagnosticsEmitted;
        }
    }

    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = .{ .function_ref = .{
        .representation = .native_callback,
        .function_id = header.id,
        .name = name,
        .ty = expected_type,
        .span = exprSpan(syntax_arg.*),
    } };
    return lowered;
}

pub fn callbackTypesCompatible(expected: model.ResolvedType, actual: model.ResolvedType) bool {
    if (expected.kind != .callback or actual.kind != .callback) return false;
    const expected_name = expected.name orelse return false;
    const actual_name = actual.name orelse return false;
    if (std.mem.eql(u8, expected_name, actual_name)) return true;

    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    const allocator = arena.allocator();
    const expected_sig = function_types.parseSignature(allocator, expected) catch return false;
    const actual_sig = function_types.parseSignature(allocator, actual) catch return false;
    if (expected_sig == null or actual_sig == null) return false;
    if (expected_sig.?.params.len != actual_sig.?.params.len) return false;
    for (expected_sig.?.params, 0..) |param, index| {
        if (index < expected_sig.?.param_ownership.len and index < actual_sig.?.param_ownership.len and expected_sig.?.param_ownership[index] != actual_sig.?.param_ownership[index]) return false;
        if (!shared.canAssignExactly(param, actual_sig.?.params[index])) return false;
    }
    return actual_sig.?.result.kind == .unknown or shared.canAssignExactly(expected_sig.?.result, actual_sig.?.result);
}
