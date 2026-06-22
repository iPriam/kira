const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const parent = @import("lower_exprs.zig");
const scope_flow = @import("lower_exprs_scope_flow.zig");
const lowerExpr = parent.lowerExpr;
const lowerBlockStatements = parent.lowerBlockStatements;

pub fn lowerMatchStatement(
    ctx: *shared.Context,
    node: syntax.ast.MatchStatement,
    imports: []const model.Import,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    loop_depth: usize,
    expected_return_type: model.ResolvedType,
) !model.MatchStatement {
    const subject = try lowerExpr(ctx, node.subject, imports, scope, function_headers);
    const subject_ty = model.hir.exprType(subject.*);
    if (subject_ty.kind != .enum_instance or subject_ty.name == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM104",
            .title = "match requires an enum subject",
            .message = "Match statements currently require the subject expression to resolve to an enum value.",
            .labels = &.{diagnostics.primaryLabel(node.span, "this value is not an enum")},
            .help = "Match on an enum instance such as `ParseError.UnexpectedEnd` or `Result.Ok(value)`.",
        });
        return error.DiagnosticsEmitted;
    }

    const enum_decl = resolveEnumDecl(ctx, subject_ty.name.?) orelse {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM104",
            .title = "match requires an enum subject",
            .message = "The subject enum type could not be resolved during match lowering.",
            .labels = &.{diagnostics.primaryLabel(node.span, "enum metadata is missing for this match subject")},
            .help = "Declare the enum before matching on it.",
        });
        return error.DiagnosticsEmitted;
    };

    var arms = std.array_list.Managed(model.MatchArm).init(ctx.allocator);
    var arm_scopes = std.array_list.Managed(model.Scope).init(ctx.allocator);
    defer {
        for (arm_scopes.items) |*arm_scope| arm_scope.deinit(ctx.allocator);
        arm_scopes.deinit();
    }
    var covered = std.AutoHashMapUnmanaged(u32, source_pkg.Span){};
    defer covered.deinit(ctx.allocator);

    for (node.arms) |arm_node| {
        for (arm_node.patterns) |pattern_node| {
            var arm_scope = try scope_flow.cloneScope(ctx.allocator, scope.*);
            const lowered_pattern = try lowerPattern(
                ctx,
                pattern_node,
                subject_ty,
                enum_decl,
                false,
                &arm_scope,
                locals,
                next_local_id,
            );
            const discriminant = topLevelDiscriminant(lowered_pattern);
            if (covered.get(discriminant)) |previous_span| {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM101",
                    .title = "duplicate match arm",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The enum variant '{s}' is matched more than once.", .{topLevelVariantName(lowered_pattern)}),
                    .labels = &.{
                        diagnostics.primaryLabel(matchPatternSpan(pattern_node), "duplicate match arm appears here"),
                        diagnostics.secondaryLabel(previous_span, "first match arm for this variant was here"),
                    },
                    .help = "Keep each enum variant in at most one match arm.",
                });
                return error.DiagnosticsEmitted;
            }
            try covered.put(ctx.allocator, discriminant, matchPatternSpan(pattern_node));
            const guard = if (arm_node.guard) |guard_expr|
                try lowerExpr(ctx, guard_expr, imports, &arm_scope, function_headers)
            else
                null;
            const body = try lowerBlockStatements(ctx, arm_node.body, imports, &arm_scope, locals, next_local_id, function_headers, loop_depth, expected_return_type);
            try arms.append(.{
                .pattern = lowered_pattern,
                .guard = guard,
                .body = body,
                .span = arm_node.span,
            });
            try arm_scopes.append(arm_scope);
        }
    }

    if (covered.count() != enum_decl.variants.len) {
        var missing = std.array_list.Managed(u8).init(ctx.allocator);
        for (enum_decl.variants) |variant_decl| {
            if (covered.contains(variant_decl.discriminant)) continue;
            if (missing.items.len != 0) try missing.appendSlice(", ");
            try missing.appendSlice(variant_decl.name);
        }
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM100",
            .title = "non-exhaustive match",
            .message = try std.fmt.allocPrint(ctx.allocator, "This match does not cover enum variant(s): {s}.", .{missing.items}),
            .labels = &.{diagnostics.primaryLabel(node.span, "match is missing one or more enum variants")},
            .help = "Add match arms for every enum variant.",
        });
        return error.DiagnosticsEmitted;
    }

    const has_guards = for (node.arms) |arm_node| {
        if (arm_node.guard != null) break true;
    } else false;
    try scope_flow.mergeMatchState(ctx.allocator, scope, arm_scopes.items, has_guards);

    return .{
        .subject = subject,
        .arms = try arms.toOwnedSlice(),
        .enum_name = try ctx.allocator.dupe(u8, subject_ty.name.?),
        .span = node.span,
    };
}

fn lowerPattern(
    ctx: *shared.Context,
    pattern: syntax.ast.MatchPattern,
    expected_ty: model.ResolvedType,
    enum_decl: model.EnumDecl,
    allow_enum_binding: bool,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
) !model.MatchPattern {
    if (expected_ty.kind != .enum_instance or expected_ty.name == null) {
        return lowerBindingPattern(ctx, pattern, expected_ty, scope, locals, next_local_id);
    }

    return switch (pattern) {
        .bare_variant => |node| blk: {
            if (findVariant(enum_decl, node.name)) |variant| {
                break :blk .{ .variant = .{
                    .variant_name = try ctx.allocator.dupe(u8, variant.name),
                    .discriminant = variant.discriminant,
                    .payload_ty = variant.payload_ty,
                    .span = node.span,
                } };
            }
            if (allow_enum_binding) {
                break :blk .{ .binding = .{
                    .local_id = try appendPatternLocal(ctx, scope, locals, next_local_id, node.name, expected_ty, node.span),
                    .name = try ctx.allocator.dupe(u8, node.name),
                    .ty = expected_ty,
                    .span = node.span,
                } };
            }
            try emitUnknownVariant(ctx, node.span, enum_decl.name, node.name);
            return error.DiagnosticsEmitted;
        },
        .destructure => |node| blk: {
            const variant = findVariant(enum_decl, node.variant_name) orelse {
                try emitUnknownVariant(ctx, node.span, enum_decl.name, node.variant_name);
                return error.DiagnosticsEmitted;
            };
            const payload_ty = variant.payload_ty orelse {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM105",
                    .title = "match pattern payload is invalid",
                    .message = "This enum variant does not carry an associated payload value.",
                    .labels = &.{diagnostics.primaryLabel(node.span, "payload destructuring is not valid for this variant")},
                    .help = "Remove the payload pattern or add an associated type to the enum variant declaration.",
                });
                return error.DiagnosticsEmitted;
            };
            const next_enum_decl = if (payload_ty.kind == .enum_instance and payload_ty.name != null) resolveEnumDecl(ctx, payload_ty.name.?) else null;
            const inner_pattern = try ctx.allocator.create(model.MatchPattern);
            inner_pattern.* = try lowerPattern(
                ctx,
                node.inner.*,
                payload_ty,
                next_enum_decl orelse enum_decl,
                true,
                scope,
                locals,
                next_local_id,
            );
            break :blk .{ .variant = .{
                .variant_name = try ctx.allocator.dupe(u8, variant.name),
                .discriminant = variant.discriminant,
                .payload_ty = payload_ty,
                .inner = inner_pattern,
                .span = node.span,
            } };
        },
        .as_binding => |node| blk: {
            var lowered = try lowerPattern(ctx, node.inner.*, expected_ty, enum_decl, allow_enum_binding, scope, locals, next_local_id);
            switch (lowered) {
                .variant => |*variant_pattern| {
                    const local_id = try appendPatternLocal(ctx, scope, locals, next_local_id, node.binding_name, expected_ty, node.span);
                    variant_pattern.as_binding_local_id = local_id;
                    variant_pattern.as_binding_ty = expected_ty;
                },
                .binding => {},
            }
            break :blk lowered;
        },
    };
}

fn lowerBindingPattern(
    ctx: *shared.Context,
    pattern: syntax.ast.MatchPattern,
    expected_ty: model.ResolvedType,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
) !model.MatchPattern {
    return switch (pattern) {
        .bare_variant => |node| .{ .binding = .{
            .local_id = try appendPatternLocal(ctx, scope, locals, next_local_id, node.name, expected_ty, node.span),
            .name = try ctx.allocator.dupe(u8, node.name),
            .ty = expected_ty,
            .span = node.span,
        } },
        .as_binding => |node| try lowerBindingPattern(ctx, node.inner.*, expected_ty, scope, locals, next_local_id),
        .destructure => |node| blk: {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM105",
                .title = "match pattern payload is invalid",
                .message = "Only enum payloads can be destructured with nested match patterns.",
                .labels = &.{diagnostics.primaryLabel(node.span, "this payload value is not another enum")},
                .help = "Bind the payload to a local name instead, for example `Ok(value)`.",
            });
            break :blk error.DiagnosticsEmitted;
        },
    };
}

fn appendPatternLocal(
    ctx: *shared.Context,
    scope: *model.Scope,
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
    name: []const u8,
    ty: model.ResolvedType,
    span: source_pkg.Span,
) !u32 {
    const local_id = next_local_id.*;
    next_local_id.* += 1;
    try scope.put(ctx.allocator, name, .{
        .id = local_id,
        .ty = ty,
        .storage = .immutable,
        .ownership = .owned,
        .initialized = true,
        .decl_span = span,
    });
    try locals.append(.{
        .id = local_id,
        .name = try ctx.allocator.dupe(u8, name),
        .ty = ty,
        .ownership = .owned,
        .span = span,
    });
    return local_id;
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

fn findVariant(enum_decl: model.EnumDecl, name: []const u8) ?model.EnumVariantHir {
    for (enum_decl.variants) |variant_decl| {
        if (std.mem.eql(u8, variant_decl.name, name)) return variant_decl;
    }
    return null;
}

fn topLevelDiscriminant(pattern: model.MatchPattern) u32 {
    return switch (pattern) {
        .variant => |node| node.discriminant,
        .binding => unreachable,
    };
}

fn topLevelVariantName(pattern: model.MatchPattern) []const u8 {
    return switch (pattern) {
        .variant => |node| node.variant_name,
        .binding => unreachable,
    };
}

fn emitUnknownVariant(ctx: *shared.Context, span: source_pkg.Span, enum_name: []const u8, variant_name: []const u8) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM104",
        .title = "unknown enum variant",
        .message = try std.fmt.allocPrint(ctx.allocator, "The enum '{s}' does not declare a variant named '{s}'.", .{ enum_name, variant_name }),
        .labels = &.{diagnostics.primaryLabel(span, "unknown enum variant")},
        .help = "Use a declared enum variant name in this match arm.",
    });
}

fn matchPatternSpan(pattern: syntax.ast.MatchPattern) source_pkg.Span {
    return switch (pattern) {
        .bare_variant => |node| node.span,
        .destructure => |node| node.span,
        .as_binding => |node| node.span,
    };
}
