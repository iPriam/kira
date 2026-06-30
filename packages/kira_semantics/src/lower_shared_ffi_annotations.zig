const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");
const shared = @import("lower_shared.zig");
const Context = shared.Context;
const canAssign = shared.canAssign;
const typeLabel = shared.typeLabel;
const resolvedTypeFromText = shared.resolvedTypeFromText;

pub fn resolveForeignFunction(ctx: *Context, annotations: []const syntax.ast.Annotation, span: source_pkg.Span) !?model.ForeignFunction {
    const annotation = findAnnotation(annotations, "FFI", "Extern") orelse return null;
    var library_name: ?[]const u8 = null;
    var symbol_name: ?[]const u8 = null;
    var calling_convention: runtime_abi.CallingConvention = .c;

    if (annotation.block) |block| {
        for (block.entries) |entry| {
            if (entry != .field) continue;
            if (std.mem.eql(u8, entry.field.name, "library")) library_name = try annotationValueText(ctx, entry.field.value);
            if (std.mem.eql(u8, entry.field.name, "symbol")) symbol_name = try annotationValueText(ctx, entry.field.value);
            if (std.mem.eql(u8, entry.field.name, "abi")) calling_convention = try annotationCallingConvention(ctx, entry.field.value);
        }
    }

    if (library_name == null or symbol_name == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM036",
            .title = "invalid FFI extern annotation",
            .message = "An @FFI.Extern annotation must declare both `library` and `symbol`.",
            .labels = &.{
                diagnostics.primaryLabel(span, "FFI extern declaration is missing required metadata"),
            },
            .help = "Write `@FFI.Extern { library: native_lib, symbol: native_symbol, abi: c }`.",
        });
        return error.DiagnosticsEmitted;
    }

    return .{
        .library_name = library_name.?,
        .symbol_name = symbol_name.?,
        .calling_convention = calling_convention,
        .span = annotation.span,
    };
}

pub fn resolveNamedTypeInfo(ctx: *Context, annotations: []const syntax.ast.Annotation, span: source_pkg.Span) !?model.NamedTypeInfo {
    var result: ?model.NamedTypeInfo = null;

    if (findAnnotation(annotations, "FFI", "Struct")) |annotation| {
        const layout = if (annotation.block) |block|
            annotationBlockText(ctx, block, "layout") catch |err| switch (err) {
                error.MissingField => "c",
                else => return err,
            }
        else
            "c";
        result = .{ .ffi_struct = .{
            .layout = layout,
            .span = annotation.span,
        } };
    }

    if (findAnnotation(annotations, "FFI", "Pointer")) |annotation| {
        if (result != null) {
            try emitConflictingFfiTypeAnnotation(ctx, span);
            return error.DiagnosticsEmitted;
        }
        const block = annotation.block orelse {
            try emitMissingFfiBlock(ctx, annotation.span, "@FFI.Pointer");
            return error.DiagnosticsEmitted;
        };
        const target_name = try annotationBlockText(ctx, block, "target");
        const ownership_text = annotationBlockText(ctx, block, "ownership") catch |err| switch (err) {
            error.MissingField => "borrowed",
            else => return err,
        };
        result = .{ .pointer = .{
            .target_name = target_name,
            .ownership = try parseOwnership(ctx, ownership_text, annotation.span),
            .span = annotation.span,
        } };
    }

    if (findAnnotation(annotations, "FFI", "Alias")) |annotation| {
        if (result != null) {
            try emitConflictingFfiTypeAnnotation(ctx, span);
            return error.DiagnosticsEmitted;
        }
        const block = annotation.block orelse {
            try emitMissingFfiBlock(ctx, annotation.span, "@FFI.Alias");
            return error.DiagnosticsEmitted;
        };
        result = .{ .alias = .{
            .target = try annotationBlockType(ctx, block, "target"),
            .span = annotation.span,
        } };
    }

    if (findAnnotation(annotations, "FFI", "Array")) |annotation| {
        if (result != null) {
            try emitConflictingFfiTypeAnnotation(ctx, span);
            return error.DiagnosticsEmitted;
        }
        const block = annotation.block orelse {
            try emitMissingFfiBlock(ctx, annotation.span, "@FFI.Array");
            return error.DiagnosticsEmitted;
        };
        result = .{ .array = .{
            .element = try annotationBlockType(ctx, block, "element"),
            .count = try annotationBlockCount(ctx, block, "count"),
            .span = annotation.span,
        } };
    }

    if (findAnnotation(annotations, "FFI", "Callback")) |annotation| {
        if (result != null) {
            try emitConflictingFfiTypeAnnotation(ctx, span);
            return error.DiagnosticsEmitted;
        }
        const block = annotation.block orelse {
            try emitMissingFfiBlock(ctx, annotation.span, "@FFI.Callback");
            return error.DiagnosticsEmitted;
        };
        result = .{ .callback = .{
            .calling_convention = annotationBlockCallingConvention(ctx, block, "abi") catch |err| switch (err) {
                error.MissingField => .c,
                else => return err,
            },
            .params = try annotationBlockTypeArray(ctx, block, "params"),
            .result = annotationBlockType(ctx, block, "result") catch |err| switch (err) {
                error.MissingField => .{ .kind = .void },
                else => return err,
            },
            .span = annotation.span,
        } };
    }

    return result;
}

fn emitConflictingFfiTypeAnnotation(ctx: *Context, span: source_pkg.Span) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM037",
        .title = "conflicting FFI type annotations",
        .message = "A type declaration can describe exactly one FFI kind.",
        .labels = &.{
            diagnostics.primaryLabel(span, "type mixes incompatible FFI annotations"),
        },
        .help = "Choose one FFI type annotation such as @FFI.Struct, @FFI.Pointer, @FFI.Alias, @FFI.Array, or @FFI.Callback.",
    });
}

fn emitMissingFfiBlock(ctx: *Context, span: source_pkg.Span, name: []const u8) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM038",
        .title = "missing FFI annotation block",
        .message = try std.fmt.allocPrint(ctx.allocator, "{s} requires a block with explicit fields.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(span, "FFI annotation metadata is missing"),
        },
        .help = "Add a block such as `{ target: native_type, ownership: borrowed }`.",
    });
}

fn findAnnotation(annotations: []const syntax.ast.Annotation, namespace: []const u8, leaf: []const u8) ?syntax.ast.Annotation {
    for (annotations) |annotation| {
        if (qualifiedNameMatches(annotation.name, namespace, leaf)) return annotation;
    }
    return null;
}

fn qualifiedNameMatches(name: syntax.ast.QualifiedName, namespace: []const u8, leaf: []const u8) bool {
    if (name.segments.len != 2) return false;
    return std.mem.eql(u8, name.segments[0].text, namespace) and std.mem.eql(u8, name.segments[1].text, leaf);
}

fn annotationBlockText(ctx: *Context, block: syntax.ast.AnnotationBlock, field_name: []const u8) ![]const u8 {
    for (block.entries) |entry| {
        if (entry != .field) continue;
        if (!std.mem.eql(u8, entry.field.name, field_name)) continue;
        return annotationValueText(ctx, entry.field.value);
    }
    return error.MissingField;
}

fn annotationBlockType(ctx: *Context, block: syntax.ast.AnnotationBlock, field_name: []const u8) !model.ResolvedType {
    const text = try annotationBlockText(ctx, block, field_name);
    return resolvedTypeFromText(text);
}

fn annotationBlockCount(ctx: *Context, block: syntax.ast.AnnotationBlock, field_name: []const u8) !usize {
    for (block.entries) |entry| {
        if (entry != .field) continue;
        if (!std.mem.eql(u8, entry.field.name, field_name)) continue;
        return annotationCountValue(ctx, entry.field.value);
    }
    return error.MissingField;
}

fn annotationBlockTypeArray(ctx: *Context, block: syntax.ast.AnnotationBlock, field_name: []const u8) ![]const model.ResolvedType {
    for (block.entries) |entry| {
        if (entry != .field) continue;
        if (!std.mem.eql(u8, entry.field.name, field_name)) continue;
        return annotationTypeArray(ctx, entry.field.value);
    }
    return error.MissingField;
}

fn annotationBlockCallingConvention(ctx: *Context, block: syntax.ast.AnnotationBlock, field_name: []const u8) !runtime_abi.CallingConvention {
    const value = try annotationBlockText(ctx, block, field_name);
    return parseCallingConvention(ctx, value, block.span);
}

fn annotationCallingConvention(ctx: *Context, value: *syntax.ast.Expr) !runtime_abi.CallingConvention {
    return parseCallingConvention(ctx, try annotationValueText(ctx, value), exprSpan(value.*));
}

fn parseCallingConvention(ctx: *Context, value: []const u8, span: source_pkg.Span) !runtime_abi.CallingConvention {
    if (std.mem.eql(u8, value, "c")) return .c;
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM039",
        .title = "unsupported FFI calling convention",
        .message = try std.fmt.allocPrint(ctx.allocator, "The calling convention '{s}' is not supported by this FFI pass.", .{value}),
        .labels = &.{
            diagnostics.primaryLabel(span, "unsupported calling convention"),
        },
        .help = "Use `abi: c` for the first-version FFI system.",
    });
    return error.DiagnosticsEmitted;
}

fn parseOwnership(ctx: *Context, value: []const u8, span: source_pkg.Span) !model.Ownership {
    if (std.mem.eql(u8, value, "borrowed")) return .borrowed;
    if (std.mem.eql(u8, value, "owned")) return .owned;
    if (std.mem.eql(u8, value, "opaque")) return .@"opaque";
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM040",
        .title = "unsupported FFI ownership mode",
        .message = try std.fmt.allocPrint(ctx.allocator, "The ownership mode '{s}' is not supported here.", .{value}),
        .labels = &.{
            diagnostics.primaryLabel(span, "unsupported ownership mode"),
        },
        .help = "Use `borrowed`, `owned`, or `opaque`.",
    });
    return error.DiagnosticsEmitted;
}

fn annotationTypeArray(ctx: *Context, expr: *syntax.ast.Expr) ![]const model.ResolvedType {
    if (expr.* != .array) return error.InvalidAnnotationValue;
    var list = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
    for (expr.array.elements) |element| {
        try list.append(try resolvedTypeFromText(try annotationValueText(ctx, element)));
    }
    return list.toOwnedSlice();
}

fn annotationValueText(ctx: *Context, expr: *syntax.ast.Expr) ![]const u8 {
    _ = ctx;
    return switch (expr.*) {
        .string => |value| value.value,
        .identifier => |value| value.name.segments[value.name.segments.len - 1].text,
        .member => |value| value.member,
        else => error.InvalidAnnotationValue,
    };
}

fn annotationCountValue(ctx: *Context, expr: *syntax.ast.Expr) !usize {
    _ = ctx;
    return switch (expr.*) {
        .integer => |value| std.math.cast(usize, value.value) orelse return error.InvalidAnnotationValue,
        else => error.InvalidAnnotationValue,
    };
}

pub const CheckedAnnotationValue = struct {
    value: model.AnnotationValue,
    ty: model.ResolvedType,
};

pub fn annotationValueForParameter(
    ctx: *Context,
    annotation_name: []const u8,
    parameter_name: []const u8,
    expected_type: model.ResolvedType,
    expr: *syntax.ast.Expr,
    is_default: bool,
) !CheckedAnnotationValue {
    const literal = annotationLiteralValue(ctx, expr) catch |err| switch (err) {
        error.InvalidAnnotationValue => {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM070",
                .title = if (is_default) "invalid annotation parameter default" else "invalid annotation parameter value",
                .message = "Annotation parameters currently support Bool, Int, Float, and String literal values.",
                .labels = &.{diagnostics.primaryLabel(exprSpan(expr.*), "unsupported annotation parameter value")},
                .help = "Use a literal value such as `true`, `0`, `1.5`, or \"text\".",
            });
            return error.DiagnosticsEmitted;
        },
    };

    if (!canAssign(expected_type, literal.ty)) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM071",
            .title = "annotation parameter type mismatch",
            .message = try std.fmt.allocPrint(
                ctx.allocator,
                "parameter '{s}' for {s} expects {s}, got {s}.",
                .{ parameter_name, annotation_name, typeLabel(expected_type), typeLabel(literal.ty) },
            ),
            .labels = &.{diagnostics.primaryLabel(exprSpan(expr.*), "annotation argument has the wrong type")},
            .help = "Change the argument value or update the annotation parameter type.",
        });
        return error.DiagnosticsEmitted;
    }

    if (expected_type.kind == .float and literal.ty.kind == .integer) {
        return .{
            .value = .{ .float = @floatFromInt(literal.value.integer) },
            .ty = expected_type,
        };
    }

    return .{
        .value = literal.value,
        .ty = expected_type,
    };
}

pub fn annotationLiteralValue(ctx: *Context, expr: *syntax.ast.Expr) !CheckedAnnotationValue {
    return switch (expr.*) {
        .integer => |value| .{
            .value = .{ .integer = value.value },
            .ty = .{ .kind = .integer },
        },
        .float => |value| .{
            .value = .{ .float = value.value },
            .ty = .{ .kind = .float },
        },
        .string => |value| .{
            .value = .{ .string = value.value },
            .ty = .{ .kind = .string },
        },
        .bool => |value| .{
            .value = .{ .boolean = value.value },
            .ty = .{ .kind = .boolean },
        },
        .unary => |node| blk: {
            if (node.op != .negate) return error.InvalidAnnotationValue;
            const operand = try annotationLiteralValue(ctx, node.operand);
            break :blk switch (operand.value) {
                .integer => |value| .{
                    .value = .{ .integer = -value },
                    .ty = operand.ty,
                },
                .float => |value| .{
                    .value = .{ .float = -value },
                    .ty = operand.ty,
                },
                else => error.InvalidAnnotationValue,
            };
        },
        else => error.InvalidAnnotationValue,
    };
}

fn exprSpan(expr: syntax.ast.Expr) source_pkg.Span {
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
        .quote => |node| node.span,
        .conditional => |node| node.span,
        .member => |node| node.span,
        .index => |node| node.span,
        .call => |node| node.span,
        .try_expr => |node| node.span,
    };
}
