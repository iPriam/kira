const std = @import("std");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");

pub const ImportedFunction = struct {
    name: []const u8,
    params: []const model.ResolvedType = &.{},
    param_ownership: []const model.OwnershipMode = &.{},
    return_type: model.ResolvedType = .{ .kind = .unknown },
    return_ownership: model.OwnershipMode = .owned,
    execution: runtime_abi.FunctionExecution = .inherited,
    is_extern: bool = false,
    foreign: ?model.ForeignFunction = null,
};

pub const ImportedType = struct {
    name: []const u8,
    kind: model.TypeKind = .struct_decl,
    execution: runtime_abi.FunctionExecution = .inherited,
    parents: []const []const u8 = &.{},
    fields: []const ImportedField = &.{},
    ffi: ?model.NamedTypeInfo = null,
};

pub const ImportedAnnotation = struct {
    name: []const u8,
    parameters: []const model.AnnotationParameterDecl = &.{},
    module_path: []const u8 = "",
    span: @import("kira_source").Span = .{ .start = 0, .end = 0 },
};

pub const ImportedField = struct {
    name: []const u8,
    storage: model.FieldStorage,
    ty: model.ResolvedType,
    default_value: ?*model.Expr = null,
};

pub const ImportedGlobals = struct {
    constructs: []const []const u8 = &.{},
    callables: []const []const u8 = &.{},
    functions: []const ImportedFunction = &.{},
    types: []const ImportedType = &.{},
    annotations: []const ImportedAnnotation = &.{},

    pub fn hasConstruct(self: ImportedGlobals, name: []const u8) bool {
        return contains(self.constructs, name);
    }

    pub fn hasCallable(self: ImportedGlobals, name: []const u8) bool {
        return contains(self.callables, name);
    }

    pub fn findFunction(self: ImportedGlobals, name: []const u8) ?ImportedFunction {
        for (self.functions) |function_decl| {
            if (std.mem.eql(u8, function_decl.name, name)) return function_decl;
        }
        return null;
    }

    pub fn findType(self: ImportedGlobals, name: []const u8) ?ImportedType {
        for (self.types) |type_decl| {
            if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
        }
        return null;
    }

    pub fn findAnnotation(self: ImportedGlobals, name: []const u8) ?ImportedAnnotation {
        for (self.annotations) |annotation_decl| {
            if (std.mem.eql(u8, annotation_decl.name, name)) return annotation_decl;
        }
        return null;
    }

    fn contains(values: []const []const u8, name: []const u8) bool {
        for (values) |value| {
            if (std.mem.eql(u8, value, name)) return true;
        }
        return false;
    }
};
