const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");
const parent = @import("lower_exprs.zig");
const resolveFieldContainerType = parent.resolveFieldContainerType;
const lowerCallArgument = parent.lowerCallArgument;
const lowerExpectedValue = parent.lowerExpectedValue;
const lowerBlockStatements = parent.lowerBlockStatements;
const lowerBuilderBlock = parent.lowerBuilderBlock;
const resolveFunctionReturnType = parent.resolveFunctionReturnType;
pub fn lowerImplicitSelfFieldExpr(
    ctx: *shared.Context,
    scope: *model.Scope,
    name: []const u8,
    span: source_pkg.Span,
) !?model.Expr {
    const self_binding = scope.get("self") orelse return null;
    const owner_type = resolveFieldContainerType(ctx, self_binding.ty) orelse return null;
    const resolved_field = (try resolveFieldMemberOrNull(ctx, owner_type, name, span)) orelse return null;
    return .{ .field = .{
        .object = try makeSelfLocalExpr(ctx, self_binding, span),
        .container_type_name = try ctx.allocator.dupe(u8, owner_type.name orelse return error.DiagnosticsEmitted),
        .field_name = try ctx.allocator.dupe(u8, name),
        .field_index = resolved_field.slot_index,
        .ty = resolved_field.ty,
        .storage = resolved_field.storage,
        .span = span,
    } };
}

pub fn lowerImplicitSelfMethodCall(
    ctx: *shared.Context,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !?model.Expr {
    if (node.callee.* != .identifier) return null;

    const self_binding = scope.get("self") orelse return null;
    const owner_type = resolveFieldContainerType(ctx, self_binding.ty) orelse return null;
    const resolved_method = (try resolveMethodMemberOrNull(ctx, owner_type, node.callee.identifier.name.segments[0].text, node.span)) orelse return null;
    const receiver = try makeSelfLocalExpr(ctx, self_binding, node.span);
    return (try buildDispatchedMethodCallExpr(ctx, resolved_method, receiver, owner_type, node, imports, scope, function_headers)).*;
}

pub fn makeSelfLocalExpr(
    ctx: *shared.Context,
    self_binding: model.LocalBinding,
    span: source_pkg.Span,
) !*model.Expr {
    const self_expr = try ctx.allocator.create(model.Expr);
    self_expr.* = .{ .local = .{
        .local_id = self_binding.id,
        .name = try ctx.allocator.dupe(u8, "self"),
        .ty = self_binding.ty,
        .storage = self_binding.storage,
        .span = span,
    } };
    return self_expr;
}

pub fn makeParentViewExpr(
    ctx: *shared.Context,
    object: *model.Expr,
    target_type_name: []const u8,
    offset: u32,
    span: source_pkg.Span,
) !*model.Expr {
    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = .{ .parent_view = .{
        .object = object,
        .ty = .{ .kind = .named, .name = try ctx.allocator.dupe(u8, target_type_name) },
        .offset = offset,
        .span = span,
    } };
    return lowered;
}

pub fn lowerParentQualifiedFieldExpr(
    ctx: *shared.Context,
    node: syntax.ast.MemberExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !?model.Expr {
    _ = function_headers;
    const qualifier = parentQualifierName(node.object) orelse return null;
    if (scope.get(qualifier) != null or shared.isImportedRoot(ctx, qualifier, imports)) return null;

    const self_binding = scope.get("self") orelse return null;
    const self_type = resolveFieldContainerType(ctx, self_binding.ty) orelse return null;
    const parent_view = resolveParentViewOrNullNoDiag(ctx, self_type, qualifier) orelse {
        if ((ctx.type_headers != null and (ctx.type_headers.?.get(qualifier) != null)) or ctx.imported_globals.findType(qualifier) != null) return null;
        return null;
    };
    const receiver = try makeParentViewExpr(ctx, try makeSelfLocalExpr(ctx, self_binding, node.span), parent_view.type_name, parent_view.offset, node.span);
    const resolved_field = try resolveFieldMember(ctx, .{ .kind = .named, .name = parent_view.type_name }, node.member, node.span);
    return .{ .field = .{
        .object = receiver,
        .container_type_name = try ctx.allocator.dupe(u8, parent_view.type_name),
        .field_name = try ctx.allocator.dupe(u8, node.member),
        .field_index = resolved_field.slot_index,
        .ty = resolved_field.ty,
        .storage = resolved_field.storage,
        .span = node.span,
    } };
}

pub fn lowerParentQualifiedMethodCall(
    ctx: *shared.Context,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !?model.Expr {
    const member = if (node.callee.* == .member) node.callee.member else return null;
    const qualifier = parentQualifierName(member.object) orelse return null;
    if (scope.get(qualifier) != null or shared.isImportedRoot(ctx, qualifier, imports)) return null;

    const self_binding = scope.get("self") orelse return null;
    const self_type = resolveFieldContainerType(ctx, self_binding.ty) orelse return null;
    const parent_view = resolveParentViewOrNullNoDiag(ctx, self_type, qualifier) orelse {
        if ((ctx.type_headers != null and (ctx.type_headers.?.get(qualifier) != null)) or ctx.imported_globals.findType(qualifier) != null) {
            _ = try resolveParentView(ctx, self_type, qualifier, node.span);
            unreachable;
        }
        return null;
    };
    const parent_receiver = try makeParentViewExpr(ctx, try makeSelfLocalExpr(ctx, self_binding, node.span), parent_view.type_name, parent_view.offset, node.span);
    const resolved_method = try resolveMethodMember(ctx, .{ .kind = .named, .name = parent_view.type_name }, member.member, node.span);
    const receiver = try adjustMethodReceiver(ctx, parent_receiver, .{ .kind = .named, .name = parent_view.type_name }, resolved_method, node.span);
    return (try buildResolvedMethodCallExpr(ctx, resolved_method, receiver, node, imports, scope, function_headers)).*;
}

pub fn parentQualifierName(expr: *syntax.ast.Expr) ?[]const u8 {
    if (expr.* != .identifier) return null;
    if (expr.identifier.name.segments.len != 1) return null;
    return expr.identifier.name.segments[0].text;
}

pub fn resolveParentView(
    ctx: *shared.Context,
    self_type: model.ResolvedType,
    qualifier_name: []const u8,
    span: source_pkg.Span,
) !shared.ParentView {
    const header = shared.namedTypeHeader(ctx, self_type) orelse return error.DiagnosticsEmitted;
    var match: ?shared.ParentView = null;
    for (header.parent_views) |parent_view| {
        if (!std.mem.eql(u8, parent_view.type_name, qualifier_name)) continue;
        if (match != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM061",
                .title = "invalid parent-qualified member reference",
                .message = try std.fmt.allocPrint(ctx.allocator, "The parent type name '{s}' is ambiguous in this inheritance graph.", .{qualifier_name}),
                .labels = &.{diagnostics.primaryLabel(span, "parent qualification does not identify a unique inherited parent")},
                .help = "Qualify through a parent type name that is unique in the current inheritance graph.",
            });
            return error.DiagnosticsEmitted;
        }
        match = parent_view;
    }
    if (match == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM061",
            .title = "invalid parent-qualified member reference",
            .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' is not an inherited parent of the current type.", .{qualifier_name}),
            .labels = &.{diagnostics.primaryLabel(span, "this qualifier is not a valid inherited parent here")},
            .help = "Use an inherited parent type name, or access the member without parent qualification.",
        });
        return error.DiagnosticsEmitted;
    }
    return match.?;
}

pub fn resolveParentViewOrNullNoDiag(
    ctx: *shared.Context,
    self_type: model.ResolvedType,
    qualifier_name: []const u8,
) ?shared.ParentView {
    const header = shared.namedTypeHeader(ctx, self_type) orelse return null;
    var match: ?shared.ParentView = null;
    for (header.parent_views) |parent_view| {
        if (!std.mem.eql(u8, parent_view.type_name, qualifier_name)) continue;
        if (match != null) return null;
        match = parent_view;
    }
    return match;
}

pub fn resolveFieldMemberOrNull(
    ctx: *shared.Context,
    object_type: model.ResolvedType,
    field_name: []const u8,
    span: source_pkg.Span,
) !?model.Field {
    const container_type = resolveFieldContainerType(ctx, object_type) orelse return null;
    const header = shared.namedTypeHeader(ctx, container_type) orelse return null;

    var match: ?model.Field = null;
    for (header.fields) |field_decl| {
        if (!std.mem.eql(u8, field_decl.name, field_name)) continue;
        if (match != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM057",
                .title = "ambiguous inherited field lookup",
                .message = try std.fmt.allocPrint(ctx.allocator, "More than one inherited field named '{s}' is visible on '{s}'.", .{ field_name, container_type.name orelse "<anonymous>" }),
                .labels = &.{diagnostics.primaryLabel(span, "field lookup is ambiguous")},
                .help = "Use explicit parent qualification or remove the conflicting inherited field names.",
            });
            return error.DiagnosticsEmitted;
        }
        match = field_decl;
    }
    return match;
}

pub fn resolveFieldMember(
    ctx: *shared.Context,
    object_type: model.ResolvedType,
    field_name: []const u8,
    span: source_pkg.Span,
) !model.Field {
    const container_type = resolveFieldContainerType(ctx, object_type) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM047",
            .title = "field access requires a structured type",
            .message = "This member access does not resolve to a Kira or FFI struct value.",
            .labels = &.{diagnostics.primaryLabel(span, "field access target is not a struct or pointer-to-struct")},
            .help = "Access fields on a named struct value or a pointer-to-struct type.",
        });
        return error.DiagnosticsEmitted;
    };
    const header = shared.namedTypeHeader(ctx, container_type) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM047",
            .title = "field access requires a structured type",
            .message = "This member access does not resolve to a Kira or FFI struct value.",
            .labels = &.{diagnostics.primaryLabel(span, "field access target is not a struct or pointer-to-struct")},
            .help = "Access fields on a named struct value or a pointer-to-struct type.",
        });
        return error.DiagnosticsEmitted;
    };

    var match: ?model.Field = null;
    for (header.fields) |field_decl| {
        if (!std.mem.eql(u8, field_decl.name, field_name)) continue;
        if (match != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM057",
                .title = "ambiguous inherited field lookup",
                .message = try std.fmt.allocPrint(ctx.allocator, "More than one inherited field named '{s}' is visible on '{s}'.", .{ field_name, container_type.name orelse "<anonymous>" }),
                .labels = &.{diagnostics.primaryLabel(span, "field lookup is ambiguous")},
                .help = "Use explicit parent qualification or remove the conflicting inherited field names.",
            });
            return error.DiagnosticsEmitted;
        }
        match = field_decl;
    }

    if (match == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM048",
            .title = "unknown field",
            .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' does not declare a field named '{s}'.", .{
                container_type.name orelse "<anonymous>",
                field_name,
            }),
            .labels = &.{diagnostics.primaryLabel(span, "field name is not declared on this type")},
            .help = "Check the field spelling or use a type that declares the field.",
        });
        return error.DiagnosticsEmitted;
    }
    return match.?;
}

pub fn resolveMethodMemberOrNull(
    ctx: *shared.Context,
    object_type: model.ResolvedType,
    method_name: []const u8,
    span: source_pkg.Span,
) !?shared.MethodMember {
    const container_type = resolveFieldContainerType(ctx, object_type) orelse return null;
    const header = shared.namedTypeHeader(ctx, container_type) orelse return null;

    var match: ?shared.MethodMember = null;
    for (header.methods) |method_decl| {
        if (!std.mem.eql(u8, method_decl.name, method_name)) continue;
        if (match != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM056",
                .title = "ambiguous inherited method lookup",
                .message = try std.fmt.allocPrint(ctx.allocator, "More than one inherited method named '{s}' is visible on '{s}'.", .{ method_name, container_type.name orelse "<anonymous>" }),
                .labels = &.{diagnostics.primaryLabel(span, "method lookup is ambiguous")},
                .help = "Use explicit parent qualification or override the inherited method explicitly.",
            });
            return error.DiagnosticsEmitted;
        }
        match = method_decl;
    }
    return match;
}

pub fn resolveMethodMember(
    ctx: *shared.Context,
    object_type: model.ResolvedType,
    method_name: []const u8,
    span: source_pkg.Span,
) !shared.MethodMember {
    const container_type = resolveFieldContainerType(ctx, object_type) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM065",
            .title = "method call requires a structured type",
            .message = "This call target does not resolve to a Kira type with methods.",
            .labels = &.{diagnostics.primaryLabel(span, "method receiver is not a Kira type value")},
            .help = "Call methods on named Kira types only.",
        });
        return error.DiagnosticsEmitted;
    };
    const header = shared.namedTypeHeader(ctx, container_type) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM065",
            .title = "method call requires a structured type",
            .message = "This call target does not resolve to a Kira type with methods.",
            .labels = &.{diagnostics.primaryLabel(span, "method receiver is not a Kira type value")},
            .help = "Call methods on named Kira types only.",
        });
        return error.DiagnosticsEmitted;
    };

    var match: ?shared.MethodMember = null;
    for (header.methods) |method_decl| {
        if (!std.mem.eql(u8, method_decl.name, method_name)) continue;
        if (match != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM056",
                .title = "ambiguous inherited method lookup",
                .message = try std.fmt.allocPrint(ctx.allocator, "More than one inherited method named '{s}' is visible on '{s}'.", .{ method_name, container_type.name orelse "<anonymous>" }),
                .labels = &.{diagnostics.primaryLabel(span, "method lookup is ambiguous")},
                .help = "Use explicit parent qualification or override the inherited method explicitly.",
            });
            return error.DiagnosticsEmitted;
        }
        match = method_decl;
    }

    if (match == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM066",
            .title = "unknown method",
            .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' does not declare a method named '{s}'.", .{ container_type.name orelse "<anonymous>", method_name }),
            .labels = &.{diagnostics.primaryLabel(span, "method name is not declared on this type")},
            .help = "Check the method spelling or call a method that the type inherits explicitly.",
        });
        return error.DiagnosticsEmitted;
    }
    return match.?;
}

pub fn adjustMethodReceiver(
    ctx: *shared.Context,
    object: *model.Expr,
    object_type: model.ResolvedType,
    method_decl: shared.MethodMember,
    span: source_pkg.Span,
) !*model.Expr {
    if (method_decl.receiver_offset == 0 and object_type.kind == .named and object_type.name != null and std.mem.eql(u8, object_type.name.?, method_decl.receiver_type_name)) {
        return object;
    }
    return makeParentViewExpr(ctx, object, method_decl.receiver_type_name, method_decl.receiver_offset, span);
}

pub fn buildResolvedMethodCallExpr(
    ctx: *shared.Context,
    method_decl: shared.MethodMember,
    receiver: *model.Expr,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    const lowered = try ctx.allocator.create(model.Expr);
    try lowerResolvedMethodCall(ctx, lowered, method_decl, receiver, node, imports, scope, function_headers);
    return lowered;
}

pub fn buildDispatchedMethodCallExpr(
    ctx: *shared.Context,
    method_decl: shared.MethodMember,
    receiver: *model.Expr,
    receiver_type: model.ResolvedType,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    const lowered = try ctx.allocator.create(model.Expr);
    if (shared.isClassType(ctx, receiver_type) and receiver_type.kind == .named and receiver_type.name != null and shared.hasKnownSubclass(ctx, receiver_type.name.?)) {
        try lowerVirtualMethodCall(ctx, lowered, method_decl, receiver, receiver_type.name.?, node, imports, scope, function_headers);
        return lowered;
    }
    const adjusted_receiver = try adjustMethodReceiver(ctx, receiver, receiver_type, method_decl, node.span);
    try lowerResolvedMethodCall(ctx, lowered, method_decl, adjusted_receiver, node, imports, scope, function_headers);
    return lowered;
}

pub fn lowerResolvedMethodCall(
    ctx: *shared.Context,
    lowered: *model.Expr,
    method_decl: shared.MethodMember,
    receiver: *model.Expr,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    if (function_headers) |headers| {
        if (headers.get(method_decl.full_name)) |resolved_header| {
            const trailing_callback_type = try trailingCallbackType(ctx, node, resolved_header.params[1..]);
            const explicit_param_count = resolved_header.params.len - 1 - (if (trailing_callback_type != null) @as(usize, 1) else 0);
            if (node.args.len != explicit_param_count) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM042",
                    .title = "wrong number of arguments",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The call to '{s}' expected {d} explicit argument(s) but received {d}.", .{ method_decl.full_name, explicit_param_count, node.args.len }),
                    .labels = &.{diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments")},
                    .help = "Update the call so it matches the method signature exactly.",
                });
                return error.DiagnosticsEmitted;
            }

            var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            try args.append(receiver);
            for (node.args, 0..) |arg, index| {
                try args.append(try lowerCallArgument(ctx, arg.value, resolved_header.params[index + 1], shared.paramOwnership(resolved_header, index + 1), method_decl.full_name, imports, scope, headers, node.span));
            }
            if (trailing_callback_type) |callback_type| {
                try args.append(try lowerTrailingCallbackValue(ctx, node, callback_type, imports, scope, headers));
            }

            lowered.* = .{ .call = .{
                .callee_name = method_decl.full_name,
                .function_id = resolved_header.id,
                .args = try args.toOwnedSlice(),
                .trailing_builder = if (trailing_callback_type == null and node.trailing_builder != null) try lowerBuilderBlock(ctx, node.trailing_builder.?, imports, scope) else null,
                .ty = resolved_header.return_type,
                .span = node.span,
            } };
            return;
        }
    }

    if (ctx.imported_globals.findFunction(method_decl.full_name)) |resolved_imported| {
        const trailing_callback_type = try trailingCallbackType(ctx, node, resolved_imported.params[1..]);
        const explicit_param_count = resolved_imported.params.len - 1 - (if (trailing_callback_type != null) @as(usize, 1) else 0);
        if (node.args.len != explicit_param_count) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM042",
                .title = "wrong number of arguments",
                .message = try std.fmt.allocPrint(ctx.allocator, "The call to '{s}' expected {d} explicit argument(s) but received {d}.", .{ method_decl.full_name, explicit_param_count, node.args.len }),
                .labels = &.{diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments")},
                .help = "Update the call so it matches the method signature exactly.",
            });
            return error.DiagnosticsEmitted;
        }

        var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
        try args.append(receiver);
        for (node.args, 0..) |arg, index| {
            const ownership = if (index + 1 < resolved_imported.param_ownership.len) resolved_imported.param_ownership[index + 1] else model.OwnershipMode.owned;
            try args.append(try lowerCallArgument(ctx, arg.value, resolved_imported.params[index + 1], ownership, method_decl.full_name, imports, scope, function_headers orelse return error.DiagnosticsEmitted, node.span));
        }
        if (trailing_callback_type) |callback_type| {
            try args.append(try lowerTrailingCallbackValue(ctx, node, callback_type, imports, scope, function_headers orelse return error.DiagnosticsEmitted));
        }

        lowered.* = .{ .call = .{
            .callee_name = method_decl.full_name,
            .function_id = null,
            .args = try args.toOwnedSlice(),
            .trailing_builder = if (trailing_callback_type == null and node.trailing_builder != null) try lowerBuilderBlock(ctx, node.trailing_builder.?, imports, scope) else null,
            .ty = resolved_imported.return_type,
            .span = node.span,
        } };
        return;
    }

    return error.DiagnosticsEmitted;
}

pub fn lowerVirtualMethodCall(
    ctx: *shared.Context,
    lowered: *model.Expr,
    method_decl: shared.MethodMember,
    receiver: *model.Expr,
    static_type_name: []const u8,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    const resolved_return_type = if (method_decl.return_type.kind == .unknown and function_headers != null)
        if (function_headers.?.get(method_decl.full_name)) |header| header.return_type else method_decl.return_type
    else
        method_decl.return_type;
    const trailing_callback_type = try trailingCallbackType(ctx, node, method_decl.params);
    const explicit_param_count = method_decl.params.len - (if (trailing_callback_type != null) @as(usize, 1) else 0);
    if (node.args.len != explicit_param_count) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM042",
            .title = "wrong number of arguments",
            .message = try std.fmt.allocPrint(ctx.allocator, "The call to '{s}' expected {d} explicit argument(s) but received {d}.", .{ method_decl.full_name, explicit_param_count, node.args.len }),
            .labels = &.{diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments")},
            .help = "Update the call so it matches the method signature exactly.",
        });
        return error.DiagnosticsEmitted;
    }

    var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
    for (node.args, 0..) |arg, index| {
        const ownership = if (index < method_decl.param_ownership.len) method_decl.param_ownership[index] else model.OwnershipMode.owned;
        try args.append(try lowerCallArgument(ctx, arg.value, method_decl.params[index], ownership, method_decl.full_name, imports, scope, function_headers orelse return error.DiagnosticsEmitted, node.span));
    }
    if (trailing_callback_type) |callback_type| {
        try args.append(try lowerTrailingCallbackValue(ctx, node, callback_type, imports, scope, function_headers orelse return error.DiagnosticsEmitted));
    }

    lowered.* = .{ .virtual_call = .{
        .receiver = receiver,
        .static_type_name = try ctx.allocator.dupe(u8, static_type_name),
        .method_name = try ctx.allocator.dupe(u8, method_decl.name),
        .args = try args.toOwnedSlice(),
        .ty = resolved_return_type,
        .span = node.span,
    } };
}

pub fn trailingCallbackType(ctx: *shared.Context, node: syntax.ast.CallExpr, params: []const model.ResolvedType) !?model.ResolvedType {
    const has_explicit_callback = node.trailing_callback != null;
    const has_block = has_explicit_callback or node.trailing_builder != null;
    if (!has_block) return null;
    if (params.len == 0 or params[params.len - 1].kind != .callback) {
        if (has_explicit_callback) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM081",
                .title = "call does not accept a trailing callback",
                .message = "This call site uses trailing callback syntax, but the callee does not declare a final function-typed parameter.",
                .labels = &.{diagnostics.primaryLabel(node.span, "trailing callback cannot bind here")},
                .help = "Add an explicit function-typed final parameter to the callee, or pass an ordinary argument instead.",
            });
            return error.DiagnosticsEmitted;
        }
        return null;
    }
    return params[params.len - 1];
}

pub fn lowerTrailingCallbackValue(
    ctx: *shared.Context,
    node: syntax.ast.CallExpr,
    expected_type: model.ResolvedType,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    if (node.trailing_callback) |callback_block| {
        return lowerCallbackBlockValue(ctx, callback_block.params, callback_block.body, callback_block.span, expected_type, imports, scope, function_headers);
    }
    if (node.trailing_builder) |builder_block| {
        const body = try statementBlockFromBuilder(ctx.allocator, builder_block);
        return lowerCallbackBlockValue(ctx, &.{}, body, builder_block.span, expected_type, imports, scope, function_headers);
    }
    return error.DiagnosticsEmitted;
}

pub fn lowerCallbackBlockValue(
    ctx: *shared.Context,
    params: []const syntax.ast.CallbackParam,
    body: syntax.ast.Block,
    span: source_pkg.Span,
    expected_type: model.ResolvedType,
    imports: []const model.Import,
    capture_scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    const signature = try function_types.parseSignature(ctx.allocator, expected_type) orelse return error.DiagnosticsEmitted;
    if (signature.params.len != params.len) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM083",
            .title = "callback parameter list does not match",
            .message = try std.fmt.allocPrint(ctx.allocator, "This callback expects {d} parameter(s) but declares {d}.", .{ signature.params.len, params.len }),
            .labels = &.{diagnostics.primaryLabel(span, "callback parameters do not match the expected function type")},
            .help = if (signature.params.len == 0)
                "Remove the parameter list, or use an explicit callback type with parameters."
            else
                "Write a parameter list that matches the expected function type exactly.",
        });
        return error.DiagnosticsEmitted;
    }

    var callback_scope = model.Scope{};
    defer callback_scope.deinit(ctx.allocator);
    var lowered_params = std.array_list.Managed(model.Parameter).init(ctx.allocator);
    var captures = std.array_list.Managed(model.Capture).init(ctx.allocator);
    var locals = std.array_list.Managed(model.LocalSymbol).init(ctx.allocator);
    var next_local_id: u32 = 0;

    for (params, 0..) |param, index| {
        const param_ty = signature.params[index];
        const param_ownership = if (index < signature.param_ownership.len) signature.param_ownership[index] else .owned;
        try callback_scope.put(ctx.allocator, param.name, .{
            .id = next_local_id,
            .ty = param_ty,
            .storage = if (param_ownership == .borrow_mut) .mutable else .immutable,
            .ownership = param_ownership,
            .initialized = true,
            .decl_span = param.span,
        });
        try lowered_params.append(.{
            .id = next_local_id,
            .name = try ctx.allocator.dupe(u8, param.name),
            .ty = param_ty,
            .ownership = param_ownership,
            .span = param.span,
        });
        try locals.append(.{
            .id = next_local_id,
            .name = try ctx.allocator.dupe(u8, param.name),
            .ty = param_ty,
            .ownership = param_ownership,
            .is_param = true,
            .span = param.span,
        });
        next_local_id += 1;
    }

    var capture_frame = shared.CallbackCaptureFrame{
        .source_scope = capture_scope,
        .active_scope = &callback_scope,
        .captures = &captures,
        .locals = &locals,
        .next_local_id = &next_local_id,
        .parent = ctx.callback_capture_frame,
    };
    const previous_capture_frame = ctx.callback_capture_frame;
    ctx.callback_capture_frame = &capture_frame;
    defer ctx.callback_capture_frame = previous_capture_frame;

    const lowered_body = try lowerBlockStatements(ctx, body, imports, &callback_scope, &locals, &next_local_id, function_headers, 0, signature.result);
    const return_type = try resolveFunctionReturnType(ctx, signature.result, lowered_body);
    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = .{ .callback = .{
        .params = try lowered_params.toOwnedSlice(),
        .captures = try captures.toOwnedSlice(),
        .locals = try locals.toOwnedSlice(),
        .body = lowered_body,
        .return_type = return_type,
        .ty = expected_type,
        .span = span,
    } };
    return lowered;
}

pub fn statementBlockFromBuilder(allocator: std.mem.Allocator, builder: syntax.ast.BuilderBlock) anyerror!syntax.ast.Block {
    var statements = std.array_list.Managed(syntax.ast.Statement).init(allocator);
    for (builder.items) |item| {
        try statements.append(try statementFromBuilderItem(allocator, item));
    }
    return .{
        .statements = try statements.toOwnedSlice(),
        .span = builder.span,
    };
}

pub fn statementFromBuilderItem(allocator: std.mem.Allocator, item: syntax.ast.BuilderItem) anyerror!syntax.ast.Statement {
    return switch (item) {
        .expr => |value| .{ .expr_stmt = .{ .expr = value.expr, .span = value.span } },
        .if_item => |value| .{ .if_stmt = .{
            .condition = value.condition,
            .then_block = try statementBlockFromBuilder(allocator, value.then_block),
            .else_block = if (value.else_block) |else_block| try statementBlockFromBuilder(allocator, else_block) else null,
            .span = value.span,
        } },
        .for_item => |value| .{ .for_stmt = .{
            .binding_name = value.binding_name,
            .iterator = value.iterator,
            .body = try statementBlockFromBuilder(allocator, value.body),
            .span = value.span,
        } },
        .switch_item => |value| blk: {
            var cases = std.array_list.Managed(syntax.ast.SwitchCase).init(allocator);
            for (value.cases) |case_node| {
                try cases.append(.{
                    .pattern = case_node.pattern,
                    .body = try statementBlockFromBuilder(allocator, case_node.body),
                    .span = case_node.span,
                });
            }
            break :blk .{ .switch_stmt = .{
                .subject = value.subject,
                .cases = try cases.toOwnedSlice(),
                .default_block = if (value.default_block) |default_block| try statementBlockFromBuilder(allocator, default_block) else null,
                .span = value.span,
            } };
        },
    };
}
