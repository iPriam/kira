const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const exprs = @import("lower_exprs.zig");

const ast = syntax.ast;

// Lower an `attempt { ... } handle { ... }` statement. The failure flow is validated
// (KSEM134/135/136/137) and then desugared to `match` statements over the `Result` values,
// reusing the existing enum match lowering so the construct executes on every backend that
// executes `Result` (vm/llvm/hybrid) with no backend-specific code.
pub fn lowerAttempt(
    ctx: *shared.Context,
    node: ast.AttemptStatement,
    imports: []const model.Import,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    loop_depth: usize,
    expected_return_type: model.ResolvedType,
) ![]model.Statement {
    try validateAttempt(ctx, node, imports, scope, function_headers);
    const desugared = try desugarStatements(ctx, node.body, node.handlers);
    return exprs.lowerBlockStatements(
        ctx,
        .{ .statements = desugared, .span = node.span },
        imports,
        scope,
        locals,
        next_local_id,
        function_headers,
        loop_depth,
        expected_return_type,
    );
}

// ---- validation ----

fn validateAttempt(
    ctx: *shared.Context,
    node: ast.AttemptStatement,
    imports: []const model.Import,
    scope: *model.Scope,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    var failure_name: ?[]const u8 = null;
    var failure_enum: ?model.EnumDecl = null;

    for (node.body) |stmt| {
        const operand = topLevelTryOperand(stmt) orelse continue;
        const span = exprSpan(operand.*);

        // Lower the operand in a throwaway copy of the scope to learn its type without
        // disturbing the real scope's move/initialization state.
        var clone = try scope.clone(ctx.allocator);
        defer clone.deinit(ctx.allocator);
        const lowered = try exprs.lowerExpr(ctx, operand, imports, &clone, function_headers);
        const ty = model.hir.exprType(lowered.*);

        const result = resolveResult(ctx, ty);
        switch (result) {
            .not_result => {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM134",
                    .title = "'try' on non-Result value",
                    .message = "`try` may only unwrap a `Result<Value, Failure>` value.",
                    .labels = &.{diagnostics.primaryLabel(span, "this expression is not a Result")},
                    .help = "Call something that returns `Result<Value, Failure>`, or handle this value without `try`.",
                });
                return error.DiagnosticsEmitted;
            },
            .failure_name_only, .failure => {
                const this_name = switch (result) {
                    .failure_name_only => |name| name,
                    .failure => |decl| decl.name,
                    else => unreachable,
                };
                if (failure_name) |prev| {
                    if (!std.mem.eql(u8, prev, this_name)) {
                        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                            .severity = .@"error",
                            .code = "KSEM137",
                            .title = "incompatible failure enums in a single 'attempt'",
                            .message = try std.fmt.allocPrint(ctx.allocator, "This `attempt` mixes failure types '{s}' and '{s}'.", .{ prev, this_name }),
                            .labels = &.{diagnostics.primaryLabel(span, "this `try` produces a different failure type")},
                            .help = "Use a single failure enum across all `try` expressions in one `attempt`, or split into separate `attempt` blocks.",
                        });
                        return error.DiagnosticsEmitted;
                    }
                } else {
                    failure_name = this_name;
                    if (result == .failure) failure_enum = result.failure;
                }
            },
        }
    }

    // With a resolved failure enum, handle cases must reference real variants (KSEM136) and
    // cover every reachable variant (KSEM135).
    if (failure_enum) |fe| {
        for (node.handlers) |handler| {
            if (findVariant(fe, handler.variant_name) == null) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM136",
                    .title = "handle case is not a failure variant",
                    .message = try std.fmt.allocPrint(ctx.allocator, "'{s}' is not a variant of the failure enum '{s}'.", .{ handler.variant_name, fe.name }),
                    .labels = &.{diagnostics.primaryLabel(handler.span, "unknown failure variant")},
                    .help = "Handle a variant declared by the failure enum.",
                });
                return error.DiagnosticsEmitted;
            }
        }
        for (fe.variants) |variant| {
            if (!handlersCover(node.handlers, variant.name)) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM135",
                    .title = "missing handle case for reachable failure variant",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The `attempt` does not handle the failure variant '{s}.{s}'.", .{ fe.name, variant.name }),
                    .labels = &.{diagnostics.primaryLabel(node.span, "this `attempt` is missing a handle case")},
                    .help = "Add a `handle` case for every reachable failure variant.",
                });
                return error.DiagnosticsEmitted;
            }
        }
    }
}

const ResolveResult = union(enum) {
    not_result,
    failure_name_only: []const u8,
    failure: model.EnumDecl,
};

fn resolveResult(ctx: *shared.Context, ty: model.ResolvedType) ResolveResult {
    if (ty.kind != .enum_instance or ty.name == null) return .not_result;
    const result_enum = resolveEnum(ctx, ty.name.?) orelse return .not_result;
    var has_ok = false;
    var error_payload: ?model.ResolvedType = null;
    for (result_enum.variants) |variant| {
        if (std.mem.eql(u8, variant.name, "Ok")) has_ok = true;
        if (std.mem.eql(u8, variant.name, "Error")) error_payload = variant.payload_ty;
    }
    if (!has_ok or error_payload == null) return .not_result;
    const failure = error_payload.?;
    if (failure.kind != .enum_instance or failure.name == null) {
        return if (failure.name) |name| .{ .failure_name_only = name } else .not_result;
    }
    if (resolveEnum(ctx, failure.name.?)) |decl| return .{ .failure = decl };
    return .{ .failure_name_only = failure.name.? };
}

fn resolveEnum(ctx: *shared.Context, name: []const u8) ?model.EnumDecl {
    if (ctx.concrete_enums) |concrete| if (concrete.get(name)) |decl| return decl;
    if (ctx.enum_headers) |headers| if (headers.get(name)) |decl| return decl;
    return null;
}

fn findVariant(enum_decl: model.EnumDecl, name: []const u8) ?model.EnumVariantHir {
    for (enum_decl.variants) |variant| {
        if (std.mem.eql(u8, variant.name, name)) return variant;
    }
    return null;
}

fn handlersCover(handlers: []const ast.HandleCase, variant_name: []const u8) bool {
    for (handlers) |handler| {
        if (std.mem.eql(u8, handler.variant_name, variant_name)) return true;
    }
    return false;
}

// ---- desugaring to match ----

// Rewrite a statement list containing `try` into nested `match` statements. The first statement
// that performs a `try` splits the continuation: on `Ok` the remaining statements run, on `Error`
// control transfers to a match over the failure that dispatches to the handle cases.
fn desugarStatements(
    ctx: *shared.Context,
    stmts: []const ast.Statement,
    handlers: []const ast.HandleCase,
) ![]ast.Statement {
    var index: usize = 0;
    while (index < stmts.len) : (index += 1) {
        if (topLevelTryOperand(stmts[index]) != null) break;
    }
    if (index == stmts.len) {
        // No `try` remains; the statements run unconditionally.
        return ctx.allocator.dupe(ast.Statement, stmts);
    }

    const try_stmt = stmts[index];
    const operand = topLevelTryOperand(try_stmt).?;
    const binding = tryBinding(try_stmt);
    const span = exprSpan(operand.*);

    const continuation = try desugarStatements(ctx, stmts[index + 1 ..], handlers);

    // Build a `match` over the `Result`: `Ok(binding)` carries the continuation, and `Error`
    // binds the failure and dispatches it to a nested `match` over the handle cases.
    var arms = std.array_list.Managed(ast.MatchArm).init(ctx.allocator);
    try arms.append(.{
        .patterns = try mkPatternList(ctx, try mkVariantPattern(ctx, "Ok", binding, span)),
        .guard = null,
        .body = .{ .statements = continuation, .span = span },
        .span = span,
    });
    const failure_binding = "__kira_failure";
    const error_body = try mkBlock(ctx, &.{try mkHandlerMatch(ctx, failure_binding, handlers, span)}, span);
    try arms.append(.{
        .patterns = try mkPatternList(ctx, try mkVariantPattern(ctx, "Error", failure_binding, span)),
        .guard = null,
        .body = error_body,
        .span = span,
    });

    const match_stmt = ast.Statement{ .match_stmt = .{
        .subject = operand,
        .arms = try arms.toOwnedSlice(),
        .span = span,
    } };

    var result = std.array_list.Managed(ast.Statement).init(ctx.allocator);
    try result.appendSlice(stmts[0..index]);
    try result.append(match_stmt);
    return result.toOwnedSlice();
}

fn mkHandlerMatch(
    ctx: *shared.Context,
    subject_name: []const u8,
    handlers: []const ast.HandleCase,
    span: source_pkg.Span,
) !ast.Statement {
    var arms = std.array_list.Managed(ast.MatchArm).init(ctx.allocator);
    for (handlers) |handler| {
        const binding = handler.binding_name orelse "_";
        try arms.append(.{
            .patterns = try mkPatternList(ctx, try mkVariantPattern(ctx, handler.variant_name, binding, handler.span)),
            .guard = null,
            .body = handler.body,
            .span = handler.span,
        });
    }
    return .{ .match_stmt = .{
        .subject = try mkIdent(ctx, subject_name, span),
        .arms = try arms.toOwnedSlice(),
        .span = span,
    } };
}

// `Variant(binding)` — destructure a variant, binding its payload to `binding`.
fn mkVariantPattern(ctx: *shared.Context, variant_name: []const u8, binding: []const u8, span: source_pkg.Span) !ast.MatchPattern {
    const inner = try ctx.allocator.create(ast.MatchPattern);
    inner.* = .{ .bare_variant = .{ .name = binding, .span = span } };
    return .{ .destructure = .{ .variant_name = variant_name, .inner = inner, .span = span } };
}

fn mkPatternList(ctx: *shared.Context, pattern: ast.MatchPattern) ![]ast.MatchPattern {
    const patterns = try ctx.allocator.alloc(ast.MatchPattern, 1);
    patterns[0] = pattern;
    return patterns;
}

fn mkBlock(ctx: *shared.Context, statements: []const ast.Statement, span: source_pkg.Span) !ast.Block {
    return .{ .statements = try ctx.allocator.dupe(ast.Statement, statements), .span = span };
}

fn mkIdent(ctx: *shared.Context, name: []const u8, span: source_pkg.Span) !*ast.Expr {
    const segments = try ctx.allocator.alloc(ast.NameSegment, 1);
    segments[0] = .{ .text = name, .span = span };
    const expr = try ctx.allocator.create(ast.Expr);
    expr.* = .{ .identifier = .{ .name = .{ .segments = segments, .span = span }, .span = span } };
    return expr;
}

fn topLevelTryOperand(stmt: ast.Statement) ?*ast.Expr {
    switch (stmt) {
        .let_stmt => |node| if (node.value) |value| if (value.* == .try_expr) return value.try_expr.operand,
        .expr_stmt => |node| if (node.expr.* == .try_expr) return node.expr.try_expr.operand,
        else => {},
    }
    return null;
}

fn tryBinding(stmt: ast.Statement) []const u8 {
    return switch (stmt) {
        .let_stmt => |node| node.name,
        else => "_",
    };
}

fn exprSpan(expr: ast.Expr) source_pkg.Span {
    return switch (expr) {
        inline else => |node| node.span,
    };
}
