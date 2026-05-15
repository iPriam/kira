const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");
const parent = @import("lower_exprs.zig");
const lowerCallbackBlockValue = parent.lowerCallbackBlockValue;
const lowerExpr = parent.lowerExpr;
const lowerImplicitSelfFieldExpr = parent.lowerImplicitSelfFieldExpr;
pub const LoweredLocalDeclaration = struct {
    ty: model.ResolvedType,
    value: ?*model.Expr,
    initialized: bool,
};

pub fn functionTypeFromResolvedSignature(
    allocator: std.mem.Allocator,
    params: []const model.ResolvedType,
    return_type: model.ResolvedType,
) !model.ResolvedType {
    return .{
        .kind = .callback,
        .name = try function_types.signatureText(allocator, params, return_type),
    };
}

pub fn functionTypeFromHeader(allocator: std.mem.Allocator, header: shared.FunctionHeader) !model.ResolvedType {
    return functionTypeFromResolvedSignature(allocator, header.params, header.return_type);
}

pub fn isCallableValueExpr(expr: *syntax.ast.Expr, scope: *model.Scope) bool {
    return switch (expr.*) {
        .identifier => |node| scope.get(node.name.segments[0].text) != null,
        .member, .index => true,
        else => false,
    };
}

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

pub fn lowerLocalDeclaration(
    ctx: *shared.Context,
    node: syntax.ast.LetStatement,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !LoweredLocalDeclaration {
    const explicit_type = if (node.type_expr) |type_expr| try shared.typeFromSyntaxChecked(ctx, type_expr.*) else null;

    if (node.value) |value_expr| {
        const lowered_value = if (explicit_type) |declared_type|
            try lowerExpectedValue(ctx, value_expr, declared_type, imports, scope, function_headers, exprSpan(value_expr.*))
        else
            try lowerExpr(ctx, value_expr, imports, scope, function_headers);

        if (explicit_type) |declared_type| {
            const actual_type = model.hir.exprType(lowered_value.*);
            if (!canInitializeDeclaredType(ctx, declared_type, actual_type)) {
                try emitDeclaredInitializerMismatch(ctx, exprSpan(value_expr.*), declared_type, actual_type);
                return error.DiagnosticsEmitted;
            }
            return .{
                .ty = declared_type,
                .value = lowered_value,
                .initialized = true,
            };
        }

        return .{
            .ty = model.hir.exprType(lowered_value.*),
            .value = lowered_value,
            .initialized = true,
        };
    }

    if (explicit_type) |declared_type| {
        return .{
            .ty = declared_type,
            .value = null,
            .initialized = false,
        };
    }

    try shared.emitAmbiguousInference(ctx.allocator, ctx.diagnostics, node.span);
    return error.DiagnosticsEmitted;
}

pub fn lowerExpectedValue(
    ctx: *shared.Context,
    syntax_arg: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    span: source_pkg.Span,
) anyerror!*model.Expr {
    if (expected_type.kind == .enum_instance) {
        if (try parent.lowerEnumVariantExprExpected(ctx, syntax_arg, expected_type, imports, scope, function_headers)) |lowered_enum| {
            return lowered_enum;
        }
    }

    if (shared.callbackInfo(ctx, expected_type)) |callback_info| {
        if (syntax_arg.* == .callback) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM095",
                .title = "inline FFI callbacks are not supported",
                .message = "Inline callback blocks cannot be converted directly to named FFI callback pointer types yet.",
                .labels = &.{diagnostics.primaryLabel(exprSpan(syntax_arg.*), "inline block cannot become an FFI callback pointer")},
                .help = "Use a named @Native function for FFI callback pointer fields, or store this block in an ordinary function-typed Kira value.",
            });
            return error.DiagnosticsEmitted;
        }
        return lowerCallbackArgument(ctx, syntax_arg, expected_type, callback_info, function_headers);
    }

    if (expected_type.kind == .callback and syntax_arg.* == .callback) {
        return lowerCallbackBlockValue(
            ctx,
            syntax_arg.callback.params,
            syntax_arg.callback.body,
            syntax_arg.callback.span,
            expected_type,
            imports,
            scope,
            function_headers,
        );
    }

    if (shared.isPointerLike(ctx, expected_type) and isNullPointerLiteral(syntax_arg.*)) {
        const lowered = try ctx.allocator.create(model.Expr);
        lowered.* = .{ .null_ptr = .{
            .ty = expected_type,
            .span = exprSpan(syntax_arg.*),
        } };
        return lowered;
    }

    const lowered = try lowerExpr(ctx, syntax_arg, imports, scope, function_headers);
    const actual_type = model.hir.exprType(lowered.*);
    if (!canPassArgument(ctx, expected_type, actual_type)) {
        try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, span, expected_type, actual_type);
        return error.DiagnosticsEmitted;
    }
    if (expected_type.kind == .string and actual_type.kind == .c_string) {
        const converted = try ctx.allocator.create(model.Expr);
        converted.* = .{ .c_string_to_string = .{
            .value = lowered,
            .span = span,
        } };
        return converted;
    }
    return lowered;
}

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

pub fn canInitializeDeclaredType(ctx: *const shared.Context, expected: model.ResolvedType, actual: model.ResolvedType) bool {
    return typesCompatibleForContext(ctx, expected, actual, false);
}

pub fn canPassArgument(ctx: *const shared.Context, expected: model.ResolvedType, actual: model.ResolvedType) bool {
    return typesCompatibleForContext(ctx, expected, actual, true);
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
        if (!shared.canAssignExactly(param, actual_sig.?.params[index])) return false;
    }
    return actual_sig.?.result.kind == .unknown or shared.canAssignExactly(expected_sig.?.result, actual_sig.?.result);
}

pub fn resolveFieldType(
    ctx: *shared.Context,
    object_type: model.ResolvedType,
    field_name: []const u8,
    span: source_pkg.Span,
) !model.ResolvedType {
    const container_type = resolveFieldContainerType(ctx, object_type) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM047",
            .title = "field access requires a structured type",
            .message = "This member access does not resolve to a Kira or FFI struct value.",
            .labels = &.{
                diagnostics.primaryLabel(span, "field access target is not a struct or pointer-to-struct"),
            },
            .help = "Access fields on a named struct value or a pointer-to-struct type.",
        });
        return error.DiagnosticsEmitted;
    };

    for (shared.namedTypeFields(ctx, container_type)) |field_decl| {
        if (std.mem.eql(u8, field_decl.name, field_name)) return field_decl.ty;
    }

    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM048",
        .title = "unknown field",
        .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' does not declare a field named '{s}'.", .{
            container_type.name orelse "<anonymous>",
            field_name,
        }),
        .labels = &.{
            diagnostics.primaryLabel(span, "field name is not declared on this type"),
        },
        .help = "Check the field spelling or use a type that declares the field.",
    });
    return error.DiagnosticsEmitted;
}

pub fn resolveFieldContainerType(ctx: *shared.Context, ty: model.ResolvedType) ?model.ResolvedType {
    return switch (ty.kind) {
        .native_state_view => if (ty.name) |name|
            .{ .kind = .named, .name = name }
        else
            null,
        .named => if (shared.namedTypeInfo(ctx, ty)) |info|
            switch (info) {
                .ffi_struct => ty,
                .pointer => |value| .{ .kind = .named, .name = value.target_name },
                .alias => |value| resolveFieldContainerType(ctx, value.target),
                else => if (shared.namedTypeHeader(ctx, ty) != null) ty else null,
            }
        else if (shared.namedTypeHeader(ctx, ty) != null)
            ty
        else
            null,
        else => null,
    };
}

pub fn isNullPointerLiteral(expr: syntax.ast.Expr) bool {
    return switch (expr) {
        .integer => |node| node.value == 0,
        else => false,
    };
}

pub fn exprSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        .integer => |node| node.span,
        .float => |node| node.span,
        .string => |node| node.span,
        .bool => |node| node.span,
        .identifier => |node| node.span,
        .array => |node| node.span,
        .callback => |node| node.span,
        .struct_literal => |node| node.span,
        .native_state => |node| node.span,
        .native_user_data => |node| node.span,
        .native_recover => |node| node.span,
        .ownership => |node| node.span,
        .unary => |node| node.span,
        .binary => |node| node.span,
        .conditional => |node| node.span,
        .member => |node| node.span,
        .index => |node| node.span,
        .call => |node| node.span,
    };
}

pub fn qualifiedLeaf(name: []const u8) []const u8 {
    const index = std.mem.lastIndexOfScalar(u8, name, '.') orelse return name;
    return name[index + 1 ..];
}

pub fn resolveSyntaxExprType(ctx: *shared.Context, expr: *syntax.ast.Expr, span: source_pkg.Span) anyerror!model.ResolvedType {
    return switch (expr.*) {
        .integer => .{ .kind = .integer },
        .float => .{ .kind = .float },
        .string => .{ .kind = .string },
        .bool => .{ .kind = .boolean },
        .array => |node| try resolveSyntaxArrayLiteralType(ctx, node.elements, node.span),
        .callback => .{ .kind = .unknown },
        .struct_literal => |node| try shared.typeFromSyntax(ctx, .{ .named = node.type_name }),
        .native_state => |node| blk: {
            const value_ty = try resolveSyntaxExprType(ctx, node.value, span);
            break :blk if (value_ty.kind == .named and value_ty.name != null)
                .{ .kind = .native_state, .name = value_ty.name }
            else
                .{ .kind = .unknown };
        },
        .native_user_data => .{ .kind = .raw_ptr, .name = "RawPtr" },
        .native_recover => |node| blk: {
            const recovered_ty = try shared.typeFromSyntaxChecked(ctx, node.state_type.*);
            break :blk if (recovered_ty.kind == .named and recovered_ty.name != null)
                .{ .kind = .native_state_view, .name = recovered_ty.name }
            else
                .{ .kind = .unknown };
        },
        .ownership => |node| try resolveSyntaxExprType(ctx, node.operand, span),
        .call => |node| {
            if (node.callee.* == .identifier) {
                const name = node.callee.identifier.name.segments[0].text;
                if (ctx.type_headers) |headers| {
                    if (headers.get(name)) |_| {
                        return .{ .kind = .named, .name = try ctx.allocator.dupe(u8, name) };
                    }
                }
                if (node.args.len > 0) {
                    for (node.args) |arg| {
                        if (arg.label != null) return .{ .kind = .named, .name = try ctx.allocator.dupe(u8, name) };
                    }
                }
            }
            return .{ .kind = .unknown };
        },
        .member => |node| blk: {
            if (node.object.* == .identifier and ctx.enum_headers != null) {
                const enum_name = node.object.identifier.name.segments[node.object.identifier.name.segments.len - 1].text;
                if (ctx.enum_headers.?.get(enum_name)) |enum_decl| {
                    for (enum_decl.variants) |variant_decl| {
                        if (std.mem.eql(u8, variant_decl.name, node.member) and variant_decl.payload_ty == null) {
                            break :blk .{ .kind = .enum_instance, .name = try ctx.allocator.dupe(u8, enum_name) };
                        }
                    }
                }
            }
            break :blk .{ .kind = .unknown };
        },
        .index => |node| blk: {
            const object_ty = try resolveSyntaxExprType(ctx, node.object, span);
            break :blk try resolveIndexElementType(ctx, object_ty, node.span);
        },
        else => .{ .kind = .unknown },
    };
}

pub fn resolveLoweredValueType(ctx: *shared.Context, explicit_type_expr: ?*syntax.ast.TypeExpr, value_expr: ?*model.Expr, span: source_pkg.Span) !model.ResolvedType {
    if (explicit_type_expr) |type_expr| {
        const explicit_type = try shared.typeFromSyntaxChecked(ctx, type_expr.*);
        if (value_expr) |expr| {
            const actual = model.hir.exprType(expr.*);
            if (explicit_type.kind == .array and actual.kind == .array and actual.name == null) {
                return explicit_type;
            }
            if (!(shared.canAssignInContext(ctx, explicit_type, actual) or canPassArgument(ctx, explicit_type, actual))) {
                try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, span, explicit_type, actual);
                return error.DiagnosticsEmitted;
            }
        }
        return explicit_type;
    }
    if (value_expr) |expr| return model.hir.exprType(expr.*);
    try shared.emitAmbiguousInference(ctx.allocator, ctx.diagnostics, span);
    return error.DiagnosticsEmitted;
}

pub fn resolveValueType(ctx: *shared.Context, explicit_type_expr: ?*syntax.ast.TypeExpr, value_expr: ?*syntax.ast.Expr, span: source_pkg.Span) !model.ResolvedType {
    if (explicit_type_expr) |type_expr| {
        const explicit_type = try shared.typeFromSyntaxChecked(ctx, type_expr.*);
        if (value_expr) |expr| {
            const inferred = try resolveSyntaxExprType(ctx, expr, span);
            if (explicit_type.kind == .callback and inferred.kind == .unknown and isFunctionNameExpr(expr.*)) return explicit_type;
            if (explicit_type.kind == .array and inferred.kind == .array and inferred.name == null) return explicit_type;
            if (inferred.kind == .unknown and syntaxExprMatchesExplicitType(expr, explicit_type)) return explicit_type;
            if (!(shared.canAssignInContext(ctx, explicit_type, inferred) or canPassArgument(ctx, explicit_type, inferred))) {
                try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, span, explicit_type, inferred);
                return error.DiagnosticsEmitted;
            }
        }
        return explicit_type;
    }
    if (value_expr) |expr| return resolveSyntaxExprType(ctx, expr, span);
    try shared.emitAmbiguousInference(ctx.allocator, ctx.diagnostics, span);
    return error.DiagnosticsEmitted;
}

pub fn syntaxExprMatchesExplicitType(expr: *syntax.ast.Expr, explicit_type: model.ResolvedType) bool {
    if (explicit_type.kind == .array) {
        if (expr.* != .array or explicit_type.name == null) return false;
        const element_type = shared.resolvedTypeFromText(explicit_type.name.?) catch return false;
        for (expr.array.elements) |element| {
            if (!syntaxExprMatchesExplicitType(element, element_type)) return false;
        }
        return true;
    }
    if (explicit_type.kind == .enum_instance and explicit_type.name != null) {
        return switch (expr.*) {
            .call => |node| switch (node.callee.*) {
                .identifier => |value| qualifiedNameRootMatches(value.name, explicit_type.name.?),
                .member => |member| enumMemberMatches(member, explicit_type.name.?),
                else => false,
            },
            .identifier => |value| qualifiedNameRootMatches(value.name, explicit_type.name.?),
            .member => |node| enumMemberMatches(node, explicit_type.name.?),
            else => false,
        };
    }
    if (explicit_type.kind != .named or explicit_type.name == null) return false;
    return switch (expr.*) {
        .struct_literal => |node| std.mem.eql(u8, node.type_name.segments[node.type_name.segments.len - 1].text, explicit_type.name.?),
        .call => |node| switch (node.callee.*) {
            .identifier => |value| std.mem.eql(u8, value.name.segments[value.name.segments.len - 1].text, explicit_type.name.?) or qualifiedNameRootMatches(value.name, explicit_type.name.?),
            .member => |value| std.mem.eql(u8, value.member, explicit_type.name.?) or enumMemberMatches(value, explicit_type.name.?),
            else => false,
        },
        .identifier => |value| qualifiedNameRootMatches(value.name, explicit_type.name.?),
        .member => |node| switch (node.object.*) {
            .identifier => |value| std.mem.eql(u8, value.name.segments[value.name.segments.len - 1].text, explicit_type.name.?),
            else => false,
        },
        else => false,
    };
}

fn qualifiedNameRootMatches(name: syntax.ast.QualifiedName, expected_name: []const u8) bool {
    return name.segments.len > 1 and std.mem.eql(u8, name.segments[0].text, expected_name);
}

fn enumMemberMatches(node: syntax.ast.MemberExpr, enum_name: []const u8) bool {
    return switch (node.object.*) {
        .identifier => |value| std.mem.eql(u8, value.name.segments[value.name.segments.len - 1].text, enum_name),
        else => false,
    };
}

pub fn resolveFunctionReturnType(ctx: *shared.Context, explicit_return_type: model.ResolvedType, body: []const model.Statement) !model.ResolvedType {
    var inferred: ?model.ResolvedType = null;
    for (body) |statement| {
        if (statement != .return_stmt) continue;
        const return_stmt = statement.return_stmt;
        const actual = if (return_stmt.value) |expr| model.hir.exprType(expr.*) else model.ResolvedType{ .kind = .void };
        if (explicit_return_type.kind != .unknown) {
            if (!typesCompatibleForContext(ctx, explicit_return_type, actual, false)) {
                try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, return_stmt.span, explicit_return_type, actual);
                return error.DiagnosticsEmitted;
            }
            continue;
        }
        if (inferred == null) {
            inferred = actual;
        } else if (inferred.?.eql(actual)) {
            continue;
        } else {
            inferred = try resolveConditionalType(ctx, inferred.?, actual, return_stmt.span);
        }
    }
    if (explicit_return_type.kind != .unknown) return explicit_return_type;
    return inferred orelse .{ .kind = .void };
}

pub fn resolveBinaryType(ctx: *shared.Context, op: syntax.ast.BinaryOp, lhs: *model.Expr, rhs: *model.Expr, span: source_pkg.Span) !model.ResolvedType {
    const lhs_ty = model.hir.exprType(lhs.*);
    const rhs_ty = model.hir.exprType(rhs.*);
    return switch (op) {
        .add, .subtract, .multiply, .divide, .modulo => blk: {
            if (lhs_ty.eql(rhs_ty) and (lhs_ty.kind == .integer or lhs_ty.kind == .float)) break :blk lhs_ty;
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM013",
                .title = "invalid binary operator types",
                .message = "Arithmetic operators require both operands to use the same numeric type.",
                .labels = &.{
                    diagnostics.primaryLabel(span, "operands do not use compatible numeric types"),
                },
                .help = "Make both operands integers or both operands floats, or add an explicit type declaration where coercion is allowed.",
            });
            return error.DiagnosticsEmitted;
        },
        .equal, .not_equal, .less, .less_equal, .greater, .greater_equal => blk: {
            if (lhs_ty.eql(rhs_ty)) break :blk .{ .kind = .boolean };
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM028",
                .title = "comparison requires matching types",
                .message = "Comparison operators require both sides to have the same type.",
                .labels = &.{
                    diagnostics.primaryLabel(span, "comparison uses incompatible operand types"),
                },
                .help = "Make both operands the same type before comparing them.",
            });
            return error.DiagnosticsEmitted;
        },
        .logical_and, .logical_or => blk: {
            if (lhs_ty.kind == .boolean and rhs_ty.kind == .boolean) break :blk .{ .kind = .boolean };
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM034",
                .title = "logical operators require booleans",
                .message = "Logical `&&` and `||` require both operands to be boolean values.",
                .labels = &.{
                    diagnostics.primaryLabel(span, "logical operands are not both booleans"),
                },
                .help = "Ensure both sides resolve to `Bool` before using a logical operator.",
            });
            return error.DiagnosticsEmitted;
        },
    };
}

pub fn resolveConditionalType(
    ctx: *shared.Context,
    then_ty: model.ResolvedType,
    else_ty: model.ResolvedType,
    span: source_pkg.Span,
) !model.ResolvedType {
    if (then_ty.eql(else_ty)) return then_ty;
    if (shared.canAssignInContext(ctx, then_ty, else_ty) and !shared.canAssignInContext(ctx, else_ty, then_ty)) return then_ty;
    if (shared.canAssignInContext(ctx, else_ty, then_ty) and !shared.canAssignInContext(ctx, then_ty, else_ty)) return else_ty;
    if (shared.commonClassType(ctx, then_ty, else_ty)) |common_ty| return common_ty;

    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM033",
        .title = "conditional branches require compatible types",
        .message = "Both branches of a conditional expression must resolve to the same type or a compatible coercion target.",
        .labels = &.{
            diagnostics.primaryLabel(span, "conditional branches do not agree on a result type"),
        },
        .help = "Make both branches the same type, or use an explicit coercion target that both branches can satisfy.",
    });
    return error.DiagnosticsEmitted;
}

fn typesCompatibleForContext(
    ctx: *const shared.Context,
    expected: model.ResolvedType,
    actual: model.ResolvedType,
    allow_numeric_widening: bool,
) bool {
    if (allow_numeric_widening) {
        if (shared.canAssignInContext(ctx, expected, actual)) return true;
    } else if (shared.canAssignExactly(expected, actual)) {
        return true;
    }

    if (expected.kind == .array and actual.kind == .array) {
        if (actual.name == null) return true;
        if (expected.name == null) return actual.name == null;
        const expected_element = shared.resolvedTypeFromText(expected.name.?) catch return false;
        const actual_element = shared.resolvedTypeFromText(actual.name.?) catch return false;
        return typesCompatibleForContext(ctx, expected_element, actual_element, allow_numeric_widening);
    }

    if (shared.isAssignableClassValue(ctx, expected, actual)) return true;

    if (expected.kind == .callback and actual.kind == .callback) {
        return callbackTypesCompatible(expected, actual);
    }
    if (expected.kind == .c_string and actual.kind == .string) return true;
    if (expected.kind == .string and actual.kind == .c_string) return true;
    if (shared.isPointerLike(ctx, expected) and actual.kind == .raw_ptr) return true;
    if (expected.kind == .raw_ptr and actual.kind == .named) {
        if (shared.namedTypeInfo(ctx, actual)) |info| {
            return switch (info) {
                .ffi_struct => true,
                .alias => |value| typesCompatibleForContext(ctx, expected, value.target, allow_numeric_widening),
                else => false,
            };
        }
    }
    if (expected.kind == .named and actual.kind == .named) {
        if (shared.namedTypeInfo(ctx, expected)) |info| {
            return switch (info) {
                .alias => |value| typesCompatibleForContext(ctx, value.target, actual, allow_numeric_widening),
                .pointer => |value| std.mem.eql(u8, value.target_name, actual.name orelse ""),
                else => false,
            };
        }
    }
    return false;
}

fn isFunctionNameExpr(expr: syntax.ast.Expr) bool {
    return switch (expr) {
        .identifier, .member => true,
        else => false,
    };
}

fn emitDeclaredInitializerMismatch(
    ctx: *shared.Context,
    initializer_span: source_pkg.Span,
    declared_type: model.ResolvedType,
    actual_type: model.ResolvedType,
) !void {
    const help = if (declared_type.kind == .float and actual_type.kind == .integer)
        "Use a floating-point initializer expression such as `0.0`. Kira does not implicitly convert integer literals in an explicit typed declaration."
    else
        "Change the initializer expression so it already has the declared type, or remove the explicit type annotation and let Kira infer the declaration.";

    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM085",
        .title = "initializer does not match declared type",
        .message = try std.fmt.allocPrint(
            ctx.allocator,
            "This declaration annotates {s}, but the initializer expression resolves to {s}.",
            .{ shared.typeLabel(declared_type), shared.typeLabel(actual_type) },
        ),
        .labels = &.{diagnostics.primaryLabel(initializer_span, "initializer expression does not match the declared type")},
        .help = help,
    });
}

pub fn resolveArrayLiteralType(ctx: *shared.Context, elements: []const *model.Expr, span: source_pkg.Span) !model.ResolvedType {
    if (elements.len == 0) return .{ .kind = .array };

    var element_ty = model.hir.exprType(elements[0].*);
    for (elements[1..]) |element| {
        const next_ty = model.hir.exprType(element.*);
        if (element_ty.eql(next_ty)) continue;
        if (shared.commonClassType(ctx, element_ty, next_ty)) |common_ty| {
            element_ty = common_ty;
            continue;
        }
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM050",
            .title = "array literal requires a consistent element type",
            .message = "All elements in an executable array literal must resolve to the same type.",
            .labels = &.{
                diagnostics.primaryLabel(span, "array literal mixes incompatible element types"),
            },
            .help = "Make every array element the same type, or split the values into separate arrays.",
        });
        return error.DiagnosticsEmitted;
    }

    return .{
        .kind = .array,
        .name = try shared.typeTextFromResolved(ctx.allocator, element_ty),
    };
}

pub fn resolveSyntaxArrayLiteralType(ctx: *shared.Context, elements: []const *syntax.ast.Expr, span: source_pkg.Span) anyerror!model.ResolvedType {
    if (elements.len == 0) return .{ .kind = .array };

    var element_ty = try resolveSyntaxExprType(ctx, elements[0], span);
    for (elements[1..]) |element| {
        const next_ty = try resolveSyntaxExprType(ctx, element, span);
        if (element_ty.eql(next_ty)) continue;
        if (shared.commonClassType(ctx, element_ty, next_ty)) |common_ty| {
            element_ty = common_ty;
            continue;
        }
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM050",
            .title = "array literal requires a consistent element type",
            .message = "All elements in an executable array literal must resolve to the same type.",
            .labels = &.{
                diagnostics.primaryLabel(span, "array literal mixes incompatible element types"),
            },
            .help = "Make every array element the same type, or split the values into separate arrays.",
        });
        return error.DiagnosticsEmitted;
    }

    return .{
        .kind = .array,
        .name = try shared.typeTextFromResolved(ctx.allocator, element_ty),
    };
}

pub fn resolveArrayElementType(ctx: *shared.Context, array_ty: model.ResolvedType, span: source_pkg.Span) !model.ResolvedType {
    return resolveElementType(ctx, array_ty, span, .for_loop);
}

pub fn resolveIndexElementType(ctx: *shared.Context, array_ty: model.ResolvedType, span: source_pkg.Span) !model.ResolvedType {
    return resolveElementType(ctx, array_ty, span, .index);
}

const ElementUse = enum {
    for_loop,
    index,
};

fn resolveElementType(
    ctx: *shared.Context,
    array_ty: model.ResolvedType,
    span: source_pkg.Span,
    use: ElementUse,
) !model.ResolvedType {
    if (array_ty.kind == .named) {
        if (shared.namedTypeInfo(ctx, array_ty)) |info| {
            switch (info) {
                .array => |value| return value.element,
                .alias => |value| return resolveElementType(ctx, value.target, span, use),
                else => {},
            }
        }
    }

    if (array_ty.kind != .array) {
        const title = switch (use) {
            .for_loop => "for loop requires an array iterator",
            .index => "indexing requires an array value",
        };
        const message = switch (use) {
            .for_loop => "Executable `for` loops currently iterate over array values.",
            .index => "Index expressions can only select elements from array values.",
        };
        const label_text = switch (use) {
            .for_loop => "iterator is not an array value",
            .index => "indexed value is not an array",
        };
        const help = switch (use) {
            .for_loop => "Use an array value in the `for ... in ...` position.",
            .index => "Use `value[index]` only when `value` has an array or fixed-array FFI type.",
        };
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM051",
            .title = title,
            .message = message,
            .labels = &.{
                diagnostics.primaryLabel(span, label_text),
            },
            .help = help,
        });
        return error.DiagnosticsEmitted;
    }
    if (array_ty.name == null) return .{ .kind = .unknown };
    return try shared.resolvedTypeFromText(array_ty.name.?);
}

pub fn flattenCalleeName(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .identifier => |node| allocator.dupe(u8, node.name.segments[0].text),
        .member => flattenMemberExprPath(allocator, expr),
        else => allocator.dupe(u8, "<expr>"),
    };
}

pub fn flattenMemberExpr(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) !struct { root: []const u8, path: []const u8 } {
    const path = try flattenMemberExprPath(allocator, expr);
    const root_end = std.mem.indexOfScalar(u8, path, '.') orelse path.len;
    return .{
        .root = try allocator.dupe(u8, path[0..root_end]),
        .path = path,
    };
}

pub fn flattenMemberExprPath(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .member => |node| blk: {
            const left = try flattenMemberExprPath(allocator, node.object);
            break :blk try std.fmt.allocPrint(allocator, "{s}.{s}", .{ left, node.member });
        },
        .identifier => |node| allocator.dupe(u8, node.name.segments[0].text),
        else => allocator.dupe(u8, "<expr>"),
    };
}
