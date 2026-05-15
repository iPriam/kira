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

pub const Context = struct {
    allocator: std.mem.Allocator,
    diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    imported_globals: ImportedGlobals = .{},
    annotation_headers: ?*const std.StringHashMapUnmanaged(AnnotationHeader) = null,
    construct_headers: ?*const std.StringHashMapUnmanaged(ConstructHeader) = null,
    type_headers: ?*const std.StringHashMapUnmanaged(TypeHeader) = null,
    function_headers: ?*const std.StringHashMapUnmanaged(FunctionHeader) = null,
    enum_headers: ?*const std.StringHashMapUnmanaged(model.EnumDecl) = null,
    concrete_enums: ?*std.StringHashMapUnmanaged(model.EnumDecl) = null,
    callback_capture_frame: ?*CallbackCaptureFrame = null,
};

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

pub const FunctionHeader = struct {
    id: u32,
    params: []const model.ResolvedType = &.{},
    param_ownership: []const model.OwnershipMode = &.{},
    execution: runtime_abi.FunctionExecution,
    return_type: model.ResolvedType,
    return_ownership: model.OwnershipMode = .owned,
    is_extern: bool = false,
    foreign: ?model.ForeignFunction = null,
    span: source_pkg.Span,
};

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
    return isAssignableClassValue(ctx, target, actual);
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
            try validateAnyConstructTarget(ctx, info.target.*, info.span);
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

fn validateAnyConstructTarget(ctx: *Context, target: syntax.ast.TypeExpr, span: source_pkg.Span) !void {
    const name = switch (target) {
        .named => |qualified| qualified.segments[qualified.segments.len - 1].text,
        else => {
            try emitAnyRequiresConstruct(ctx, span, "target is not a construct");
            return error.DiagnosticsEmitted;
        },
    };
    if (ctx.construct_headers) |headers| if (headers.contains(name)) return;
    if (ctx.imported_globals.hasConstruct(name)) return;
    if (isBuiltinTypeName(name) or isResolvedNonConstructSymbol(ctx, name)) {
        try emitAnyRequiresConstruct(ctx, span, "resolved target is not a construct");
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

fn emitAnyRequiresConstruct(ctx: *Context, span: source_pkg.Span, label: []const u8) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM097",
        .title = "any requires a construct",
        .message = "The `any` qualifier can only be applied to a construct name.",
        .labels = &.{diagnostics.primaryLabel(span, label)},
        .help = "Use `any ConstructName` with a declared construct, or remove `any` from non-construct types.",
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
    for (info.params) |param| try params.append(try typeFromSyntax(ctx, param.*));
    return function_types.signatureText(ctx.allocator, params.items, try typeFromSyntax(ctx, info.result.*));
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

fn exprSpan(expr: syntax.ast.Expr) source_pkg.Span {
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

pub const CaptureResolution = struct {
    binding: model.LocalBinding,
    captured: bool,
};

pub fn resolveLocalOrCapture(ctx: *Context, active_scope: model.Scope, name: []const u8, use_span: source_pkg.Span) !?CaptureResolution {
    if (active_scope.get(name)) |binding| return .{ .binding = binding, .captured = false };
    const frame = ctx.callback_capture_frame orelse return null;
    const outer = try resolveCaptureSource(ctx, frame, name, use_span) orelse return null;
    const captured = try captureBinding(ctx, frame, name, outer, use_span);
    return .{ .binding = captured, .captured = true };
}

fn resolveCaptureSource(ctx: *Context, frame: *CallbackCaptureFrame, name: []const u8, use_span: source_pkg.Span) !?model.LocalBinding {
    if (frame.source_scope.get(name)) |binding| return binding;
    const parent = frame.parent orelse return null;
    if (parent.active_scope.get(name)) |binding| return binding;
    const parent_outer = try resolveCaptureSource(ctx, parent, name, use_span) orelse return null;
    return try captureBinding(ctx, parent, name, parent_outer, use_span);
}

fn captureBinding(
    ctx: *Context,
    frame: *CallbackCaptureFrame,
    name: []const u8,
    outer: model.LocalBinding,
    use_span: source_pkg.Span,
) !model.LocalBinding {
    _ = use_span;
    if (frame.active_scope.get(name)) |binding| return binding;
    const by_ref = outer.storage != .immutable;

    const local_id = frame.next_local_id.*;
    frame.next_local_id.* += 1;
    const local_name = try ctx.allocator.dupe(u8, name);
    try frame.active_scope.put(ctx.allocator, local_name, .{
        .id = local_id,
        .ty = outer.ty,
        .storage = outer.storage,
        .ownership = outer.ownership,
        .initialized = true,
        .moved = outer.moved,
        .move_span = outer.move_span,
        .decl_span = outer.decl_span,
    });
    try frame.locals.append(.{
        .id = local_id,
        .name = local_name,
        .ty = outer.ty,
        .ownership = outer.ownership,
        .is_capture = true,
        .span = outer.decl_span,
    });
    try frame.captures.append(.{
        .local_id = local_id,
        .source_local_id = outer.id,
        .by_ref = by_ref,
        .name = local_name,
        .ty = outer.ty,
        .span = outer.decl_span,
    });
    return frame.active_scope.get(name).?;
}

pub fn emitUnsupportedMutableCapture(
    ctx: *Context,
    name: []const u8,
    use_span: source_pkg.Span,
    decl_span: source_pkg.Span,
) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM094",
        .title = "mutable callback capture is not supported",
        .message = try std.fmt.allocPrint(ctx.allocator, "The trailing callback captures mutable local '{s}', but mutable captures are not supported yet.", .{name}),
        .labels = &.{
            diagnostics.primaryLabel(use_span, "mutable local is captured here"),
            diagnostics.secondaryLabel(decl_span, "mutable local is declared here"),
        },
        .help = "Capture an immutable `let` value, or pass mutable state explicitly through a supported state object.",
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

pub fn isImportedRoot(name: []const u8, imports: []const model.Import) bool {
    for (imports) |import_decl| {
        if (import_decl.alias) |alias| {
            if (std.mem.eql(u8, alias, name)) return true;
        }
        if (std.mem.eql(u8, import_decl.module_name, name)) return true;
    }
    return false;
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
