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
const requirements = @import("lower_construct_requirements.zig");
const field_requirements = @import("lower_construct_field_requirements.zig");
const widget_content = @import("lower_widget_content.zig");
const content_composition = @import("lower_construct_content.zig");
const content_validation = @import("lower_construct_content_validation.zig");
const construct_functions = @import("lower_construct_functions.zig");
const construct_extensions = @import("lower_construct_extensions.zig");
const construct_defaults = @import("lower_construct_default_accessors.zig");
const construct_members = @import("lower_construct_members.zig");
const construct_tests = @import("lower_construct_tests.zig");
const node_bridge = @import("lower_construct_node_bridge.zig");
const type_accessors = @import("lower_type_constant_accessors.zig");
const form_surface = @import("construct_form_surface.zig");

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
const field_defaults = @import("lower_program_field_defaults.zig");
pub const lowerField = field_defaults.lowerField;
pub const lowerFieldDefaultExpr = field_defaults.lowerFieldDefaultExpr;
pub const lowerFieldDefaultExprExpected = field_defaults.lowerFieldDefaultExprExpected;

pub const ResolvedFieldOverride = struct {
    field: model.Field,
    inherited_offset: u32,
};

pub const ResolvedMethodMember = shared.MethodMember;

pub const AnalysisOptions = struct {
    require_main: bool = true,
    /// Set for the VM target: ordinary runtime functions may call FFI-bound
    /// symbols directly because the VM dispatches them through LibFFI. Other
    /// backends keep the KSEM093 "@Native" requirement.
    allow_runtime_direct_ffi: bool = false,
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
            // Extension declarations add no new top-level name; they extend an existing construct.
            .extend_decl => {},
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
        .allow_runtime_direct_ffi = options.allow_runtime_direct_ffi,
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
    var tests = std.array_list.Managed(model.TestCase).init(allocator);
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
            .annotation_decl, .capability_decl, .extend_decl => {},
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
                // Gap #1 (runtime): a concrete declaration is also a struct type of its stored
                // scalar fields, so `Text(text: "hi")` lowers through the ordinary struct
                // construction path into an `alloc_struct`. Composition members (`@Content`
                // children, computed `let node { ... }`) are excluded — they are not stored state.
                try local_types.put(allocator, form_decl.name, try synthesizeFormStruct(&ctx, form_decl));
            },
            .function_decl => |function_decl| {
                try registerScopedTopLevelName(allocator, out_diagnostics, &top_level_names, origin, function_decl.name, function_decl.span);
                const annotation_info = try shared.resolveFunctionAnnotations(&ctx, function_decl.annotations);
                const foreign = try shared.resolveForeignFunction(&ctx, function_decl.annotations, function_decl.span);
                var param_types = std.array_list.Managed(model.ResolvedType).init(allocator);
                var param_ownership = std.array_list.Managed(model.OwnershipMode).init(allocator);
                var param_defaults = std.array_list.Managed(?*syntax.ast.Expr).init(allocator);
                for (function_decl.params) |param| {
                    try param_ownership.append(shared.ownershipModeFromSyntax(param.type_expr));
                    try param_defaults.append(param.default_value);
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
                    .param_defaults = try param_defaults.toOwnedSlice(),
                    .execution = if (foreign != null and annotation_info.execution == .inherited) .native else annotation_info.execution,
                    .return_type = if (function_decl.return_type) |return_type| try shared.typeFromSyntax(&ctx, return_type.*) else .{ .kind = .unknown },
                    .return_ownership = return_ownership,
                    .is_extern = foreign != null,
                    .is_comptime = function_decl.is_comptime,
                    .comptime_decl = if (function_decl.is_comptime) function_decl else null,
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

    // All constructs are registered above; validate `extends` (unknown parent + cycles)
    // before registering construct-family methods that depend on the family graph.
    try type_impl.validateConstructInheritance(&ctx, constructs.items, &construct_headers);
    try content_composition.validateConstructContentComposition(&ctx, constructs.items, &construct_headers);

    // Map each construct-backed declaration to its declared parent (a construct or another
    // declaration), so a declaration's construct family can be resolved through a chain such as
    // `Drawable Sprite { ... }` then `Sprite Player { ... }`.
    var form_parent = std.StringHashMapUnmanaged([]const u8){};
    defer form_parent.deinit(allocator);
    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        const parent_leaf = form_decl.construct_name.segments[form_decl.construct_name.segments.len - 1].text;
        try form_parent.put(allocator, form_decl.name, parent_leaf);
    }
    // Reject declaration-parent cycles before lowering, so they surface as cycles (KSEM119)
    // rather than as unresolvable parents during form lowering.
    try requirements.validateFormParentCycles(&ctx, program, &form_parent);

    // Map each declaration's type name to the construct families it satisfies, so concrete widget
    // values coerce to `any Widget` during body lowering and method registration.
    var form_families = std.StringHashMapUnmanaged([]const []const u8){};
    defer form_families.deinit(allocator);
    try buildFormFamilies(allocator, program, constructs.items, &construct_headers, &form_parent, &form_families);
    ctx.form_families = &form_families;

    try registerImportedFunctionHeaders(&ctx, &function_headers);
    for (program.decls) |decl| {
        switch (decl) {
            .type_decl => |type_decl| {
                try registerTypeMethodHeaders(&ctx, type_decl, &function_headers);
                try type_accessors.registerTypeAccessorHeaders(&ctx, type_decl, &function_headers);
            },
            .construct_form_decl => |form_decl| {
                try construct_tests.registerTestSectionHeaders(&ctx, form_decl, &function_headers);
                try construct_functions.registerConstructFormFunctionHeaders(&ctx, form_decl, &function_headers);
                try construct_functions.registerConstructFormMethods(&ctx, form_decl, &type_headers, &function_headers);
                try node_bridge.registerFormAccessorHeaders(&ctx, form_decl, &function_headers);
                try node_bridge.registerFormAccessorMethods(&ctx, form_decl, &type_headers, &function_headers);
                try construct_defaults.registerDefaultFunctionHeaders(&ctx, program, form_decl, &function_headers);
                try construct_defaults.registerDefaultMethods(&ctx, program, form_decl, &type_headers, &function_headers);
            },
            .extend_decl => |extend_decl| {
                try construct_extensions.registerExtendFunctionHeaders(&ctx, extend_decl, &function_headers);
                try construct_extensions.registerExtendMethods(&ctx, extend_decl, &type_headers, &function_headers);
            },
            .function_decl => {},
            else => {},
        }
    }
    try validatePrintableTypes(&ctx, &type_headers, &function_headers);

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

    // Record each declaration's `@Content` fields so construction routes trailing blocks into them.
    var form_content_fields = std.StringHashMapUnmanaged([]const shared.ContentFieldRef){};
    defer form_content_fields.deinit(allocator);
    try buildFormContentFields(&ctx, program, &form_content_fields);
    ctx.form_content_fields = &form_content_fields;

    // Validate caller-provided `@Content` at construction sites before lowering the forms. The
    // content diagnostics (KSEM142-145) are more specific than the struct-construction errors
    // (e.g. KSEM081 missing field) that `@Content`-as-real-fields lowering would otherwise raise
    // first for the same mistake.
    try widget_content.validateWidgetContent(&ctx, program);

    for (program.decls, 0..) |decl, decl_index| {
        const previous_package = ctx.current_package;
        const previous_source_path = ctx.current_source_path;
        ctx.current_package = declOrigin(program, decl_index).package_name;
        ctx.current_source_path = declOrigin(program, decl_index).source_path;
        switch (decl) {
            .construct_form_decl => |form_decl| {
                try forms.append(try lowerConstructForm(&ctx, form_decl, imports, constructs.items, &construct_headers, &form_parent));
                try functions.appendSlice(try construct_functions.lowerConstructFormFunctions(&ctx, form_decl, imports, &function_headers));
                try functions.appendSlice(try construct_tests.lowerTestSections(&ctx, form_decl, imports, &function_headers, &tests));
                try functions.appendSlice(try node_bridge.lowerFormAccessors(&ctx, form_decl, imports, &function_headers));
                try functions.appendSlice(try construct_defaults.lowerDefaultFunctions(&ctx, program, form_decl, imports, &function_headers));
            },
            .function_decl => |function_decl| {
                if (function_decl.is_comptime) continue;
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
                try functions.appendSlice(try type_accessors.lowerTypeAccessors(&ctx, type_decl, imports, &function_headers));
            },
            .extend_decl => |extend_decl| {
                try functions.appendSlice(try construct_extensions.lowerExtendFunctions(&ctx, extend_decl, imports, &function_headers));
            },
            else => {},
        }
        ctx.current_package = previous_package;
        ctx.current_source_path = previous_source_path;
    }

    // Validate required-function satisfaction across the mixed construct/declaration graph.
    try requirements.validateConstructFormRequirements(&ctx, program, constructs.items, &construct_headers, &form_parent);
    // Validate `@Required` field satisfaction with the terminal-`node` rule.
    try field_requirements.validateConstructFormFieldRequirements(&ctx, program, constructs.items, &construct_headers, &form_parent);
    // `extend C { ... }` must target a known construct family.
    for (program.decls) |decl| {
        if (decl != .extend_decl) continue;
        const extend_decl = decl.extend_decl;
        const name = try shared.qualifiedNameText(allocator, extend_decl.construct_name);
        const leaf = extend_decl.construct_name.segments[extend_decl.construct_name.segments.len - 1].text;
        const root = extend_decl.construct_name.segments[0].text;
        const known = construct_headers.get(leaf) != null or
            ctx.imported_globals.hasConstruct(name) or
            shared.isImportedRoot(&ctx, root, imports);
        if (!known) {
            try diagnostics.appendOwned(allocator, out_diagnostics, .{
                .severity = .@"error",
                .code = "KSEM146",
                .title = "unknown extend target",
                .message = try std.fmt.allocPrint(allocator, "Kira could not find a construct named '{s}' to extend.", .{name}),
                .labels = &.{diagnostics.primaryLabel(extend_decl.construct_name.span, "unknown construct")},
                .help = "Declare the construct before extending it, or import the module that provides it.",
            });
            return error.DiagnosticsEmitted;
        }
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
        .tests = try tests.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .entry_index = main_index orelse 0,
    };
}

// Build the struct type backing a concrete declaration: its stored scalar fields only. Computed
// composition members (`let node: Node { ... }`) and caller-provided `@Content` children are not
// stored state, so they are excluded from the runtime layout.
// For each construct-backed declaration, record the construct families it satisfies: its family
// construct plus that construct's transitive `extends` ancestors. Used to coerce a concrete widget
// value to `any Widget`.
fn buildFormFamilies(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    form_parent: *const std.StringHashMapUnmanaged([]const u8),
    out: *std.StringHashMapUnmanaged([]const []const u8),
) !void {
    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        const parent_leaf = form_decl.construct_name.segments[form_decl.construct_name.segments.len - 1].text;
        const family = requirements.resolveFamilyConstructModel(constructs, construct_headers, form_parent, parent_leaf) orelse continue;
        var names = std.array_list.Managed([]const u8).init(allocator);
        try collectConstructAncestry(allocator, family, constructs, construct_headers, &names);
        try out.put(allocator, form_decl.name, try names.toOwnedSlice());
    }
}

fn collectConstructAncestry(
    allocator: std.mem.Allocator,
    construct_model: model.Construct,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    out: *std.array_list.Managed([]const u8),
) !void {
    for (out.items) |existing| {
        if (std.mem.eql(u8, existing, construct_model.name)) return;
    }
    try out.append(construct_model.name);
    for (construct_model.parents) |parent_link| {
        if (construct_headers.get(parent_link.name)) |header| {
            try collectConstructAncestry(allocator, constructs[header.index], constructs, construct_headers, out);
        }
    }
}

fn buildFormContentFields(
    ctx: *shared.Context,
    program: syntax.ast.Program,
    out: *std.StringHashMapUnmanaged([]const shared.ContentFieldRef),
) !void {
    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        const body_members = try form_surface.effectiveMembers(ctx, form_decl);
        var fields = std.array_list.Managed(shared.ContentFieldRef).init(ctx.allocator);
        for (body_members) |member| {
            if (member != .field_decl) continue;
            const field = member.field_decl;
            if (!construct_members.hasContentAnnotation(field.annotations)) continue;
            try fields.append(.{
                .name = field.name,
                .is_list = field.type_expr != null and field.type_expr.?.* == .array,
            });
        }
        if (fields.items.len > 0) try out.put(ctx.allocator, form_decl.name, try fields.toOwnedSlice());
    }
}

fn synthesizeFormStruct(ctx: *shared.Context, form_decl: syntax.ast.ConstructFormDecl) !syntax.ast.TypeDecl {
    const body_members = try form_surface.effectiveMembers(ctx, form_decl);
    var members = std.array_list.Managed(syntax.ast.BodyMember).init(ctx.allocator);
    for (body_members) |member| {
        if (member != .field_decl) continue;
        const field = member.field_decl;
        // A computed `let node: T { ... }` is the bridge accessor, never stored state.
        if (field.body != null) continue;
        if (construct_members.hasContentAnnotation(field.annotations)) {
            // Caller-provided children are stored as `any Family` slots so heterogeneous widgets
            // coexist. Rewrite the family-typed field (`[Widget]`/`Widget`) to `any` form.
            var content_field = field;
            content_field.type_expr = if (field.type_expr) |type_expr| try anyifyContentType(ctx.allocator, type_expr) else null;
            try members.append(.{ .field_decl = content_field });
            continue;
        }
        try members.append(.{ .field_decl = field });
    }
    return .{
        .kind = .struct_decl,
        .annotations = &.{},
        .name = form_decl.name,
        .parents = &.{},
        .members = try members.toOwnedSlice(),
        .span = form_decl.span,
    };
}

// Rewrite an `@Content` field's family type to its `any` form: `[Widget]` -> `[any Widget]`,
// `Widget` -> `any Widget`. A type already wrapped in `any` is left as-is.
fn anyifyContentType(allocator: std.mem.Allocator, type_expr: *syntax.ast.TypeExpr) !*syntax.ast.TypeExpr {
    switch (type_expr.*) {
        .array => |array| {
            const element = try anyifyContentType(allocator, array.element_type);
            const rewritten = try allocator.create(syntax.ast.TypeExpr);
            rewritten.* = .{ .array = .{ .element_type = element, .span = array.span } };
            return rewritten;
        },
        .named => |named| {
            const target = try allocator.create(syntax.ast.TypeExpr);
            target.* = .{ .named = named };
            const rewritten = try allocator.create(syntax.ast.TypeExpr);
            rewritten.* = .{ .any = .{ .target = target, .span = named.span } };
            return rewritten;
        },
        else => return type_expr,
    }
}

fn lowerConstructForm(
    ctx: *shared.Context,
    form_decl: syntax.ast.ConstructFormDecl,
    imports: []const model.Import,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    form_parent: *const std.StringHashMapUnmanaged([]const u8),
) !model.ConstructForm {
    try shared.validateAnnotationPlacement(ctx, form_decl.annotations, .construct_form_decl, null);
    const construct_name = try shared.qualifiedNameText(ctx.allocator, form_decl.construct_name);
    const construct_root = form_decl.construct_name.segments[0].text;
    const imported_construct_visible = form_decl.construct_name.segments.len == 1 and ctx.imported_globals.hasConstruct(construct_name);

    // The parent may be a construct or another construct-backed declaration; resolve the
    // construct family that ultimately governs this declaration's content/properties/lifecycle.
    var construct_model: ?model.Construct = null;
    if (requirements.resolveFamilyConstructModel(constructs, construct_headers, form_parent, construct_name)) |family| {
        construct_model = family;
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

    const body_members = try form_surface.effectiveMembers(ctx, form_decl);
    for (body_members) |member| {
        switch (member) {
            // A computed/composition member (`let node: Node { ... }`) is the typed Widget->Node
            // bridge, and an `@Content` field is caller-provided children — both are part of the
            // composition contract, not stored runtime state, so neither becomes a runtime field.
            // Their roles are validated separately (terminal-`node` rule, content routing).
            .field_decl => |field_decl| {
                if (field_decl.body != null) continue;
                if (construct_members.hasContentAnnotation(field_decl.annotations)) continue;
                try fields.append(try lowerField(ctx, field_decl, construct_model));
            },
            .content_section => |content_section| {
                try shared.validateAnnotationPlacement(ctx, content_section.annotations, .content_section, construct_model);
                const lowered_content = try exprs.lowerBuilderBlock(ctx, content_section.builder, imports, null);
                if (construct_model) |construct_info| {
                    if (construct_info.content_element_type) |element_type| {
                        try content_validation.validateBlock(ctx, lowered_content, element_type);
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
        try type_impl.validateFormProperties(ctx, form_decl, construct_info, constructs, construct_headers);
        try type_impl.validateFormContentChannels(ctx, form_decl, construct_info, constructs, construct_headers);
    }

    return .{
        .construct = .{ .construct_name = construct_name },
        .name = try ctx.allocator.dupe(u8, form_decl.name),
        .families = try formFamiliesFor(ctx, form_decl.name),
        .fields = try fields.toOwnedSlice(),
        .content = content,
        .lifecycle_hooks = try lifecycle_hooks.toOwnedSlice(),
        .span = form_decl.span,
    };
}

fn formFamiliesFor(ctx: *shared.Context, form_name: []const u8) ![]const []const u8 {
    const families = ctx.form_families orelse return &.{};
    const list = families.get(form_name) orelse return &.{};
    const owned = try ctx.allocator.alloc([]const u8, list.len);
    for (list, 0..) |family, index| owned[index] = try ctx.allocator.dupe(u8, family);
    return owned;
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
    const body = if (function_decl.body) |syntax_body| blk: {
        const previous_locals = ctx.active_locals;
        const previous_next_local_id = ctx.active_next_local_id;
        ctx.active_locals = &locals;
        ctx.active_next_local_id = &next_local_id;
        defer {
            ctx.active_locals = previous_locals;
            ctx.active_next_local_id = previous_next_local_id;
        }
        break :blk try exprs.lowerBlockStatements(ctx, syntax_body, imports, &scope, &locals, &next_local_id, function_headers, 0, explicit_return_type);
    } else if (foreign != null)
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
    // A partial field move that was never written back leaves the base owning storage it has
    // already handed to another owner; dropping it at scope exit double-frees. Reject before
    // the function returns (the in-place idiom clears the field via write-back, so it passes).
    if (foreign == null) try exprs.rejectOutstandingMovedFields(ctx, &scope);

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
