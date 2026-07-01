const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");
const ImportedGlobals = @import("imported_globals.zig").ImportedGlobals;
const function_types = @import("function_types.zig");
const decls = @import("lower_shared_decls.zig");

pub const lowerAnnotationDecl = decls.lowerAnnotationDecl;
pub const lowerCapabilityDecl = decls.lowerCapabilityDecl;
pub const lowerGeneratedFunctions = decls.lowerGeneratedFunctions;
const ffi_annotations = @import("lower_shared_ffi_annotations.zig");
pub const resolveForeignFunction = ffi_annotations.resolveForeignFunction;
pub const resolveNamedTypeInfo = ffi_annotations.resolveNamedTypeInfo;
pub const CheckedAnnotationValue = ffi_annotations.CheckedAnnotationValue;
pub const annotationValueForParameter = ffi_annotations.annotationValueForParameter;
pub const annotationLiteralValue = ffi_annotations.annotationLiteralValue;
const captures = @import("lower_shared_captures.zig");
pub const CaptureResolution = captures.CaptureResolution;
pub const resolveLocalOrCapture = captures.resolveLocalOrCapture;
pub const emitUnsupportedMutableCapture = captures.emitUnsupportedMutableCapture;

pub const Context = struct {
    allocator: std.mem.Allocator,
    diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    imported_globals: ImportedGlobals = .{},
    annotation_headers: ?*const std.StringHashMapUnmanaged(AnnotationHeader) = null,
    construct_headers: ?*const std.StringHashMapUnmanaged(ConstructHeader) = null,
    type_headers: ?*const std.StringHashMapUnmanaged(TypeHeader) = null,
    function_headers: ?*const std.StringHashMapUnmanaged(FunctionHeader) = null,
    enum_headers: ?*const std.StringHashMapUnmanaged(model.EnumDecl) = null,
    // Maps a concrete construct-backed declaration's type name to the construct families it
    // satisfies (its family plus that family's `extends` ancestors), so a concrete widget value
    // coerces to `any Widget`. Populated before function bodies are lowered.
    form_families: ?*const std.StringHashMapUnmanaged([]const []const u8) = null,
    // Maps a declaration's type name to its `@Content` fields, so a trailing `{ ... }` block at a
    // construction site is routed into them (single `Widget` vs `[Widget]` arity, named fills).
    form_content_fields: ?*const std.StringHashMapUnmanaged([]const ContentFieldRef) = null,
    concrete_enums: ?*std.StringHashMapUnmanaged(model.EnumDecl) = null,
    callback_capture_frame: ?*CallbackCaptureFrame = null,
    active_locals: ?*std.array_list.Managed(model.LocalSymbol) = null,
    active_next_local_id: ?*u32 = null,
    current_package: ?[]const u8 = null,
    current_source_path: ?[]const u8 = null,
    /// When true, the active backend (the VM) can execute direct FFI calls from
    /// ordinary runtime functions through LibFFI, so the KSEM093 "@Native"
    /// requirement is lifted. Set per-target by the build pipeline.
    allow_runtime_direct_ffi: bool = false,
    /// Current depth of the recursive expression walkers (enum registration and
    /// type lowering). Bounded by `max_lower_depth` so a pathologically deep
    /// expression tree (e.g. a long flat `1 + 1 + 1 + ...` chain, which the parser
    /// builds iteratively and so escapes the parser's nesting guard) raises a clean
    /// diagnostic instead of overflowing the native stack.
    lower_depth: u32 = 0,
};

/// Maximum recursion depth of the semantic expression walkers. Kept well below
/// the observed native-stack overflow threshold (~900 frames for the type-lowering
/// walker on an 8 MiB stack) while remaining far above any realistic hand- or
/// machine-written expression.
pub const max_lower_depth: u32 = 512;

/// The span of an arbitrary expression node, used to locate depth-limit
/// diagnostics. Mirrors the per-variant `.span` accessor the parser exposes.
pub fn exprSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        inline else => |node| node.span,
    };
}

/// Enter one level of an expression walker (the caller must pair this with a
/// `defer ctx.lower_depth -= 1;`). Emits KSEM155 and returns
/// `error.DiagnosticsEmitted` once the nesting/chain depth exceeds
/// `max_lower_depth`, before the walker can overflow the native stack.
pub fn checkLoweringDepth(ctx: *Context, span: source_pkg.Span) !void {
    if (ctx.lower_depth <= max_lower_depth) return;
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM155",
        .title = "expression nests too deeply",
        .message = "This expression exceeds the maximum nesting/chain depth the compiler can lower without overflowing its stack.",
        .labels = &.{diagnostics.primaryLabel(span, "expression too deeply nested")},
        .help = "Split this into smaller subexpressions bound to intermediate `let` values.",
    });
    return error.DiagnosticsEmitted;
}

pub const CallbackCaptureFrame = struct {
    source_scope: *const model.Scope,
    active_scope: *model.Scope,
    captures: *std.array_list.Managed(model.Capture),
    locals: *std.array_list.Managed(model.LocalSymbol),
    next_local_id: *u32,
    parent: ?*CallbackCaptureFrame = null,
};

pub const AnnotationHeader = struct {
    index: ?usize = null,
    decl: model.AnnotationDecl,
    allows_block: bool = false,
    compiler_builtin: bool = false,
};

// A caller-provided `@Content` field of a declaration. `is_list` distinguishes `[Widget]`
// (ordered many) from a single `Widget`.
pub const ContentFieldRef = struct {
    name: []const u8,
    is_list: bool,
};

pub const FunctionHeader = struct {
    id: u32,
    params: []const model.ResolvedType = &.{},
    param_ownership: []const model.OwnershipMode = &.{},
    param_defaults: []const ?*syntax.ast.Expr = &.{},
    execution: runtime_abi.FunctionExecution,
    return_type: model.ResolvedType,
    return_ownership: model.OwnershipMode = .owned,
    is_extern: bool = false,
    is_comptime: bool = false,
    comptime_decl: ?syntax.ast.FunctionDecl = null,
    foreign: ?model.ForeignFunction = null,
    // A computed-property accessor synthesized from a `let name: T { ... }` member. Such a
    // method may be invoked by bare member access (`widget.node`, no parentheses), which is how
    // the Widget->Node bridge runs. Ordinary methods require an explicit call.
    is_accessor: bool = false,
    span: source_pkg.Span,
};

pub fn scopedSymbolName(allocator: std.mem.Allocator, package_name: []const u8, name: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}.{s}", .{ package_name, name });
}

pub fn findFunctionHeader(ctx: *const Context, headers: *const std.StringHashMapUnmanaged(FunctionHeader), name: []const u8) ?FunctionHeader {
    if (ctx.current_package) |package_name| {
        if (std.mem.indexOfScalar(u8, name, '.') == null) {
            const scoped = std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ package_name, name }) catch return null;
            if (headers.get(scoped)) |header| return header;
        }
    }
    return headers.get(name);
}

pub const ConstructHeader = struct {
    index: usize,
    span: source_pkg.Span,
};

pub const ParentView = struct {
    type_name: []const u8,
    offset: u32,
    span: source_pkg.Span,
};

pub const MethodMember = struct {
    name: []const u8,
    full_name: []const u8,
    receiver_type_name: []const u8,
    receiver_offset: u32,
    generated_by: ?[]const u8 = null,
    overridable: bool = true,
    params: []const model.ResolvedType = &.{},
    param_ownership: []const model.OwnershipMode = &.{},
    return_type: model.ResolvedType,
    return_ownership: model.OwnershipMode = .owned,
    span: source_pkg.Span,
};

pub const TypeHeader = struct {
    kind: model.TypeKind = .struct_decl,
    execution: runtime_abi.FunctionExecution = .inherited,
    fields: []const model.Field = &.{},
    methods: []const MethodMember = &.{},
    parent_views: []const ParentView = &.{},
    ffi: ?model.NamedTypeInfo = null,
    is_printable: bool = false,
    span: source_pkg.Span,
};

pub const AnnotationPlacement = enum {
    function_decl,
    class_decl,
    struct_decl,
    construct_decl,
    construct_form_decl,
    field_decl,
    content_section,
};

pub fn registerBuiltinAnnotationHeaders(
    allocator: std.mem.Allocator,
    headers: *std.StringHashMapUnmanaged(AnnotationHeader),
) !void {
    try putBuiltinAnnotation(allocator, headers, "Main", false);
    try putBuiltinAnnotation(allocator, headers, "Native", false);
    try putBuiltinAnnotation(allocator, headers, "Runtime", false);
    try putBuiltinAnnotation(allocator, headers, "Printable", false);
    // SwiftUI-style construct surface: `@Required` marks required construct members; `@Content`
    // marks caller-provided child fields on concrete declarations.
    try putBuiltinAnnotation(allocator, headers, "Required", false);
    try putBuiltinAnnotation(allocator, headers, "Content", false);
    try putBuiltinAnnotation(allocator, headers, "FFI.Extern", true);
    try putBuiltinAnnotation(allocator, headers, "FFI.Struct", true);
    try putBuiltinAnnotation(allocator, headers, "FFI.Pointer", true);
    try putBuiltinAnnotation(allocator, headers, "FFI.Alias", true);
    try putBuiltinAnnotation(allocator, headers, "FFI.Array", true);
    try putBuiltinAnnotation(allocator, headers, "FFI.Callback", true);
}

fn putBuiltinAnnotation(
    allocator: std.mem.Allocator,
    headers: *std.StringHashMapUnmanaged(AnnotationHeader),
    name: []const u8,
    allows_block: bool,
) !void {
    try headers.put(allocator, name, .{
        .decl = .{
            .name = name,
            .parameters = &.{},
            .module_path = "kira.compiler",
            .span = .{ .start = 0, .end = 0 },
        },
        .allows_block = allows_block,
        .compiler_builtin = true,
    });
}

pub fn qualifiedNameText(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName) ![]const u8 {
    var builder = std.array_list.Managed(u8).init(allocator);
    for (name.segments, 0..) |segment, index| {
        if (index != 0) try builder.append('.');
        try builder.appendSlice(segment.text);
    }
    return builder.toOwnedSlice();
}

pub fn qualifiedNameLeaf(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName) ![]const u8 {
    return allocator.dupe(u8, name.segments[name.segments.len - 1].text);
}

/// Maps a primitive numeric type name used as a cast call target — `Int(x)`,
/// `Float(x)`, and every width-specific form (`U64(x)`, `I32(x)`, `F32(x)`, …) —
/// to its resolved target type, or null when `name` is not a numeric cast.
///
/// Width is carried in `.name`; the runtime representation of every integer
/// (and of every float) is identical, so a same-kind cast lowers to the
/// existing int↔float `convert` instruction as an identity copy — no new
/// opcode or backend support is required. This is the single source of truth
/// for which names are numeric casts, so the full set stays consistent with the
/// width types `typeFromSyntax` accepts below.
pub fn numericCastTargetType(name: []const u8) ?model.ResolvedType {
    if (std.mem.eql(u8, name, "Int")) return .{ .kind = .integer };
    if (std.mem.eql(u8, name, "Float")) return .{ .kind = .float };
    const integer_names = [_][]const u8{ "I8", "U8", "I16", "U16", "I32", "U32", "I64", "U64" };
    for (integer_names) |integer_name| {
        if (std.mem.eql(u8, name, integer_name)) return .{ .kind = .integer, .name = name };
    }
    if (std.mem.eql(u8, name, "F32") or std.mem.eql(u8, name, "F64")) {
        return .{ .kind = .float, .name = name };
    }
    return null;
}

pub fn typeFromSyntax(ctx: *const Context, ty: syntax.ast.TypeExpr) anyerror!model.ResolvedType {
    return switch (ty) {
        .array => |info| .{ .kind = .array, .name = try typeTextFromSyntax(ctx, info.element_type.*) },
        .function => |info| .{ .kind = .callback, .name = try functionTypeTextFromSyntax(ctx, info) },
        .ownership => |info| try typeFromSyntax(ctx, info.target.*),
        .any => |info| switch (info.target.*) {
            .named => |name| .{
                .kind = .construct_any,
                .name = try typeTextFromSyntax(ctx, .{ .any = info }),
                .construct_constraint = .{ .construct_name = try ctx.allocator.dupe(u8, name.segments[name.segments.len - 1].text) },
            },
            else => .{ .kind = .construct_any, .name = try typeTextFromSyntax(ctx, .{ .any = info }) },
        },
        .named => |name| blk: {
            const leaf = name.segments[name.segments.len - 1].text;
            if (std.mem.eql(u8, leaf, "Int")) break :blk .{ .kind = .integer };
            if (std.mem.eql(u8, leaf, "Float")) break :blk .{ .kind = .float };
            if (std.mem.eql(u8, leaf, "Bool")) break :blk .{ .kind = .boolean };
            if (std.mem.eql(u8, leaf, "String")) break :blk .{ .kind = .string };
            if (std.mem.eql(u8, leaf, "Void")) break :blk .{ .kind = .void };
            if (std.mem.eql(u8, leaf, "I8") or
                std.mem.eql(u8, leaf, "U8") or
                std.mem.eql(u8, leaf, "I16") or
                std.mem.eql(u8, leaf, "U16") or
                std.mem.eql(u8, leaf, "I32") or
                std.mem.eql(u8, leaf, "U32") or
                std.mem.eql(u8, leaf, "I64") or
                std.mem.eql(u8, leaf, "U64"))
            {
                break :blk .{ .kind = .integer, .name = leaf };
            }
            if (std.mem.eql(u8, leaf, "F32") or std.mem.eql(u8, leaf, "F64")) {
                break :blk .{ .kind = .float, .name = leaf };
            }
            if (std.mem.eql(u8, leaf, "CBool")) break :blk .{ .kind = .boolean, .name = leaf };
            if (std.mem.eql(u8, leaf, "CString")) break :blk .{ .kind = .c_string, .name = leaf };
            if (std.mem.eql(u8, leaf, "RawPtr")) break :blk .{ .kind = .raw_ptr, .name = leaf };
            if (ctx.enum_headers) |headers| {
                if (headers.get(leaf)) |enum_decl| {
                    if (enum_decl.type_params.len == 0) break :blk .{ .kind = .enum_instance, .name = leaf };
                }
            }
            if (ctx.construct_headers) |headers| {
                if (headers.get(leaf) != null) {
                    break :blk .{
                        .kind = .construct_any,
                        .name = try std.fmt.allocPrint(ctx.allocator, "any {s}", .{leaf}),
                        .construct_constraint = .{ .construct_name = try ctx.allocator.dupe(u8, leaf) },
                    };
                }
            }
            break :blk .{ .kind = .named, .name = leaf };
        },
        .generic => |info| .{
            .kind = .enum_instance,
            .name = try genericTypeTextFromSyntax(ctx, info),
        },
    };
}

pub fn typeTextFromSyntax(ctx: *const Context, ty: syntax.ast.TypeExpr) anyerror![]const u8 {
    return switch (ty) {
        .array => |info| std.fmt.allocPrint(ctx.allocator, "[{s}]", .{try typeTextFromSyntax(ctx, info.element_type.*)}),
        .function => |info| functionTypeTextFromSyntax(ctx, info),
        .ownership => |info| switch (info.mode) {
            .borrow_read => std.fmt.allocPrint(ctx.allocator, "borrow {s}", .{try typeTextFromSyntax(ctx, info.target.*)}),
            .borrow_mut => std.fmt.allocPrint(ctx.allocator, "borrow mut {s}", .{try typeTextFromSyntax(ctx, info.target.*)}),
            .move => std.fmt.allocPrint(ctx.allocator, "move {s}", .{try typeTextFromSyntax(ctx, info.target.*)}),
            .copy => std.fmt.allocPrint(ctx.allocator, "copy {s}", .{try typeTextFromSyntax(ctx, info.target.*)}),
            .owned => typeTextFromSyntax(ctx, info.target.*),
        },
        // Keyword-independent by design: `some Target` and `any Target` are the same existential
        // `construct_any` today, and this text feeds resolved type NAMES used for coercion/dispatch
        // matching. The surface keyword is surfaced only in display paths (ast_dump, diagnostics).
        .any => |info| std.fmt.allocPrint(ctx.allocator, "any {s}", .{try typeTextFromSyntax(ctx, info.target.*)}),
        .named => |name| ctx.allocator.dupe(u8, name.segments[name.segments.len - 1].text),
        .generic => |info| genericTypeTextFromSyntax(ctx, info),
    };
}

pub fn typeTextFromResolved(allocator: std.mem.Allocator, ty: model.ResolvedType) ![]const u8 {
    return switch (ty.kind) {
        .void => allocator.dupe(u8, "Void"),
        .integer => allocator.dupe(u8, ty.name orelse "Int"),
        .float => allocator.dupe(u8, ty.name orelse "Float"),
        .boolean => allocator.dupe(u8, ty.name orelse "Bool"),
        .string => allocator.dupe(u8, "String"),
        .c_string => allocator.dupe(u8, ty.name orelse "CString"),
        .raw_ptr => allocator.dupe(u8, ty.name orelse "RawPtr"),
        .construct_any => if (ty.name) |name| allocator.dupe(u8, name) else std.fmt.allocPrint(allocator, "any {s}", .{(ty.construct_constraint orelse return allocator.dupe(u8, "any Unknown")).construct_name}),
        .native_state => std.fmt.allocPrint(allocator, "NativeState<{s}>", .{ty.name orelse "Unknown"}),
        .native_state_view => std.fmt.allocPrint(allocator, "NativeStateView<{s}>", .{ty.name orelse "Unknown"}),
        .callback, .ffi_struct, .named, .enum_instance => allocator.dupe(u8, ty.name orelse "Unknown"),
        .array => std.fmt.allocPrint(allocator, "[{s}]", .{ty.name orelse ""}),
        .unknown => allocator.dupe(u8, "Unknown"),
    };
}

pub fn canAssign(target: model.ResolvedType, actual: model.ResolvedType) bool {
    if (target.eql(actual)) return true;
    if (target.kind == .array or actual.kind == .array) return false;
    return target.kind == .float and actual.kind == .integer;
}

pub fn canAssignExactly(target: model.ResolvedType, actual: model.ResolvedType) bool {
    return target.eql(actual);
}

pub fn canAssignInContext(ctx: *const Context, target: model.ResolvedType, actual: model.ResolvedType) bool {
    if (canAssign(target, actual)) return true;
    if (sameEnumIdentity(ctx, target, actual)) return true;
    if (isConstructFamilyCoercion(ctx, target, actual)) return true;
    return isAssignableClassValue(ctx, target, actual);
}

// A concrete construct-backed declaration value (`Text`) coerces to `any Family` when its family
// (or one of that family's `extends` ancestors) is the constraint. `any Family` also coerces to
// the same `any Family`.
pub fn isConstructFamilyCoercion(ctx: *const Context, target: model.ResolvedType, actual: model.ResolvedType) bool {
    if (target.kind != .construct_any) return false;
    const constraint = (target.construct_constraint orelse return false).construct_name;
    if (actual.kind == .construct_any) {
        const actual_constraint = (actual.construct_constraint orelse return false).construct_name;
        return std.mem.eql(u8, constraint, actual_constraint);
    }
    const actual_name = actual.name orelse return false;
    const families = ctx.form_families orelse return false;
    const list = families.get(actual_name) orelse return false;
    for (list) |family| {
        if (std.mem.eql(u8, family, constraint)) return true;
    }
    return false;
}

fn sameEnumIdentity(ctx: *const Context, target: model.ResolvedType, actual: model.ResolvedType) bool {
    if (!((target.kind == .enum_instance and actual.kind == .named) or (target.kind == .named and actual.kind == .enum_instance))) return false;
    if (target.name == null or actual.name == null) return false;
    if (!std.mem.eql(u8, target.name.?, actual.name.?)) return false;
    if (ctx.enum_headers) |headers| if (headers.get(target.name.?) != null) return true;
    if (ctx.concrete_enums) |enums| if (enums.get(target.name.?) != null) return true;
    return false;
}

pub fn emitTypeMismatch(
    allocator: std.mem.Allocator,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    span: source_pkg.Span,
    target: model.ResolvedType,
    actual: model.ResolvedType,
) !void {
    try diagnostics.appendOwned(allocator, out_diagnostics, .{
        .severity = .@"error",
        .code = "KSEM031",
        .title = "type mismatch",
        .message = try std.fmt.allocPrint(allocator, "Kira expected {s} here, but the value resolves to {s}.", .{ typeLabel(target), typeLabel(actual) }),
        .labels = &.{
            diagnostics.primaryLabel(span, "value does not match the required type"),
        },
        .help = "Add an explicit type declaration where coercion is allowed, or change the value so the type is unambiguous.",
    });
}

pub fn typeLabel(ty: model.ResolvedType) []const u8 {
    if (ty.kind == .construct_any) return ty.name orelse "any Unknown";
    if (ty.kind == .array) return ty.name orelse "[]";
    if (ty.name) |name| return name;
    return switch (ty.kind) {
        .void => "Void",
        .integer => "Int",
        .float => "Float",
        .boolean => "Bool",
        .string => "String",
        .c_string => "CString",
        .raw_ptr => "RawPtr",
        .construct_any => "any Unknown",
        .native_state => "NativeState",
        .native_state_view => "NativeStateView",
        .callback, .ffi_struct, .named, .enum_instance => "Unknown",
        .array => "[]",
        .unknown => "Unknown",
    };
}

pub fn typeFromSyntaxChecked(ctx: *Context, ty: syntax.ast.TypeExpr) anyerror!model.ResolvedType {
    const resolved = try typeFromSyntax(ctx, ty);
    const semantic_ty = stripOwnershipType(ty);
    if (semantic_ty == .generic) {
        const base_name = semantic_ty.generic.base.segments[semantic_ty.generic.base.segments.len - 1].text;
        if (ctx.enum_headers == null or ctx.enum_headers.?.get(base_name) == null) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM031",
                .title = "type mismatch",
                .message = "Generic type syntax currently requires a declared enum base.",
                .labels = &.{diagnostics.primaryLabel(semantic_ty.generic.span, "generic type base could not be resolved as an enum")},
                .help = "Declare the enum first and use its generic type parameters in type positions only.",
            });
            return error.DiagnosticsEmitted;
        }
    }
    try validateAnyConstructType(ctx, ty);
    return resolved;
}

pub fn validateAnyConstructType(ctx: *Context, ty: syntax.ast.TypeExpr) !void {
    switch (ty) {
        .ownership => |info| try validateAnyConstructType(ctx, info.target.*),
        .any => |info| {
            try validateAnyConstructTarget(ctx, info.target.*, info.span, info.existential);
            try validateAnyConstructType(ctx, info.target.*);
        },
        .array => |info| try validateAnyConstructType(ctx, info.element_type.*),
        .function => |info| {
            for (info.params) |param| try validateAnyConstructType(ctx, param.*);
            try validateAnyConstructType(ctx, info.result.*);
        },
        .named, .generic => {},
    }
}

pub fn ownershipModeFromSyntax(ty: ?*syntax.ast.TypeExpr) model.OwnershipMode {
    const resolved = ty orelse return .owned;
    return switch (resolved.*) {
        .ownership => |info| ownershipModeFromSyntaxMode(info.mode),
        else => .owned,
    };
}

fn ownershipModeFromSyntaxMode(mode: syntax.ast.OwnershipMode) model.OwnershipMode {
    return switch (mode) {
        .owned => .owned,
        .borrow_read => .borrow_read,
        .borrow_mut => .borrow_mut,
        .move => .move,
        .copy => .copy,
    };
}

pub fn stripOwnershipType(ty: syntax.ast.TypeExpr) syntax.ast.TypeExpr {
    return switch (ty) {
        .ownership => |info| stripOwnershipType(info.target.*),
        else => ty,
    };
}

pub fn paramOwnership(header: FunctionHeader, index: usize) model.OwnershipMode {
    if (index < header.param_ownership.len) return header.param_ownership[index];
    return .owned;
}

fn validateAnyConstructTarget(ctx: *Context, target: syntax.ast.TypeExpr, span: source_pkg.Span, existential: bool) !void {
    const name = switch (target) {
        .named => |qualified| qualified.segments[qualified.segments.len - 1].text,
        else => {
            try emitAnyRequiresConstruct(ctx, span, "target is not a construct", existential);
            return error.DiagnosticsEmitted;
        },
    };
    if (ctx.construct_headers) |headers| if (headers.contains(name)) return;
    if (ctx.imported_globals.hasConstruct(name)) return;
    if (isBuiltinTypeName(name) or isResolvedNonConstructSymbol(ctx, name)) {
        try emitAnyRequiresConstruct(ctx, span, "resolved target is not a construct", existential);
        return error.DiagnosticsEmitted;
    }
}

fn isResolvedNonConstructSymbol(ctx: *const Context, name: []const u8) bool {
    if (ctx.type_headers) |headers| if (headers.contains(name)) return true;
    if (ctx.imported_globals.findType(name) != null) return true;
    if (ctx.function_headers) |headers| if (headers.contains(name)) return true;
    if (ctx.imported_globals.findFunction(name) != null) return true;
    if (ctx.annotation_headers) |headers| if (headers.contains(name)) return true;
    return false;
}

fn isBuiltinTypeName(name: []const u8) bool {
    return std.mem.eql(u8, name, "Int") or std.mem.eql(u8, name, "Float") or
        std.mem.eql(u8, name, "Bool") or std.mem.eql(u8, name, "String") or
        std.mem.eql(u8, name, "Void") or std.mem.eql(u8, name, "I8") or
        std.mem.eql(u8, name, "U8") or std.mem.eql(u8, name, "I16") or
        std.mem.eql(u8, name, "U16") or std.mem.eql(u8, name, "I32") or
        std.mem.eql(u8, name, "U32") or std.mem.eql(u8, name, "I64") or
        std.mem.eql(u8, name, "U64") or std.mem.eql(u8, name, "F32") or
        std.mem.eql(u8, name, "F64") or std.mem.eql(u8, name, "CBool") or
        std.mem.eql(u8, name, "CString") or std.mem.eql(u8, name, "RawPtr");
}

fn emitAnyRequiresConstruct(ctx: *Context, span: source_pkg.Span, label: []const u8, existential: bool) !void {
    const keyword = if (existential) "some" else "any";
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM097",
        .title = try std.fmt.allocPrint(ctx.allocator, "{s} requires a construct", .{keyword}),
        .message = try std.fmt.allocPrint(ctx.allocator, "The `{s}` qualifier can only be applied to a construct name.", .{keyword}),
        .labels = &.{diagnostics.primaryLabel(span, label)},
        .help = try std.fmt.allocPrint(ctx.allocator, "Use `{s} ConstructName` with a declared construct, or remove `{s}` from non-construct types.", .{ keyword, keyword }),
    });
}

pub fn namedTypeInfo(ctx: *const Context, ty: model.ResolvedType) ?model.NamedTypeInfo {
    if (ty.kind != .named or ty.name == null) return null;
    if (ctx.type_headers) |headers| {
        if (headers.get(ty.name.?)) |header| return header.ffi;
    }
    if (ctx.imported_globals.findType(ty.name.?)) |type_decl| return type_decl.ffi;
    return null;
}

pub fn namedTypeHeader(ctx: *const Context, ty: model.ResolvedType) ?TypeHeader {
    if (ty.kind != .named or ty.name == null) return null;
    if (ctx.type_headers) |headers| {
        if (headers.get(ty.name.?)) |header| return header;
    }
    return null;
}

pub fn namedTypeKind(ctx: *const Context, ty: model.ResolvedType) ?model.TypeKind {
    if (ty.kind != .named or ty.name == null) return null;
    if (ctx.type_headers) |headers| {
        if (headers.get(ty.name.?)) |header| return header.kind;
    }
    if (ctx.imported_globals.findType(ty.name.?)) |type_decl| return type_decl.kind;
    return null;
}

pub fn isClassType(ctx: *const Context, ty: model.ResolvedType) bool {
    return namedTypeKind(ctx, ty) == .class;
}

pub fn hasKnownSubclass(ctx: *const Context, type_name: []const u8) bool {
    if (ctx.type_headers) |headers| {
        var iterator = headers.iterator();
        while (iterator.next()) |entry| {
            if (entry.value_ptr.kind != .class) continue;
            if (std.mem.eql(u8, entry.key_ptr.*, type_name)) continue;
            for (entry.value_ptr.parent_views) |parent_view| {
                if (std.mem.eql(u8, parent_view.type_name, type_name)) return true;
            }
        }
    }
    for (ctx.imported_globals.types) |type_decl| {
        if (type_decl.kind != .class) continue;
        if (std.mem.eql(u8, type_decl.name, type_name)) continue;
        if (classNameMatchesOrInherits(ctx, type_decl.name, type_name)) return true;
    }
    return false;
}

pub fn isAssignableClassValue(ctx: *const Context, target: model.ResolvedType, actual: model.ResolvedType) bool {
    if (target.kind != .named or actual.kind != .named) return false;
    if (!isClassType(ctx, target) or !isClassType(ctx, actual)) return false;
    const target_name = target.name orelse return false;
    const actual_name = actual.name orelse return false;
    return classNameMatchesOrInherits(ctx, actual_name, target_name);
}

// The least common `any Family` of two construct-backed declaration values (or of an `any Family`
// and a declaration), so a heterogeneous widget array literal (`[Text(...), Spacer()]`) unifies to
// `[any Widget]`. Returns null when the values share no construct family.
pub fn commonConstructAnyType(
    allocator: std.mem.Allocator,
    ctx: *const Context,
    lhs: model.ResolvedType,
    rhs: model.ResolvedType,
) ?model.ResolvedType {
    const lhs_families = familyList(allocator, ctx, lhs) orelse return null;
    const rhs_families = familyList(allocator, ctx, rhs) orelse return null;
    for (lhs_families) |candidate| {
        for (rhs_families) |other| {
            if (std.mem.eql(u8, candidate, other)) return constructAnyType(allocator, candidate) catch null;
        }
    }
    return null;
}

// The construct families a value satisfies: a concrete declaration's recorded families, or the
// single constraint of an `any Family` value.
fn familyList(allocator: std.mem.Allocator, ctx: *const Context, ty: model.ResolvedType) ?[]const []const u8 {
    if (ty.kind == .construct_any) {
        const constraint = (ty.construct_constraint orelse return null).construct_name;
        const single = allocator.alloc([]const u8, 1) catch return null;
        single[0] = constraint;
        return single;
    }
    const name = ty.name orelse return null;
    const families = ctx.form_families orelse return null;
    return families.get(name);
}

fn constructAnyType(allocator: std.mem.Allocator, family: []const u8) !model.ResolvedType {
    return .{
        .kind = .construct_any,
        .name = try std.fmt.allocPrint(allocator, "any {s}", .{family}),
        .construct_constraint = .{ .construct_name = family },
    };
}

pub fn commonClassType(
    ctx: *const Context,
    lhs: model.ResolvedType,
    rhs: model.ResolvedType,
) ?model.ResolvedType {
    if (lhs.kind != .named or rhs.kind != .named) return null;
    if (!isClassType(ctx, lhs) or !isClassType(ctx, rhs)) return null;
    const lhs_name = lhs.name orelse return null;
    const rhs_name = rhs.name orelse return null;

    if (classNameMatchesOrInherits(ctx, rhs_name, lhs_name)) return lhs;
    if (classNameMatchesOrInherits(ctx, lhs_name, rhs_name)) return rhs;

    if (ctx.type_headers) |headers| {
        if (headers.get(lhs_name)) |header| {
            for (header.parent_views) |parent_view| {
                if (classNameMatchesOrInherits(ctx, rhs_name, parent_view.type_name)) {
                    return .{ .kind = .named, .name = parent_view.type_name };
                }
            }
        }
    }

    var current = ctx.imported_globals.findType(lhs_name);
    while (current) |type_decl| {
        for (type_decl.parents) |parent_name| {
            if (classNameMatchesOrInherits(ctx, rhs_name, parent_name)) {
                return .{ .kind = .named, .name = parent_name };
            }
        }
        if (type_decl.parents.len == 0) break;
        current = ctx.imported_globals.findType(type_decl.parents[0]);
    }

    return null;
}

fn classNameMatchesOrInherits(ctx: *const Context, actual_name: []const u8, target_name: []const u8) bool {
    if (std.mem.eql(u8, actual_name, target_name)) return true;
    if (ctx.type_headers) |headers| {
        if (headers.get(actual_name)) |header| {
            if (header.kind != .class) return false;
            for (header.parent_views) |parent_view| {
                if (std.mem.eql(u8, parent_view.type_name, target_name)) return true;
            }
            return false;
        }
    }
    if (ctx.imported_globals.findType(actual_name)) |type_decl| {
        if (type_decl.kind != .class) return false;
        for (type_decl.parents) |parent_name| {
            if (std.mem.eql(u8, parent_name, target_name)) return true;
            if (classNameMatchesOrInherits(ctx, parent_name, target_name)) return true;
        }
    }
    return false;
}

pub fn namedTypeFields(ctx: *const Context, ty: model.ResolvedType) []const model.Field {
    if (namedTypeHeader(ctx, ty)) |header| return header.fields;
    return &.{};
}

pub fn isPointerLike(ctx: *const Context, ty: model.ResolvedType) bool {
    return switch (ty.kind) {
        .raw_ptr, .c_string => true,
        .named => if (namedTypeInfo(ctx, ty)) |info|
            switch (info) {
                .pointer, .callback => true,
                .alias => |value| isPointerLike(ctx, value.target),
                .ffi_struct, .array => false,
            }
        else
            false,
        else => false,
    };
}

pub fn callbackInfo(ctx: *const Context, ty: model.ResolvedType) ?model.CallbackInfo {
    if (ty.kind != .named) return null;
    return if (namedTypeInfo(ctx, ty)) |info|
        switch (info) {
            .callback => |value| value,
            .alias => |value| callbackInfo(ctx, value.target),
            else => null,
        }
    else
        null;
}

pub fn resolvedTypeFromText(text: []const u8) !model.ResolvedType {
    if (std.mem.startsWith(u8, text, "any ")) {
        return .{
            .kind = .construct_any,
            .name = text,
            .construct_constraint = .{ .construct_name = text[4..] },
        };
    }
    if (text.len >= 4 and text[0] == '(' and std.mem.indexOf(u8, text, "->") != null) {
        return .{ .kind = .callback, .name = text };
    }
    if (text.len >= 2 and text[0] == '[' and text[text.len - 1] == ']') {
        return .{ .kind = .array, .name = text[1 .. text.len - 1] };
    }
    if (std.mem.eql(u8, text, "Void")) return .{ .kind = .void };
    if (std.mem.eql(u8, text, "Int")) return .{ .kind = .integer };
    if (std.mem.eql(u8, text, "Float")) return .{ .kind = .float };
    if (std.mem.eql(u8, text, "Bool")) return .{ .kind = .boolean };
    if (std.mem.eql(u8, text, "String")) return .{ .kind = .string };
    if (std.mem.eql(u8, text, "CString")) return .{ .kind = .c_string, .name = text };
    if (std.mem.eql(u8, text, "RawPtr")) return .{ .kind = .raw_ptr, .name = text };
    if (std.mem.eql(u8, text, "I8") or
        std.mem.eql(u8, text, "U8") or
        std.mem.eql(u8, text, "I16") or
        std.mem.eql(u8, text, "U16") or
        std.mem.eql(u8, text, "I32") or
        std.mem.eql(u8, text, "U32") or
        std.mem.eql(u8, text, "I64") or
        std.mem.eql(u8, text, "U64"))
    {
        return .{ .kind = .integer, .name = text };
    }
    if (std.mem.eql(u8, text, "F32") or std.mem.eql(u8, text, "F64")) return .{ .kind = .float, .name = text };
    if (std.mem.eql(u8, text, "CBool")) return .{ .kind = .boolean, .name = text };
    return .{ .kind = .named, .name = text };
}

fn functionTypeTextFromSyntax(ctx: *const Context, info: syntax.ast.FunctionTypeExpr) anyerror![]const u8 {
    var params = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
    var param_ownership = std.array_list.Managed(model.OwnershipMode).init(ctx.allocator);
    for (info.params) |param| {
        try params.append(try typeFromSyntax(ctx, stripOwnershipType(param.*)));
        try param_ownership.append(ownershipModeFromSyntax(param));
    }
    return function_types.signatureText(
        ctx.allocator,
        params.items,
        param_ownership.items,
        try typeFromSyntax(ctx, stripOwnershipType(info.result.*)),
    );
}

fn genericTypeTextFromSyntax(ctx: *const Context, info: syntax.ast.GenericTypeExpr) ![]const u8 {
    const base_name = info.base.segments[info.base.segments.len - 1].text;
    var text = std.array_list.Managed(u8).init(ctx.allocator);
    try text.appendSlice(base_name);
    for (info.args) |arg| {
        try text.appendSlice("__");
        const arg_text = try typeTextFromSyntax(ctx, arg.*);
        for (arg_text) |byte| {
            if (std.ascii.isAlphanumeric(byte)) {
                try text.append(byte);
            } else {
                try text.append('_');
            }
        }
    }
    return text.toOwnedSlice();
}

pub fn emitAmbiguousInference(
    allocator: std.mem.Allocator,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    span: source_pkg.Span,
) !void {
    try diagnostics.appendOwned(allocator, out_diagnostics, .{
        .severity = .@"error",
        .code = "KSEM029",
        .title = "type inference is ambiguous",
        .message = "Kira cannot infer a type here because no explicit type or value was provided.",
        .labels = &.{
            diagnostics.primaryLabel(span, "type is ambiguous here"),
        },
        .help = "Add an explicit type annotation.",
    });
}

pub fn registerTopLevelName(
    allocator: std.mem.Allocator,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    map: *std.StringHashMapUnmanaged(source_pkg.Span),
    name: []const u8,
    span: source_pkg.Span,
) !void {
    if (map.get(name)) |previous_span| {
        try diagnostics.appendOwned(allocator, out_diagnostics, .{
            .severity = .@"error",
            .code = "KSEM003",
            .title = "duplicate top-level name",
            .message = try std.fmt.allocPrint(allocator, "Kira found more than one top-level declaration named '{s}'.", .{name}),
            .labels = &.{
                diagnostics.primaryLabel(span, "duplicate declaration"),
                diagnostics.secondaryLabel(previous_span, "first declaration was here"),
            },
            .help = "Rename one of the declarations so the symbol is unambiguous.",
        });
        return error.DiagnosticsEmitted;
    }
    try map.put(allocator, name, span);
}

pub fn containsAnnotationRule(rules: []const model.AnnotationRule, name: []const u8) bool {
    for (rules) |rule| if (std.mem.eql(u8, rule.name, name)) return true;
    return false;
}

pub fn containsString(values: [][]const u8, name: []const u8) bool {
    for (values) |value| if (std.mem.eql(u8, value, name)) return true;
    return false;
}

pub fn isImportedRoot(ctx: *const Context, name: []const u8, imports: []const model.Import) bool {
    for (imports) |import_decl| {
        if (!importVisibleToContext(ctx, import_decl)) continue;
        if (import_decl.alias) |alias| {
            if (std.mem.eql(u8, alias, name)) return true;
        }
        if (std.mem.eql(u8, import_decl.module_name, name)) return true;
    }
    return false;
}

pub fn importedQualifiedName(ctx: *const Context, imports: []const model.Import, name: []const u8) ?[]const u8 {
    const root_end = std.mem.indexOfScalar(u8, name, '.') orelse return null;
    const root = name[0..root_end];
    const member = name[root_end + 1 ..];
    for (imports) |import_decl| {
        if (!importVisibleToContext(ctx, import_decl)) continue;
        if (import_decl.alias) |alias| {
            if (std.mem.eql(u8, alias, root)) {
                return std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ import_decl.module_name, member }) catch null;
            }
        }
        if (std.mem.eql(u8, import_decl.module_name, root)) {
            return std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ import_decl.module_name, member }) catch null;
        }
    }
    return null;
}

fn importVisibleToContext(ctx: *const Context, import_decl: model.Import) bool {
    if (import_decl.source_path.len != 0) {
        if (ctx.current_source_path == null) return false;
        if (!std.mem.eql(u8, import_decl.source_path, ctx.current_source_path.?)) return false;
    }
    if (import_decl.package_name) |package_name| {
        if (ctx.current_package == null) return false;
        return std.mem.eql(u8, package_name, ctx.current_package.?);
    }
    return ctx.current_package == null;
}

pub fn resolveAnnotationHeader(ctx: *Context, name: syntax.ast.QualifiedName) !AnnotationHeader {
    const full_name = try qualifiedNameText(ctx.allocator, name);
    const leaf = name.segments[name.segments.len - 1].text;
    if (ctx.annotation_headers) |headers| {
        if (headers.get(full_name)) |header| return header;
        if (headers.get(leaf)) |header| return header;
    }
    if (ctx.imported_globals.findAnnotation(full_name)) |annotation_decl| {
        return .{
            .decl = .{
                .name = annotation_decl.name,
                .parameters = @constCast(annotation_decl.parameters),
                .module_path = annotation_decl.module_path,
                .span = annotation_decl.span,
            },
        };
    }
    if (ctx.imported_globals.findAnnotation(leaf)) |annotation_decl| {
        return .{
            .decl = .{
                .name = annotation_decl.name,
                .parameters = @constCast(annotation_decl.parameters),
                .module_path = annotation_decl.module_path,
                .span = annotation_decl.span,
            },
        };
    }

    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM063",
        .title = "unknown annotation",
        .message = try std.fmt.allocPrint(ctx.allocator, "unknown annotation @{s}", .{full_name}),
        .labels = &.{diagnostics.primaryLabel(name.span, "annotation has not been declared")},
        .help = "Declare the annotation with `annotation Name { }` or import the module that declares it.",
    });
    return error.DiagnosticsEmitted;
}

const annotation_impl = @import("lower_shared_annotations.zig");
pub const lowerAnnotation = annotation_impl.lowerAnnotation;
pub const validateAnnotationUse = annotation_impl.validateAnnotationUse;
pub const resolveFunctionAnnotations = annotation_impl.resolveFunctionAnnotations;
pub const lowerAnnotations = annotation_impl.lowerAnnotations;
pub const validateAnnotationPlacement = annotation_impl.validateAnnotationPlacement;
