const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const exprs = @import("lower_exprs.zig");
const ImportedGlobals = @import("imported_globals.zig").ImportedGlobals;
const parent = @import("lower_program.zig");
const ResolvedFieldOverride = parent.ResolvedFieldOverride;
const ResolvedMethodMember = parent.ResolvedMethodMember;
const TypeSource = parent.TypeSource;
const LocalTypeMap = parent.LocalTypeMap;
const ResolverState = parent.ResolverState;
const lowerFieldDefaultExprExpected = parent.lowerFieldDefaultExprExpected;
const lowerField = parent.lowerField;
const lowerFunction = parent.lowerFunction;
pub fn lowerImports(ctx: *shared.Context, program: syntax.ast.Program) ![]model.Import {
    const lowered = try ctx.allocator.alloc(model.Import, program.imports.len);
    for (program.imports, 0..) |import_decl, index| {
        const origin = if (index < program.import_origins.len) program.import_origins[index] else syntax.ast.DeclOrigin{};
        lowered[index] = .{
            .module_name = try shared.qualifiedNameText(ctx.allocator, import_decl.module_name),
            .alias = if (import_decl.alias) |alias| try ctx.allocator.dupe(u8, alias) else null,
            .package_name = if (origin.package_name) |package_name| try ctx.allocator.dupe(u8, package_name) else null,
            .span = import_decl.span,
        };
    }
    return lowered;
}

pub fn composeAnnotationGeneratedFunctions(
    ctx: *shared.Context,
    annotation_decl: model.AnnotationDecl,
    capabilities: []const model.CapabilityDecl,
    capability_headers: *const std.StringHashMapUnmanaged(usize),
) ![]model.GeneratedFunction {
    var generated = std.array_list.Managed(model.GeneratedFunction).init(ctx.allocator);
    var names = std.StringHashMapUnmanaged(source_pkg.Span){};
    defer names.deinit(ctx.allocator);

    for (annotation_decl.uses) |capability_name| {
        const capability_index = capability_headers.get(capability_name) orelse {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM075",
                .title = "unknown capability",
                .message = try std.fmt.allocPrint(ctx.allocator, "The annotation '{s}' uses unknown capability '{s}'.", .{ annotation_decl.name, capability_name }),
                .labels = &.{diagnostics.primaryLabel(annotation_decl.span, "capability use cannot be resolved")},
                .help = "Declare the capability before composing it into an annotation.",
            });
            return error.DiagnosticsEmitted;
        };
        for (capabilities[capability_index].generated_functions) |function_decl| {
            try appendGeneratedFunctionUnique(ctx, &generated, &names, function_decl);
        }
    }
    for (annotation_decl.generated_functions) |function_decl| {
        try appendGeneratedFunctionUnique(ctx, &generated, &names, function_decl);
    }
    return generated.toOwnedSlice();
}

pub fn appendGeneratedFunctionUnique(
    ctx: *shared.Context,
    generated: *std.array_list.Managed(model.GeneratedFunction),
    names: *std.StringHashMapUnmanaged(source_pkg.Span),
    function_decl: model.GeneratedFunction,
) !void {
    if (names.get(function_decl.name)) |previous_span| {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM076",
            .title = "duplicate generated member",
            .message = try std.fmt.allocPrint(ctx.allocator, "More than one composed annotation capability generates function '{s}'.", .{function_decl.name}),
            .labels = &.{
                diagnostics.primaryLabel(function_decl.span, "duplicate generated function"),
                diagnostics.secondaryLabel(previous_span, "first generated function was here"),
            },
            .help = "Remove one capability use or rename one generated function so composition is explicit.",
        });
        return error.DiagnosticsEmitted;
    }
    try names.put(ctx.allocator, function_decl.name, function_decl.span);
    try generated.append(function_decl);
}

pub fn registerImportAliases(ctx: *shared.Context, imports: []const model.Import, map: *std.StringHashMapUnmanaged(source_pkg.Span)) !void {
    for (imports) |import_decl| {
        if (import_decl.package_name != null) continue;
        const visible = import_decl.alias orelse import_decl.module_name;
        try shared.registerTopLevelName(ctx.allocator, ctx.diagnostics, map, visible, import_decl.span);
    }
}

pub fn lowerConstructDecl(ctx: *shared.Context, construct_decl: syntax.ast.ConstructDecl) !model.Construct {
    try shared.validateAnnotationPlacement(ctx, construct_decl.annotations, .construct_decl, null);
    var allowed_annotations = std.array_list.Managed(model.AnnotationRule).init(ctx.allocator);
    var allowed_lifecycle_hooks = std.array_list.Managed([]const u8).init(ctx.allocator);
    var required_content = false;
    var content_element_type: ?[]const u8 = null;

    for (construct_decl.sections) |section| {
        // A typed content section `content: Content<T>;` parses as a custom section
        // named "content" whose single entry is a named_rule carrying the type expr.
        // It both requires a content block and pins the element type used to validate
        // construct-backed declarations.
        if (std.mem.eql(u8, section.name, "content")) {
            for (section.entries) |entry| {
                if (entry == .named_rule) {
                    required_content = true;
                    if (entry.named_rule.type_expr) |type_expr| {
                        content_element_type = contentElementTypeName(type_expr.*);
                    }
                }
            }
        }
        switch (section.kind) {
            .annotations => {
                for (section.entries) |entry| {
                    if (entry == .annotation_spec) {
                        _ = try shared.resolveAnnotationHeader(ctx, entry.annotation_spec.name);
                        try allowed_annotations.append(.{
                            .name = try shared.qualifiedNameLeaf(ctx.allocator, entry.annotation_spec.name),
                            .span = entry.annotation_spec.span,
                        });
                    }
                }
            },
            .requires => {
                for (section.entries) |entry| {
                    if (entry == .named_rule) {
                        const rule_name = entry.named_rule.name.segments[0].text;
                        if (std.mem.eql(u8, rule_name, "content")) required_content = true;
                    }
                }
            },
            .lifecycle => {
                for (section.entries) |entry| {
                    if (entry == .lifecycle_hook) {
                        try allowed_lifecycle_hooks.append(try ctx.allocator.dupe(u8, entry.lifecycle_hook.name));
                    }
                }
            },
            else => {},
        }
    }

    return .{
        .name = try ctx.allocator.dupe(u8, construct_decl.name),
        .allowed_annotations = try allowed_annotations.toOwnedSlice(),
        .required_content = required_content,
        .content_element_type = if (content_element_type) |name| try ctx.allocator.dupe(u8, name) else null,
        .allowed_lifecycle_hooks = try allowed_lifecycle_hooks.toOwnedSlice(),
        .span = construct_decl.span,
    };
}

// Extract the element type leaf from a typed content section. `content: Content<Widget>`
// yields "Widget"; a bare `content: Widget` yields "Widget". Returns null for shapes that
// do not name a single element type (so no element validation is imposed).
fn contentElementTypeName(type_expr: syntax.ast.TypeExpr) ?[]const u8 {
    return switch (type_expr) {
        .generic => |generic| if (generic.args.len == 1)
            leafTypeName(generic.args[0].*)
        else
            null,
        .named => |named| named.segments[named.segments.len - 1].text,
        else => null,
    };
}

fn leafTypeName(type_expr: syntax.ast.TypeExpr) ?[]const u8 {
    return switch (type_expr) {
        .named => |named| named.segments[named.segments.len - 1].text,
        .generic => |generic| generic.base.segments[generic.base.segments.len - 1].text,
        else => null,
    };
}

pub fn registerImportedFunctionHeaders(
    ctx: *shared.Context,
    function_headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    for (ctx.imported_globals.functions) |function_decl| {
        if (!function_decl.is_extern) continue;
        try function_headers.put(ctx.allocator, function_decl.name, .{
            .id = @as(u32, @intCast(function_headers.count())),
            .params = function_decl.params,
            .param_ownership = function_decl.param_ownership,
            .execution = if (function_decl.execution == .inherited) .native else function_decl.execution,
            .return_type = function_decl.return_type,
            .return_ownership = function_decl.return_ownership,
            .is_extern = true,
            .foreign = function_decl.foreign,
            .span = .{ .start = 0, .end = 0 },
        });
    }
}

pub fn resolveTypeHeader(
    ctx: *shared.Context,
    local_types: *const LocalTypeMap,
    resolver_states: *std.StringHashMapUnmanaged(ResolverState),
    type_headers: *std.StringHashMapUnmanaged(shared.TypeHeader),
    source: TypeSource,
    type_name: []const u8,
) anyerror!shared.TypeHeader {
    if (type_headers.get(type_name)) |header| return header;
    if (resolver_states.get(type_name)) |state| {
        if (state == .resolving) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM050",
                .title = "inheritance cycle",
                .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' participates in an inheritance cycle.", .{type_name}),
                .labels = &.{diagnostics.primaryLabel(typeSourceSpan(source), "cycle reaches this type again")},
                .help = "Remove the cycle so each inheritance chain terminates at a concrete base type.",
            });
            return error.DiagnosticsEmitted;
        }
    }
    try resolver_states.put(ctx.allocator, type_name, .resolving);

    const header = switch (source) {
        .local => |type_decl| try resolveLocalTypeHeader(ctx, local_types, resolver_states, type_headers, type_decl),
        .imported => |type_decl| try resolveImportedTypeHeader(ctx, local_types, resolver_states, type_headers, type_decl),
    };

    try type_headers.put(ctx.allocator, type_name, header);
    try resolver_states.put(ctx.allocator, type_name, .resolved);
    return header;
}

pub fn resolveLocalTypeHeader(
    ctx: *shared.Context,
    local_types: *const LocalTypeMap,
    resolver_states: *std.StringHashMapUnmanaged(ResolverState),
    type_headers: *std.StringHashMapUnmanaged(shared.TypeHeader),
    type_decl: syntax.ast.TypeDecl,
) anyerror!shared.TypeHeader {
    try shared.validateAnnotationPlacement(ctx, type_decl.annotations, if (type_decl.kind == .class) .class_decl else .struct_decl, null);
    const execution = try @import("lower_shared_annotations.zig").resolveTypeExecutionAnnotations(
        ctx,
        type_decl.annotations,
        if (type_decl.kind == .class) .class else .struct_decl,
    );
    if (type_decl.kind == .struct_decl and type_decl.parents.len != 0) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM073",
            .title = "struct cannot inherit",
            .message = try std.fmt.allocPrint(ctx.allocator, "The struct '{s}' cannot declare an `extends` clause.", .{type_decl.name}),
            .labels = &.{diagnostics.primaryLabel(type_decl.span, "struct declarations do not inherit")},
            .help = "Use `class` when inheritance is intended, or remove the `extends` clause.",
        });
        return error.DiagnosticsEmitted;
    }
    const ffi_type = try shared.resolveNamedTypeInfo(ctx, type_decl.annotations, type_decl.span);
    if (ffi_type != null and type_decl.parents.len != 0) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM053",
            .title = "invalid inheritance target",
            .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' cannot inherit because it is an FFI-defined type.", .{type_decl.name}),
            .labels = &.{diagnostics.primaryLabel(type_decl.span, "FFI types cannot participate in inheritance")},
            .help = "Remove the FFI annotation or inherit from a regular Kira type instead.",
        });
        return error.DiagnosticsEmitted;
    }

    var fields = std.array_list.Managed(model.Field).init(ctx.allocator);
    var methods = std.array_list.Managed(shared.MethodMember).init(ctx.allocator);
    var parent_views = std.array_list.Managed(shared.ParentView).init(ctx.allocator);

    try appendResolvedParents(ctx, local_types, resolver_states, type_headers, type_decl.name, type_decl.parents, &fields, &methods, &parent_views, type_decl.span);
    try appendGeneratedAnnotationMethods(ctx, type_decl, &methods);
    try applyLocalTypeMembers(ctx, type_decl, &fields, &methods);

    return .{
        .kind = if (type_decl.kind == .class) .class else .struct_decl,
        .execution = execution,
        .fields = try fields.toOwnedSlice(),
        .methods = try methods.toOwnedSlice(),
        .parent_views = try parent_views.toOwnedSlice(),
        .ffi = ffi_type,
        .is_printable = hasAnnotationNamed(type_decl.annotations, "Printable"),
        .span = type_decl.span,
    };
}

pub fn resolveImportedTypeHeader(
    ctx: *shared.Context,
    local_types: *const LocalTypeMap,
    resolver_states: *std.StringHashMapUnmanaged(ResolverState),
    type_headers: *std.StringHashMapUnmanaged(shared.TypeHeader),
    type_decl: @import("imported_globals.zig").ImportedType,
) anyerror!shared.TypeHeader {
    if (type_decl.ffi != null and type_decl.parents.len != 0) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM053",
            .title = "invalid inheritance target",
            .message = try std.fmt.allocPrint(ctx.allocator, "The imported type '{s}' cannot inherit because it is an FFI-defined type.", .{type_decl.name}),
            .labels = &.{diagnostics.primaryLabel(.{ .start = 0, .end = 0 }, "FFI types cannot participate in inheritance")},
            .help = "Remove the FFI annotation or inherit from a regular Kira type instead.",
        });
        return error.DiagnosticsEmitted;
    }

    var fields = std.array_list.Managed(model.Field).init(ctx.allocator);
    var methods = std.array_list.Managed(shared.MethodMember).init(ctx.allocator);
    var parent_views = std.array_list.Managed(shared.ParentView).init(ctx.allocator);

    try appendImportedParents(ctx, local_types, resolver_states, type_headers, type_decl.name, type_decl.parents, &fields, &methods, &parent_views);
    for (type_decl.fields) |field_decl| {
        try fields.append(.{
            .name = try ctx.allocator.dupe(u8, field_decl.name),
            .owner_type_name = try ctx.allocator.dupe(u8, type_decl.name),
            .storage = field_decl.storage,
            .slot_index = @as(u32, @intCast(fields.items.len)),
            .ty = field_decl.ty,
            .explicit_type = true,
            .default_value = null,
            .annotations = &.{},
            .span = .{ .start = 0, .end = 0 },
        });
    }
    try appendDeclaredImportedMethods(ctx, type_decl.name, &methods);

    return .{
        .kind = type_decl.kind,
        .execution = type_decl.execution,
        .fields = try fields.toOwnedSlice(),
        .methods = try methods.toOwnedSlice(),
        .parent_views = try parent_views.toOwnedSlice(),
        .ffi = type_decl.ffi,
        .is_printable = false,
        .span = .{ .start = 0, .end = 0 },
    };
}

pub fn typeSourceSpan(source: TypeSource) source_pkg.Span {
    return switch (source) {
        .local => |type_decl| type_decl.span,
        .imported => .{ .start = 0, .end = 0 },
    };
}

pub fn findTypeSource(ctx: *shared.Context, local_types: *const LocalTypeMap, type_name: []const u8) ?TypeSource {
    if (local_types.get(type_name)) |type_decl| return .{ .local = type_decl };
    if (ctx.imported_globals.findType(type_name)) |type_decl| return .{ .imported = type_decl };
    return null;
}

pub fn appendResolvedParents(
    ctx: *shared.Context,
    local_types: *const LocalTypeMap,
    resolver_states: *std.StringHashMapUnmanaged(ResolverState),
    type_headers: *std.StringHashMapUnmanaged(shared.TypeHeader),
    owner_type_name: []const u8,
    parents: []const syntax.ast.QualifiedName,
    fields: *std.array_list.Managed(model.Field),
    methods: *std.array_list.Managed(shared.MethodMember),
    parent_views: *std.array_list.Managed(shared.ParentView),
    owner_span: source_pkg.Span,
) anyerror!void {
    var seen = std.StringHashMapUnmanaged(source_pkg.Span){};
    defer seen.deinit(ctx.allocator);

    for (parents) |parent_name| {
        const parent_leaf = parent_name.segments[parent_name.segments.len - 1].text;
        if (std.mem.eql(u8, owner_type_name, parent_leaf)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM050",
                .title = "inheritance cycle",
                .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' cannot inherit from itself.", .{owner_type_name}),
                .labels = &.{diagnostics.primaryLabel(parent_name.span, "self-inheritance starts a cycle")},
                .help = "Remove the self-reference from the `extends` list.",
            });
            return error.DiagnosticsEmitted;
        }
        if (seen.get(parent_leaf)) |previous_span| {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM051",
                .title = "duplicate parent type",
                .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' lists '{s}' more than once in `extends`.", .{ owner_type_name, parent_leaf }),
                .labels = &.{
                    diagnostics.primaryLabel(parent_name.span, "duplicate parent appears here"),
                    diagnostics.secondaryLabel(previous_span, "the same parent was already listed here"),
                },
                .help = "Keep each direct parent type at most once.",
            });
            return error.DiagnosticsEmitted;
        }
        try seen.put(ctx.allocator, parent_leaf, parent_name.span);

        const parent_source = findTypeSource(ctx, local_types, parent_leaf) orelse {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM052",
                .title = "unknown parent type",
                .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not resolve the parent type '{s}'.", .{parent_leaf}),
                .labels = &.{diagnostics.primaryLabel(parent_name.span, "unknown parent type")},
                .help = "Declare the parent type before using it, or import the module that defines it.",
            });
            return error.DiagnosticsEmitted;
        };
        const parent_header = try resolveTypeHeader(ctx, local_types, resolver_states, type_headers, parent_source, parent_leaf);
        if (parent_header.ffi != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM053",
                .title = "invalid inheritance target",
                .message = try std.fmt.allocPrint(ctx.allocator, "The parent type '{s}' cannot be inherited because it is an FFI-defined type.", .{parent_leaf}),
                .labels = &.{diagnostics.primaryLabel(parent_name.span, "this parent is not a regular Kira type")},
                .help = "Inherit only from regular Kira types.",
            });
            return error.DiagnosticsEmitted;
        }

        const parent_offset = @as(u32, @intCast(fields.items.len));
        try parent_views.append(.{
            .type_name = try ctx.allocator.dupe(u8, parent_leaf),
            .offset = parent_offset,
            .span = parent_name.span,
        });
        for (parent_header.parent_views) |parent_view| {
            try parent_views.append(.{
                .type_name = parent_view.type_name,
                .offset = parent_offset + parent_view.offset,
                .span = parent_view.span,
            });
        }
        for (parent_header.fields) |field_decl| {
            var cloned = field_decl;
            cloned.slot_index = parent_offset + field_decl.slot_index;
            try fields.append(cloned);
        }
        for (parent_header.methods) |method_decl| {
            var cloned = method_decl;
            cloned.receiver_offset = parent_offset + method_decl.receiver_offset;
            try methods.append(cloned);
        }
    }

    _ = owner_span;
}

pub fn appendImportedParents(
    ctx: *shared.Context,
    local_types: *const LocalTypeMap,
    resolver_states: *std.StringHashMapUnmanaged(ResolverState),
    type_headers: *std.StringHashMapUnmanaged(shared.TypeHeader),
    owner_type_name: []const u8,
    parents: []const []const u8,
    fields: *std.array_list.Managed(model.Field),
    methods: *std.array_list.Managed(shared.MethodMember),
    parent_views: *std.array_list.Managed(shared.ParentView),
) anyerror!void {
    var seen = std.StringHashMapUnmanaged(void){};
    defer seen.deinit(ctx.allocator);

    for (parents) |parent_name| {
        if (std.mem.eql(u8, owner_type_name, parent_name)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM050",
                .title = "inheritance cycle",
                .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' cannot inherit from itself.", .{owner_type_name}),
                .labels = &.{diagnostics.primaryLabel(.{ .start = 0, .end = 0 }, "self-inheritance starts a cycle")},
                .help = "Remove the self-reference from the imported type's `extends` list.",
            });
            return error.DiagnosticsEmitted;
        }
        if (seen.contains(parent_name)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM051",
                .title = "duplicate parent type",
                .message = try std.fmt.allocPrint(ctx.allocator, "The imported type '{s}' lists '{s}' more than once in `extends`.", .{ owner_type_name, parent_name }),
                .labels = &.{diagnostics.primaryLabel(.{ .start = 0, .end = 0 }, "duplicate parent appears in imported metadata")},
                .help = "Keep each direct parent type at most once.",
            });
            return error.DiagnosticsEmitted;
        }
        try seen.put(ctx.allocator, parent_name, {});

        const parent_source = findTypeSource(ctx, local_types, parent_name) orelse {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM052",
                .title = "unknown parent type",
                .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not resolve the imported parent type '{s}'.", .{parent_name}),
                .labels = &.{diagnostics.primaryLabel(.{ .start = 0, .end = 0 }, "unknown imported parent type")},
                .help = "Import the parent type's module before relying on this inheritance chain.",
            });
            return error.DiagnosticsEmitted;
        };
        const parent_header = try resolveTypeHeader(ctx, local_types, resolver_states, type_headers, parent_source, parent_name);
        if (parent_header.ffi != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM053",
                .title = "invalid inheritance target",
                .message = try std.fmt.allocPrint(ctx.allocator, "The parent type '{s}' cannot be inherited because it is an FFI-defined type.", .{parent_name}),
                .labels = &.{diagnostics.primaryLabel(.{ .start = 0, .end = 0 }, "this imported parent is not a regular Kira type")},
                .help = "Inherit only from regular Kira types.",
            });
            return error.DiagnosticsEmitted;
        }

        const parent_offset = @as(u32, @intCast(fields.items.len));
        try parent_views.append(.{
            .type_name = try ctx.allocator.dupe(u8, parent_name),
            .offset = parent_offset,
            .span = .{ .start = 0, .end = 0 },
        });
        for (parent_header.parent_views) |parent_view| {
            try parent_views.append(.{
                .type_name = parent_view.type_name,
                .offset = parent_offset + parent_view.offset,
                .span = parent_view.span,
            });
        }
        for (parent_header.fields) |field_decl| {
            var cloned = field_decl;
            cloned.slot_index = parent_offset + field_decl.slot_index;
            try fields.append(cloned);
        }
        for (parent_header.methods) |method_decl| {
            var cloned = method_decl;
            cloned.receiver_offset = parent_offset + method_decl.receiver_offset;
            try methods.append(cloned);
        }
    }
}

pub fn appendDeclaredImportedMethods(
    ctx: *shared.Context,
    owner_type_name: []const u8,
    methods: *std.array_list.Managed(shared.MethodMember),
) !void {
    const prefix = try std.fmt.allocPrint(ctx.allocator, "{s}.", .{owner_type_name});
    for (ctx.imported_globals.functions) |function_decl| {
        if (!std.mem.startsWith(u8, function_decl.name, prefix)) continue;
        const leaf = function_decl.name[prefix.len..];
        if (std.mem.indexOfScalar(u8, leaf, '.') != null) continue;
        try methods.append(.{
            .name = leaf,
            .full_name = function_decl.name,
            .receiver_type_name = try ctx.allocator.dupe(u8, owner_type_name),
            .receiver_offset = 0,
            .params = if (function_decl.params.len > 0) function_decl.params[1..] else &.{},
            .param_ownership = if (function_decl.param_ownership.len > 0) function_decl.param_ownership[1..] else &.{},
            .return_type = function_decl.return_type,
            .return_ownership = function_decl.return_ownership,
            .span = .{ .start = 0, .end = 0 },
        });
    }
}

pub fn appendGeneratedAnnotationMethods(
    ctx: *shared.Context,
    type_decl: syntax.ast.TypeDecl,
    methods: *std.array_list.Managed(shared.MethodMember),
) !void {
    for (type_decl.annotations) |annotation| {
        const header = try shared.resolveAnnotationHeader(ctx, annotation.name);
        for (header.decl.generated_functions) |function_decl| {
            if (methodNameExists(methods.items, function_decl.name)) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM076",
                    .title = "duplicate generated member",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The annotation-generated function '{s}' conflicts with another inherited or generated method.", .{function_decl.name}),
                    .labels = &.{diagnostics.primaryLabel(function_decl.span, "duplicate generated function")},
                    .help = "Remove one annotation or capability that generates this member.",
                });
                return error.DiagnosticsEmitted;
            }
            try methods.append(.{
                .name = try ctx.allocator.dupe(u8, function_decl.name),
                .full_name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ type_decl.name, function_decl.name }),
                .receiver_type_name = try ctx.allocator.dupe(u8, type_decl.name),
                .receiver_offset = 0,
                .generated_by = try ctx.allocator.dupe(u8, header.decl.name),
                .overridable = function_decl.overridable,
                .params = function_decl.params,
                .param_ownership = function_decl.param_ownership,
                .return_type = function_decl.return_type,
                .return_ownership = function_decl.return_ownership,
                .span = function_decl.span,
            });
        }
    }
}

fn hasAnnotationNamed(annotations: []const syntax.ast.Annotation, name: []const u8) bool {
    for (annotations) |annotation| {
        const leaf = annotation.name.segments[annotation.name.segments.len - 1].text;
        if (std.mem.eql(u8, leaf, name)) return true;
    }
    return false;
}

pub fn applyLocalTypeMembers(
    ctx: *shared.Context,
    type_decl: syntax.ast.TypeDecl,
    fields: *std.array_list.Managed(model.Field),
    methods: *std.array_list.Managed(shared.MethodMember),
) !void {
    const inherited_field_count = fields.items.len;
    const inherited_method_count = methods.items.len;

    for (type_decl.members) |member| {
        if (member != .field_decl) continue;
        const field_decl = member.field_decl;

        if (field_decl.is_override) {
            const match = try findSingleInheritedField(ctx, fields.items[0..inherited_field_count], field_decl.name, field_decl.span);
            if (field_decl.annotations.len != 0) {
                try emitInvalidFieldOverride(ctx, field_decl.span, "Field overrides cannot add annotations.");
                return error.DiagnosticsEmitted;
            }
            if (field_decl.type_expr) |type_expr| {
                const explicit_type = try shared.typeFromSyntaxChecked(ctx, type_expr.*);
                if (!shared.canAssignExactly(match.field.ty, explicit_type)) {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM059",
                        .title = "field override changes type",
                        .message = try std.fmt.allocPrint(ctx.allocator, "The override for '{s}' must keep the inherited field type {s}.", .{ field_decl.name, try shared.typeTextFromResolved(ctx.allocator, match.field.ty) }),
                        .labels = &.{diagnostics.primaryLabel(field_decl.span, "override changes the inherited field type")},
                        .help = "Remove the type annotation or keep it exactly equal to the inherited field type.",
                    });
                    return error.DiagnosticsEmitted;
                }
            }
            if (match.field.storage != @as(model.FieldStorage, @enumFromInt(@intFromEnum(field_decl.storage)))) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM060",
                    .title = "field override changes mutability",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The override for '{s}' must keep the inherited field mutability.", .{field_decl.name}),
                    .labels = &.{diagnostics.primaryLabel(field_decl.span, "override changes the inherited field mutability")},
                    .help = "Use the same `let` or `var` spelling as the inherited field.",
                });
                return error.DiagnosticsEmitted;
            }
            if (field_decl.value == null) {
                try emitInvalidFieldOverride(ctx, field_decl.span, "Field overrides must provide a replacement default value.");
                return error.DiagnosticsEmitted;
            }
            fields.items[match.inherited_offset].default_value = try lowerFieldDefaultExprExpected(ctx, field_decl.value.?, match.field.ty, ctx.function_headers);
            fields.items[match.inherited_offset].span = field_decl.span;
            continue;
        }

        if (fieldNameExists(fields.items, field_decl.name)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM062",
                .title = "field name conflicts with inherited member",
                .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' already exposes a field named '{s}'.", .{ type_decl.name, field_decl.name }),
                .labels = &.{diagnostics.primaryLabel(field_decl.span, "this field would create a shadow field")},
                .help = "Rename the field or use `override` to replace the inherited default value.",
            });
            return error.DiagnosticsEmitted;
        }

        var lowered = try lowerField(ctx, field_decl, null);
        lowered.owner_type_name = try ctx.allocator.dupe(u8, type_decl.name);
        lowered.slot_index = @as(u32, @intCast(fields.items.len));
        try fields.append(lowered);
    }

    var local_methods = std.array_list.Managed(shared.MethodMember).init(ctx.allocator);
    var overridden_method_names = std.StringHashMapUnmanaged(void){};
    defer overridden_method_names.deinit(ctx.allocator);

    for (type_decl.members) |member| {
        if (member != .function_decl) continue;
        const function_decl = member.function_decl;
        if (methodNameExists(local_methods.items, function_decl.name)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM064",
                .title = "duplicate method name",
                .message = try std.fmt.allocPrint(ctx.allocator, "The type '{s}' declares more than one method named '{s}'.", .{ type_decl.name, function_decl.name }),
                .labels = &.{diagnostics.primaryLabel(function_decl.span, "duplicate method declaration")},
                .help = "Keep each method name unique within a type.",
            });
            return error.DiagnosticsEmitted;
        }

        const same_name = countMethodsByName(methods.items[0..inherited_method_count], function_decl.name);
        if (function_decl.is_override) {
            if (same_name == 0) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM054",
                    .title = "override has no matching inherited method",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The override '{s}' does not match any inherited method.", .{function_decl.name}),
                    .labels = &.{diagnostics.primaryLabel(function_decl.span, "no inherited method matches this override")},
                    .help = "Remove `override` or inherit a method with the same exact signature.",
                });
                return error.DiagnosticsEmitted;
            }
            const local_method = try makeDeclaredMethodMember(ctx, type_decl.name, function_decl);
            const exact_matches = countExactMethodMatches(methods.items[0..inherited_method_count], local_method);
            if (exact_matches == 0) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM055",
                    .title = "override signature mismatch",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The override '{s}' must match an inherited method signature exactly.", .{function_decl.name}),
                    .labels = &.{diagnostics.primaryLabel(function_decl.span, "override signature does not match any inherited method")},
                    .help = "Match the inherited parameter and return types exactly in v1.",
                });
                return error.DiagnosticsEmitted;
            }
            if (hasNonOverridableExactMethod(methods.items[0..inherited_method_count], local_method)) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM077",
                    .title = "generated member is not overridable",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The generated function '{s}' was not marked `overridable` by its annotation or capability.", .{function_decl.name}),
                    .labels = &.{diagnostics.primaryLabel(function_decl.span, "override targets a non-overridable generated member")},
                    .help = "Remove the override or mark the generated function `overridable` where it is declared.",
                });
                return error.DiagnosticsEmitted;
            }
            if (exact_matches != same_name) {
                try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                    .severity = .@"error",
                    .code = "KSEM056",
                    .title = "ambiguous inherited method lookup",
                    .message = try std.fmt.allocPrint(ctx.allocator, "The inherited method name '{s}' resolves to multiple different signatures.", .{function_decl.name}),
                    .labels = &.{diagnostics.primaryLabel(function_decl.span, "override does not resolve the inherited ambiguity")},
                    .help = "Rename one of the parent methods or qualify calls explicitly by parent type name.",
                });
                return error.DiagnosticsEmitted;
            }
            try overridden_method_names.put(ctx.allocator, function_decl.name, {});
            try local_methods.append(local_method);
            continue;
        }

        if (same_name != 0) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM054",
                .title = "override required for inherited method",
                .message = try std.fmt.allocPrint(ctx.allocator, "The method '{s}' already exists on an inherited parent type.", .{function_decl.name}),
                .labels = &.{diagnostics.primaryLabel(function_decl.span, "use `override` to replace the inherited method")},
                .help = "Mark the method with `override` and match the inherited signature exactly.",
            });
            return error.DiagnosticsEmitted;
        }

        try local_methods.append(try makeDeclaredMethodMember(ctx, type_decl.name, function_decl));
    }

    var final_methods = std.array_list.Managed(shared.MethodMember).init(ctx.allocator);
    for (methods.items[0..inherited_method_count]) |method_decl| {
        if (overridden_method_names.contains(method_decl.name)) continue;
        try final_methods.append(method_decl);
    }
    try final_methods.appendSlice(local_methods.items);
    methods.* = final_methods;
}

pub fn emitInvalidFieldOverride(ctx: *shared.Context, span: source_pkg.Span, message: []const u8) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM063",
        .title = "invalid field override",
        .message = message,
        .labels = &.{diagnostics.primaryLabel(span, "field override is not valid in this form")},
        .help = "Use `override let name = value` or `override var name = value` to replace only the inherited default value.",
    });
}

pub fn findSingleInheritedField(
    ctx: *shared.Context,
    fields: []const model.Field,
    field_name: []const u8,
    span: source_pkg.Span,
) !ResolvedFieldOverride {
    var match_index: ?u32 = null;
    for (fields, 0..) |field_decl, index| {
        if (!std.mem.eql(u8, field_decl.name, field_name)) continue;
        if (match_index != null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM058",
                .title = "ambiguous variable override target",
                .message = try std.fmt.allocPrint(ctx.allocator, "More than one inherited field named '{s}' is visible here.", .{field_name}),
                .labels = &.{diagnostics.primaryLabel(span, "override target is ambiguous")},
                .help = "Rename one of the conflicting parent fields or avoid overriding the ambiguous name.",
            });
            return error.DiagnosticsEmitted;
        }
        match_index = @as(u32, @intCast(index));
    }
    if (match_index == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM054",
            .title = "override has no matching inherited field",
            .message = try std.fmt.allocPrint(ctx.allocator, "The override '{s}' does not match any inherited field.", .{field_name}),
            .labels = &.{diagnostics.primaryLabel(span, "no inherited field matches this override")},
            .help = "Remove `override` or inherit a field with the same name first.",
        });
        return error.DiagnosticsEmitted;
    }
    return .{
        .field = fields[match_index.?],
        .inherited_offset = match_index.?,
    };
}

pub fn fieldNameExists(fields: []const model.Field, field_name: []const u8) bool {
    for (fields) |field_decl| {
        if (std.mem.eql(u8, field_decl.name, field_name)) return true;
    }
    return false;
}

pub fn methodNameExists(methods: []const shared.MethodMember, method_name: []const u8) bool {
    for (methods) |method_decl| {
        if (std.mem.eql(u8, method_decl.name, method_name)) return true;
    }
    return false;
}

pub fn countMethodsByName(methods: []const shared.MethodMember, method_name: []const u8) usize {
    var count: usize = 0;
    for (methods) |method_decl| {
        if (std.mem.eql(u8, method_decl.name, method_name)) count += 1;
    }
    return count;
}

pub fn countExactMethodMatches(methods: []const shared.MethodMember, candidate: shared.MethodMember) usize {
    var count: usize = 0;
    for (methods) |method_decl| {
        if (!std.mem.eql(u8, method_decl.name, candidate.name)) continue;
        if (sameMethodSignature(method_decl, candidate)) count += 1;
    }
    return count;
}

pub fn hasNonOverridableExactMethod(methods: []const shared.MethodMember, candidate: shared.MethodMember) bool {
    for (methods) |method_decl| {
        if (!std.mem.eql(u8, method_decl.name, candidate.name)) continue;
        if (sameMethodSignature(method_decl, candidate) and !method_decl.overridable) return true;
    }
    return false;
}

pub fn sameMethodSignature(lhs: shared.MethodMember, rhs: shared.MethodMember) bool {
    if (lhs.params.len != rhs.params.len) return false;
    if (!shared.canAssignExactly(lhs.return_type, rhs.return_type)) return false;
    for (lhs.params, rhs.params) |lhs_param, rhs_param| {
        if (!shared.canAssignExactly(lhs_param, rhs_param)) return false;
    }
    return true;
}

pub fn makeDeclaredMethodMember(
    ctx: *shared.Context,
    owner_type_name: []const u8,
    function_decl: syntax.ast.FunctionDecl,
) !shared.MethodMember {
    var params = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
    var param_ownership = std.array_list.Managed(model.OwnershipMode).init(ctx.allocator);
    for (function_decl.params) |param| {
        try param_ownership.append(shared.ownershipModeFromSyntax(param.type_expr));
        if (param.type_expr) |type_expr| {
            try params.append(try shared.typeFromSyntaxChecked(ctx, type_expr.*));
        } else {
            try params.append(.{ .kind = .unknown });
        }
    }
    return .{
        .name = try ctx.allocator.dupe(u8, function_decl.name),
        .full_name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ owner_type_name, function_decl.name }),
        .receiver_type_name = try ctx.allocator.dupe(u8, owner_type_name),
        .receiver_offset = 0,
        .params = try params.toOwnedSlice(),
        .param_ownership = try param_ownership.toOwnedSlice(),
        .return_type = if (function_decl.return_type) |return_type| try shared.typeFromSyntaxChecked(ctx, return_type.*) else .{ .kind = .unknown },
        .return_ownership = shared.ownershipModeFromSyntax(function_decl.return_type),
        .span = function_decl.span,
    };
}

pub fn registerTypeMethodHeaders(
    ctx: *shared.Context,
    type_decl: syntax.ast.TypeDecl,
    function_headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    for (type_decl.members) |member| {
        if (member != .function_decl) continue;
        const function_decl = member.function_decl;
        const annotation_info = try shared.resolveFunctionAnnotations(ctx, function_decl.annotations);
        const foreign = try shared.resolveForeignFunction(ctx, function_decl.annotations, function_decl.span);
        var param_types = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
        var param_ownership = std.array_list.Managed(model.OwnershipMode).init(ctx.allocator);
        try param_types.append(.{ .kind = .named, .name = type_decl.name });
        try param_ownership.append(.borrow_read);
        for (function_decl.params) |param| {
            try param_ownership.append(shared.ownershipModeFromSyntax(param.type_expr));
            if (param.type_expr) |type_expr| {
                try param_types.append(try shared.typeFromSyntaxChecked(ctx, type_expr.*));
            } else {
                try param_types.append(.{ .kind = .unknown });
            }
        }
        const method_name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ type_decl.name, function_decl.name });
        try function_headers.put(ctx.allocator, method_name, .{
            .id = @as(u32, @intCast(function_headers.count())),
            .params = try param_types.toOwnedSlice(),
            .param_ownership = try param_ownership.toOwnedSlice(),
            .execution = if (foreign != null and annotation_info.execution == .inherited) .native else annotation_info.execution,
            .return_type = if (function_decl.return_type) |return_type| try shared.typeFromSyntaxChecked(ctx, return_type.*) else .{ .kind = .unknown },
            .return_ownership = shared.ownershipModeFromSyntax(function_decl.return_type),
            .is_extern = foreign != null,
            .foreign = foreign,
            .span = function_decl.span,
        });
    }
}

pub fn lowerTypeMethods(
    ctx: *shared.Context,
    type_decl: syntax.ast.TypeDecl,
    imports: []const model.Import,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ![]model.Function {
    var methods = std.array_list.Managed(model.Function).init(ctx.allocator);
    for (type_decl.members) |member| {
        if (member != .function_decl) continue;
        try methods.append(try lowerMethodFunction(ctx, type_decl.name, member.function_decl, imports, function_headers));
    }
    return methods.toOwnedSlice();
}

pub fn lowerMethodFunction(
    ctx: *shared.Context,
    owner_type_name: []const u8,
    function_decl: syntax.ast.FunctionDecl,
    imports: []const model.Import,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !model.Function {
    const self_type_expr = try ctx.allocator.create(syntax.ast.TypeExpr);
    const self_segments = try ctx.allocator.alloc(syntax.ast.NameSegment, 1);
    self_segments[0] = .{ .text = owner_type_name, .span = function_decl.span };
    self_type_expr.* = .{ .named = .{
        .segments = self_segments,
        .span = function_decl.span,
    } };
    const borrowed_self_type_expr = try ctx.allocator.create(syntax.ast.TypeExpr);
    borrowed_self_type_expr.* = .{ .ownership = .{
        .mode = .borrow_read,
        .target = self_type_expr,
        .span = function_decl.span,
    } };

    var params = std.array_list.Managed(syntax.ast.ParamDecl).init(ctx.allocator);
    try params.append(.{
        .annotations = &.{},
        .name = "self",
        .type_expr = borrowed_self_type_expr,
        .span = function_decl.span,
    });
    try params.appendSlice(function_decl.params);

    const lowered = try lowerFunction(ctx, .{
        .annotations = function_decl.annotations,
        .name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ owner_type_name, function_decl.name }),
        .params = try params.toOwnedSlice(),
        .return_type = function_decl.return_type,
        .body = function_decl.body,
        .span = function_decl.span,
    }, imports, function_headers);
    return lowered;
}
