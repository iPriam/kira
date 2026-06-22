const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const parent = @import("lower_exprs.zig");
const resolvers = @import("lower_exprs_type_resolvers.zig");
const names = @import("lower_exprs_names.zig");
const signature_types = @import("lower_exprs_function_types.zig");
const ownership_exprs = @import("lower_exprs_ownership.zig");
const assignment = @import("lower_exprs_assignment.zig");
const callbacks = @import("lower_exprs_callbacks.zig");
const lowerCallbackBlockValue = parent.lowerCallbackBlockValue;
const lowerExpr = parent.lowerExpr;
pub const resolveSyntaxExprType = resolvers.resolveSyntaxExprType;
pub const resolveSyntaxArrayLiteralType = resolvers.resolveSyntaxArrayLiteralType;
pub const resolveBinaryType = resolvers.resolveBinaryType;
pub const resolveConditionalType = resolvers.resolveConditionalType;
pub const resolveArrayElementType = resolvers.resolveArrayElementType;
pub const resolveIndexElementType = resolvers.resolveIndexElementType;
pub const isCallableValueExpr = names.isCallableValueExpr;
pub const flattenCalleeName = names.flattenCalleeName;
pub const flattenMemberExpr = names.flattenMemberExpr;
pub const flattenMemberExprPath = names.flattenMemberExprPath;
pub const functionTypeFromResolvedSignature = signature_types.functionTypeFromResolvedSignature;
pub const functionTypeFromHeader = signature_types.functionTypeFromHeader;
pub const lowerCallArgument = ownership_exprs.lowerCallArgument;
pub const lowerOwnershipExpr = ownership_exprs.lowerOwnershipExpr;
pub const emitUseAfterMove = ownership_exprs.emitUseAfterMove;
pub const rejectOutstandingMovedFields = ownership_exprs.rejectOutstandingMovedFields;
pub const lowerAssignmentTarget = assignment.lowerAssignmentTarget;
pub const commitAssignmentTarget = assignment.commitAssignmentTarget;
pub const lowerCallbackArgument = callbacks.lowerCallbackArgument;
pub const callbackTypesCompatible = callbacks.callbackTypesCompatible;
const qualifiedNameRootMatches = names.qualifiedNameRootMatches;
const enumMemberMatches = names.enumMemberMatches;
pub const LoweredLocalDeclaration = struct {
    ty: model.ResolvedType,
    value: ?*model.Expr,
    initialized: bool,
};

const BindingMoveTarget = struct {
    root: []const u8,
    /// The top-level field read out of `root`, or null for a bare whole-local read.
    field: ?[]const u8,
};

/// Resolve a binding initializer to the owned local it consumes, and which top-level
/// field of that local (if any). A bare identifier is a whole read (`field == null`);
/// a field chain (`a.b.c`) roots at `a` and names the field directly under the root (`b`),
/// because the move model tracks one level below the binding root. Anything else (calls,
/// literals, index reads) has no single owning root, so it returns null. Namespaced paths
/// like `Kit.Card` still produce a root segment, but the scope lookup at the use site
/// rejects it because it is not a local binding.
fn bindingMoveTarget(value_expr: *syntax.ast.Expr) ?BindingMoveTarget {
    return switch (value_expr.*) {
        .identifier => |node| .{ .root = node.name.segments[0].text, .field = null },
        .member => |node| blk: {
            const inner = bindingMoveTarget(node.object) orelse break :blk null;
            break :blk .{ .root = inner.root, .field = inner.field orelse node.member };
        },
        else => null,
    };
}

/// Apply Rust-style implicit move when a binding initializer reads an owned aggregate that
/// the binding lowering would otherwise *alias*. Arrays and enums lower to `store_local`
/// (a shared reference), so `let x = y` / `let x = obj.field` leave two owners pointing at
/// one heap object — the use-after-free under allocation churn that `check` never saw.
///
/// A bare local read is a whole move: the source binding is marked moved (any later use is
/// KSEM107) and the lowered read is re-tagged as a move so codegen transfers ownership
/// instead of aliasing. A field read is a *partial* move: only that field is recorded as
/// moved, so the base cannot be used as a whole until the field is re-initialized
/// (`obj.field = ...`), exactly mirroring Rust. This keeps the in-place mutation idiom
/// (`var x = obj.arr; x[i] = ...; obj.arr = x`) legal while rejecting a persisted alias.
/// Structs already deep-copy via `copy_indirect`, so struct-valued bindings stay safe and
/// are left untouched. Code that wants an independent value must say `copy`.
fn applyBindingMove(ctx: *shared.Context, scope: *model.Scope, value_expr: *syntax.ast.Expr, lowered_value: *model.Expr) !void {
    const target = bindingMoveTarget(value_expr) orelse return;
    const binding = scope.entries.getPtr(target.root) orelse return;
    if (binding.moved) return;
    if (binding.ownership != .owned) return;
    // Only the aggregates that actually alias on binding need move enforcement. The bound
    // value's own type (the field type for a field read, the local type for a bare read)
    // is what decides this, so a trivially-copyable field like `obj.count` never moves `obj`.
    const bound_ty = model.hir.exprType(lowered_value.*);
    if (bound_ty.kind != .array and bound_ty.kind != .enum_instance) return;
    if (target.field) |field_name| {
        try binding.markFieldMoved(ctx.allocator, field_name);
        if (binding.move_span == null) binding.move_span = exprSpan(value_expr.*);
    } else {
        if (lowered_value.* == .local) lowered_value.local.ownership = .move;
        binding.moved = true;
        binding.move_span = exprSpan(value_expr.*);
    }
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

        // Rust ownership: binding a bare owned, non-trivially-copyable local consumes it.
        // Without this, `let alias = values` leaves both bindings owning the same heap object
        // (a use-after-free under allocation churn that `check` never saw). The move makes any
        // later use a compile error (KSEM107); independence requires explicit `copy`.
        try applyBindingMove(ctx, scope, value_expr, lowered_value);

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

    if (syntax_arg.* == .builder_array) {
        if (expected_type.kind != .array) {
            try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, span, expected_type, .{ .kind = .array });
            return error.DiagnosticsEmitted;
        }
        const lowered = try ctx.allocator.create(model.Expr);
        lowered.* = .{ .builder_array = .{
            .builder = try parent.lowerBuilderBlock(ctx, syntax_arg.builder_array.builder, imports, scope),
            .ty = expected_type,
            .span = syntax_arg.builder_array.span,
        } };
        return lowered;
    }

    const lowered = try lowerExpr(ctx, syntax_arg, imports, scope, function_headers);
    const actual_type = model.hir.exprType(lowered.*);
    if (!canPassArgument(ctx, expected_type, actual_type)) {
        try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, span, expected_type, actual_type);
        return error.DiagnosticsEmitted;
    }
    // Ownership: an element-less array literal (`[]`) infers no element type, so its
    // lowered value carries `[<unknown>]` with a null name. Without the element type
    // the backend cannot reach the per-element destructor and leaks every element it
    // later accumulates. The coercion target supplies the missing element type, so
    // stamp it onto the value here — the array now knows precisely what it owns.
    propagateExpectedArrayElementType(lowered, expected_type);
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

// Stamp an expected named-array type onto an array-literal value whose own inference
// produced no element type (an empty `[]`, or a literal nested under one). The element
// type is what lets ownership-driven drop reach each element's destructor; an array that
// does not know its element type cannot free what it holds. Recurses into nested array
// literals so `[[]]`-style values inherit their element type at every level.
fn propagateExpectedArrayElementType(value: *model.Expr, expected_type: model.ResolvedType) void {
    if (expected_type.kind != .array) return;
    const expected_name = expected_type.name orelse return;
    if (value.* != .array) return;
    if (value.array.ty.name == null) value.array.ty = expected_type;
    const element_type = shared.resolvedTypeFromText(expected_name) catch return;
    if (element_type.kind != .array) return;
    for (value.array.elements) |element| propagateExpectedArrayElementType(element, element_type);
}

pub fn canInitializeDeclaredType(ctx: *const shared.Context, expected: model.ResolvedType, actual: model.ResolvedType) bool {
    return typesCompatibleForContext(ctx, expected, actual, false);
}

pub fn canPassArgument(ctx: *const shared.Context, expected: model.ResolvedType, actual: model.ResolvedType) bool {
    return typesCompatibleForContext(ctx, expected, actual, true);
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
        .builder_array => |node| node.span,
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
        .try_expr => |node| node.span,
    };
}

pub fn qualifiedLeaf(name: []const u8) []const u8 {
    const index = std.mem.lastIndexOfScalar(u8, name, '.') orelse return name;
    return name[index + 1 ..];
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

    // A concrete widget value coerces to `any Widget` in any typed context (declared `let`,
    // return position, array element), not only at argument-passing sites.
    if (shared.isConstructFamilyCoercion(ctx, expected, actual)) return true;

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
        if (shared.commonConstructAnyType(ctx.allocator, ctx, element_ty, next_ty)) |common_ty| {
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
