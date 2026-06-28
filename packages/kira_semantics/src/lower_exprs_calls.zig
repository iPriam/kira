const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");
const comptime_eval = @import("lower_exprs_comptime.zig");
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
const lowerComptimeCall = comptime_eval.lowerComptimeCall;
/// A lowered value that *aliases* existing owned storage rather than producing a fresh
/// owned value: a bare local read (unless explicitly `move`d), a field read, or an element
/// read. Fresh values (array/struct/enum literals, calls, conditionals) own what they yield.
fn isAliasingAggregateRead(value: model.Expr) bool {
    return switch (value) {
        .local => |node| node.ownership != .move,
        .field, .index => true,
        else => false,
    };
}

/// Reject initializing an array-typed struct field from an aliasing array read.
///
/// Kira has no deep copy for arrays (KSEM116) and the backend cannot transfer ownership of
/// an array *field* out of a struct, so `Foo { items: other.items }` / `Foo { items: local }`
/// would make the new struct share the source's backing store. When the source is then
/// dropped (e.g. a by-value parameter at function exit), the field dangles — the latent
/// use-after-free behind the widget-lowering `enum native copy could not resolve discriminant`
/// crash. Until ownership transfer/copy of array fields is implemented, this is rejected at
/// `check` instead of detonating at runtime under allocation churn.
fn rejectAliasedArrayField(
    ctx: *shared.Context,
    field_ty: model.ResolvedType,
    field_value: *const model.Expr,
    span: source_pkg.Span,
) !void {
    if (field_ty.kind != .array) return;
    if (!isAliasingAggregateRead(field_value.*)) return;
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM118",
        .title = "array field aliases an existing array",
        .message = "Initializing an array-typed field from an existing array value shares its backing store. Kira cannot yet copy or transfer ownership of an array field, so the field would dangle when the source is freed.",
        .labels = &.{diagnostics.primaryLabel(span, "this array field aliases an existing array")},
        .help = "Build the array fresh in place (an array literal, or a call that returns a new array), or `move` an owned local array into the field.",
    });
    return error.DiagnosticsEmitted;
}

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
                    std.debug.print("DEBUG constructor overflow callee={s} source={s} start={d}\n", .{ callee_leaf, arg.span.source_path orelse "<unknown>", arg.span.start });
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
                    std.debug.print("DEBUG constructor overflow callee={s} source={s} start={d}\n", .{ callee_leaf, arg.span.source_path orelse "<unknown>", arg.span.start });
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
            try rejectAliasedArrayField(ctx, field_ty, field_value, arg.span);
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
            try rejectAliasedArrayField(ctx, field_ty, field_value, field.span);
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

// Build the field arguments for a declaration construction `Foo(...) { ... }` whose `@Content`
// fields are filled by the trailing block. Returns null when `Foo` has no content fields or no
// trailing block (ordinary construction). Explicit `(...)` args are preserved, then each content
// field is filled: a `[Widget]` field from all block items as an array literal, a single `Widget`
// field from one item, and multiple fields from named fills (`header { ... }`). Arity/type are
// already validated by the widget-content pass.
fn buildContentArgs(
    ctx: *shared.Context,
    callee_leaf: []const u8,
    node: syntax.ast.CallExpr,
) !?[]syntax.ast.CallArg {
    const content_fields = (ctx.form_content_fields orelse return null).get(callee_leaf) orelse return null;
    const builder = node.trailing_builder orelse return null;

    var args = std.array_list.Managed(syntax.ast.CallArg).init(ctx.allocator);
    try args.appendSlice(node.args);

    if (content_fields.len == 1) {
        if (try contentArg(ctx, content_fields[0], builder.items, node.span)) |arg| try args.append(arg);
    } else {
        for (builder.items) |item| {
            if (item != .expr) continue;
            const value = item.expr.expr;
            if (value.* != .call) continue;
            const fill = value.*.call;
            const name = calleeIdentifierName(fill.callee) orelse continue;
            const field = findContentField(content_fields, name) orelse continue;
            const inner = fill.trailing_builder orelse continue;
            if (try contentArg(ctx, field, inner.items, fill.span)) |arg| try args.append(arg);
        }
    }
    return try args.toOwnedSlice();
}

fn contentArg(
    ctx: *shared.Context,
    field: shared.ContentFieldRef,
    items: []const syntax.ast.BuilderItem,
    span: source_pkg.Span,
) !?syntax.ast.CallArg {
    if (field.is_list) {
        var all_expr_items = true;
        var elements = std.array_list.Managed(*syntax.ast.Expr).init(ctx.allocator);
        for (items) |item| {
            if (item == .expr) {
                // Preserve full modifier chains inside content-array fields so a widget list
                // sees the same runtime values the author wrote, including chained `extend`
                // modifiers such as `.padding(..)` / `.background(..)`.
                try elements.append(item.expr.expr);
                continue;
            }
            all_expr_items = false;
        }
        const array_expr = try ctx.allocator.create(syntax.ast.Expr);
        if (all_expr_items) {
            array_expr.* = .{ .array = .{ .elements = try elements.toOwnedSlice(), .span = span } };
        } else {
            array_expr.* = .{ .builder_array = .{ .builder = .{ .items = try ctx.allocator.dupe(syntax.ast.BuilderItem, items), .span = span }, .span = span } };
        }
        return .{ .label = field.name, .value = array_expr, .span = span };
    }
    for (items) |item| {
        if (item == .expr) return .{ .label = field.name, .value = item.expr.expr, .span = span };
    }
    return null;
}

fn calleeIdentifierName(callee: *const syntax.ast.Expr) ?[]const u8 {
    return switch (callee.*) {
        .identifier => |ident| if (ident.name.segments.len == 1) ident.name.segments[0].text else null,
        else => null,
    };
}

fn findContentField(fields: []const shared.ContentFieldRef, name: []const u8) ?shared.ContentFieldRef {
    for (fields) |field| {
        if (std.mem.eql(u8, field.name, name)) return field;
    }
    return null;
}

fn localOrImportedTypeFieldCount(
    ctx: *shared.Context,
    callee_name: []const u8,
    callee_leaf: []const u8,
) ?usize {
    if (ctx.type_headers) |headers| {
        if (headers.get(callee_name)) |header| return header.fields.len;
        if (headers.get(callee_leaf)) |header| return header.fields.len;
    }
    if (ctx.imported_globals.findType(callee_name)) |type_decl| return type_decl.fields.len;
    if (ctx.imported_globals.findType(callee_leaf)) |type_decl| return type_decl.fields.len;
    return null;
}

fn requiredExplicitArgCount(param_defaults: []const ?*syntax.ast.Expr, explicit_param_count: usize) usize {
    var required = explicit_param_count;
    while (required > 0) {
        const default_index = required - 1;
        if (default_index >= param_defaults.len) break;
        if (param_defaults[default_index] == null) break;
        required -= 1;
    }
    return required;
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
        const is_static_callable = scope.get(flattened_member.root) == null and
            function_headers != null and
            shared.findFunctionHeader(ctx, function_headers.?, flattened_member.path) != null;
        if (is_static_callable) {
            if (ctx.form_families != null and ctx.form_families.?.get(flattened_member.root) != null) {
                const receiver = try ctx.allocator.create(model.Expr);
                receiver.* = try lowerTypeConstruction(ctx, flattened_member.root, flattened_member.root, &.{}, &.{}, node.span, imports, scope, function_headers);
                const receiver_type = model.hir.exprType(receiver.*);
                if (try resolveMethodMemberOrNull(ctx, receiver_type, member.member, node.span)) |resolved_method| {
                    const dispatched = try buildDispatchedMethodCallExpr(ctx, resolved_method, receiver, receiver_type, node, imports, scope, function_headers);
                    lowered.* = dispatched.*;
                    return;
                }
            }
        } else if (shared.isImportedRoot(ctx, flattened_member.root, imports) and scope.get(flattened_member.root) == null) {
            // Imported namespace calls such as `Foundation.Text(...)` are not instance methods.
            // Fall through so ordinary function resolution has a chance before constructor
            // fallback examines the leaf type name.
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
                const value = try lowerCallArgument(ctx, node.args[0].value, element_type, .owned, "array.append", imports, scope, function_headers orelse return error.DiagnosticsEmitted, node.span);
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

    // `Int(x)` / `Float(x)` numeric casts. Recognized before ordinary function
    // and constructor resolution: `Int` and `Float` are primitive type names,
    // not user-callable functions, so a call against them is always a cast.
    if ((std.mem.eql(u8, callee_name, "Int") or std.mem.eql(u8, callee_name, "Float")) and scope.get(callee_name) == null) {
        const target_type: model.ResolvedType = if (callee_name[0] == 'I') .{ .kind = .integer } else .{ .kind = .float };
        if (node.args.len != 1 or node.args[0].label != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM118",
                .title = "invalid numeric cast",
                .message = try std.fmt.allocPrint(ctx.allocator, "`{s}(...)` is a numeric cast and takes exactly one positional argument.", .{callee_name}),
                .labels = &.{diagnostics.primaryLabel(node.span, "cast call has the wrong arguments")},
                .help = "Write the cast as `Int(value)` or `Float(value)` with a single numeric value.",
            });
            return error.DiagnosticsEmitted;
        }
        const operand = try lowerExpr(ctx, node.args[0].value, imports, scope, function_headers);
        const operand_ty = model.hir.exprType(operand.*);
        if (operand_ty.kind != .integer and operand_ty.kind != .float) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM118",
                .title = "invalid numeric cast",
                .message = try std.fmt.allocPrint(ctx.allocator, "`{s}(...)` can only convert `Int` or `Float` values.", .{callee_name}),
                .labels = &.{diagnostics.primaryLabel(node.span, "operand is not a numeric value")},
                .help = "Pass an `Int` or `Float` expression to the cast.",
            });
            return error.DiagnosticsEmitted;
        }
        lowered.* = .{ .cast = .{
            .operand = operand,
            .ty = target_type,
            .span = node.span,
        } };
        return;
    }

    const imported_qualified = if (function_headers != null)
        shared.importedQualifiedName(ctx, imports, callee_name)
    else
        null;
    const exact_function_header = if (function_headers) |headers| shared.findFunctionHeader(ctx, headers, callee_name) else null;
    const exact_qualified_function_header = if (function_headers) |headers|
        if (imported_qualified) |qualified| shared.findFunctionHeader(ctx, headers, qualified) else null
    else
        null;
    const is_qualified_callee = std.mem.indexOfScalar(u8, callee_name, '.') != null;
    const has_local_type = if (ctx.type_headers) |headers|
        headers.get(callee_name) != null or headers.get(callee_leaf) != null
    else
        false;
    const has_imported_type = ctx.imported_globals.findType(callee_name) != null or ctx.imported_globals.findType(callee_leaf) != null;
    const qualified_type_fits = if (is_qualified_callee)
        if (localOrImportedTypeFieldCount(ctx, callee_name, callee_leaf)) |field_count| node.args.len <= field_count else false
    else
        false;
    const should_prefer_type = if (has_local_type or has_imported_type)
        if (is_qualified_callee)
            exact_function_header == null and exact_qualified_function_header == null and qualified_type_fits
        else
            ctx.current_package == null or exact_function_header == null
    else
        false;
    if (should_prefer_type) {
        const content_args = try buildContentArgs(ctx, callee_leaf, node);
        lowered.* = try lowerTypeConstruction(ctx, callee_name, callee_leaf, content_args orelse node.args, &.{}, node.span, imports, scope, function_headers);
        return;
    }

    if (function_headers) |headers| {
        const header = (if (imported_qualified) |qualified| shared.findFunctionHeader(ctx, headers, qualified) else null) orelse
            shared.findFunctionHeader(ctx, headers, callee_name) orelse
            shared.findFunctionHeader(ctx, headers, callee_leaf) orelse blk: {
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
            if (resolved_header.is_comptime) {
                if (try lowerComptimeCall(ctx, node, resolved_header)) |folded| {
                    lowered.* = folded;
                    return;
                }
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM147",
                    .title = "comptime function was not evaluated",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The comptime function '{s}' must be evaluated during compilation and cannot be emitted as a runtime call.", .{callee_name}),
                    .labels = &.{diagnostics.primaryLabel(node.span, "runtime call to comptime function")},
                    .help = "Use a comptime function form the compiler can fold or generate before backend lowering.",
                });
                return error.DiagnosticsEmitted;
            }
            const trailing_callback_type = try trailingCallbackType(ctx, node, resolved_header.params);
            const explicit_param_count = resolved_header.params.len - (if (trailing_callback_type != null) @as(usize, 1) else 0);
            const required_arg_count = requiredExplicitArgCount(resolved_header.param_defaults, explicit_param_count);
            if (node.args.len < required_arg_count or node.args.len > explicit_param_count) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM042",
                    .title = "wrong number of arguments",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The call to '{s}' expected between {d} and {d} explicit argument(s) but received {d}.", .{ callee_name, required_arg_count, explicit_param_count, node.args.len }),
                    .labels = &.{
                        diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments"),
                    },
                    .help = "Update the call so it matches the function signature exactly.",
                });
                return error.DiagnosticsEmitted;
            }
            var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            var index: usize = 0;
            while (index < explicit_param_count) : (index += 1) {
                if (index < node.args.len) {
                    const arg = node.args[index];
                    try args.append(try lowerCallArgument(ctx, arg.value, resolved_header.params[index], shared.paramOwnership(resolved_header, index), callee_name, imports, scope, headers, node.span));
                    continue;
                }
                const default_expr = resolved_header.param_defaults[index] orelse return error.DiagnosticsEmitted;
                try args.append(try lowerCallArgument(ctx, default_expr, resolved_header.params[index], shared.paramOwnership(resolved_header, index), callee_name, imports, scope, headers, node.span));
            }
            if (trailing_callback_type) |callback_type| {
                try args.append(try lowerTrailingCallbackValue(ctx, node, callback_type, imports, scope, headers));
            }
            lowered.* = .{ .call = .{
                .callee_name = callee_name,
                .function_id = resolved_header.id,
                .args = try args.toOwnedSlice(),
                .trailing_builder = if (trailing_callback_type == null and node.trailing_builder != null) try lowerBuilderBlock(ctx, node.trailing_builder.?, imports, scope, function_headers) else null,
                .ty = resolved_header.return_type,
                .span = node.span,
            } };
            return;
        }
    }

    const can_fallback_to_leaf_type = !is_qualified_callee or qualified_type_fits;

    if (ctx.type_headers) |headers| {
        if (headers.get(callee_name) != null or (can_fallback_to_leaf_type and headers.get(callee_leaf) != null)) {
            // A declaration with `@Content` fields routes its trailing `{ ... }` block into those
            // fields (single `Widget`, `[Widget]` list, or named fills) as ordinary field args.
            const content_args = try buildContentArgs(ctx, callee_leaf, node);
            lowered.* = try lowerTypeConstruction(ctx, callee_name, callee_leaf, content_args orelse node.args, &.{}, node.span, imports, scope, function_headers);
            return;
        }
    }

    if (ctx.imported_globals.findType(callee_name) != null or (can_fallback_to_leaf_type and ctx.imported_globals.findType(callee_leaf) != null)) {
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
            .trailing_builder = if (node.trailing_builder) |builder| try lowerBuilderBlock(ctx, builder, imports, scope, function_headers) else null,
            .ty = .{ .kind = .unknown },
            .span = node.span,
        } };
        return;
    }

    if (std.mem.indexOfScalar(u8, callee_name, '.')) |root_end| {
        if (shared.isImportedRoot(ctx, callee_name[0..root_end], imports)) {
            if (function_headers) |headers| {
                const type_field_count = localOrImportedTypeFieldCount(ctx, callee_name, callee_leaf);
                const namespaced_type_fits = if (type_field_count) |field_count|
                    node.args.len <= field_count
                else
                    false;
                const namespaced_qualified = shared.importedQualifiedName(ctx, imports, callee_name);
                const namespaced_header = (if (namespaced_qualified) |qualified| shared.findFunctionHeader(ctx, headers, qualified) else null) orelse
                    shared.findFunctionHeader(ctx, headers, callee_name) orelse
                    (if (!namespaced_type_fits) shared.findFunctionHeader(ctx, headers, callee_leaf) else null);
                if (namespaced_header) |resolved_header| {
                    const trailing_callback_type = try trailingCallbackType(ctx, node, resolved_header.params);
                    const explicit_param_count = resolved_header.params.len - (if (trailing_callback_type != null) @as(usize, 1) else 0);
                    const required_arg_count = requiredExplicitArgCount(resolved_header.param_defaults, explicit_param_count);
                    if (node.args.len < required_arg_count or node.args.len > explicit_param_count) {
                        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                            .severity = .@"error",
                            .code = "KSEM042",
                            .title = "wrong number of arguments",
                            .message = try std.fmt.allocPrint(ctx.allocator, "The call to '{s}' expected between {d} and {d} explicit argument(s) but received {d}.", .{ callee_name, required_arg_count, explicit_param_count, node.args.len }),
                            .labels = &.{diagnostics.primaryLabel(node.span, "call uses the wrong number of arguments")},
                            .help = "Update the call so it matches the function signature exactly.",
                        });
                        return error.DiagnosticsEmitted;
                    }
                    var args = std.array_list.Managed(*model.Expr).init(ctx.allocator);
                    var index: usize = 0;
                    while (index < explicit_param_count) : (index += 1) {
                        if (index < node.args.len) {
                            const arg = node.args[index];
                            try args.append(try lowerCallArgument(ctx, arg.value, resolved_header.params[index], shared.paramOwnership(resolved_header, index), callee_name, imports, scope, headers, node.span));
                            continue;
                        }
                        const default_expr = resolved_header.param_defaults[index] orelse return error.DiagnosticsEmitted;
                        try args.append(try lowerCallArgument(ctx, default_expr, resolved_header.params[index], shared.paramOwnership(resolved_header, index), callee_name, imports, scope, headers, node.span));
                    }
                    if (trailing_callback_type) |callback_type| {
                        try args.append(try lowerTrailingCallbackValue(ctx, node, callback_type, imports, scope, headers));
                    }
                    lowered.* = .{ .call = .{
                        .callee_name = callee_name,
                        .function_id = resolved_header.id,
                        .args = try args.toOwnedSlice(),
                        .trailing_builder = if (trailing_callback_type == null and node.trailing_builder != null) try lowerBuilderBlock(ctx, node.trailing_builder.?, imports, scope, function_headers) else null,
                        .ty = resolved_header.return_type,
                        .span = node.span,
                    } };
                    return;
                }
            }
        }
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
                try args.append(try lowerCallArgument(
                    ctx,
                    arg.value,
                    signature.params[index],
                    if (index < signature.param_ownership.len) signature.param_ownership[index] else .owned,
                    "callable value",
                    imports,
                    scope,
                    function_headers.?,
                    node.span,
                ));
            }
            lowered.* = .{ .call_value = .{
                .callee = callee,
                .args = try args.toOwnedSlice(),
                .param_types = signature.params,
                .param_ownership = signature.param_ownership,
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
        if (!shared.isImportedRoot(ctx, callee_name[0..root_end], imports)) {
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
            .trailing_builder = if (node.trailing_builder) |builder| try lowerBuilderBlock(ctx, builder, imports, scope, function_headers) else null,
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
