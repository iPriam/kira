const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const Context = shared.Context;
const CallbackCaptureFrame = shared.CallbackCaptureFrame;
const typeTextFromResolved = shared.typeTextFromResolved;

pub const CaptureResolution = struct {
    binding: model.LocalBinding,
    captured: bool,
};

pub fn resolveLocalOrCapture(ctx: *Context, active_scope: model.Scope, name: []const u8, use_span: source_pkg.Span) !?CaptureResolution {
    if (active_scope.get(name)) |binding| return .{ .binding = binding, .captured = false };
    const frame = ctx.callback_capture_frame orelse return null;
    const outer = try resolveCaptureSource(ctx, frame, name, use_span) orelse return null;
    const captured = try captureBinding(ctx, frame, name, outer, use_span);
    return .{ .binding = captured, .captured = true };
}

fn resolveCaptureSource(ctx: *Context, frame: *CallbackCaptureFrame, name: []const u8, use_span: source_pkg.Span) !?model.LocalBinding {
    if (frame.source_scope.get(name)) |binding| return binding;
    const parent = frame.parent orelse return null;
    if (parent.active_scope.get(name)) |binding| return binding;
    const parent_outer = try resolveCaptureSource(ctx, parent, name, use_span) orelse return null;
    return try captureBinding(ctx, parent, name, parent_outer, use_span);
}

fn captureBinding(
    ctx: *Context,
    frame: *CallbackCaptureFrame,
    name: []const u8,
    outer: model.LocalBinding,
    use_span: source_pkg.Span,
) !model.LocalBinding {
    if (frame.active_scope.get(name)) |binding| return binding;

    if (!isTrivialCaptureType(outer.ty)) {
        try emitNonCopyClosureCapture(ctx, name, outer.ty, use_span, outer.decl_span);
        return error.DiagnosticsEmitted;
    }

    // Mutable locals are captured as explicit borrows of their enclosing storage slot.
    // Immutable trivial locals are copied into the closure. Non-trivial user values are
    // rejected above so the VM cannot hide ownership bugs with heap retention.
    const by_ref = outer.storage != .immutable;
    const capture_ownership: model.OwnershipMode = if (by_ref) .borrow_mut else .copy;

    const local_id = frame.next_local_id.*;
    frame.next_local_id.* += 1;
    const local_name = try ctx.allocator.dupe(u8, name);
    try frame.active_scope.put(ctx.allocator, local_name, .{
        .id = local_id,
        .ty = outer.ty,
        .storage = outer.storage,
        .ownership = outer.ownership,
        .initialized = true,
        .moved = outer.moved,
        .move_span = outer.move_span,
        .decl_span = outer.decl_span,
    });
    try frame.locals.append(.{
        .id = local_id,
        .name = local_name,
        .ty = outer.ty,
        .ownership = outer.ownership,
        .is_capture = true,
        .span = outer.decl_span,
    });
    try frame.captures.append(.{
        .local_id = local_id,
        .source_local_id = outer.id,
        .by_ref = by_ref,
        .ownership = capture_ownership,
        .name = local_name,
        .ty = outer.ty,
        .span = outer.decl_span,
    });
    return frame.active_scope.get(name).?;
}

fn isTrivialCaptureType(ty: model.ResolvedType) bool {
    return switch (ty.kind) {
        .void, .integer, .float, .boolean, .c_string, .raw_ptr, .callback => true,
        else => false,
    };
}

fn emitNonCopyClosureCapture(
    ctx: *Context,
    name: []const u8,
    ty: model.ResolvedType,
    use_span: source_pkg.Span,
    decl_span: source_pkg.Span,
) !void {
    const ty_text = try typeTextFromResolved(ctx.allocator, ty);
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM117",
        .title = "closure capture requires explicit ownership",
        .message = try std.fmt.allocPrint(ctx.allocator, "The closure captures non-Copy value `{s}` of type {s}.", .{ name, ty_text }),
        .labels = &.{
            diagnostics.primaryLabel(use_span, "non-Copy value is captured here"),
            diagnostics.secondaryLabel(decl_span, "captured value is declared here"),
        },
        .help = "Pass the value through a borrow parameter, move it into a supported owner before creating the closure, or capture only Copy values.",
    });
}

pub fn emitUnsupportedMutableCapture(
    ctx: *Context,
    name: []const u8,
    use_span: source_pkg.Span,
    decl_span: source_pkg.Span,
) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM094",
        .title = "mutable callback capture is not supported",
        .message = try std.fmt.allocPrint(ctx.allocator, "The trailing callback captures mutable local '{s}', but mutable captures are not supported yet.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(use_span, "mutable local is captured here"),
            diagnostics.secondaryLabel(decl_span, "mutable local is declared here"),
        },
        .help = "Capture an immutable `let` value, or pass mutable state explicitly through a supported state object.",
    });
}
