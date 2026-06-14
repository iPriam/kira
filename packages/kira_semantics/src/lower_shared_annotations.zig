const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");
const parent = @import("lower_shared.zig");

const Context = parent.Context;
const AnnotationHeader = parent.AnnotationHeader;
const AnnotationPlacement = parent.AnnotationPlacement;
const qualifiedNameLeaf = parent.qualifiedNameLeaf;
const qualifiedNameText = parent.qualifiedNameText;
const resolveAnnotationHeader = parent.resolveAnnotationHeader;
const annotationValueForParameter = parent.annotationValueForParameter;
const typeTextFromResolved = parent.typeTextFromResolved;
const containsAnnotationRule = parent.containsAnnotationRule;

pub fn lowerAnnotation(ctx: *Context, annotation: syntax.ast.Annotation) !model.Annotation {
    const header = try resolveAnnotationHeader(ctx, annotation.name);
    const leaf = try qualifiedNameLeaf(ctx.allocator, annotation.name);
    const arguments = try lowerAnnotationArguments(ctx, annotation, header);
    return .{
        .name = leaf,
        .is_namespaced = annotation.name.segments.len > 1,
        .symbol_index = header.index,
        .arguments = arguments,
        .span = annotation.span,
    };
}

pub fn validateAnnotationUse(ctx: *Context, annotation: syntax.ast.Annotation) !void {
    _ = try lowerAnnotation(ctx, annotation);
}

fn lowerAnnotationArguments(ctx: *Context, annotation: syntax.ast.Annotation, header: AnnotationHeader) ![]model.AnnotationArgument {
    const params = header.decl.parameters;
    const display_name = try annotationDisplayName(ctx.allocator, annotation.name);

    if (params.len == 0) {
        if (annotation.args.len != 0 or (annotation.block != null and !header.allows_block)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM064",
                .title = "annotation does not accept parameters",
                .message = try std.fmt.allocPrint(ctx.allocator, "{s} does not accept parameters.", .{display_name}),
                .labels = &.{diagnostics.primaryLabel(annotation.span, "unexpected annotation parameters")},
                .help = "Remove the annotation arguments.",
            });
            return error.DiagnosticsEmitted;
        }
        return &.{};
    }

    if (annotation.block != null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM065",
            .title = "annotation parameters require parentheses",
            .message = try std.fmt.allocPrint(ctx.allocator, "{s} declares parameters and must be applied with parentheses.", .{display_name}),
            .labels = &.{diagnostics.primaryLabel(annotation.span, "annotation block cannot fill declared parameters")},
            .help = "Write annotation parameters as `@Name(value)` or `@Name(parameter: value)`.",
        });
        return error.DiagnosticsEmitted;
    }

    var filled = try ctx.allocator.alloc(bool, params.len);
    @memset(filled, false);
    var values = try ctx.allocator.alloc(model.AnnotationArgument, params.len);
    var next_positional: usize = 0;

    for (annotation.args) |arg| {
        const param_index = if (arg.label) |label|
            findAnnotationParameter(params, label) orelse {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM066",
                    .title = "unknown annotation parameter",
                    .message = try std.fmt.allocPrint(ctx.allocator, "{s} does not declare a parameter named '{s}'.", .{ display_name, label }),
                    .labels = &.{diagnostics.primaryLabel(arg.span, "unknown annotation parameter")},
                    .help = "Use one of the parameters declared by the annotation.",
                });
                return error.DiagnosticsEmitted;
            }
        else blk: {
            while (next_positional < params.len and filled[next_positional]) next_positional += 1;
            if (next_positional >= params.len) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM067",
                    .title = "too many annotation parameters",
                    .message = try std.fmt.allocPrint(ctx.allocator, "{s} received more parameters than it declares.", .{display_name}),
                    .labels = &.{diagnostics.primaryLabel(arg.span, "extra annotation parameter")},
                    .help = "Remove the extra argument or declare another annotation parameter.",
                });
                return error.DiagnosticsEmitted;
            }
            const index = next_positional;
            next_positional += 1;
            break :blk index;
        };

        if (filled[param_index]) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM068",
                .title = "duplicate annotation parameter",
                .message = try std.fmt.allocPrint(ctx.allocator, "{s} receives parameter '{s}' more than once.", .{ display_name, params[param_index].name }),
                .labels = &.{diagnostics.primaryLabel(arg.span, "duplicate annotation parameter")},
                .help = "Pass each annotation parameter once.",
            });
            return error.DiagnosticsEmitted;
        }

        const checked = try annotationValueForParameter(ctx, display_name, params[param_index].name, params[param_index].ty, arg.value, false);
        values[param_index] = .{
            .name = params[param_index].name,
            .value = checked.value,
            .ty = params[param_index].ty,
            .span = arg.span,
        };
        filled[param_index] = true;
    }

    for (params, 0..) |param, index| {
        if (filled[index]) continue;
        if (param.default_value) |default_value| {
            values[index] = .{
                .name = param.name,
                .value = default_value,
                .ty = param.ty,
                .span = param.span,
            };
            filled[index] = true;
            continue;
        }

        const type_text = try typeTextFromResolved(ctx.allocator, param.ty);
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM069",
            .title = "missing annotation parameter",
            .message = try std.fmt.allocPrint(ctx.allocator, "{s} requires parameter '{s}: {s}'.", .{ display_name, param.name, type_text }),
            .labels = &.{diagnostics.primaryLabel(annotation.span, "required annotation parameter is missing")},
            .help = "Pass the required parameter or add a default value to the annotation declaration.",
        });
        return error.DiagnosticsEmitted;
    }

    return values;
}

fn findAnnotationParameter(params: []const model.AnnotationParameterDecl, name: []const u8) ?usize {
    for (params, 0..) |param, index| {
        if (std.mem.eql(u8, param.name, name)) return index;
    }
    return null;
}

fn annotationDisplayName(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName) ![]const u8 {
    return std.fmt.allocPrint(allocator, "@{s}", .{try qualifiedNameText(allocator, name)});
}

pub fn resolveFunctionAnnotations(ctx: *Context, annotations: []const syntax.ast.Annotation) !struct { annotations: []model.Annotation, is_main: bool, execution: runtime_abi.FunctionExecution } {
    var lowered = std.array_list.Managed(model.Annotation).init(ctx.allocator);
    var is_main = false;
    var main_span: ?source_pkg.Span = null;
    var execution: runtime_abi.FunctionExecution = .inherited;
    var execution_span: ?source_pkg.Span = null;

    for (annotations) |annotation| {
        const lowered_annotation = try lowerAnnotation(ctx, annotation);
        const name = lowered_annotation.name;
        try lowered.append(lowered_annotation);

        if (std.mem.eql(u8, name, "Main")) {
            if (is_main) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM004",
                    .title = "duplicate @Main annotation",
                    .message = "The same function cannot declare @Main more than once.",
                    .labels = &.{
                        diagnostics.primaryLabel(annotation.span, "duplicate @Main annotation"),
                        diagnostics.secondaryLabel(main_span.?, "the first @Main annotation was here"),
                    },
                    .help = "Remove the extra @Main annotation.",
                });
                return error.DiagnosticsEmitted;
            }
            is_main = true;
            main_span = annotation.span;
            continue;
        }

        if (std.mem.eql(u8, name, "Runtime") or std.mem.eql(u8, name, "Native")) {
            const next_execution: runtime_abi.FunctionExecution = if (std.mem.eql(u8, name, "Runtime")) .runtime else .native;
            if (execution != .inherited) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM005",
                    .title = "conflicting execution annotations",
                    .message = "A function can use at most one execution annotation.",
                    .labels = &.{
                        diagnostics.primaryLabel(annotation.span, "conflicting execution annotation"),
                        diagnostics.secondaryLabel(execution_span.?, "the first execution annotation was here"),
                    },
                    .help = "Choose either @Runtime or @Native for this function.",
                });
                return error.DiagnosticsEmitted;
            }
            execution = next_execution;
            execution_span = annotation.span;
        }
    }

    return .{
        .annotations = try lowered.toOwnedSlice(),
        .is_main = is_main,
        .execution = execution,
    };
}

pub fn resolveTypeExecutionAnnotations(
    ctx: *Context,
    annotations: []const syntax.ast.Annotation,
    kind: model.TypeKind,
) !runtime_abi.FunctionExecution {
    var execution: runtime_abi.FunctionExecution = .inherited;
    var execution_span: ?source_pkg.Span = null;

    for (annotations) |annotation| {
        const name = try qualifiedNameLeaf(ctx.allocator, annotation.name);
        if (!isTypeExecutionAnnotation(name)) continue;
        const next_execution: runtime_abi.FunctionExecution = if (std.mem.eql(u8, name, "Runtime")) .runtime else .native;
        if (execution != .inherited) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM005",
                .title = "conflicting execution annotations",
                .message = switch (kind) {
                    .class => "A type can use at most one execution annotation.",
                    .struct_decl => "A struct can use at most one execution annotation.",
                },
                .labels = &.{
                    diagnostics.primaryLabel(annotation.span, "conflicting execution annotation"),
                    diagnostics.secondaryLabel(execution_span.?, "the first execution annotation was here"),
                },
                .help = switch (kind) {
                    .class => "Choose either @Runtime or @Native for this type.",
                    .struct_decl => "Choose either @Runtime or @Native for this struct.",
                },
            });
            return error.DiagnosticsEmitted;
        }
        execution = next_execution;
        execution_span = annotation.span;
    }

    return execution;
}

pub fn lowerAnnotations(ctx: *Context, annotations: []const syntax.ast.Annotation) ![]model.Annotation {
    var lowered = std.array_list.Managed(model.Annotation).init(ctx.allocator);
    for (annotations) |annotation| {
        try lowered.append(try lowerAnnotation(ctx, annotation));
    }
    return lowered.toOwnedSlice();
}

pub fn validateAnnotationPlacement(
    ctx: *Context,
    annotations: []const syntax.ast.Annotation,
    placement: AnnotationPlacement,
    construct_model: ?model.Construct,
) !void {
    for (annotations) |annotation| {
        const header = try resolveAnnotationHeader(ctx, annotation.name);
        _ = try lowerAnnotationArguments(ctx, annotation, header);
        const name = try qualifiedNameLeaf(ctx.allocator, annotation.name);
        const is_main = std.mem.eql(u8, name, "Main");
        const is_type_execution = isTypeExecutionAnnotation(name);
        const is_execution = is_main or is_type_execution;
        const allows_execution = placement == .function_decl or ((placement == .class_decl or placement == .struct_decl) and is_type_execution);
        if (is_execution and !allows_execution) {
            const message = if (is_main)
                try std.fmt.allocPrint(ctx.allocator, "The annotation '@{s}' is only valid on functions.", .{name})
            else
                try std.fmt.allocPrint(ctx.allocator, "The annotation '@{s}' is only valid on functions and type declarations.", .{name});
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM025",
                .title = "illegal annotation placement",
                .message = message,
                .labels = &.{
                    diagnostics.primaryLabel(annotation.span, "annotation cannot be applied here"),
                },
                .help = if (is_main)
                    "Move the annotation onto a function declaration or remove it."
                else
                    "Move the annotation onto a function, class, or struct declaration, or remove it.",
            });
            return error.DiagnosticsEmitted;
        }
        if (placement == .struct_decl and !structAnnotationAllowed(header, name)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM096",
                .title = "invalid struct annotation",
                .message = try std.fmt.allocPrint(
                    ctx.allocator,
                    "The struct annotation '@{s}' is not allowed here.",
                    .{name},
                ),
                .labels = &.{diagnostics.primaryLabel(annotation.span, "struct annotations are limited to execution-boundary and compiler-reserved FFI annotations")},
                .help = "Use @Runtime or @Native for ordinary struct execution, or keep compiler-reserved FFI annotations only where the FFI type system requires them.",
            });
            return error.DiagnosticsEmitted;
        }
        if (header.decl.targets.len != 0 and !annotationTargetsIncludePlacement(header.decl.targets, placement)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM071",
                .title = "invalid annotation target",
                .message = try std.fmt.allocPrint(ctx.allocator, "The annotation '@{s}' is not valid on this declaration kind.", .{name}),
                .labels = &.{diagnostics.primaryLabel(annotation.span, "annotation target does not match this declaration")},
                .help = "Move the annotation to a supported target or update its `targets:` declaration.",
            });
            return error.DiagnosticsEmitted;
        }
        if ((placement == .field_decl or placement == .content_section) and !header.compiler_builtin) {
            // Compiler-builtin annotations (`@Content`, `@Required`, FFI markers) are part of the
            // language surface, not a construct's user-declared `annotations { ... }` allow-list.
            if (construct_model) |construct_info| {
                if (!containsAnnotationRule(construct_info.allowed_annotations, name)) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM026",
                        .title = "annotation is not allowed in this construct",
                        .message = try std.fmt.allocPrint(ctx.allocator, "The construct '{s}' does not declare the annotation '@{s}'.", .{ construct_info.name, name }),
                        .labels = &.{
                            diagnostics.primaryLabel(annotation.span, "annotation is not declared by this construct"),
                        },
                        .help = "Declare the annotation in the construct's `annotations { ... }` section or remove it.",
                    });
                    return error.DiagnosticsEmitted;
                }
            }
        }
    }
}

fn isTypeExecutionAnnotation(name: []const u8) bool {
    return std.mem.eql(u8, name, "Native") or std.mem.eql(u8, name, "Runtime");
}

fn structAnnotationAllowed(header: AnnotationHeader, name: []const u8) bool {
    if (isTypeExecutionAnnotation(name)) return true;
    if (std.mem.eql(u8, name, "Printable")) return true;
    return header.compiler_builtin and std.mem.startsWith(u8, header.decl.name, "FFI.");
}

fn annotationTargetsIncludePlacement(targets: []const model.AnnotationTarget, placement: AnnotationPlacement) bool {
    const actual: ?model.AnnotationTarget = switch (placement) {
        .function_decl => .function,
        .class_decl => .class,
        .struct_decl => .struct_decl,
        .construct_decl => .construct,
        .field_decl, .content_section => .field,
        .construct_form_decl => null,
    };
    const required = actual orelse return false;
    for (targets) |target| {
        if (target == required) return true;
    }
    return false;
}
