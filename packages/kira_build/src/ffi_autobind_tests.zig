const std = @import("std");
const autobind = @import("ffi_autobind.zig");

test "renders enum and macro constants for large native APIs" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var index = autobind.AstIndex{};
    try index.enums.put(allocator, "VkResult", .{
        .name = "VkResult",
        .items = &.{
            .{ .name = "VK_SUCCESS", .value = 0 },
            .{ .name = "VK_ERROR_INITIALIZATION_FAILED", .value = -3 },
        },
    });
    try index.macros.put(allocator, "VK_API_VERSION_1_0", .{
        .name = "VK_API_VERSION_1_0",
        .value = "4194304",
    });

    const rendered = try autobind.renderBindings(allocator, .{
        .name = "vulkan",
        .link_mode = .dynamic,
        .abi = .c,
        .artifact_path = "",
        .target = .{
            .architecture = "x86_64",
            .operating_system = "windows",
            .abi = "msvc",
        },
        .headers = .{},
        .autobinding = null,
        .build = .{},
        .link = .{},
    }, .{ .mode = .all_public }, index);

    try std.testing.expect(std.mem.indexOf(u8, rendered, "struct VkResultConstants") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "let VK_ERROR_INITIALIZATION_FAILED: I64 = -3") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "struct vulkanConstants") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "let VK_API_VERSION_1_0: U64 = 4194304") != null);
}

test "vulkan profile emits dynamic loader and profile selected API surface" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var index = autobind.AstIndex{};
    try index.functions.put(allocator, "vkEnumerateInstanceVersion", .{
        .name = "vkEnumerateInstanceVersion",
        .return_type = "VkResult",
        .params = &.{.{ .name = "pApiVersion", .qual_type = "uint32_t *" }},
    });
    try index.enums.put(allocator, "VkResult", .{
        .name = "VkResult",
        .items = &.{.{ .name = "VK_SUCCESS", .value = 0 }},
    });

    const rendered = try autobind.renderBindings(allocator, .{
        .name = "vulkan",
        .link_mode = .dynamic,
        .abi = .c,
        .artifact_path = "",
        .target = .{
            .architecture = "x86_64",
            .operating_system = "windows",
            .abi = "msvc",
        },
        .headers = .{},
        .autobinding = null,
        .build = .{},
        .link = .{},
    }, .{ .profile = .vulkan }, index);

    try std.testing.expect(std.mem.indexOf(u8, rendered, "function vulkanDynamicLibraryOpen") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function vulkanDynamicFfiCallI32Ptr") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function vkEnumerateInstanceVersion") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "struct VkResultConstants") != null);
}

test "directx12 profile emits COM scale bindings without hand listed functions" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var index = autobind.AstIndex{};
    try index.functions.put(allocator, "D3D12CreateDevice", .{
        .name = "D3D12CreateDevice",
        .return_type = "HRESULT",
        .params = &.{
            .{ .name = "pAdapter", .qual_type = "IUnknown *" },
            .{ .name = "MinimumFeatureLevel", .qual_type = "uint32_t" },
            .{ .name = "riid", .qual_type = "const GUID *" },
            .{ .name = "ppDevice", .qual_type = "void **" },
        },
    });
    try index.typedefs.put(allocator, "HRESULT", .{
        .name = "HRESULT",
        .qual_type = "int32_t",
        .kind = .alias,
    });
    try index.records.put(allocator, "GUID", .{
        .name = "GUID",
        .fields = &.{
            .{ .name = "Data1", .qual_type = "uint32_t" },
            .{ .name = "Data2", .qual_type = "uint16_t" },
            .{ .name = "Data3", .qual_type = "uint16_t" },
            .{ .name = "Data4", .qual_type = "uint8_t[8]" },
        },
    });

    const rendered = try autobind.renderBindings(allocator, .{
        .name = "directx12",
        .link_mode = .dynamic,
        .abi = .c,
        .artifact_path = "",
        .target = .{
            .architecture = "x86_64",
            .operating_system = "windows",
            .abi = "msvc",
        },
        .headers = .{},
        .autobinding = null,
        .build = .{},
        .link = .{},
    }, .{ .profile = .directx12 }, index);

    try std.testing.expect(std.mem.indexOf(u8, rendered, "function directx12DynamicLibraryOpen") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function directx12DynamicFfiCallI32PtrU32PtrPtr") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function directx12DynamicFfiCall(functionPtr: RawPtr, resultType: U32, argTypes: RawPtr, argValues: RawPtr, argCount: U32, resultOut: RawPtr): I32") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function directx12DynamicReadPtrAt") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function directx12DynamicWriteU64At") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function directx12DynamicCStringDup") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "function D3D12CreateDevice") != null);
    try std.testing.expect(std.mem.indexOf(u8, rendered, "struct GUID") != null);
}
