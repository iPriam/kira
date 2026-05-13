const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");
const parent = @import("lower_exprs.zig");
const lowerExpr = parent.lowerExpr;
const lowerExpectedValue = parent.lowerExpectedValue;
const lowerImplicitSelfFieldExpr = parent.lowerImplicitSelfFieldExpr;
const lowerImplicitSelfMethodCall = parent.lowerImplicitSelfMethodCall;
const lowerParentQualifiedFieldExpr = parent.lowerParentQualifiedFieldExpr;
const lowerParentQualifiedMethodCall = parent.lowerParentQualifiedMethodCall;
const resolveFieldType = parent.resolveFieldType;
const resolveFieldContainerType = parent.resolveFieldContainerType;
const resolveMethodMember = parent.resolveMethodMember;
const resolveMethodMemberOrNull = parent.resolveMethodMemberOrNull;
const buildDispatchedMethodCallExpr = parent.buildDispatchedMethodCallExpr;
const functionTypeFromHeader = parent.functionTypeFromHeader;
const lowerCallArgument = parent.lowerCallArgument;
const trailingCallbackType = parent.trailingCallbackType;
const lowerTrailingCallbackValue = parent.lowerTrailingCallbackValue;
const lowerBuilderBlock = parent.lowerBuilderBlock;
const isCallableValueExpr = parent.isCallableValueExpr;
const flattenCalleeName = parent.flattenCalleeName;
const flattenMemberExpr = parent.flattenMemberExpr;
const qualifiedLeaf = parent.qualifiedLeaf;
pub fn lowerStructLiteralExpr(
    ctx: *shared.Context,
    node: syntax.ast.StructLiteralExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !model.Expr {
    const callee_name = try shared.qualifiedNameText(ctx.allocator, node.type_name);
    const callee_leaf = node.type_name.segments[node.type_name.segments.len - 1].text;
    return lowerTypeConstruction(ctx, callee_name, callee_leaf, null, node.fields, node.span, imports, scope, function_headers);
}

pub fn lowerTypeConstruction(
    ctx: *shared.Context,
    callee_name: []const u8,
    callee_leaf: []const u8,
    call_args: ?[]const syntax.ast.CallArg,
    literal_fields: []const syntax.ast.StructLiteralField,
    span: source_pkg.Span,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !model.Expr {
    const type_header = if (ctx.type_headers) |headers| headers.get(callee_name) orelse headers.get(callee_leaf) else null;
    const imported_type = ctx.imported_globals.findType(callee_name) orelse ctx.imported_globals.findType(callee_leaf);
    if (type_header == null and imported_type == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM078",
            .title = "unknown type in struct literal",
            .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not find a type named '{s}'.", .{callee_name}),
            .labels = &.{diagnostics.primaryLabel(span, "unknown type")},
            .help = "Declare the type first or import the module that provides it.",
        });
        return error.DiagnosticsEmitted;
    }

    const field_count: usize = if (type_header) |header| header.fields.len else imported_type.?.fields.len;
    const is_ffi_struct = if (type_header) |header|
        header.ffi != null and header.ffi.? == .ffi_struct
    else
        imported_type.?.ffi != null and imported_type.?.ffi.? == .ffi_struct;
    var filled = try ctx.allocator.alloc(bool, field_count);
    @memset(filled, false);
    var fields = std.array_list.Managed(model.ConstructFieldInit).init(ctx.allocator);
    var required_missing = false;

    if (call_args) |items| {
        var next_index: usize = 0;
        for (items) |arg| {
            const field_index = if (arg.label) |label|
                resolveTypeConstructionFieldIndex(ctx, callee_name, callee_leaf, type_header, imported_type, label, arg.span) orelse return error.DiagnosticsEmitted
            else blk: {
                if (next_index >= field_count) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM079",
                        .title = "too many constructor arguments",
                        .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' declares only {d} field(s).", .{ callee_leaf, field_count }),
                        .labels = &.{diagnostics.primaryLabel(arg.span, "extra constructor argument")},
                        .help = "Remove the extra argument or add a field to the type.",
                    });
                    return error.DiagnosticsEmitted;
                }
                while (next_index < field_count and filled[next_index]) next_index += 1;
                if (next_index >= field_count) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM079",
                        .title = "too many constructor arguments",
                        .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' declares only {d} field(s).", .{ callee_leaf, field_count }),
                        .labels = &.{diagnostics.primaryLabel(arg.span, "extra constructor argument")},
                        .help = "Remove the extra argument or add a field to the type.",
                    });
                    return error.DiagnosticsEmitted;
                }
                break :blk next_index;
            };
            if (filled[field_index]) {
                const duplicate_name = if (type_header) |header| header.fields[field_index].name else imported_type.?.fields[field_index].name;
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM080",
                    .title = "duplicate struct field",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The field '{s}' is initialized more than once.", .{duplicate_name}),
                    .labels = &.{diagnostics.primaryLabel(arg.span, "duplicate field initializer")},
                    .help = "Initialize each field at most once.",
                });
                return error.DiagnosticsEmitted;
            }
            const field_ty = if (type_header) |header| header.fields[field_index].ty else imported_type.?.fields[field_index].ty;
            const field_value = if (function_headers) |headers|
                try lowerExpectedValue(ctx, arg.value, field_ty, imports, scope, headers, arg.span)
            else
                try lowerExpr(ctx, arg.value, imports, scope, function_headers);
            const field_name = if (type_header) |header| header.fields[field_index].name else imported_type.?.fields[field_index].name;
            try fields.append(.{
                .field_name = try ctx.allocator.dupe(u8, field_name),
                .field_index = @as(u32, @intCast(field_index)),
                .value = field_value,
                .span = arg.span,
            });
            filled[field_index] = true;
        }
    } else {
        for (literal_fields) |field| {
            const field_index = resolveTypeConstructionFieldIndex(ctx, callee_name, callee_leaf, type_header, imported_type, field.name, field.span) orelse return error.DiagnosticsEmitted;
            if (filled[field_index]) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM080",
                    .title = "duplicate struct field",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The field '{s}' is initialized more than once.", .{field.name}),
                    .labels = &.{diagnostics.primaryLabel(field.span, "duplicate field initializer")},
                    .help = "Initialize each field at most once.",
                });
                return error.DiagnosticsEmitted;
            }
            const field_ty = if (type_header) |header| header.fields[field_index].ty else imported_type.?.fields[field_index].ty;
            const field_value = if (function_headers) |headers|
                try lowerExpectedValue(ctx, field.value, field_ty, imports, scope, headers, field.span)
            else
                try lowerExpr(ctx, field.value, imports, scope, function_headers);
            try fields.append(.{
                .field_name = try ctx.allocator.dupe(u8, field.name),
                .field_index = @as(u32, @intCast(field_index)),
                .value = field_value,
                .span = field.span,
            });
            filled[field_index] = true;
        }
    }

    for (0..field_count) |index| {
        if (filled[index]) continue;
        if (is_ffi_struct) continue;
        if (type_header) |header| {
            if (isTypeConstantField(header.fields[index].ty, header.fields[index].storage, callee_leaf)) {
                continue;
            }
            if (header.fields[index].default_value) |default_value| {
                try fields.append(.{
                    .field_name = try ctx.allocator.dupe(u8, header.fields[index].name),
                    .field_index = @as(u32, @intCast(index)),
                    .value = default_value,
                    .span = span,
                });
                filled[index] = true;
                continue;
            }
        } else if (imported_type.?.fields[index].default_value) |default_value| {
            try fields.append(.{
                .field_name = try ctx.allocator.dupe(u8, imported_type.?.fields[index].name),
                .field_index = @as(u32, @intCast(index)),
                .value = default_value,
                .span = span,
            });
            filled[index] = true;
            continue;
        }
        required_missing = true;
        const field_name = if (type_header) |header| header.fields[index].name else imported_type.?.fields[index].name;
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM081",
            .title = "missing required struct field",
            .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' requires a value for field '{s}'.", .{ callee_leaf, field_name }),
            .labels = &.{diagnostics.primaryLabel(span, "required field is missing")},
            .help = "Initialize the missing field or add a default value on the type declaration.",
        });
        break;
    }
    if (required_missing) return error.DiagnosticsEmitted;

    return .{ .construct = .{
        .type_name = try ctx.allocator.dupe(u8, callee_leaf),
        .fields = try fields.toOwnedSlice(),
        .fill_mode = if (is_ffi_struct) .zeroed_ffi_c_layout else .defaults,
        .ty = .{ .kind = .named, .name = try ctx.allocator.dupe(u8, callee_leaf) },
        .span = span,
    } };
}

pub fn isTypeConstantField(field_ty: model.ResolvedType, storage: model.FieldStorage, owner_type_name: []const u8) bool {
    return storage == .immutable and field_ty.kind == .named and field_ty.name != null and std.mem.eql(u8, field_ty.name.?, owner_type_name);
}

pub fn resolveTypeConstructionFieldIndex(
    ctx: *shared.Context,
    callee_name: []const u8,
    callee_leaf: []const u8,
    type_header: ?shared.TypeHeader,
    imported_type: ?@import("imported_globals.zig").ImportedType,
    field_name: []const u8,
    span: source_pkg.Span,
) ?usize {
    if (type_header) |header| {
        for (header.fields, 0..) |field, index| {
            if (std.mem.eql(u8, field.name, field_name)) return index;
        }
    } else if (imported_type) |type_decl| {
        for (type_decl.fields, 0..) |field, index| {
            if (std.mem.eql(u8, field.name, field_name)) return index;
        }
    }
    diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM082",
        .title = "unknown struct field",
        .message = std.fmt.allocPrint(ctx.allocator, "The type '{s}' does not declare a field named '{s}'.", .{ callee_leaf, field_name }) catch return null,
        .labels = &.{diagnostics.primaryLabel(span, "unknown field")},
        .help = "Use a declared field name or update the type definition.",
    }) catch return null;
    _ = callee_name;
    return null;
}

pub fn lowerCallExpr(
    ctx: *shared.Context,
    lowered: *model.Expr,
    node: syntax.ast.CallExpr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    if (node.callee.* == .member) {
        const member = node.callee.member;
        if (try lowerParentQualifiedMethodCall(ctx, node, imports, scope, function_headers)) |call_expr| {
            lowered.* = call_expr;
            return;
        }
        const flattened_member = try flattenMemberExpr(ctx.allocator, node.callee);
        if (shared.isImportedRoot(flattened_member.root, imports) and scope.get(flattened_member.root) == null) {
            // Imported namespace calls such as `Support.value()` are not instance methods.
        } else {
            const object = try lowerExpr(ctx, member.object, imports, scope, function_headers);
            const object_type = model.hir.exprType(object.*);
            if (object_type.kind == .array and std.mem.eql(u8, member.member, "append")) {
                if (node.args.len != 1) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM042",
                        .title = "wrong number of arguments",
                        .message = "Array append expects exactly one value.",
                        .labels = &.{diagnostics.primaryLabel(node.span, "append call has the wrong number of arguments")},
                        .help = "Call `array.append(value)` with exactly one element.",
                    });
                    return error.DiagnosticsEmitted;
                }
                const element_type = if (object_type.name) |name| try shared.resolvedTypeFromText(name) else model.ResolvedType{ .kind = .unknown };
                const value = try lowerCallArgument(ctx, node.args[0].value, element_type, imports, scope, function_headers orelse return error.DiagnosticsEmitted, node.span);
                const args = try ctx.allocator.alloc(*model.Expr, 2);
                args[0] = object;
                args[1] = value;
                lowered.* = .{ .call = .{
                    .callee_name = "array.append",
                    .function_id = null,
                    .args = args,
                    .ty = .{ .kind = .void },
                    .span = node.span,
                } };
                return;
            }
            if (object_type.kind == .array) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM101",
                    .title = "unsupported array method",
                    .message = try std.fmt.allocPrint(ctx.allocator, "Array method '{s}' is not supported.", .{member.member}),
                    .labels = &.{diagnostics.primaryLabel(node.span, "unsupported array method")},
                    .help = "Use `array.append(value)` for growth; resizing, pop, and remove are not part of the supported array surface.",
                });
                return error.DiagnosticsEmitted;
            }
            if (object_type.kind != .native_state_view) {
                if (try resolveMethodMemberOrNull(ctx, object_type, member.member, node.span)) |resolved_method| {
                    const dispatched = try buildDispatchedMethodCallExpr(ctx, resolved_method, object, object_type, node, imports, scope, function_headers);
                    lowered.* = dispatched.*;
                    return;
                }
            }
        }
    }

    const callee_name = try flattenCalleeName(ctx.allocator, node.callee);
    const callee_leaf = qualifiedLeaf(callee_name);

    if (std.mem.eql(u8, callee_name, "print")) {
        var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
        for (node.args) |arg| try args.append(try lowerExpr(ctx, arg.value, imports, scope, function_headers));
        if (args.items.len != 1) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM007",
                .title = "wrong number of arguments to print",
                .message = "The builtin `print` expects exactly one argument.",
                .labels = &.{
                    diagnostics.primaryLabel(node.span, "print call has the wrong number of arguments"),
                },
                .help = "Call `print(value);` with exactly one value.",
            });
            return error.DiagnosticsEmitted;
        }
        const arg_ty = model.hir.exprType(args.items[0].*);
        if (arg_ty.kind == .named) {
            if (shared.namedTypeHeader(ctx, arg_ty)) |type_header| {
                if (type_header.is_printable) {
                    const method_key = try std.fmt.allocPrint(ctx.allocator, "{s}.onPrint", .{arg_ty.name.?});
                    const header = function_headers.?.get(method_key) orelse return error.DiagnosticsEmitted;
                    const lowered_receiver = args.items[0];
                    const lowered_call = try ctx.allocator.create(model.Expr);
                    const call_args = try ctx.allocator.alloc(*model.Expr, 1);
                    call_args[0] = lowered_receiver;
                    lowered_call.* = .{ .call = .{
                        .callee_name = method_key,
                        .function_id = header.id,
                        .args = call_args,
                        .ty = header.return_type,
                        .span = node.span,
                    } };
                    args.items[0] = lowered_call;
                }
            }
        }
        lowered.* = .{ .call = .{
            .callee_name = callee_name,
            .function_id = null,
            .args = try args.toOwnedSlice(),
            .ty = .{ .kind = .void },
            .span = node.span,
        } };
        return;
    }

    if (function_headers) |headers| {
        const header = headers.get(callee_name) orelse headers.get(callee_leaf) orelse blk: {
            if (ctx.imported_globals.findFunction(callee_leaf)) |function_decl| {
                break :blk shared.FunctionHeader{
                    .id = 0,
                    .params = function_decl.params,
                    .execution = function_decl.execution,
                    .return_type = function_decl.return_type,
                    .is_extern = function_decl.is_extern,
                    .foreign = function_decl.foreign,
                    .span = .{ .start = 0, .end = 0 },
                };
            }
            break :blk null;
        };
        if (header) |resolved_header| {
            const trailing_callback_type = try trailingCallbackType(ctx, node, resolved_header.params);
            const explicit_param_count = resolved_header.params.len - (if (trailing_callback_type != null) @as(usize, 1) else 0);
            if (node.args.len != explicit_param_count) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM042",
                    .title = "wrong number of arguments",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The call to '{s}' expected {d} explicit argument(s) but received {d}.", .{ callee_name, explicit_param_count, node.args.len }),
                    .labels = &.{
                        diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments"),
                    },
                    .help = "Update the call so it matches the function signature exactly.",
                });
                return error.DiagnosticsEmitted;
            }
            var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            for (node.args, 0..) |arg, index| {
                try args.append(try lowerCallArgument(ctx, arg.value, resolved_header.params[index], imports, scope, headers, node.span));
            }
            if (trailing_callback_type) |callback_type| {
                try args.append(try lowerTrailingCallbackValue(ctx, node, callback_type, imports, scope, headers));
            }
            lowered.* = .{ .call = .{
                .callee_name = callee_name,
                .function_id = resolved_header.id,
                .args = try args.toOwnedSlice(),
                .trailing_builder = if (trailing_callback_type == null and node.trailing_builder != null) try lowerBuilderBlock(ctx, node.trailing_builder.?, imports, scope) else null,
                .ty = resolved_header.return_type,
                .span = node.span,
            } };
            return;
        }
    }

    if (ctx.type_headers) |headers| {
        if (headers.get(callee_name) != null or headers.get(callee_leaf) != null) {
            lowered.* = try lowerTypeConstruction(ctx, callee_name, callee_leaf, node.args, &.{}, node.span, imports, scope, function_headers);
            return;
        }
    }

    if (ctx.imported_globals.findType(callee_name) != null or ctx.imported_globals.findType(callee_leaf) != null) {
        lowered.* = try lowerTypeConstruction(ctx, callee_name, callee_leaf, node.args, &.{}, node.span, imports, scope, function_headers);
        return;
    }

    if (ctx.imported_globals.hasCallable(callee_name)) {
        if (node.trailing_callback != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM081",
                .title = "call does not accept a trailing callback",
                .message = "This callable is not declared with a typed final callback parameter, so trailing callback syntax cannot bind here.",
                .labels = &.{diagnostics.primaryLabel(node.span, "trailing callback cannot bind here")},
                .help = "Use a direct function or method that declares the callback parameter explicitly, or pass an ordinary argument.",
            });
            return error.DiagnosticsEmitted;
        }
        var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
        for (node.args) |arg| try args.append(try lowerExpr(ctx, arg.value, imports, scope, function_headers));
        lowered.* = .{ .call = .{
            .callee_name = callee_name,
            .function_id = null,
            .args = try args.toOwnedSlice(),
            .trailing_builder = if (node.trailing_builder) |builder| try lowerBuilderBlock(ctx, builder, imports, scope) else null,
            .ty = .{ .kind = .unknown },
            .span = node.span,
        } };
        return;
    }

    if (function_headers) |headers| {
        if (try lowerImplicitSelfMethodCall(ctx, node, imports, scope, headers)) |call_expr| {
            lowered.* = call_expr;
            return;
        }
    }

    if (function_headers != null and isCallableValueExpr(node.callee, scope)) {
        const callee = try lowerExpr(ctx, node.callee, imports, scope, function_headers);
        if (try function_types.parseSignature(ctx.allocator, model.hir.exprType(callee.*))) |signature| {
            if (node.trailing_builder != null or node.trailing_callback != null) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM082",
                    .title = "callable value does not accept a trailing builder",
                    .message = "Trailing blocks currently attach only to direct calls with known final callback parameters or builder-aware call sites.",
                    .labels = &.{diagnostics.primaryLabel(node.span, "trailing block cannot bind here")},
                    .help = "Call the value with ordinary arguments, or call a direct function or method that declares the callback parameter explicitly.",
                });
                return error.DiagnosticsEmitted;
            }
            if (node.args.len != signature.params.len) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM042",
                    .title = "wrong number of arguments",
                    .message = try std.fmt.allocPrint(ctx.allocator, "This callable value expected {d} argument(s) but received {d}.", .{ signature.params.len, node.args.len }),
                    .labels = &.{diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments")},
                    .help = "Update the call so it matches the callable value type exactly.",
                });
                return error.DiagnosticsEmitted;
            }
            var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            for (node.args, 0..) |arg, index| {
                try args.append(try lowerCallArgument(ctx, arg.value, signature.params[index], imports, scope, function_headers.?, node.span));
            }
            lowered.* = .{ .call_value = .{
                .callee = callee,
                .args = try args.toOwnedSlice(),
                .param_types = signature.params,
                .ty = signature.result,
                .span = node.span,
            } };
            return;
        }
        if (node.callee.* == .member) {
            try diagnostics.Emitter.init(ctx.allocator, ctx.diagnostics).err(.{
                .code = "KSEM092",
                .title = "member is not callable",
                .message = "This member access resolves to a field, but the field does not have a function type.",
                .span = node.span,
                .label = "member call target is not a function-typed field",
                .help = "Call only methods or fields declared with a function type such as `(RawPtr) -> Void`.",
            });
            return error.DiagnosticsEmitted;
        }
    }

    _ = try shared.resolveLocalOrCapture(ctx, scope.*, callee_leaf, node.span);

    if (std.mem.indexOfScalar(u8, callee_name, '.')) |root_end| {
        if (!shared.isImportedRoot(callee_name[0..root_end], imports)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM027",
                .title = "invalid namespaced reference",
                .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not resolve the namespace root '{s}'.", .{callee_name[0..root_end]}),
                .labels = &.{
                    diagnostics.primaryLabel(node.span, "unknown namespace root"),
                },
                .help = "Import the module first or use a local function name.",
            });
            return error.DiagnosticsEmitted;
        }
        if (node.trailing_callback != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM081",
                .title = "call does not accept a trailing callback",
                .message = "This namespaced call is not declared with a typed final callback parameter, so trailing callback syntax cannot bind here.",
                .labels = &.{diagnostics.primaryLabel(node.span, "trailing callback cannot bind here")},
                .help = "Use a direct function or method that declares the callback parameter explicitly, or pass an ordinary argument.",
            });
            return error.DiagnosticsEmitted;
        }
        var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
        for (node.args) |arg| try args.append(try lowerExpr(ctx, arg.value, imports, scope, function_headers));
        lowered.* = .{ .call = .{
            .callee_name = callee_name,
            .function_id = null,
            .args = try args.toOwnedSlice(),
            .trailing_builder = if (node.trailing_builder) |builder| try lowerBuilderBlock(ctx, builder, imports, scope) else null,
            .ty = .{ .kind = .unknown },
            .span = node.span,
        } };
        return;
    }

    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM010",
        .title = "unknown call target",
        .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not find a function named '{s}'.", .{callee_name}),
        .labels = &.{
            diagnostics.primaryLabel(node.span, "unknown function call"),
        },
        .help = "Declare the function before calling it, or import the module that provides the symbol.",
    });
    return error.DiagnosticsEmitted;
}
