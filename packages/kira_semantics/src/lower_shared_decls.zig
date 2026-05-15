const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");
const parent = @import("lower_shared.zig");
const Context = parent.Context;
const qualifiedNameText = parent.qualifiedNameText;
const qualifiedNameLeaf = parent.qualifiedNameLeaf;
const typeFromSyntaxChecked = parent.typeFromSyntaxChecked;
const typeLabel = parent.typeLabel;
const annotationLiteralValue = parent.annotationLiteralValue;
const annotationValueForParameter = parent.annotationValueForParameter;
pub fn lowerAnnotationDecl(ctx: *Context, decl: syntax.ast.AnnotationDecl, module_path: []const u8) !model.AnnotationDecl {
    var parameter_names = std.StringHashMapUnmanaged(source_pkg.Span){};
    defer parameter_names.deinit(ctx.allocator);
    var parameters = std.array_list.Managed(model.AnnotationParameterDecl).init(ctx.allocator);

    for (decl.parameters) |param| {
        if (parameter_names.get(param.name)) |previous_span| {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM061",
                .title = "duplicate annotation parameter",
                .message = try std.fmt.allocPrint(ctx.allocator, "The annotation '{s}' declares parameter '{s}' more than once.", .{ decl.name, param.name }),
                .labels = &.{
                    diagnostics.primaryLabel(param.span, "duplicate parameter"),
                    diagnostics.secondaryLabel(previous_span, "first parameter was declared here"),
                },
                .help = "Remove or rename one of the parameters.",
            });
            return error.DiagnosticsEmitted;
        }
        try parameter_names.put(ctx.allocator, param.name, param.span);

        const param_type = try typeFromSyntaxChecked(ctx, param.type_expr.*);
        if (!isSupportedAnnotationParameterType(param_type)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM062",
                .title = "unsupported annotation parameter type",
                .message = try std.fmt.allocPrint(ctx.allocator, "Annotation parameter '{s}' uses unsupported type {s}.", .{ param.name, typeLabel(param_type) }),
                .labels = &.{diagnostics.primaryLabel(param.span, "unsupported annotation parameter type")},
                .help = "Use Bool, Int, Float, or String for first-version annotation parameters.",
            });
            return error.DiagnosticsEmitted;
        }

        const default_value = if (param.default_value) |value|
            (try annotationValueForParameter(ctx, decl.name, param.name, param_type, value, true)).value
        else
            null;

        try parameters.append(.{
            .name = try ctx.allocator.dupe(u8, param.name),
            .ty = param_type,
            .default_value = default_value,
            .span = param.span,
        });
    }

    return .{
        .name = try ctx.allocator.dupe(u8, decl.name),
        .targets = try lowerAnnotationTargets(ctx.allocator, decl.targets),
        .uses = try lowerCapabilityUses(ctx.allocator, decl.uses),
        .generated_functions = try lowerGeneratedFunctions(ctx, decl.name, decl.generated_members),
        .parameters = try parameters.toOwnedSlice(),
        .module_path = try ctx.allocator.dupe(u8, module_path),
        .span = decl.span,
    };
}

pub fn lowerCapabilityDecl(ctx: *Context, decl: syntax.ast.CapabilityDecl, module_path: []const u8) !model.CapabilityDecl {
    return .{
        .name = try ctx.allocator.dupe(u8, decl.name),
        .generated_functions = try lowerGeneratedFunctions(ctx, decl.name, decl.generated_members),
        .module_path = try ctx.allocator.dupe(u8, module_path),
        .span = decl.span,
    };
}

pub fn lowerAnnotationTargets(allocator: std.mem.Allocator, targets: []const syntax.ast.AnnotationTarget) ![]model.AnnotationTarget {
    const lowered = try allocator.alloc(model.AnnotationTarget, targets.len);
    for (targets, 0..) |target, index| {
        lowered[index] = switch (target) {
            .class => .class,
            .struct_decl => .struct_decl,
            .function => .function,
            .construct => .construct,
            .field => .field,
        };
    }
    return lowered;
}

pub fn lowerCapabilityUses(allocator: std.mem.Allocator, uses: []const syntax.ast.QualifiedName) ![]const []const u8 {
    const lowered = try allocator.alloc([]const u8, uses.len);
    for (uses, 0..) |use_name, index| {
        lowered[index] = try qualifiedNameLeaf(allocator, use_name);
    }
    return lowered;
}

pub fn lowerGeneratedFunctions(ctx: *Context, source_name: []const u8, members: []const syntax.ast.GeneratedMember) ![]model.GeneratedFunction {
    var lowered = std.array_list.Managed(model.GeneratedFunction).init(ctx.allocator);
    for (members) |generated_member| {
        switch (generated_member.member) {
            .function_decl => |function_decl| {
                var params = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
                var param_ownership = std.array_list.Managed(model.OwnershipMode).init(ctx.allocator);
                for (function_decl.params) |param| {
                    try param_ownership.append(parent.ownershipModeFromSyntax(param.type_expr));
                    if (param.type_expr) |type_expr| {
                        try params.append(try typeFromSyntaxChecked(ctx, type_expr.*));
                    } else {
                        try params.append(.{ .kind = .unknown });
                    }
                }
                try lowered.append(.{
                    .name = try ctx.allocator.dupe(u8, function_decl.name),
                    .overridable = generated_member.overridable,
                    .params = try params.toOwnedSlice(),
                    .param_ownership = try param_ownership.toOwnedSlice(),
                    .return_type = if (function_decl.return_type) |return_type| try typeFromSyntaxChecked(ctx, return_type.*) else .{ .kind = .unknown },
                    .return_ownership = parent.ownershipModeFromSyntax(function_decl.return_type),
                    .source_annotation = try ctx.allocator.dupe(u8, source_name),
                    .span = generated_member.span,
                });
            },
            else => {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM070",
                    .title = "unsupported generated member",
                    .message = "Generated blocks currently support function members.",
                    .labels = &.{diagnostics.primaryLabel(generated_member.span, "unsupported generated member")},
                    .help = "Use `function` inside `generated { ... }`.",
                });
                return error.DiagnosticsEmitted;
            },
        }
    }
    return lowered.toOwnedSlice();
}

pub fn isSupportedAnnotationParameterType(ty: model.ResolvedType) bool {
    return ty.kind == .boolean or ty.kind == .integer or ty.kind == .float or ty.kind == .string;
}
