const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const parent = @import("lower_exprs.zig");
const types = @import("lower_exprs_types.zig");
const lowerExpr = parent.lowerExpr;
const lowerExpectedValue = types.lowerExpectedValue;
const canPassArgument = types.canPassArgument;
const exprSpan = types.exprSpan;

pub fn lowerCallArgument(
    ctx: *shared.Context,
    syntax_arg: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    ownership: model.OwnershipMode,
    callee_name: []const u8,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    call_span: source_pkg.Span,
) !*model.Expr {
    const explicit_ownership = if (syntax_arg.* == .ownership) syntax_arg.ownership.op else null;
    const local = localBindingForSyntax(scope, syntax_arg);
    if (local) |binding| {
        if (binding.binding.moved) {
            try emitUseAfterMove(ctx, binding.name, exprSpan(syntax_arg.*), binding.binding.move_span);
            return error.DiagnosticsEmitted;
        }
        // A binding with an outstanding partial move cannot be passed as a whole: one of
        // its fields was moved out (`let x = obj.field`) and not re-initialized, so the
        // aggregate is incomplete. Re-store the field (`obj.field = ...`) before reuse.
        if (binding.binding.hasMovedFields()) {
            try emitUseAfterPartialMove(ctx, binding.name, exprSpan(syntax_arg.*), binding.binding.move_span);
            return error.DiagnosticsEmitted;
        }
    }

    if (ownership == .borrow_read or ownership == .borrow_mut) {
        if (explicit_ownership == .move) {
            try emitMoveIntoBorrow(ctx, callee_name, exprSpan(syntax_arg.*));
            return error.DiagnosticsEmitted;
        }
        if (ownership == .borrow_mut) {
            if (local) |binding| {
                if (binding.binding.storage == .immutable) {
                    try emitImmutableBorrowMut(ctx, binding.name, binding.binding.decl_span, exprSpan(syntax_arg.*), expected_type);
                    return error.DiagnosticsEmitted;
                }
            }
        }
        return lowerExpectedValue(ctx, syntax_arg, expected_type, imports, scope, function_headers, call_span);
    }

    if (ownership == .copy) {
        if (explicit_ownership == .move) {
            try emitMoveIntoCopy(ctx, callee_name, exprSpan(syntax_arg.*));
            return error.DiagnosticsEmitted;
        }
        if (explicit_ownership == .copy) {
            const lowered = try lowerOwnershipExpr(ctx, syntax_arg.ownership, imports, scope, function_headers);
            return ensureExpectedArgumentType(ctx, lowered, expected_type, call_span);
        }
        const lowered = try lowerExpectedValue(ctx, syntax_arg, expected_type, imports, scope, function_headers, call_span);
        if (local) |binding| {
            if (!isTriviallyCopyable(expected_type) and !isTriviallyCopyable(model.hir.exprType(lowered.*))) {
                try emitMissingCopy(ctx, binding.name, callee_name, exprSpan(syntax_arg.*));
                return error.DiagnosticsEmitted;
            }
        }
        return lowered;
    }

    if (ownership == .move or ownership == .owned) {
        if (explicit_ownership == .move) {
            const lowered = try lowerOwnershipExpr(ctx, syntax_arg.ownership, imports, scope, function_headers);
            return ensureExpectedArgumentType(ctx, lowered, expected_type, call_span);
        }
        const lowered = try lowerExpectedValue(ctx, syntax_arg, expected_type, imports, scope, function_headers, call_span);
        if (local) |binding| {
            if (!isTriviallyCopyable(expected_type) and !isTriviallyCopyable(model.hir.exprType(lowered.*))) {
                try emitMissingMove(ctx, binding.name, callee_name, exprSpan(syntax_arg.*));
                return error.DiagnosticsEmitted;
            }
        }
        return lowered;
    }

    unreachable;
}

fn isTriviallyCopyable(ty: model.ResolvedType) bool {
    return switch (ty.kind) {
        .void, .integer, .float, .boolean, .c_string, .raw_ptr => true,
        else => false,
    };
}

pub fn lowerOwnershipExpr(
    ctx: *shared.Context,
    node: syntax.ast.OwnershipExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    return switch (node.op) {
        .copy => lowerCopyExpr(ctx, node, imports, scope, function_headers),
        .move => lowerMoveExpr(ctx, node, imports, scope, function_headers),
    };
}

fn lowerCopyExpr(
    ctx: *shared.Context,
    node: syntax.ast.OwnershipExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    const lowered = try lowerExpr(ctx, node.operand, imports, scope, function_headers);
    const ty = model.hir.exprType(lowered.*);
    if (isTriviallyCopyable(ty)) return lowered;

    try emitNonTrivialCopyNotImplemented(ctx, node.span, ty);
    return error.DiagnosticsEmitted;
}

fn lowerMoveExpr(
    ctx: *shared.Context,
    node: syntax.ast.OwnershipExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    if (localBindingForSyntax(scope, node.operand)) |local| {
        if (local.binding.moved) {
            try emitAlreadyMoved(ctx, local.name, node.span, local.binding.move_span);
            return error.DiagnosticsEmitted;
        }
        if (local.binding.hasMovedFields()) {
            try emitUseAfterPartialMove(ctx, local.name, node.span, local.binding.move_span);
            return error.DiagnosticsEmitted;
        }
        if (local.binding.ownership == .borrow_read or local.binding.ownership == .borrow_mut) {
            try emitMoveBorrowedValue(ctx, local.name, node.span, local.binding.decl_span);
            return error.DiagnosticsEmitted;
        }
        if (!local.binding.initialized) {
            try emitUninitializedMove(ctx, local.name, node.span, local.binding.decl_span);
            return error.DiagnosticsEmitted;
        }

        const lowered = try ctx.allocator.create(model.Expr);
        lowered.* = .{ .local = .{
            .local_id = local.binding.id,
            .name = try ctx.allocator.dupe(u8, local.name),
            .ty = local.binding.ty,
            .storage = local.binding.storage,
            .ownership = .move,
            .span = exprSpan(node.operand.*),
        } };
        local.binding.moved = true;
        local.binding.move_span = node.span;
        return lowered;
    }

    return lowerExpr(ctx, node.operand, imports, scope, function_headers);
}

const LocalBindingRef = struct {
    name: []const u8,
    binding: *model.LocalBinding,
};

fn localBindingForSyntax(scope: *model.Scope, expr: *syntax.ast.Expr) ?LocalBindingRef {
    if (expr.* != .identifier) return null;
    const name = expr.identifier.name.segments[0].text;
    const binding = scope.entries.getPtr(name) orelse return null;
    return .{ .name = name, .binding = binding };
}

fn ensureExpectedArgumentType(
    ctx: *shared.Context,
    lowered: *model.Expr,
    expected_type: model.ResolvedType,
    span: source_pkg.Span,
) !*model.Expr {
    const actual_type = model.hir.exprType(lowered.*);
    if (!canPassArgument(ctx, expected_type, actual_type)) {
        try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, span, expected_type, actual_type);
        return error.DiagnosticsEmitted;
    }
    return lowered;
}

pub fn emitUseAfterMove(ctx: *shared.Context, name: []const u8, use_span: source_pkg.Span, move_span: ?source_pkg.Span) !void {
    const labels = if (move_span) |span|
        &.{
            diagnostics.primaryLabel(use_span, "cannot use moved value"),
            diagnostics.secondaryLabel(span, "ownership moved here"),
        }
    else
        &.{diagnostics.primaryLabel(use_span, "cannot use moved value")};
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM107",
        .title = "local was moved",
        .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` was moved and is no longer available here.", .{name}),
        .labels = labels,
        .help = "Move a value at most once, or pass it to borrowing functions without `move`.",
    });
}

/// At scope exit, a binding that still has a field moved out (a partial move that was never
/// restored) cannot be dropped safely: a field read aliases the source field rather than
/// transferring it, so both the field's new owner and the base own the same array/enum
/// backing. Writing the field back (`x.field = ...`) clears the mark and makes the base whole
/// again (the in-place mutation idiom); anything else is the same double-free/use-after-free
/// class KSEM107 prevents and must be rejected here rather than at runtime.
pub fn rejectOutstandingMovedFields(ctx: *shared.Context, scope: *model.Scope) !void {
    var it = scope.entries.iterator();
    while (it.next()) |entry| {
        const binding = entry.value_ptr;
        if (!binding.hasMovedFields()) continue;
        const span = binding.move_span orelse binding.decl_span;
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM107",
            .title = "local was moved",
            .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` has a field (`{s}`) moved out and never restored, so it cannot be dropped safely.", .{ entry.key_ptr.*, binding.moved_fields.items[0] }),
            .labels = &.{diagnostics.primaryLabel(span, "a field was moved out here and never written back")},
            .help = "Re-initialize the moved field (`x.field = ...`) before the value goes out of scope, or `move` the whole value instead of a single field.",
        });
        return error.DiagnosticsEmitted;
    }
}

pub fn emitUseAfterPartialMove(ctx: *shared.Context, name: []const u8, use_span: source_pkg.Span, move_span: ?source_pkg.Span) !void {
    const labels = if (move_span) |span|
        &.{
            diagnostics.primaryLabel(use_span, "cannot use partially moved value"),
            diagnostics.secondaryLabel(span, "a field was moved out here"),
        }
    else
        &.{diagnostics.primaryLabel(use_span, "cannot use partially moved value")};
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM107",
        .title = "local was moved",
        .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` had a field moved out and cannot be used as a whole here.", .{name}),
        .labels = labels,
        .help = "Re-initialize the moved field (`x.field = ...`) before using the value again, or `copy` the field instead of moving it.",
    });
}

fn emitAlreadyMoved(ctx: *shared.Context, name: []const u8, move_span: source_pkg.Span, previous_move_span: ?source_pkg.Span) !void {
    const labels = if (previous_move_span) |span|
        &.{
            diagnostics.primaryLabel(move_span, "cannot move value again"),
            diagnostics.secondaryLabel(span, "first move was here"),
        }
    else
        &.{diagnostics.primaryLabel(move_span, "cannot move value again")};
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM110",
        .title = "local was already moved",
        .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` was already moved.", .{name}),
        .labels = labels,
        .help = "Do not move the same owned value more than once.",
    });
}

fn emitMissingMove(ctx: *shared.Context, name: []const u8, callee_name: []const u8, arg_span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM108",
        .title = "ownership transfer requires explicit move",
        .message = try std.fmt.allocPrint(ctx.allocator, "Passing `{s}` to `{s}` transfers ownership.", .{ name, callee_name }),
        .labels = &.{diagnostics.primaryLabel(arg_span, "this argument is consumed")},
        .help = try std.fmt.allocPrint(ctx.allocator, "Write `move {s}` to transfer ownership explicitly.", .{name}),
    });
}

fn emitMissingCopy(ctx: *shared.Context, name: []const u8, callee_name: []const u8, arg_span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM113",
        .title = "copy requires explicit copy",
        .message = try std.fmt.allocPrint(ctx.allocator, "Passing `{s}` to `{s}` copies a non-trivial value.", .{ name, callee_name }),
        .labels = &.{diagnostics.primaryLabel(arg_span, "this argument is copied")},
        .help = try std.fmt.allocPrint(ctx.allocator, "Write `copy {s}` to copy this value explicitly.", .{name}),
    });
}

fn emitImmutableBorrowMut(ctx: *shared.Context, name: []const u8, decl_span: source_pkg.Span, arg_span: source_pkg.Span, expected_type: model.ResolvedType) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM109",
        .title = "cannot mutably borrow immutable binding",
        .message = try std.fmt.allocPrint(ctx.allocator, "Cannot mutably borrow immutable binding `{s}`.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(arg_span, try std.fmt.allocPrint(ctx.allocator, "requires `borrow mut {s}`", .{shared.typeLabel(expected_type)})),
            diagnostics.secondaryLabel(decl_span, "binding is declared immutable here"),
        },
        .help = "Declare the binding with `var` before passing it to a `borrow mut` parameter.",
    });
}

fn emitMoveBorrowedValue(ctx: *shared.Context, name: []const u8, move_span: source_pkg.Span, decl_span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM111",
        .title = "cannot move borrowed value",
        .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` is borrowed and cannot be moved by this scope.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(move_span, "move requires ownership"),
            diagnostics.secondaryLabel(decl_span, "borrowed binding was declared here"),
        },
        .help = "Only owned locals can be moved. Pass borrowed values to borrowing parameters instead.",
    });
}

fn emitUninitializedMove(ctx: *shared.Context, name: []const u8, move_span: source_pkg.Span, decl_span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM086",
        .title = "local is not initialized",
        .message = try std.fmt.allocPrint(ctx.allocator, "The local declaration `{s}` does not have a value to move yet.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(move_span, "local is moved before it has been initialized"),
            diagnostics.secondaryLabel(decl_span, "declaration appears here"),
        },
        .help = "Assign to the local before moving it, or add an initializer expression to the declaration.",
    });
}

fn emitMoveIntoBorrow(ctx: *shared.Context, callee_name: []const u8, span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM114",
        .title = "borrow parameter does not take ownership",
        .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` borrows this argument, so `move` is not allowed here.", .{callee_name}),
        .labels = &.{diagnostics.primaryLabel(span, "remove `move` when passing to a borrow parameter")},
        .help = "Pass the value directly to a borrowing parameter so the caller keeps ownership.",
    });
}

fn emitMoveIntoCopy(ctx: *shared.Context, callee_name: []const u8, span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM115",
        .title = "copy parameter does not consume the source value",
        .message = try std.fmt.allocPrint(ctx.allocator, "`{s}` copies this argument, so `move` is not allowed here.", .{callee_name}),
        .labels = &.{diagnostics.primaryLabel(span, "replace `move` with `copy`, or pass the value directly if it is trivial")},
        .help = "Use `copy` for non-trivial copied values, or remove the ownership keyword for trivial values.",
    });
}

fn emitNonTrivialCopyNotImplemented(ctx: *shared.Context, span: source_pkg.Span, ty: model.ResolvedType) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM116",
        .title = "non-trivial copy is not implemented yet",
        .message = try std.fmt.allocPrint(ctx.allocator, "Kira parsed `copy`, but cloning `{s}` is not implemented yet.", .{shared.typeLabel(ty)}),
        .labels = &.{diagnostics.primaryLabel(span, "non-trivial values cannot be copied yet")},
        .help = "Borrow the value, move it, or add explicit clone semantics before using `copy` on this type.",
    });
}
