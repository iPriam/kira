const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const function_types = @import("function_types.zig");
const calls = @import("lower_exprs_calls.zig");
const builder = @import("lower_exprs_builder.zig");
const members = @import("lower_exprs_members.zig");
const native_state = @import("lower_exprs_native_state.zig");
const matches = @import("lower_stmts_match.zig");
const attempts = @import("lower_stmts_attempt.zig");
const scope_flow = @import("lower_exprs_scope_flow.zig");
const types = @import("lower_exprs_types.zig");

pub const lowerStructLiteralExpr = calls.lowerStructLiteralExpr;
pub const lowerTypeConstruction = calls.lowerTypeConstruction;
pub const isTypeConstantField = calls.isTypeConstantField;
pub const resolveTypeConstructionFieldIndex = calls.resolveTypeConstructionFieldIndex;
pub const lowerCallExpr = calls.lowerCallExpr;
pub const lowerNativeStateExpr = native_state.lowerNativeStateExpr;
pub const lowerNativeUserDataExpr = native_state.lowerNativeUserDataExpr;
pub const lowerNativeRecoverExpr = native_state.lowerNativeRecoverExpr;
pub const lowerMatchStatement = matches.lowerMatchStatement;
pub const rejectOutstandingMovedFields = types.rejectOutstandingMovedFields;

pub const lowerImplicitSelfFieldExpr = members.lowerImplicitSelfFieldExpr;
pub const lowerImplicitSelfMethodCall = members.lowerImplicitSelfMethodCall;
pub const makeSelfLocalExpr = members.makeSelfLocalExpr;
pub const makeParentViewExpr = members.makeParentViewExpr;
pub const lowerParentQualifiedFieldExpr = members.lowerParentQualifiedFieldExpr;
pub const lowerParentQualifiedMethodCall = members.lowerParentQualifiedMethodCall;
pub const parentQualifierName = members.parentQualifierName;
pub const resolveParentView = members.resolveParentView;
pub const resolveParentViewOrNullNoDiag = members.resolveParentViewOrNullNoDiag;
pub const resolveFieldMemberOrNull = members.resolveFieldMemberOrNull;
pub const resolveFieldMember = members.resolveFieldMember;
pub const resolveMethodMemberOrNull = members.resolveMethodMemberOrNull;
pub const resolveMethodMember = members.resolveMethodMember;
pub const adjustMethodReceiver = members.adjustMethodReceiver;
pub const buildResolvedMethodCallExpr = members.buildResolvedMethodCallExpr;
pub const buildDispatchedMethodCallExpr = members.buildDispatchedMethodCallExpr;
pub const lowerResolvedMethodCall = members.lowerResolvedMethodCall;
pub const lowerVirtualMethodCall = members.lowerVirtualMethodCall;
pub const trailingCallbackType = members.trailingCallbackType;
pub const lowerTrailingCallbackValue = members.lowerTrailingCallbackValue;
pub const lowerCallbackBlockValue = members.lowerCallbackBlockValue;
pub const statementBlockFromBuilder = members.statementBlockFromBuilder;
pub const statementFromBuilderItem = members.statementFromBuilderItem;

pub const functionTypeFromResolvedSignature = types.functionTypeFromResolvedSignature;
pub const functionTypeFromHeader = types.functionTypeFromHeader;
pub const isCallableValueExpr = types.isCallableValueExpr;
pub const lowerCallArgument = types.lowerCallArgument;
pub const lowerOwnershipExpr = types.lowerOwnershipExpr;
pub const lowerExpectedValue = types.lowerExpectedValue;
pub const lowerAssignmentTarget = types.lowerAssignmentTarget;
pub const commitAssignmentTarget = types.commitAssignmentTarget;
pub const lowerCallbackArgument = types.lowerCallbackArgument;
pub const canPassArgument = types.canPassArgument;
pub const callbackTypesCompatible = types.callbackTypesCompatible;
pub const resolveFieldType = types.resolveFieldType;
pub const resolveFieldContainerType = types.resolveFieldContainerType;
pub const isNullPointerLiteral = types.isNullPointerLiteral;
pub const exprSpan = types.exprSpan;
pub const qualifiedLeaf = types.qualifiedLeaf;
pub const resolveSyntaxExprType = types.resolveSyntaxExprType;
pub const resolveLoweredValueType = types.resolveLoweredValueType;
pub const resolveValueType = types.resolveValueType;
pub const syntaxExprMatchesExplicitType = types.syntaxExprMatchesExplicitType;
pub const resolveFunctionReturnType = types.resolveFunctionReturnType;
pub const resolveBinaryType = types.resolveBinaryType;
pub const resolveConditionalType = types.resolveConditionalType;
pub const resolveArrayLiteralType = types.resolveArrayLiteralType;
pub const resolveSyntaxArrayLiteralType = types.resolveSyntaxArrayLiteralType;
pub const resolveArrayElementType = types.resolveArrayElementType;
pub const flattenCalleeName = types.flattenCalleeName;
pub const flattenMemberExpr = types.flattenMemberExpr;
pub const flattenMemberExprPath = types.flattenMemberExprPath;
pub const stripContentModifiers = builder.stripContentModifiers;
pub const lowerBuilderBlock = builder.lowerBuilderBlock;
const emitUseAfterMove = types.emitUseAfterMove;
pub const lowerEnumVariantExprExpected = lowerEnumVariantExprExpectedInternal;

fn tryLowerCountMemberExpr(object: *model.Expr, member_name: []const u8, span: source_pkg.Span) ?model.Expr {
    if (!std.mem.eql(u8, member_name, "count")) return null;
    return switch (model.hir.exprType(object.*).kind) {
        .array => .{ .array_len = .{
            .object = object,
            .span = span,
        } },
        .string => .{ .string_len = .{
            .object = object,
            .span = span,
        } },
        else => null,
    };
}

fn emitMemberAccessRequiresStructuredType(
    ctx: *shared.Context,
    span: source_pkg.Span,
) !void {
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
}

pub fn lowerBlockStatements(
    ctx: *shared.Context,
    block: syntax.ast.Block,
    imports: []const model.Import,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    loop_depth: usize,
    expected_return_type: model.ResolvedType,
) anyerror![]model.Statement {
    var statements = std.array_list.Managed(model.Statement).init(ctx.allocator);
    for (block.statements) |statement| {
        // `attempt` desugars to one or more `match` statements, so it is expanded here where
        // multiple lowered statements can be appended (HIR has no block statement).
        if (statement == .attempt_stmt) {
            const lowered = try attempts.lowerAttempt(ctx, statement.attempt_stmt, imports, scope, locals, next_local_id, function_headers, loop_depth, expected_return_type);
            try statements.appendSlice(lowered);
            continue;
        }
        try statements.append(try lowerStatement(ctx, statement, imports, scope, locals, next_local_id, function_headers, loop_depth, expected_return_type));
    }
    return statements.toOwnedSlice();
}

pub fn lowerStatement(
    ctx: *shared.Context,
    statement: syntax.ast.Statement,
    imports: []const model.Import,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    loop_depth: usize,
    expected_return_type: model.ResolvedType,
) anyerror!model.Statement {
    return switch (statement) {
        .let_stmt => |node| blk: {
            try shared.validateAnnotationPlacement(ctx, node.annotations, .field_decl, null);
            const declaration = try types.lowerLocalDeclaration(ctx, node, imports, scope, function_headers);
            const local_id = next_local_id.*;
            next_local_id.* += 1;
            // Rust reborrow: `var r = t` where `t` is a `borrow mut` (`&mut`) rebinds
            // `r` as a mutable alias of the same storage rather than copying it — the
            // engine's `var result = tree` pattern. Restricted to `borrow mut`: a shared
            // `borrow` (`&`) keeps value-copy semantics (you cannot mutate through `&`),
            // which existing code relies on to get an independent, mutable, owned copy.
            const is_reborrow = if (declaration.value) |value| switch (value.*) {
                .local => |local_read| if (scope.get(local_read.name)) |source|
                    source.ownership == .borrow_mut
                else
                    false,
                else => false,
            } else false;
            const local_ownership: model.OwnershipMode = if (is_reborrow) .borrow_mut else .owned;
            try scope.put(ctx.allocator, node.name, .{
                .id = local_id,
                .ty = declaration.ty,
                .storage = @enumFromInt(@intFromEnum(node.storage)),
                .ownership = local_ownership,
                .initialized = declaration.initialized,
                .decl_span = node.span,
            });
            try locals.append(.{
                .id = local_id,
                .name = try ctx.allocator.dupe(u8, node.name),
                .ty = declaration.ty,
                .ownership = local_ownership,
                .span = node.span,
            });
            break :blk .{ .let_stmt = .{
                .local_id = local_id,
                .ty = declaration.ty,
                .explicit_type = node.type_expr != null,
                .value = declaration.value,
                .is_reborrow = is_reborrow,
                .span = node.span,
            } };
        },
        .assign_stmt => |node| blk: {
            const target = try lowerAssignmentTarget(ctx, node.target, imports, scope, function_headers);
            const value = try lowerExpectedValue(ctx, node.value, model.hir.exprType(target.*), imports, scope, function_headers, node.span);
            commitAssignmentTarget(scope, node.target);
            try markInitializedFromAssignment(scope, target.*);
            break :blk .{ .assign_stmt = .{
                .target = target,
                .value = value,
                .span = node.span,
            } };
        },
        .expr_stmt => |node| .{ .expr_stmt = .{
            .expr = try lowerExpr(ctx, node.expr, imports, scope, function_headers),
            .span = node.span,
        } },
        .if_stmt => |node| blk: {
            const condition = try lowerExpr(ctx, node.condition, imports, scope, function_headers);
            var then_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
            defer then_scope.deinit(ctx.allocator);
            const then_body = try lowerBlockStatements(ctx, node.then_block, imports, &then_scope, locals, next_local_id, function_headers, loop_depth, expected_return_type);

            var else_scope: ?model.Scope = null;
            defer if (else_scope) |*value| value.deinit(ctx.allocator);
            const else_body = if (node.else_block) |else_block| else_body: {
                var branch_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
                const lowered_else = try lowerBlockStatements(ctx, else_block, imports, &branch_scope, locals, next_local_id, function_headers, loop_depth, expected_return_type);
                else_scope = branch_scope;
                break :else_body lowered_else;
            } else null;

            try scope_flow.mergeIfState(ctx.allocator, scope, then_scope, else_scope);
            break :blk .{ .if_stmt = .{
                .condition = condition,
                .then_body = then_body,
                .else_body = else_body,
                .span = node.span,
            } };
        },
        .for_stmt => |node| blk: {
            const iterator = try lowerExpr(ctx, node.iterator, imports, scope, function_headers);
            const binding_ty = try resolveArrayElementType(ctx, model.hir.exprType(iterator.*), node.span);
            if (iterator.* == .array and iterator.array.elements.len == 0 and binding_ty.kind == .unknown) {
                var empty_body_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
                defer empty_body_scope.deinit(ctx.allocator);
                break :blk .{ .for_stmt = .{
                    .binding_name = try ctx.allocator.dupe(u8, node.binding_name),
                    .binding_local_id = 0,
                    .binding_ty = binding_ty,
                    .iterator = iterator,
                    .body = try lowerBlockStatements(ctx, node.body, imports, &empty_body_scope, locals, next_local_id, function_headers, loop_depth + 1, expected_return_type),
                    .span = node.span,
                } };
            }
            const local_id = next_local_id.*;
            next_local_id.* += 1;

            var body_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
            defer body_scope.deinit(ctx.allocator);
            try body_scope.put(ctx.allocator, node.binding_name, .{
                .id = local_id,
                .ty = binding_ty,
                .storage = .immutable,
                .initialized = true,
                .decl_span = node.span,
            });
            try locals.append(.{
                .id = local_id,
                .name = try ctx.allocator.dupe(u8, node.binding_name),
                .ty = binding_ty,
                .ownership = .owned,
                .span = node.span,
            });
            const lowered: model.Statement = .{ .for_stmt = .{
                .binding_name = try ctx.allocator.dupe(u8, node.binding_name),
                .binding_local_id = local_id,
                .binding_ty = binding_ty,
                .iterator = iterator,
                .body = try lowerBlockStatements(ctx, node.body, imports, &body_scope, locals, next_local_id, function_headers, loop_depth + 1, expected_return_type),
                .span = node.span,
            } };
            try scope_flow.mergeLoopState(ctx.allocator, scope, body_scope);
            break :blk lowered;
        },
        .while_stmt => |node| blk: {
            var body_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
            defer body_scope.deinit(ctx.allocator);
            const lowered: model.Statement = .{ .while_stmt = .{
                .condition = try lowerExpr(ctx, node.condition, imports, scope, function_headers),
                .body = try lowerBlockStatements(ctx, node.body, imports, &body_scope, locals, next_local_id, function_headers, loop_depth + 1, expected_return_type),
                .span = node.span,
            } };
            try scope_flow.mergeLoopState(ctx.allocator, scope, body_scope);
            break :blk lowered;
        },
        .break_stmt => |node| blk: {
            if (loop_depth == 0) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM075",
                    .title = "break requires a loop",
                    .message = "`break` can only appear inside a `for` or `while` loop.",
                    .labels = &.{diagnostics.primaryLabel(node.span, "break appears outside a loop")},
                    .help = "Move this `break` into a loop body or remove it.",
                });
                return error.DiagnosticsEmitted;
            }
            break :blk .{ .break_stmt = .{ .span = node.span } };
        },
        .continue_stmt => |node| blk: {
            if (loop_depth == 0) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM076",
                    .title = "continue requires a loop",
                    .message = "`continue` can only appear inside a `for` or `while` loop.",
                    .labels = &.{diagnostics.primaryLabel(node.span, "continue appears outside a loop")},
                    .help = "Move this `continue` into a loop body or remove it.",
                });
                return error.DiagnosticsEmitted;
            }
            break :blk .{ .continue_stmt = .{ .span = node.span } };
        },
        .match_stmt => |node| .{ .match_stmt = try lowerMatchStatement(
            ctx,
            node,
            imports,
            scope,
            locals,
            next_local_id,
            function_headers,
            loop_depth,
            expected_return_type,
        ) },
        .switch_stmt => |node| blk: {
            const subject = try lowerExpr(ctx, node.subject, imports, scope, function_headers);
            var cases = std.array_list.Managed(model.SwitchCase).init(ctx.allocator);
            var case_scopes = std.array_list.Managed(model.Scope).init(ctx.allocator);
            defer {
                for (case_scopes.items) |*case_scope| case_scope.deinit(ctx.allocator);
                case_scopes.deinit();
            }
            for (node.cases) |case_node| {
                var case_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
                try cases.append(.{
                    .pattern = try lowerExpr(ctx, case_node.pattern, imports, scope, function_headers),
                    .body = try lowerBlockStatements(ctx, case_node.body, imports, &case_scope, locals, next_local_id, function_headers, loop_depth, expected_return_type),
                    .span = case_node.span,
                });
                try case_scopes.append(case_scope);
            }
            var default_scope: ?model.Scope = null;
            defer if (default_scope) |*value| value.deinit(ctx.allocator);
            const default_body = if (node.default_block) |default_block| default_body: {
                var branch_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
                const lowered_default = try lowerBlockStatements(ctx, default_block, imports, &branch_scope, locals, next_local_id, function_headers, loop_depth, expected_return_type);
                default_scope = branch_scope;
                break :default_body lowered_default;
            } else null;
            try scope_flow.mergeSwitchState(ctx.allocator, scope, case_scopes.items, default_scope);
            break :blk .{ .switch_stmt = .{
                .subject = subject,
                .cases = try cases.toOwnedSlice(),
                .default_body = default_body,
                .span = node.span,
            } };
        },
        .return_stmt => |node| .{ .return_stmt = .{
            .value = if (node.value) |expr|
                if (expected_return_type.kind == .unknown)
                    try lowerExpr(ctx, expr, imports, scope, function_headers)
                else
                    try lowerExpectedValue(ctx, expr, expected_return_type, imports, scope, function_headers, node.span)
            else
                null,
            .span = node.span,
        } },
        // `attempt` is expanded in `lowerBlockStatements` (it may yield multiple statements),
        // so it should never reach the single-statement lowering path.
        .attempt_stmt => |node| {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM139",
                .title = "attempt is not valid here",
                .message = "An `attempt { ... } handle { ... }` block may only appear as a statement in a block body.",
                .labels = &.{diagnostics.primaryLabel(node.span, "attempt is not valid in this position")},
                .help = "Move the `attempt` into a function or block body.",
            });
            return error.DiagnosticsEmitted;
        },
    };
}

fn markInitializedFromAssignment(scope: *model.Scope, target: model.Expr) !void {
    switch (target) {
        .local => |node| {
            var iterator = scope.entries.iterator();
            while (iterator.next()) |entry| {
                if (entry.value_ptr.id != node.local_id) continue;
                entry.value_ptr.initialized = true;
                entry.value_ptr.clearMoveState();
                return;
            }
        },
        else => {},
    }
}

fn emitUnknownLocalName(ctx: *shared.Context, name: []const u8, span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM012",
        .title = "unknown local name",
        .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not find a local binding named '{s}'.", .{name}),
        .labels = &.{diagnostics.primaryLabel(span, "unknown local name")},
        .help = "Declare the value before using it, or qualify imported names.",
    });
}

fn emitUninitializedLocalUse(
    ctx: *shared.Context,
    name: []const u8,
    use_span: source_pkg.Span,
    decl_span: source_pkg.Span,
) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM086",
        .title = "local is not initialized",
        .message = try std.fmt.allocPrint(ctx.allocator, "The local declaration '{s}' does not have a value yet.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(use_span, "local is read before it has been initialized"),
            diagnostics.secondaryLabel(decl_span, "declaration appears here"),
        },
        .help = "Assign to the local before reading it, or add an initializer expression to the declaration.",
    });
}

fn lowerEnumVariantExprExpectedInternal(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) anyerror!?*model.Expr {
    if (expected_type.kind != .enum_instance or expected_type.name == null) return null;
    return lowerEnumVariantExpr(ctx, expr, expected_type, imports, scope, function_headers);
}

fn lowerEnumVariantExpr(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) anyerror!?*model.Expr {
    const EnumTarget = struct {
        enum_name: []const u8,
        variant_name: []const u8,
        payload_expr: ?*syntax.ast.Expr,
        span: source_pkg.Span,
    };

    const enum_target: EnumTarget = switch (expr.*) {
        .member => |node| .{
            .enum_name = expected_type.name orelse try flattenMemberExprPath(ctx.allocator, node.object),
            .variant_name = node.member,
            .payload_expr = @as(?*syntax.ast.Expr, null),
            .span = node.span,
        },
        .call => |node| blk: {
            if (node.callee.* != .member or node.trailing_builder != null or node.trailing_callback != null) return null;
            if (node.args.len > 1) return null;
            break :blk .{
                .enum_name = expected_type.name orelse try flattenMemberExprPath(ctx.allocator, node.callee.member.object),
                .variant_name = node.callee.member.member,
                .payload_expr = if (node.args.len == 1) node.args[0].value else null,
                .span = node.span,
            };
        },
        else => return null,
    };

    const resolved_name = resolveEnumName(ctx, enum_target.enum_name, expected_type.name orelse "");
    const enum_decl = resolveEnumDecl(ctx, resolved_name) orelse return null;
    if (enum_decl.type_params.len != 0 and (expected_type.name == null or !std.mem.eql(u8, expected_type.name.?, resolved_name))) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM106",
            .title = "generic enum constructor needs an explicit type",
            .message = "Generic enum variant construction currently needs an explicit surrounding enum type.",
            .labels = &.{diagnostics.primaryLabel(enum_target.span, "constructor does not provide enough type information")},
            .help = "Write an explicit type such as `let value: Result<String, ParseError> = Result.Ok(\"ok\")`.",
        });
        return error.DiagnosticsEmitted;
    }

    const variant_decl = findEnumVariant(enum_decl, enum_target.variant_name) orelse return null;
    const payload = if (enum_target.payload_expr) |payload_expr|
        if (variant_decl.payload_ty) |payload_ty|
            if (function_headers) |headers|
                try types.lowerExpectedValue(ctx, payload_expr, payload_ty, imports, scope, headers, enum_target.span)
            else
                try lowerExpr(ctx, payload_expr, imports, scope, function_headers)
        else {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM105",
                .title = "match pattern payload is invalid",
                .message = "This enum variant does not accept a payload value.",
                .labels = &.{diagnostics.primaryLabel(enum_target.span, "payload value is not valid for this enum variant")},
                .help = "Remove the argument from this enum constructor call.",
            });
            return error.DiagnosticsEmitted;
        }
    else if (variant_decl.payload_ty != null)
        variant_decl.default_value
    else
        null;

    if (variant_decl.payload_ty != null and payload == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM105",
            .title = "match pattern payload is invalid",
            .message = "This enum variant requires an associated payload value.",
            .labels = &.{diagnostics.primaryLabel(enum_target.span, "missing enum payload value")},
            .help = "Pass the payload argument or add a default value on the enum variant declaration.",
        });
        return error.DiagnosticsEmitted;
    }

    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = .{ .construct_enum_variant = .{
        .enum_name = try ctx.allocator.dupe(u8, resolved_name),
        .variant_name = try ctx.allocator.dupe(u8, variant_decl.name),
        .discriminant = variant_decl.discriminant,
        .payload = payload,
        .ty = .{ .kind = .enum_instance, .name = try ctx.allocator.dupe(u8, resolved_name) },
        .span = enum_target.span,
    } };
    return lowered;
}

fn resolveEnumDecl(ctx: *shared.Context, name: []const u8) ?model.EnumDecl {
    if (ctx.concrete_enums) |concrete_enums| {
        if (concrete_enums.get(name)) |enum_decl| return enum_decl;
    }
    if (ctx.enum_headers) |headers| {
        if (headers.get(name)) |enum_decl| return enum_decl;
    }
    return null;
}

fn resolveEnumName(ctx: *shared.Context, candidate: []const u8, fallback: []const u8) []const u8 {
    if (candidate.len != 0) {
        if (resolveEnumDecl(ctx, candidate) != null) return candidate;
        const leaf = qualifiedLeaf(candidate);
        if (resolveEnumDecl(ctx, leaf) != null) return leaf;
    }
    return fallback;
}

fn findEnumVariant(enum_decl: model.EnumDecl, name: []const u8) ?model.EnumVariantHir {
    for (enum_decl.variants) |variant_decl| {
        if (std.mem.eql(u8, variant_decl.name, name)) return variant_decl;
    }
    return null;
}

pub fn lowerExpr(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) anyerror!*model.Expr {
    if (try lowerEnumVariantExpr(ctx, expr, .{ .kind = .unknown }, imports, scope, function_headers)) |enum_expr| {
        return enum_expr;
    }
    const lowered = try ctx.allocator.create(model.Expr);
    switch (expr.*) {
        .integer => |node| lowered.* = .{ .integer = .{ .value = node.value, .span = node.span } },
        .float => |node| lowered.* = .{ .float = .{ .value = node.value, .span = node.span } },
        .string => |node| lowered.* = .{ .string = .{ .value = node.value, .span = node.span } },
        .bool => |node| lowered.* = .{ .boolean = .{ .value = node.value, .span = node.span } },
        .array => |node| {
            var elements = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            for (node.elements) |element| try elements.append(try lowerExpr(ctx, element, imports, scope, function_headers));
            const array_ty = try resolveArrayLiteralType(ctx, elements.items, node.span);
            lowered.* = .{ .array = .{
                .elements = try elements.toOwnedSlice(),
                .ty = array_ty,
                .span = node.span,
            } };
        },
        .builder_array => |node| {
            lowered.* = .{ .builder_array = .{
                .builder = try lowerBuilderBlock(ctx, node.builder, imports, scope),
                .ty = .{ .kind = .array },
                .span = node.span,
            } };
        },
        .callback => |node| {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM084",
                .title = "callback literal needs an explicit function type",
                .message = "Standalone callback literals need an expected function type from a declaration, field, argument, or return position.",
                .labels = &.{diagnostics.primaryLabel(node.span, "callback literal has no expected function type")},
                .help = "Add an explicit function type such as `let cb: (Int) -> Void = { value in ... }`.",
            });
            return error.DiagnosticsEmitted;
        },
        .struct_literal => |node| {
            lowered.* = try lowerStructLiteralExpr(ctx, node, imports, scope, function_headers);
        },
        .native_state => |node| {
            lowered.* = try lowerNativeStateExpr(ctx, node, imports, scope, function_headers);
        },
        .native_user_data => |node| {
            lowered.* = try lowerNativeUserDataExpr(ctx, node, imports, scope, function_headers);
        },
        .native_recover => |node| {
            lowered.* = try lowerNativeRecoverExpr(ctx, node, imports, scope, function_headers);
        },
        .ownership => |node| {
            return try lowerOwnershipExpr(ctx, node, imports, scope, function_headers);
        },
        .identifier => |node| {
            const name = node.name.segments[0].text;
            if (try shared.resolveLocalOrCapture(ctx, scope.*, name, node.span)) |resolution| {
                const binding = resolution.binding;
                if (!binding.initialized) {
                    try emitUninitializedLocalUse(ctx, name, node.span, binding.decl_span);
                    return error.DiagnosticsEmitted;
                }
                if (binding.moved) {
                    try emitUseAfterMove(ctx, name, node.span, binding.move_span);
                    return error.DiagnosticsEmitted;
                }
                lowered.* = .{ .local = .{
                    .local_id = binding.id,
                    .name = try ctx.allocator.dupe(u8, name),
                    .ty = binding.ty,
                    .storage = binding.storage,
                    .span = node.span,
                } };
            } else if (shared.isImportedRoot(ctx, name, imports)) {
                lowered.* = .{ .namespace_ref = .{
                    .root = try ctx.allocator.dupe(u8, name),
                    .path = try ctx.allocator.dupe(u8, name),
                    .span = node.span,
                } };
            } else if (function_headers) |headers| {
                if (shared.findFunctionHeader(ctx, headers, name)) |header| {
                    lowered.* = .{ .function_ref = .{
                        .representation = .callable_value,
                        .function_id = header.id,
                        .name = try ctx.allocator.dupe(u8, name),
                        .ty = try functionTypeFromHeader(ctx.allocator, header),
                        .span = node.span,
                    } };
                } else if (ctx.imported_globals.findFunction(name)) |function_decl| {
                    lowered.* = .{ .function_ref = .{
                        .representation = .callable_value,
                        .function_id = 0,
                        .name = try ctx.allocator.dupe(u8, function_decl.name),
                        .ty = try functionTypeFromResolvedSignature(ctx.allocator, function_decl.params, function_decl.return_type),
                        .span = node.span,
                    } };
                } else if (try lowerImplicitSelfFieldExpr(ctx, scope, name, node.span)) |field_expr| {
                    lowered.* = field_expr;
                } else {
                    try emitUnknownLocalName(ctx, name, node.span);
                    return error.DiagnosticsEmitted;
                }
            } else if (try lowerImplicitSelfFieldExpr(ctx, scope, name, node.span)) |field_expr| {
                lowered.* = field_expr;
            } else {
                try emitUnknownLocalName(ctx, name, node.span);
                return error.DiagnosticsEmitted;
            }
        },
        .member => |node| {
            if (try lowerParentQualifiedFieldExpr(ctx, node, imports, scope, function_headers)) |field_expr| {
                lowered.* = field_expr;
                return lowered;
            }
            const flattened = try flattenMemberExpr(ctx.allocator, expr);
            if (std.mem.eql(u8, flattened.root, "<expr>")) {
                const object = try lowerExpr(ctx, node.object, imports, scope, function_headers);
                if (tryLowerCountMemberExpr(object, node.member, node.span)) |count_expr| {
                    lowered.* = count_expr;
                    return lowered;
                }
                const object_type = resolveFieldContainerType(ctx, model.hir.exprType(object.*)) orelse {
                    try emitMemberAccessRequiresStructuredType(ctx, node.span);
                    return error.DiagnosticsEmitted;
                };
                const resolved_field = try resolveFieldMember(ctx, model.hir.exprType(object.*), node.member, node.span);
                lowered.* = .{ .field = .{
                    .object = object,
                    .container_type_name = try ctx.allocator.dupe(u8, object_type.name orelse return error.DiagnosticsEmitted),
                    .field_name = try ctx.allocator.dupe(u8, node.member),
                    .field_index = resolved_field.slot_index,
                    .ty = resolved_field.ty,
                    .storage = resolved_field.storage,
                    .span = node.span,
                } };
                return lowered;
            }
            const root_is_type = (ctx.type_headers != null and (ctx.type_headers.?.get(flattened.root) != null)) or
                ctx.imported_globals.findType(flattened.root) != null;
            if ((shared.isImportedRoot(ctx, flattened.root, imports) or root_is_type) and scope.get(flattened.root) == null) {
                if (function_headers) |headers| {
                    if (headers.get(flattened.path)) |header| {
                        if (header.is_accessor) {
                            lowered.* = .{ .call = .{
                                .callee_name = try ctx.allocator.dupe(u8, flattened.path),
                                .function_id = header.id,
                                .args = &.{},
                                .ty = header.return_type,
                                .span = node.span,
                            } };
                            return lowered;
                        }
                        lowered.* = .{ .function_ref = .{
                            .representation = .callable_value,
                            .function_id = header.id,
                            .name = try ctx.allocator.dupe(u8, flattened.path),
                            .ty = try functionTypeFromHeader(ctx.allocator, header),
                            .span = node.span,
                        } };
                        return lowered;
                    }
                }
                // Try to resolve the type of a constant field access like TypeName.fieldName
                var ns_ty: model.ResolvedType = .{ .kind = .unknown };
                const root_type: model.ResolvedType = .{ .kind = .named, .name = flattened.root };
                if (shared.namedTypeHeader(ctx, root_type)) |header| {
                    for (header.fields) |field| {
                        if (std.mem.eql(u8, field.name, node.member)) {
                            ns_ty = field.ty;
                            break;
                        }
                    }
                }
                lowered.* = .{ .namespace_ref = .{
                    .root = flattened.root,
                    .path = flattened.path,
                    .ty = ns_ty,
                    .span = node.span,
                } };
                return lowered;
            }
            if (scope.get(flattened.root) == null) {
                if (node.object.* == .identifier) {
                    if (try lowerImplicitSelfFieldExpr(ctx, scope, flattened.root, exprSpan(node.object.*))) |object_value| {
                        const object = try ctx.allocator.create(model.Expr);
                        object.* = object_value;
                        if (tryLowerCountMemberExpr(object, node.member, node.span)) |count_expr| {
                            lowered.* = count_expr;
                            return lowered;
                        }
                        const object_type = resolveFieldContainerType(ctx, model.hir.exprType(object.*)) orelse {
                            try emitMemberAccessRequiresStructuredType(ctx, node.span);
                            return error.DiagnosticsEmitted;
                        };
                        const resolved_field = try resolveFieldMember(ctx, model.hir.exprType(object.*), node.member, node.span);
                        lowered.* = .{ .field = .{
                            .object = object,
                            .container_type_name = try ctx.allocator.dupe(u8, object_type.name orelse return error.DiagnosticsEmitted),
                            .field_name = try ctx.allocator.dupe(u8, node.member),
                            .field_index = resolved_field.slot_index,
                            .ty = resolved_field.ty,
                            .storage = resolved_field.storage,
                            .span = node.span,
                        } };
                        return lowered;
                    }
                }
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM027",
                    .title = "invalid namespaced reference",
                    .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not resolve the namespace root '{s}'.", .{flattened.root}),
                    .labels = &.{
                        diagnostics.primaryLabel(node.span, "unknown namespace root"),
                    },
                    .help = "Import the module first or use a local name instead.",
                });
                return error.DiagnosticsEmitted;
            }

            const object = try lowerExpr(ctx, node.object, imports, scope, function_headers);
            if (tryLowerCountMemberExpr(object, node.member, node.span)) |count_expr| {
                lowered.* = count_expr;
                return lowered;
            }
            // A computed-property accessor (`widget.node`) runs the Widget->Node bridge.
            if (try members.tryLowerComputedAccessor(ctx, object, node.member, node.span)) |accessor| {
                lowered.* = accessor.*;
                return lowered;
            }
            const object_type = resolveFieldContainerType(ctx, model.hir.exprType(object.*)) orelse {
                try emitMemberAccessRequiresStructuredType(ctx, node.span);
                return error.DiagnosticsEmitted;
            };
            const resolved_field = try resolveFieldMember(ctx, model.hir.exprType(object.*), node.member, node.span);
            lowered.* = .{ .field = .{
                .object = object,
                .container_type_name = try ctx.allocator.dupe(u8, object_type.name orelse return error.DiagnosticsEmitted),
                .field_name = try ctx.allocator.dupe(u8, node.member),
                .field_index = resolved_field.slot_index,
                .ty = resolved_field.ty,
                .storage = resolved_field.storage,
                .span = node.span,
            } };
        },
        .index => |node| {
            const object = try lowerExpr(ctx, node.object, imports, scope, function_headers);
            const index = try lowerExpr(ctx, node.index, imports, scope, function_headers);
            const index_ty = model.hir.exprType(index.*);
            if (index_ty.kind != .integer) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM077",
                    .title = "array index must be an integer",
                    .message = "Index expressions currently require an integer index.",
                    .labels = &.{diagnostics.primaryLabel(exprSpan(node.index.*), "index is not an integer")},
                    .help = "Use an `Int` or fixed-width integer value inside `[...]`.",
                });
                return error.DiagnosticsEmitted;
            }
            const element_ty = try types.resolveIndexElementType(ctx, model.hir.exprType(object.*), node.span);
            lowered.* = .{ .index = .{
                .object = object,
                .index = index,
                .ty = element_ty,
                .span = node.span,
            } };
        },
        .unary => |node| {
            const operand = try lowerExpr(ctx, node.operand, imports, scope, function_headers);
            const operand_type = model.hir.exprType(operand.*);
            lowered.* = .{ .unary = .{
                .op = @enumFromInt(@intFromEnum(node.op)),
                .operand = operand,
                .ty = switch (node.op) {
                    .negate => operand_type,
                    .not => .{ .kind = .boolean },
                },
                .span = node.span,
            } };
        },
        .binary => |node| {
            const lhs = try lowerExpr(ctx, node.lhs, imports, scope, function_headers);
            if (try lowerBinaryOperatorMethod(ctx, node, lhs, imports, scope, function_headers)) |operator_call| {
                lowered.* = operator_call;
                return lowered;
            }
            const rhs = try lowerExpr(ctx, node.rhs, imports, scope, function_headers);
            const ty = try resolveBinaryType(ctx, node.op, lhs, rhs, node.span);
            lowered.* = .{ .binary = .{
                .op = switch (node.op) {
                    .add => .add,
                    .subtract => .subtract,
                    .multiply => .multiply,
                    .divide => .divide,
                    .modulo => .modulo,
                    .equal => .equal,
                    .not_equal => .not_equal,
                    .less => .less,
                    .less_equal => .less_equal,
                    .greater => .greater,
                    .greater_equal => .greater_equal,
                    .logical_and => .logical_and,
                    .logical_or => .logical_or,
                },
                .lhs = lhs,
                .rhs = rhs,
                .ty = ty,
                .span = node.span,
            } };
        },
        .conditional => |node| {
            const condition = try lowerExpr(ctx, node.condition, imports, scope, function_headers);
            if (model.hir.exprType(condition.*).kind != .boolean) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM049",
                    .title = "conditional expression requires a boolean condition",
                    .message = "The condition in a `? :` expression must resolve to `Bool`.",
                    .labels = &.{
                        diagnostics.primaryLabel(exprSpan(node.condition.*), "condition is not a boolean"),
                    },
                    .help = "Make the condition resolve to `Bool` before using a conditional expression.",
                });
                return error.DiagnosticsEmitted;
            }

            const then_expr = try lowerExpr(ctx, node.then_expr, imports, scope, function_headers);
            const else_expr = try lowerExpr(ctx, node.else_expr, imports, scope, function_headers);
            const ty = try resolveConditionalType(ctx, model.hir.exprType(then_expr.*), model.hir.exprType(else_expr.*), node.span);
            lowered.* = .{ .conditional = .{
                .condition = condition,
                .then_expr = then_expr,
                .else_expr = else_expr,
                .ty = ty,
                .span = node.span,
            } };
        },
        .call => |node| try lowerCallExpr(ctx, lowered, node, imports, scope, function_headers),
        // A `try` expression is only meaningful as the initializer of a `let` binding or an
        // expression statement inside an `attempt` block, where it is desugared before lowering.
        // Any `try` reaching general expression lowering is in an unsupported position.
        .try_expr => |node| {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM133",
                .title = "'try' is not valid here",
                .message = "`try` may only be used as a `let` initializer or expression statement inside an `attempt { ... }` block.",
                .labels = &.{diagnostics.primaryLabel(node.span, "`try` is not valid in this position")},
                .help = "Wrap the failing call in an `attempt { ... } handle { ... }` block, and use `try` as `let x = try call()` or as a statement `try call()`.",
            });
            return error.DiagnosticsEmitted;
        },
    }
    return lowered;
}

fn lowerBinaryOperatorMethod(
    ctx: *shared.Context,
    node: syntax.ast.BinaryExpr,
    lhs: *model.Expr,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !?model.Expr {
    const method_name = binaryOperatorMethodName(node.op) orelse return null;
    const lhs_type = model.hir.exprType(lhs.*);
    if (lhs_type.kind != .named and lhs_type.kind != .native_state_view) return null;

    const resolved_method = (try resolveMethodMemberOrNull(ctx, lhs_type, method_name, node.span)) orelse return null;
    const args = try ctx.allocator.alloc(syntax.ast.CallArg, 1);
    args[0] = .{
        .label = null,
        .value = node.rhs,
        .span = exprSpan(node.rhs.*),
    };
    const fake_call = syntax.ast.CallExpr{
        .callee = node.lhs,
        .args = args,
        .trailing_builder = null,
        .trailing_callback = null,
        .span = node.span,
    };
    const lowered = try buildDispatchedMethodCallExpr(ctx, resolved_method, lhs, lhs_type, fake_call, imports, scope, function_headers);
    return lowered.*;
}

fn binaryOperatorMethodName(op: syntax.ast.BinaryOp) ?[]const u8 {
    return switch (op) {
        .add => "add",
        .subtract => "subtract",
        .multiply => "multiply",
        .divide => "divide",
        else => null,
    };
}
