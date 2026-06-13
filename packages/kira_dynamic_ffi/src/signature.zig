const std = @import("std");

pub const Abi = enum {
    c,
    system,
    win64,
    sysv,
    unix64,
    aarch64,

    pub fn platformDefault(target: std.Target) Abi {
        return switch (target.os.tag) {
            .windows => .win64,
            .macos, .ios, .tvos, .watchos, .visionos => .aarch64,
            else => .system,
        };
    }
};

pub const Ownership = enum {
    borrowed,
    owned_by_caller,
    owned_by_callee,
    retained,
};

pub const Type = union(enum) {
    void,
    bool,
    i8,
    u8,
    i16,
    u16,
    i32,
    u32,
    i64,
    u64,
    f32,
    f64,
    pointer: Pointer,
    handle: Handle,
    enumeration: Enum,
    bitflags: Bitflags,
    structure: Struct,
    union_: Union,
    array: Array,
    callback: Callback,

    pub fn isReturnable(self: Type) bool {
        return switch (self) {
            .callback, .array => false,
            .structure => |layout| layout.fields.len > 0 and layout.size != 0,
            .union_ => |layout| layout.fields.len > 0 and layout.size != 0,
            else => true,
        };
    }
};

pub const Pointer = struct {
    child: ?*const Type = null,
    mutable: bool = false,
    ownership: Ownership = .borrowed,
};

pub const Handle = struct {
    name: []const u8,
    is_opaque: bool = true,
};

pub const Enum = struct {
    name: []const u8,
    backing: IntBacking = .i32,
};

pub const Bitflags = struct {
    name: []const u8,
    backing: IntBacking = .u32,
};

pub const IntBacking = enum {
    i32,
    u32,
    i64,
    u64,
};

pub const Field = struct {
    name: []const u8,
    ty: Type,
    offset: ?usize = null,
};

pub const Struct = struct {
    name: []const u8,
    fields: []const Field,
    size: usize,
    alignment: usize,
};

pub const Union = struct {
    name: []const u8,
    fields: []const Field,
    size: usize,
    alignment: usize,
    tagged: bool = false,
};

pub const Array = struct {
    element: *const Type,
    len: usize,
};

pub const Callback = struct {
    parameters: []const Type,
    result: *const Type,
    abi: Abi = .c,
};

pub const Parameter = struct {
    name: []const u8,
    ty: Type,
};

pub const Signature = struct {
    symbol: []const u8,
    abi: Abi = .c,
    parameters: []const Parameter,
    result: Type = .void,
};

pub const DiagnosticCode = enum {
    empty_symbol,
    invalid_void_parameter,
    unsupported_return_type,
    unsupported_layout,
    unsupported_callback_result,
    unsafe_ownership,
};

pub const Diagnostic = struct {
    code: DiagnosticCode,
    message: []const u8,
};

pub fn validateSignature(signature: Signature) ?Diagnostic {
    if (signature.symbol.len == 0) return .{ .code = .empty_symbol, .message = "FFI symbol name must not be empty" };
    for (signature.parameters) |param| {
        if (param.ty == .void) return .{ .code = .invalid_void_parameter, .message = "FFI parameters cannot use Void" };
        if (validateType(param.ty)) |diag| return diag;
    }
    if (!signature.result.isReturnable()) return .{ .code = .unsupported_return_type, .message = "FFI return type is not directly returnable by LibFFI" };
    return validateType(signature.result);
}

fn validateType(ty: Type) ?Diagnostic {
    return switch (ty) {
        .structure => |layout| validateAggregate(layout.name, layout.fields, layout.size, layout.alignment),
        .union_ => |layout| validateAggregate(layout.name, layout.fields, layout.size, layout.alignment),
        .array => |layout| if (layout.len == 0) .{ .code = .unsupported_layout, .message = "FFI arrays must have a non-zero length" } else validateType(layout.element.*),
        .callback => |callback| validateCallback(callback),
        .pointer => |pointer| if (pointer.ownership == .owned_by_callee and pointer.mutable) .{
            .code = .unsafe_ownership,
            .message = "mutable callee-owned pointers need an explicit lifetime bridge",
        } else null,
        else => null,
    };
}

fn validateAggregate(_: []const u8, fields: []const Field, size: usize, alignment: usize) ?Diagnostic {
    if (fields.len == 0 or size == 0 or alignment == 0) return .{
        .code = .unsupported_layout,
        .message = "FFI aggregate layouts require known fields, size, and alignment",
    };
    for (fields) |field| {
        if (field.offset == null) return .{ .code = .unsupported_layout, .message = "FFI aggregate fields require explicit offsets" };
        if (validateType(field.ty)) |diag| return diag;
    }
    return null;
}

fn validateCallback(callback: Callback) ?Diagnostic {
    for (callback.parameters) |param| {
        if (param == .void) return .{ .code = .invalid_void_parameter, .message = "callback parameters cannot use Void" };
        if (validateType(param)) |diag| return diag;
    }
    if (!callback.result.*.isReturnable()) return .{ .code = .unsupported_callback_result, .message = "callback result is not directly returnable by LibFFI" };
    return validateType(callback.result.*);
}

test "rejects incomplete aggregate layouts" {
    const params = [_]Parameter{.{
        .name = "value",
        .ty = .{ .structure = .{ .name = "VkExtent2D", .fields = &.{}, .size = 0, .alignment = 0 } },
    }};
    const diag = validateSignature(.{ .symbol = "vkCreateDevice", .parameters = &params }).?;
    try std.testing.expectEqual(DiagnosticCode.unsupported_layout, diag.code);
}

test "accepts Vulkan-scale pointer and bitflag signatures" {
    const params = [_]Parameter{
        .{ .name = "instance", .ty = .{ .handle = .{ .name = "VkInstance" } } },
        .{ .name = "flags", .ty = .{ .bitflags = .{ .name = "VkDeviceCreateFlags", .backing = .u32 } } },
        .{ .name = "out", .ty = .{ .pointer = .{ .mutable = true, .ownership = .borrowed } } },
    };
    try std.testing.expect(validateSignature(.{
        .symbol = "vkCreateDevice",
        .abi = .c,
        .parameters = &params,
        .result = .{ .enumeration = .{ .name = "VkResult", .backing = .i32 } },
    }) == null);
}
