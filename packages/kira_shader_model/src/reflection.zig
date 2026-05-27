const std = @import("std");
const module = @import("module.zig");
const shader_types = @import("types.zig");

pub const BackendTarget = enum {
    glsl_330,
    wgsl,
    hlsl,
    msl,
    spirv,

    pub fn parse(value: []const u8) ?BackendTarget {
        if (std.mem.eql(u8, value, "glsl") or std.mem.eql(u8, value, "glsl_330") or std.mem.eql(u8, value, "glsl330")) return .glsl_330;
        if (std.mem.eql(u8, value, "wgsl")) return .wgsl;
        if (std.mem.eql(u8, value, "hlsl")) return .hlsl;
        if (std.mem.eql(u8, value, "msl") or std.mem.eql(u8, value, "metal") or std.mem.eql(u8, value, "mlsl")) return .msl;
        if (std.mem.eql(u8, value, "spirv") or std.mem.eql(u8, value, "spir-v") or std.mem.eql(u8, value, "spv")) return .spirv;
        return null;
    }

    pub fn label(self: BackendTarget) []const u8 {
        return @tagName(self);
    }
};

pub const BackendBinding = struct {
    target: BackendTarget,
    group_index: u32,
    binding_index: u32,
    glsl_name: ?[]const u8 = null,
};

pub const ReflectedOption = struct {
    name: []const u8,
    type_name: []const u8,
    default_value: []const u8,
};

pub const ReflectedField = struct {
    name: []const u8,
    type_name: []const u8,
    builtin: ?shader_types.Builtin = null,
    interpolation: ?shader_types.Interpolation = null,
    location: ?u32 = null,
};

pub const ReflectedLayoutField = struct {
    name: []const u8,
    offset: u32,
    alignment: u32,
    size: u32,
    stride: u32 = 0,
};

pub const ReflectedLayout = struct {
    class: []const u8,
    alignment: u32,
    size: u32,
    fields: []const ReflectedLayoutField,
};

pub const ReflectedType = struct {
    name: []const u8,
    fields: []const ReflectedField,
    uniform_layout: ?ReflectedLayout = null,
    storage_layout: ?ReflectedLayout = null,
};

pub const ReflectedStage = struct {
    stage: shader_types.Stage,
    entry_name: []const u8,
    input_type: ?[]const u8,
    output_type: ?[]const u8 = null,
    threads: ?[3]u32 = null,
    inputs: []const ReflectedField = &.{},
    outputs: []const ReflectedField = &.{},
};

pub const ReflectedResource = struct {
    group_name: []const u8,
    group_class: module.GroupClass,
    group_index: u32,
    resource_name: []const u8,
    resource_kind: module.ResourceKind,
    type_name: []const u8,
    visibility: []const shader_types.Stage,
    access: ?shader_types.AccessMode = null,
    backend_bindings: []const BackendBinding,
};

pub const Reflection = struct {
    shader_name: []const u8,
    shader_kind: module.ShaderKind,
    backend: BackendTarget,
    options: []const ReflectedOption = &.{},
    stages: []const ReflectedStage = &.{},
    types: []const ReflectedType = &.{},
    resources: []const ReflectedResource,
};
