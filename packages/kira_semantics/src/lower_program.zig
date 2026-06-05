const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const exprs = @import("lower_exprs.zig");
const ImportedGlobals = @import("imported_globals.zig").ImportedGlobals;
const type_impl = @import("lower_program_types.zig");
const enum_impl = @import("lower_program_enums.zig");
const ffi_boundary = @import("lower_program_ffi_boundary.zig");

pub const lowerImports = type_impl.lowerImports;
pub const composeAnnotationGeneratedFunctions = type_impl.composeAnnotationGeneratedFunctions;
pub const appendGeneratedFunctionUnique = type_impl.appendGeneratedFunctionUnique;
pub const registerImportAliases = type_impl.registerImportAliases;
pub const lowerConstructDecl = type_impl.lowerConstructDecl;
pub const registerImportedFunctionHeaders = type_impl.registerImportedFunctionHeaders;
pub const resolveTypeHeader = type_impl.resolveTypeHeader;
pub const resolveLocalTypeHeader = type_impl.resolveLocalTypeHeader;
pub const resolveImportedTypeHeader = type_impl.resolveImportedTypeHeader;
pub const typeSourceSpan = type_impl.typeSourceSpan;
pub const findTypeSource = type_impl.findTypeSource;
pub const appendResolvedParents = type_impl.appendResolvedParents;
pub const appendImportedParents = type_impl.appendImportedParents;
pub const appendDeclaredImportedMethods = type_impl.appendDeclaredImportedMethods;
pub const appendGeneratedAnnotationMethods = type_impl.appendGeneratedAnnotationMethods;
pub const applyLocalTypeMembers = type_impl.applyLocalTypeMembers;
pub const emitInvalidFieldOverride = type_impl.emitInvalidFieldOverride;
pub const findSingleInheritedField = type_impl.findSingleInheritedField;
pub const fieldNameExists = type_impl.fieldNameExists;
pub const methodNameExists = type_impl.methodNameExists;
pub const countMethodsByName = type_impl.countMethodsByName;
pub const countExactMethodMatches = type_impl.countExactMethodMatches;
pub const hasNonOverridableExactMethod = type_impl.hasNonOverridableExactMethod;
pub const sameMethodSignature = type_impl.sameMethodSignature;
pub const makeDeclaredMethodMember = type_impl.makeDeclaredMethodMember;
pub const registerTypeMethodHeaders = type_impl.registerTypeMethodHeaders;
pub const lowerTypeMethods = type_impl.lowerTypeMethods;
pub const lowerMethodFunction = type_impl.lowerMethodFunction;

pub const ResolvedFieldOverride = struct {
    field: model.Field,
    inherited_offset: u32,
};

pub const ResolvedMethodMember = shared.MethodMember;

pub const AnalysisOptions = struct {
    require_main: bool = true,
};

pub const TypeSource = union(enum) {
    local: syntax.ast.TypeDecl,
    imported: @import("imported_globals.zig").ImportedType,
};

pub const LocalTypeMap = std.StringHashMapUnmanaged(syntax.ast.TypeDecl);

pub const ResolverState = enum {
    resolving,
    resolved,
};

fn declOrigin(program: syntax.ast.Program, index: usize) syntax.ast.DeclOrigin {
    if (index < program.decl_origins.len) return program.decl_origins[index];
    return .{};
}

fn scopedTopLevelName(allocator: std.mem.Allocator, origin: syntax.ast.DeclOrigin, name: []const u8) ![]const u8 {
    if (origin.package_name) |package_name| {
        return shared.scopedSymbolName(allocator, package_name, name);
    }
    return name;
}

fn registerScopedTopLevelName(
    allocator: std.mem.Allocator,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    map: *std.StringHashMapUnmanaged(source_pkg.Span),
    origin: syntax.ast.DeclOrigin,
    name: []const u8,
    span: source_pkg.Span,
) !void {
    const key = try scopedTopLevelName(allocator, origin, name);
    try shared.registerTopLevelName(allocator, out_diagnostics, map, key, span);
}

fn collectRootTopLevelNames(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    names: *std.StringHashMapUnmanaged(void),
) !void {
    for (program.decls, 0..) |decl, decl_index| {
        if (declOrigin(program, decl_index).package_name != null) continue;
        switch (decl) {
            .annotation_decl => |item| try names.put(allocator, item.name, {}),
            .capability_decl => |item| try names.put(allocator, item.name, {}),
            .enum_decl => |item| try names.put(allocator, item.name, {}),
            .type_decl => |item| try names.put(allocator, item.name, {}),
            .construct_decl => |item| try names.put(allocator, item.name, {}),
            .construct_form_decl => |item| try names.put(allocator, item.name, {}),
            .function_decl => |item| try names.put(allocator, item.name, {}),
        }
    }
}

fn putFunctionHeader(
    allocator: std.mem.Allocator,
    headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
    root_top_level_names: *const std.StringHashMapUnmanaged(void),
    origin: syntax.ast.DeclOrigin,
    name: []const u8,
    header: shared.FunctionHeader,
) !void {
    const scoped_name = try scopedTopLevelName(allocator, origin, name);
    try headers.put(allocator, scoped_name, header);
    if (origin.package_name == null) return;
    if (root_top_level_names.contains(name)) return;
    if (headers.get(name) != null) return;
    try headers.put(allocator, name, header);
}

pub fn lowerProgram(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    imported_globals: ImportedGlobals,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !model.Program {
    return lowerProgramWithOptions(allocator, program, imported_globals, .{}, out_diagnostics);
}

pub fn lowerProgramWithOptions(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    imported_globals: ImportedGlobals,
    options: AnalysisOptions,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !model.Program {
    var ctx = shared.Context{
        .allocator = allocator,
        .diagnostics = out_diagnostics,
        .imported_globals = imported_globals,
    };

    const imports = try lowerImports(&ctx, program);

    var top_level_names = std.StringHashMapUnmanaged(source_pkg.Span){};
    defer top_level_names.deinit(allocator);
    try registerImportAliases(&ctx, imports, &top_level_names);

    var construct_headers = std.StringHashMapUnmanaged(shared.ConstructHeader){};
    defer construct_headers.deinit(allocator);
    ctx.construct_headers = &construct_headers;
    var function_headers = std.StringHashMapUnmanaged(shared.FunctionHeader){};
    defer function_headers.deinit(allocator);
    ctx.function_headers = &function_headers;
    var enum_headers = std.StringHashMapUnmanaged(model.EnumDecl){};
    defer enum_headers.deinit(allocator);
    ctx.enum_headers = &enum_headers;
    var concrete_enums = std.StringHashMapUnmanaged(model.EnumDecl){};
    defer concrete_enums.deinit(allocator);
    ctx.concrete_enums = &concrete_enums;
    var type_headers = std.StringHashMapUnmanaged(shared.TypeHeader){};
    defer type_headers.deinit(allocator);
    ctx.type_headers = &type_headers;
    var annotation_headers = std.StringHashMapUnmanaged(shared.AnnotationHeader){};
    defer annotation_headers.deinit(allocator);
    try shared.registerBuiltinAnnotationHeaders(allocator, &annotation_headers);
    ctx.annotation_headers = &annotation_headers;

    var annotations = std.array_list.Managed(model.AnnotationDecl).init(allocator);
    var capabilities = std.array_list.Managed(model.CapabilityDecl).init(allocator);
    var capability_headers = std.StringHashMapUnmanaged(usize){};
    defer capability_headers.deinit(allocator);
    var constructs = std.array_list.Managed(model.Construct).init(allocator);
    var types = std.array_list.Managed(model.TypeDecl).init(allocator);
    var forms = std.array_list.Managed(model.ConstructForm).init(allocator);
    var functions = std.array_list.Managed(model.Function).init(allocator);
    var local_types = LocalTypeMap{};
    defer local_types.deinit(allocator);
    var resolver_states = std.StringHashMapUnmanaged(ResolverState){};
    defer resolver_states.deinit(allocator);
    var root_top_level_names = std.StringHashMapUnmanaged(void){};
    defer root_top_level_names.deinit(allocator);
    try collectRootTopLevelNames(allocator, program, &root_top_level_names);

    for (program.decls, 0..) |decl, decl_index| {
        const origin = declOrigin(program, decl_index);
        switch (decl) {
            .annotation_decl => |annotation_decl| {
                if (annotation_headers.get(annotation_decl.name)) |previous| {
                    try diagnostics.appendOwned(allocator, out_diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM060",
                        .title = "duplicate annotation declaration",
                        .message = try std.fmt.allocPrint(allocator, "Kira found more than one annotation declaration named '{s}'.", .{annotation_decl.name}),
                        .labels = &.{
                            diagnostics.primaryLabel(annotation_decl.span, "duplicate annotation declaration"),
                            diagnostics.secondaryLabel(previous.decl.span, "first annotation declaration was here"),
                        },
                        .help = "Rename one of the annotations so the symbol is unambiguous.",
                    });
                    return error.DiagnosticsEmitted;
                }
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, annotation_decl.name, annotation_decl.span);
                const lowered = try shared.lowerAnnotationDecl(&ctx, annotation_decl, "");
                try annotation_headers.put(allocator, lowered.name, .{
                    .index = annotations.items.len,
                    .decl = lowered,
                });
                try annotations.append(lowered);
            },
            .capability_decl => |capability_decl| {
                if (capability_headers.get(capability_decl.name)) |previous_index| {
                    try diagnostics.appendOwned(allocator, out_diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM072",
                        .title = "duplicate capability declaration",
                        .message = try std.fmt.allocPrint(allocator, "Kira found more than one capability declaration named '{s}'.", .{capability_decl.name}),
                        .labels = &.{
                            diagnostics.primaryLabel(capability_decl.span, "duplicate capability declaration"),
                            diagnostics.secondaryLabel(capabilities.items[previous_index].span, "first capability declaration was here"),
                        },
                        .help = "Rename one of the capabilities so annotation composition stays unambiguous.",
                    });
                    return error.DiagnosticsEmitted;
                }
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, capability_decl.name, capability_decl.span);
                const lowered = try shared.lowerCapabilityDecl(&ctx, capability_decl, "");
                try capability_headers.put(allocator, lowered.name, capabilities.items.len);
                try capabilities.append(lowered);
            },
            else => {},
        }
    }

    for (annotations.items) |*annotation_decl| {
        annotation_decl.generated_functions = try composeAnnotationGeneratedFunctions(&ctx, annotation_decl.*, capabilities.items, &capability_headers);
        if (annotation_headers.getPtr(annotation_decl.name)) |header| {
            header.decl = annotation_decl.*;
        }
    }

    for (program.decls, 0..) |decl, decl_index| {
        const origin = declOrigin(program, decl_index);
        switch (decl) {
            .annotation_decl, .capability_decl => {},
            .construct_decl => |construct_decl| {
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, construct_decl.name, construct_decl.span);
                const lowered = try lowerConstructDecl(&ctx, construct_decl);
                try construct_headers.put(allocator, lowered.name, .{
                    .index = constructs.items.len,
                    .span = lowered.span,
                });
                try constructs.append(lowered);
            },
            .enum_decl => |enum_decl| {
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, enum_decl.name, enum_decl.span);
                const lowered = try enum_impl.lowerEnumDecl(&ctx, enum_decl);
                try enum_headers.put(allocator, lowered.name, lowered);
                if (lowered.type_params.len == 0) try concrete_enums.put(allocator, lowered.name, lowered);
            },
            .type_decl => |type_decl| {
                if (!hasFfiAnnotation(type_decl.annotations)) {
                    try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, type_decl.name, type_decl.span);
                }
                try local_types.put(allocator, type_decl.name, type_decl);
            },
            .construct_form_decl => |form_decl| {
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, form_decl.name, form_decl.span);
            },
            .function_decl => |function_decl| {
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, function_decl.name, function_decl.span);
                const annotation_info = try shared.resolveFunctionAnnotations(&ctx, function_decl.annotations);
                const foreign = try shared.resolveForeignFunction(&ctx, function_decl.annotations, function_decl.span);
                var param_types = std.array_list.Managed(model.ResolvedType).init(allocator);
                var param_ownership = std.array_list.Managed(model.OwnershipMode).init(allocator);
                for (function_decl.params) |param| {
                    try param_ownership.append(shared.ownershipModeFromSyntax(param.type_expr));
                    if (param.type_expr) |type_expr| {
                        try param_types.append(try shared.typeFromSyntax(&ctx, type_expr.*));
                    } else {
                        try param_types.append(.{ .kind = .unknown });
                    }
                }
                const return_ownership = shared.ownershipModeFromSyntax(function_decl.return_type);
                try putFunctionHeader(allocator, &function_headers, &root_top_level_names, origin, function_decl.name, .{
                    .id = @as(u32, @intCast(function_headers.count())),
                    .params = try param_types.toOwnedSlice(),
                    .param_ownership = try param_ownership.toOwnedSlice(),
                    .execution = if (foreign != null and annotation_info.execution == .inherited) .native else annotation_info.execution,
                    .return_type = if (function_decl.return_type) |return_type| try shared.typeFromSyntax(&ctx, return_type.*) else .{ .kind = .unknown },
                    .return_ownership = return_ownership,
                    .is_extern = foreign != null,
                    .foreign = foreign,
                    .span = function_decl.span,
                });
            },
        }
    }

    try enum_impl.registerGenericEnumInstantiations(&ctx, program);

    for (imported_globals.types) |type_decl| {
        _ = try resolveTypeHeader(&ctx, &local_types, &resolver_states, &type_headers, .{ .imported = type_decl }, type_decl.name);
    }
    var local_type_iterator = local_types.iterator();
    while (local_type_iterator.next()) |entry| {
        _ = try resolveTypeHeader(&ctx, &local_types, &resolver_states, &type_headers, .{ .local = entry.value_ptr.* }, entry.key_ptr.*);
    }

    var type_header_iterator = type_headers.iterator();
    while (type_header_iterator.next()) |entry| {
        try types.append(.{
            .name = try allocator.dupe(u8, entry.key_ptr.*),
            .kind = entry.value_ptr.kind,
            .execution = entry.value_ptr.execution,
            .fields = @constCast(entry.value_ptr.fields),
            .methods = try lowerTypeMethodMembers(allocator, entry.value_ptr.methods),
            .ffi = entry.value_ptr.ffi,
            .span = entry.value_ptr.span,
        });
    }

    try registerImportedFunctionHeaders(&ctx, &function_headers);
    for (program.decls) |decl| {
        switch (decl) {
            .type_decl => |type_decl| try registerTypeMethodHeaders(&ctx, type_decl, &function_headers),
            .function_decl => {},
            else => {},
        }
    }
    try validatePrintableTypes(&ctx, &type_headers, &function_headers);

    var main_index: ?usize = null;
    var first_main_span: ?source_pkg.Span = null;

    for (imported_globals.functions) |function_decl| {
        if (!function_decl.is_extern) continue;
        const header = function_headers.get(function_decl.name) orelse continue;
        const empty_statements = try allocator.alloc(model.Statement, 0);
        const params = try lowerImportedParams(allocator, function_decl.params, function_decl.param_ownership);
        try functions.append(.{
            .id = header.id,
            .name = try allocator.dupe(u8, function_decl.name),
            .is_main = false,
            .execution = header.execution,
            .is_extern = true,
            .foreign = function_decl.foreign,
            .annotations = &.{},
            .params = params,
            .locals = &.{},
            .return_type = function_decl.return_type,
            .return_ownership = function_decl.return_ownership,
            .body = empty_statements,
            .span = header.span,
        });
    }

    for (program.decls, 0..) |decl, decl_index| {
        const previous_package = ctx.current_package;
        ctx.current_package = declOrigin(program, decl_index).package_name;
        switch (decl) {
            .construct_form_decl => |form_decl| try forms.append(try lowerConstructForm(&ctx, form_decl, imports, constructs.items, &construct_headers)),
            .function_decl => |function_decl| {
                const lowered = try lowerFunction(&ctx, function_decl, imports, &function_headers);
                if (lowered.is_main) {
                    if (first_main_span) |previous_span| {
                        try diagnostics.appendOwned(allocator, out_diagnostics, .{
                            .severity = .@"error",
                            .code = "KSEM002",
                            .title = "multiple @Main entrypoints",
                            .message = "A module can only have one @Main entrypoint.",
                            .labels = &.{
                                diagnostics.primaryLabel(function_decl.span, "this function is marked as another entrypoint"),
                                diagnostics.secondaryLabel(previous_span, "the first @Main entrypoint was declared here"),
                            },
                            .help = "Keep @Main on exactly one function.",
                        });
                        return error.DiagnosticsEmitted;
                    }
                    first_main_span = function_decl.span;
                    main_index = functions.items.len;
                }
                try functions.append(lowered);
            },
            .type_decl => |type_decl| {
                const lowered_methods = try lowerTypeMethods(&ctx, type_decl, imports, &function_headers);
                try functions.appendSlice(lowered_methods);
            },
            else => {},
        }
        ctx.current_package = previous_package;
    }

    if (main_index == null and options.require_main) {
        try diagnostics.appendOwned(allocator, out_diagnostics, .{
            .severity = .@"error",
            .code = "KSEM001",
            .title = "missing @Main entrypoint",
            .message = "This module cannot run because no function is marked with @Main.",
            .help = "Add @Main to exactly one zero-argument function, for example `@Main function entry() { ... }`.",
        });
        return error.DiagnosticsEmitted;
    }

    if (diagnostics.hasErrors(out_diagnostics.items)) return error.DiagnosticsEmitted;

    return .{
        .imports = imports,
        .annotations = try annotations.toOwnedSlice(),
        .capabilities = try capabilities.toOwnedSlice(),
        .enums = try ownedEnumSlice(allocator, &concrete_enums),
        .constructs = try constructs.toOwnedSlice(),
        .types = try types.toOwnedSlice(),
        .forms = try forms.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .entry_index = main_index orelse 0,
    };
}

fn lowerConstructForm(
    ctx: *shared.Context,
    form_decl: syntax.ast.ConstructFormDecl,
    imports: []const model.Import,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
) !model.ConstructForm {
    try shared.validateAnnotationPlacement(ctx, form_decl.annotations, .construct_form_decl, null);
    const construct_name = try shared.qualifiedNameText(ctx.allocator, form_decl.construct_name);
    const construct_root = form_decl.construct_name.segments[0].text;
    const imported_construct_visible = form_decl.construct_name.segments.len == 1 and ctx.imported_globals.hasConstruct(construct_name);

    var construct_model: ?model.Construct = null;
    if (construct_headers.get(construct_name)) |header| {
        construct_model = constructs[header.index];
    } else if (!imported_construct_visible and !shared.isImportedRoot(ctx, construct_root, imports)) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM020",
            .title = "unknown construct",
            .message = try std.fmt.allocPrint(ctx.allocator, "Kira could not find a construct named '{s}'.", .{construct_name}),
            .labels = &.{
                diagnostics.primaryLabel(form_decl.construct_name.span, "unknown construct"),
            },
            .help = "Declare the construct before using its declaration form, or import the library that provides it.",
        });
        return error.DiagnosticsEmitted;
    }

    var fields = std.array_list.Managed(model.Field).init(ctx.allocator);
    var lifecycle_hooks = std.array_list.Managed(model.LifecycleHook).init(ctx.allocator);
    var content: ?model.BuilderBlock = null;

    for (form_decl.body.members) |member| {
        switch (member) {
            .field_decl => |field_decl| try fields.append(try lowerField(ctx, field_decl, construct_model)),
            .content_section => |content_section| {
                try shared.validateAnnotationPlacement(ctx, content_section.annotations, .content_section, construct_model);
                const lowered_content = try exprs.lowerBuilderBlock(ctx, content_section.builder, imports, null);
                if (construct_model) |construct_info| {
                    if (construct_info.content_element_type) |element_type| {
                        try validateContentBlock(ctx, lowered_content, element_type);
                    }
                }
                content = lowered_content;
            },
            .lifecycle_hook => |hook| {
                if (construct_model) |construct_info| {
                    if (!shared.containsString(construct_info.allowed_lifecycle_hooks, hook.name)) {
                        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                            .severity = .@"error",
                            .code = "KSEM021",
                            .title = "invalid lifecycle hook",
                            .message = try std.fmt.allocPrint(ctx.allocator, "The construct '{s}' does not declare a lifecycle hook named '{s}'.", .{ construct_info.name, hook.name }),
                            .labels = &.{
                                diagnostics.primaryLabel(hook.span, "lifecycle hook is not declared by this construct"),
                            },
                            .help = "Declare the lifecycle hook in the construct's `lifecycle { ... }` section or remove it here.",
                        });
                        return error.DiagnosticsEmitted;
                    }
                }
                try lifecycle_hooks.append(.{
                    .name = try ctx.allocator.dupe(u8, hook.name),
                    .span = hook.span,
                });
            },
            else => {},
        }
    }

    if (construct_model) |construct_info| {
        if (construct_info.required_content and content == null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM022",
                .title = "missing required content block",
                .message = try std.fmt.allocPrint(ctx.allocator, "The construct '{s}' requires a `content {{ ... }}` block.", .{construct_info.name}),
                .labels = &.{
                    diagnostics.primaryLabel(form_decl.span, "required content block is missing"),
                },
                .help = "Add a `content { ... }` section to this declaration.",
            });
            return error.DiagnosticsEmitted;
        }
    }

    return .{
        .construct = .{ .construct_name = construct_name },
        .name = try ctx.allocator.dupe(u8, form_decl.name),
        .fields = try fields.toOwnedSlice(),
        .content = content,
        .lifecycle_hooks = try lifecycle_hooks.toOwnedSlice(),
        .span = form_decl.span,
    };
}

// A construct that declares `content: Content<T>;` accepts only element-typed
// (widget-producing) values in a declaration's content block. Raw primitive
// literals are never widgets, so reject them with a precise diagnostic instead of
// silently lowering a String where a Widget was expected.
fn validateContentBlock(ctx: *shared.Context, block: model.BuilderBlock, element_type: []const u8) anyerror!void {
    for (block.items) |item| {
        switch (item) {
            .expr => |expr_item| try validateContentValue(ctx, expr_item.expr, expr_item.span, element_type),
            .if_item => |if_item| {
                try validateContentBlock(ctx, if_item.then_block, element_type);
                if (if_item.else_block) |else_block| try validateContentBlock(ctx, else_block, element_type);
            },
            .for_item => |for_item| try validateContentBlock(ctx, for_item.body, element_type),
            .switch_item => |switch_item| {
                for (switch_item.cases) |case_node| try validateContentBlock(ctx, case_node.body, element_type);
                if (switch_item.default_block) |default_block| try validateContentBlock(ctx, default_block, element_type);
            },
        }
    }
}

fn validateContentValue(ctx: *shared.Context, expr: *model.Expr, span: source_pkg.Span, element_type: []const u8) anyerror!void {
    const found = model.exprType(expr.*);
    const found_label: ?[]const u8 = switch (found.kind) {
        .string, .c_string => "String",
        .integer => "Int",
        .float => "Float",
        .boolean => "Bool",
        else => null,
    };
    if (found_label) |label| {
        const text_hint = if (found.kind == .string or found.kind == .c_string)
            "; use Text(...) if visible text was intended"
        else
            "";
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM098",
            .title = "content value is not a widget",
            .message = try std.fmt.allocPrint(ctx.allocator, "Kira expected {s} content, found {s}{s}.", .{ element_type, label, text_hint }),
            .labels = &.{
                diagnostics.primaryLabel(span, "this value is not a widget"),
            },
            .help = "Content blocks accept widget-producing expressions, not raw values.",
        });
        return error.DiagnosticsEmitted;
    }
}

pub fn lowerFunction(
    ctx: *shared.Context,
    function_decl: syntax.ast.FunctionDecl,
    imports: []const model.Import,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !model.Function {
    try shared.validateAnnotationPlacement(ctx, function_decl.annotations, .function_decl, null);
    const annotation_info = try shared.resolveFunctionAnnotations(ctx, function_decl.annotations);
    const foreign = try shared.resolveForeignFunction(ctx, function_decl.annotations, function_decl.span);

    var scope = model.Scope{};
    defer scope.deinit(ctx.allocator);
    var locals = std.array_list.Managed(model.LocalSymbol).init(ctx.allocator);
    var params = std.array_list.Managed(model.Parameter).init(ctx.allocator);
    var next_local_id: u32 = 0;

    for (function_decl.params) |param| {
        if (param.type_expr == null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM024",
                .title = "parameter type is required",
                .message = "Parameters do not have enough context for inference and must declare a type.",
                .labels = &.{
                    diagnostics.primaryLabel(param.span, "parameter type is missing"),
                },
                .help = "Write the parameter type explicitly, for example `value: Int`.",
            });
            return error.DiagnosticsEmitted;
        }

        const param_type = try shared.typeFromSyntaxChecked(ctx, param.type_expr.?.*);
        const param_ownership = shared.ownershipModeFromSyntax(param.type_expr);
        const local_ownership: model.OwnershipMode = switch (param_ownership) {
            .borrow_read, .borrow_mut => param_ownership,
            .owned, .move, .copy => .owned,
        };
        try scope.put(ctx.allocator, param.name, .{
            .id = next_local_id,
            .ty = param_type,
            .storage = .immutable,
            .ownership = local_ownership,
            .initialized = true,
            .decl_span = param.span,
        });
        try params.append(.{
            .id = next_local_id,
            .name = try ctx.allocator.dupe(u8, param.name),
            .ty = param_type,
            .ownership = param_ownership,
            .span = param.span,
        });
        try locals.append(.{
            .id = next_local_id,
            .name = try ctx.allocator.dupe(u8, param.name),
            .ty = param_type,
            .ownership = local_ownership,
            .is_param = true,
            .span = param.span,
        });
        next_local_id += 1;
    }

    if (annotation_info.is_main and function_decl.params.len != 0) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM023",
            .title = "invalid @Main signature",
            .message = "The @Main entrypoint must not declare parameters.",
            .labels = &.{
                diagnostics.primaryLabel(function_decl.span, "@Main entrypoint declares parameters"),
            },
            .help = "Move inputs into library-level code and keep the entrypoint parameter-free.",
        });
        return error.DiagnosticsEmitted;
    }

    if (foreign != null and function_decl.body != null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM041",
            .title = "FFI extern must not declare a body",
            .message = "An @FFI.Extern function must be a declaration terminated with `;`.",
            .labels = &.{
                diagnostics.primaryLabel(function_decl.span, "FFI extern unexpectedly declares a body"),
            },
            .help = "Remove the body and keep only the signature for the foreign declaration.",
        });
        return error.DiagnosticsEmitted;
    }

    const explicit_return_ownership = shared.ownershipModeFromSyntax(function_decl.return_type);
    if (explicit_return_ownership == .borrow_read or explicit_return_ownership == .borrow_mut) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM112",
            .title = "returned borrow is not supported yet",
            .message = "Borrowed return types are reserved, but this compiler slice does not validate returned-borrow lifetimes yet.",
            .labels = &.{diagnostics.primaryLabel(function_decl.return_type.?.ownership.span, "borrowed return type is not implemented yet")},
            .help = "Return an owned value for now. Returned borrows will be enabled with input-borrow lifetime validation.",
        });
        return error.DiagnosticsEmitted;
    }
    const explicit_return_type = if (function_decl.return_type) |return_type| try shared.typeFromSyntaxChecked(ctx, return_type.*) else model.ResolvedType{ .kind = .unknown };
    const body = if (function_decl.body) |syntax_body|
        try exprs.lowerBlockStatements(ctx, syntax_body, imports, &scope, &locals, &next_local_id, function_headers, 0, explicit_return_type)
    else if (foreign != null)
        try ctx.allocator.alloc(model.Statement, 0)
    else {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM035",
            .title = "function body is required",
            .message = "This declaration does not have a function body, and non-FFI bodyless functions are not supported.",
            .labels = &.{
                diagnostics.primaryLabel(function_decl.span, "missing function body"),
            },
            .help = "Add a `{ ... }` body or mark the declaration with @FFI.Extern.",
        });
        return error.DiagnosticsEmitted;
    };
    const return_type = if (foreign != null)
        (if (explicit_return_type.kind == .unknown) model.ResolvedType{ .kind = .void } else explicit_return_type)
    else
        try exprs.resolveFunctionReturnType(ctx, explicit_return_type, body);
    const header = shared.findFunctionHeader(ctx, function_headers, function_decl.name).?;
    try ffi_boundary.validateDirectFfiBoundary(ctx, function_decl.name, header, body, function_headers);

    return .{
        .id = header.id,
        .name = try ctx.allocator.dupe(u8, function_decl.name),
        .is_main = annotation_info.is_main,
        .execution = header.execution,
        .is_extern = foreign != null,
        .foreign = foreign,
        .annotations = annotation_info.annotations,
        .params = try params.toOwnedSlice(),
        .locals = try locals.toOwnedSlice(),
        .return_type = return_type,
        .return_ownership = explicit_return_ownership,
        .body = body,
        .span = function_decl.span,
    };
}

pub fn lowerField(
    ctx: *shared.Context,
    field_decl: syntax.ast.FieldDecl,
    construct_model: ?model.Construct,
) !model.Field {
    try shared.validateAnnotationPlacement(ctx, field_decl.annotations, .field_decl, construct_model);
    const field_type = try exprs.resolveValueType(ctx, field_decl.type_expr, field_decl.value, field_decl.span);
    return .{
        .name = try ctx.allocator.dupe(u8, field_decl.name),
        .owner_type_name = "",
        .storage = @enumFromInt(@intFromEnum(field_decl.storage)),
        .slot_index = 0,
        .ty = field_type,
        .explicit_type = field_decl.type_expr != null,
        .default_value = if (field_decl.value) |value| try lowerFieldDefaultExprExpected(ctx, value, field_type, ctx.function_headers) else null,
        .annotations = try shared.lowerAnnotations(ctx, field_decl.annotations),
        .span = field_decl.span,
    };
}

pub fn lowerFieldDefaultExpr(ctx: *shared.Context, expr: *syntax.ast.Expr) !*model.Expr {
    return lowerFieldDefaultExprExpected(ctx, expr, .{ .kind = .unknown }, null);
}

pub fn lowerFieldDefaultExprExpected(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !*model.Expr {
    if (expected_type.kind == .enum_instance) {
        if (try lowerFieldDefaultEnumVariantExpr(ctx, expr, expected_type, function_headers)) |enum_expr| {
            return enum_expr;
        }
    }

    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = switch (expr.*) {
        .integer => |node| .{ .integer = .{
            .value = node.value,
            .ty = .{ .kind = .integer },
            .span = node.span,
        } },
        .float => |node| .{ .float = .{
            .value = node.value,
            .ty = .{ .kind = .float },
            .span = node.span,
        } },
        .string => |node| .{ .string = .{
            .value = try ctx.allocator.dupe(u8, node.value),
            .ty = .{ .kind = .string },
            .span = node.span,
        } },
        .bool => |node| .{ .boolean = .{
            .value = node.value,
            .ty = .{ .kind = .boolean },
            .span = node.span,
        } },
        .array => |node| blk: {
            var elements = std.array_list.Managed(*model.Expr).init(ctx.allocator);
            const element_type = try fieldDefaultArrayElementType(ctx, expected_type);
            for (node.elements) |element| try elements.append(try lowerFieldDefaultExprExpected(ctx, element, element_type, function_headers));
            break :blk .{ .array = .{
                .elements = try elements.toOwnedSlice(),
                .ty = if (expected_type.kind == .array) expected_type else .{ .kind = .array },
                .span = node.span,
            } };
        },
        .struct_literal => |node| blk: {
            var fields = std.array_list.Managed(model.ConstructFieldInit).init(ctx.allocator);
            for (node.fields) |field| {
                try fields.append(.{
                    .field_name = try ctx.allocator.dupe(u8, field.name),
                    .field_index = null,
                    .value = try lowerFieldDefaultExprExpected(ctx, field.value, .{ .kind = .unknown }, function_headers),
                    .span = field.span,
                });
            }
            break :blk .{ .construct = .{
                .type_name = try shared.qualifiedNameLeaf(ctx.allocator, node.type_name),
                .fields = try fields.toOwnedSlice(),
                .fill_mode = .defaults,
                .ty = .{ .kind = .named, .name = try shared.qualifiedNameLeaf(ctx.allocator, node.type_name) },
                .span = node.span,
            } };
        },
        .call => |node| blk: {
            const callee_name = switch (node.callee.*) {
                .identifier => |value| try shared.qualifiedNameText(ctx.allocator, value.name),
                .member => try flattenDefaultCalleeName(ctx.allocator, node.callee),
                else => {
                    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                        .severity = .@"error",
                        .code = "KSEM049",
                        .title = "unsupported field default value",
                        .message = "Field default constructor calls currently require a named type call target.",
                        .labels = &.{diagnostics.primaryLabel(defaultExprSpan(expr.*), "unsupported field default call target")},
                        .help = "Use a named type constructor such as `Point(x: 0.0, y: 0.0)`.",
                    });
                    return error.DiagnosticsEmitted;
                },
            };
            var fields = std.array_list.Managed(model.ConstructFieldInit).init(ctx.allocator);
            for (node.args) |arg| {
                try fields.append(.{
                    .field_name = if (arg.label) |label| try ctx.allocator.dupe(u8, label) else null,
                    .field_index = null,
                    .value = try lowerFieldDefaultExprExpected(ctx, arg.value, .{ .kind = .unknown }, function_headers),
                    .span = arg.span,
                });
            }
            break :blk .{ .construct = .{
                .type_name = try ctx.allocator.dupe(u8, qualifiedLeafText(callee_name)),
                .fields = try fields.toOwnedSlice(),
                .fill_mode = .defaults,
                .ty = .{ .kind = .named, .name = try ctx.allocator.dupe(u8, qualifiedLeafText(callee_name)) },
                .span = node.span,
            } };
        },
        .member => |node| .{ .namespace_ref = .{
            .root = switch (node.object.*) {
                .identifier => |value| try shared.qualifiedNameLeaf(ctx.allocator, value.name),
                else => "",
            },
            .path = switch (node.object.*) {
                .identifier => |value| try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ value.name.segments[value.name.segments.len - 1].text, node.member }),
                else => try ctx.allocator.dupe(u8, node.member),
            },
            .ty = .{ .kind = .unknown },
            .span = node.span,
        } },
        .identifier => |node| blk: {
            if (expected_type.kind == .callback) {
                if (function_headers) |headers| {
                    const name = try shared.qualifiedNameText(ctx.allocator, node.name);
                    if (headers.get(name)) |header| {
                        break :blk .{ .function_ref = .{
                            .representation = .callable_value,
                            .function_id = header.id,
                            .name = name,
                            .ty = expected_type,
                            .span = node.span,
                        } };
                    }
                }
            }
            try diagnostics.Emitter.init(ctx.allocator, ctx.diagnostics).err(.{
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Field defaults can only use a bare function name when the field has an explicit function type.",
                .span = defaultExprSpan(expr.*),
                .label = "unsupported field default value",
                .help = "Add an explicit function type to the field or use a literal/constructor default.",
            });
            return error.DiagnosticsEmitted;
        },
        .callback => {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Field defaults do not support callback literals.",
                .labels = &.{diagnostics.primaryLabel(defaultExprSpan(expr.*), "unsupported field default value")},
                .help = "Use a literal, constructor, or named constant for the field default.",
            });
            return error.DiagnosticsEmitted;
        },
        .unary => |node| blk: {
            const operand = try lowerFieldDefaultExprExpected(ctx, node.operand, .{ .kind = .unknown }, function_headers);
            break :blk .{ .unary = .{
                .operand = operand,
                .op = @enumFromInt(@intFromEnum(node.op)),
                .ty = model.hir.exprType(operand.*),
                .span = node.span,
            } };
        },
        else => {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Field default values in the executable pipeline currently require a literal or simple unary literal.",
                .labels = &.{diagnostics.primaryLabel(defaultExprSpan(expr.*), "unsupported field default value")},
                .help = "Use a literal default such as `7`, `true`, `1.5`, or `-1`.",
            });
            return error.DiagnosticsEmitted;
        },
    };
    return lowered;
}

fn lowerFieldDefaultEnumVariantExpr(
    ctx: *shared.Context,
    expr: *syntax.ast.Expr,
    expected_type: model.ResolvedType,
    function_headers: ?*const std.StringHashMapUnmanaged(shared.FunctionHeader),
) anyerror!?*model.Expr {
    const enum_name = expected_type.name orelse return null;
    const enum_decl = findEnumDeclForDefault(ctx, enum_name) orelse return null;
    const VariantTarget = struct {
        name: []const u8,
        payload: ?*syntax.ast.Expr,
        span: source_pkg.Span,
    };
    const target: VariantTarget = switch (expr.*) {
        .identifier => |node| blk: {
            if (node.name.segments.len < 2 or !std.mem.eql(u8, node.name.segments[0].text, enum_name)) return null;
            break :blk .{
                .name = node.name.segments[node.name.segments.len - 1].text,
                .payload = null,
                .span = node.span,
            };
        },
        .member => |node| .{
            .name = node.member,
            .payload = null,
            .span = node.span,
        },
        .call => |node| blk: {
            const variant_name = switch (node.callee.*) {
                .member => |member| member.member,
                .identifier => |callee| name_blk: {
                    if (callee.name.segments.len < 2 or !std.mem.eql(u8, callee.name.segments[0].text, enum_name)) return null;
                    break :name_blk callee.name.segments[callee.name.segments.len - 1].text;
                },
                else => return null,
            };
            break :blk .{
                .name = variant_name,
                .payload = if (node.args.len == 1) node.args[0].value else null,
                .span = node.span,
            };
        },
        else => return null,
    };
    const variant = findEnumVariantForDefault(enum_decl, target.name) orelse return null;
    const payload = if (target.payload) |payload_expr|
        if (variant.payload_ty) |payload_ty|
            try lowerFieldDefaultExprExpected(ctx, payload_expr, payload_ty, function_headers)
        else {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM105",
                .title = "match pattern payload is invalid",
                .message = "This enum variant does not accept a payload value.",
                .labels = &.{diagnostics.primaryLabel(target.span, "payload value is not valid for this enum variant")},
                .help = "Remove the argument from this enum constructor call.",
            });
            return error.DiagnosticsEmitted;
        }
    else if (variant.payload_ty != null)
        variant.default_value
    else
        null;
    if (variant.payload_ty != null and payload == null) {
        try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
            .severity = .@"error",
            .code = "KSEM105",
            .title = "match pattern payload is invalid",
            .message = "This enum variant requires an associated payload value.",
            .labels = &.{diagnostics.primaryLabel(target.span, "missing enum payload value")},
            .help = "Pass the payload argument or add a default value on the enum variant declaration.",
        });
        return error.DiagnosticsEmitted;
    }
    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = .{ .construct_enum_variant = .{
        .enum_name = try ctx.allocator.dupe(u8, enum_decl.name),
        .variant_name = try ctx.allocator.dupe(u8, variant.name),
        .discriminant = variant.discriminant,
        .payload = payload,
        .ty = .{ .kind = .enum_instance, .name = try ctx.allocator.dupe(u8, enum_decl.name) },
        .span = target.span,
    } };
    return lowered;
}

fn findEnumDeclForDefault(ctx: *shared.Context, enum_name: []const u8) ?model.EnumDecl {
    if (ctx.concrete_enums) |concrete_enums| {
        if (concrete_enums.get(enum_name)) |enum_decl| return enum_decl;
    }
    if (ctx.enum_headers) |enum_headers| {
        if (enum_headers.get(enum_name)) |enum_decl| return enum_decl;
    }
    return null;
}

fn findEnumVariantForDefault(enum_decl: model.EnumDecl, variant_name: []const u8) ?model.EnumVariantHir {
    for (enum_decl.variants) |variant| {
        if (std.mem.eql(u8, variant.name, variant_name)) return variant;
    }
    return null;
}

fn fieldDefaultArrayElementType(ctx: *shared.Context, expected_type: model.ResolvedType) !model.ResolvedType {
    if (expected_type.kind != .array or expected_type.name == null) return .{ .kind = .unknown };
    const element_name = expected_type.name.?;
    if (ctx.enum_headers) |enum_headers| {
        if (enum_headers.get(element_name) != null) return .{ .kind = .enum_instance, .name = element_name };
    }
    if (ctx.concrete_enums) |concrete_enums| {
        if (concrete_enums.get(element_name) != null) return .{ .kind = .enum_instance, .name = element_name };
    }
    return try shared.resolvedTypeFromText(element_name);
}

fn defaultExprSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        .integer => |node| node.span,
        .float => |node| node.span,
        .string => |node| node.span,
        .bool => |node| node.span,
        .identifier => |node| node.span,
        .array => |node| node.span,
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
    };
}

fn flattenDefaultCalleeName(allocator: std.mem.Allocator, expr: *syntax.ast.Expr) ![]const u8 {
    return switch (expr.*) {
        .identifier => |value| shared.qualifiedNameText(allocator, value.name),
        .member => |value| blk: {
            const left = try flattenDefaultCalleeName(allocator, value.object);
            break :blk try std.fmt.allocPrint(allocator, "{s}.{s}", .{ left, value.member });
        },
        else => allocator.dupe(u8, "<expr>"),
    };
}

fn hasFfiAnnotation(annotations: []const syntax.ast.Annotation) bool {
    for (annotations) |annotation| {
        if (annotation.name.segments.len >= 2 and
            std.mem.eql(u8, annotation.name.segments[0].text, "FFI"))
        {
            return true;
        }
    }
    return false;
}

fn lowerTypeMethodMembers(
    allocator: std.mem.Allocator,
    methods: []const shared.MethodMember,
) ![]model.MethodMember {
    const lowered = try allocator.alloc(model.MethodMember, methods.len);
    for (methods, 0..) |method_decl, index| {
        lowered[index] = .{
            .name = try allocator.dupe(u8, method_decl.name),
            .full_name = try allocator.dupe(u8, method_decl.full_name),
            .receiver_offset = method_decl.receiver_offset,
            .span = method_decl.span,
        };
    }
    return lowered;
}

fn ownedEnumSlice(
    allocator: std.mem.Allocator,
    concrete_enums: *const std.StringHashMapUnmanaged(model.EnumDecl),
) ![]model.EnumDecl {
    const owned = try allocator.alloc(model.EnumDecl, concrete_enums.count());
    var iterator = concrete_enums.iterator();
    var index: usize = 0;
    while (iterator.next()) |entry| : (index += 1) {
        owned[index] = entry.value_ptr.*;
    }
    return owned;
}

fn validatePrintableTypes(
    ctx: *shared.Context,
    type_headers: *const std.StringHashMapUnmanaged(shared.TypeHeader),
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    var iterator = type_headers.iterator();
    while (iterator.next()) |entry| {
        if (!entry.value_ptr.is_printable) continue;
        const method_key = try std.fmt.allocPrint(ctx.allocator, "{s}.onPrint", .{entry.key_ptr.*});
        const header = function_headers.get(method_key);
        if (header == null or header.?.return_type.kind != .string) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM102",
                .title = "missing onPrint for @Printable type",
                .message = try std.fmt.allocPrint(ctx.allocator, "The @Printable type '{s}' must declare `function onPrint() -> String`.", .{entry.key_ptr.*}),
                .labels = &.{diagnostics.primaryLabel(entry.value_ptr.span, "@Printable type is missing a compatible onPrint method")},
                .help = "Add `function onPrint() -> String` to the type, or remove @Printable.",
            });
            return error.DiagnosticsEmitted;
        }
    }
}

fn qualifiedLeafText(name: []const u8) []const u8 {
    const index = std.mem.lastIndexOfScalar(u8, name, '.') orelse return name;
    return name[index + 1 ..];
}

fn lowerImportedParams(allocator: std.mem.Allocator, param_types: []const model.ResolvedType, param_ownership: []const model.OwnershipMode) ![]model.Parameter {
    var params = std.array_list.Managed(model.Parameter).init(allocator);
    for (param_types, 0..) |param_type, index| {
        try params.append(.{
            .id = @as(u32, @intCast(index)),
            .name = try std.fmt.allocPrint(allocator, "arg_{d}", .{index}),
            .ty = param_type,
            .ownership = if (index < param_ownership.len) param_ownership[index] else .owned,
            .span = .{ .start = 0, .end = 0 },
        });
    }
    return params.toOwnedSlice();
}
