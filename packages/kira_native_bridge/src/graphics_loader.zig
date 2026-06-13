const std = @import("std");
const builtin = @import("builtin");
const dynamic_ffi = @import("kira_dynamic_ffi");

pub const GraphicsBackend = enum {
    vulkan,
    directx12,
};

pub const ProbeResult = struct {
    backend: GraphicsBackend,
    supported: bool,
    library_name: []const u8,
    symbol_name: []const u8,
    reason: []const u8,
    ffi_invoked: bool = false,
    native_status: i32 = 0,
};

pub fn probeBackend(allocator: std.mem.Allocator, backend: GraphicsBackend) !ProbeResult {
    return switch (backend) {
        .vulkan => probeVulkan(allocator),
        .directx12 => probeDirectX12(allocator),
    };
}

pub fn probeVulkan(allocator: std.mem.Allocator) !ProbeResult {
    const library_name = vulkanLoaderName() orelse return .{
        .backend = .vulkan,
        .supported = false,
        .library_name = "",
        .symbol_name = "",
        .reason = "Vulkan is not supported on this target platform",
    };

    var libffi = dynamic_ffi.Libffi.openManagedInstall(allocator) catch |err| return .{
        .backend = .vulkan,
        .supported = false,
        .library_name = library_name,
        .symbol_name = "vkEnumerateInstanceVersion",
        .reason = @errorName(err),
    };
    defer libffi.close();

    var vulkan = dynamic_ffi.DynamicLibrary.open(allocator, library_name) catch |err| return .{
        .backend = .vulkan,
        .supported = false,
        .library_name = library_name,
        .symbol_name = "vkEnumerateInstanceVersion",
        .reason = @errorName(err),
    };
    defer vulkan.close();

    const function = vulkan.lookup(*const anyopaque, "vkEnumerateInstanceVersion") catch |err| return .{
        .backend = .vulkan,
        .supported = false,
        .library_name = library_name,
        .symbol_name = "vkEnumerateInstanceVersion",
        .reason = @errorName(err),
    };

    const params = [_]dynamic_ffi.Parameter{.{
        .name = "pApiVersion",
        .ty = .{ .pointer = .{ .mutable = true, .ownership = .borrowed } },
    }};
    var prepared = try libffi.prepare(allocator, .{
        .symbol = "vkEnumerateInstanceVersion",
        .abi = .system,
        .parameters = &params,
        .result = .{ .enumeration = .{ .name = "VkResult", .backing = .i32 } },
    });
    defer prepared.deinit();

    var api_version: u32 = 0;
    var api_version_ptr: usize = @intFromPtr(&api_version);
    var status: i32 = 0;
    var args = [_]?*anyopaque{@ptrCast(&api_version_ptr)};
    try prepared.invoke(function, &status, &args);

    return .{
        .backend = .vulkan,
        .supported = status == 0 and api_version != 0,
        .library_name = library_name,
        .symbol_name = "vkEnumerateInstanceVersion",
        .reason = if (status == 0 and api_version != 0) "Vulkan loader responded through LibFFI" else "Vulkan loader returned a failing VkResult",
        .ffi_invoked = true,
        .native_status = status,
    };
}

pub fn probeDirectX12(allocator: std.mem.Allocator) !ProbeResult {
    if (builtin.os.tag != .windows) return .{
        .backend = .directx12,
        .supported = false,
        .library_name = "d3d12.dll",
        .symbol_name = "D3D12CreateDevice",
        .reason = "DirectX 12 is only supported on Windows targets",
    };

    var libffi = dynamic_ffi.Libffi.openManagedInstall(allocator) catch |err| return .{
        .backend = .directx12,
        .supported = false,
        .library_name = "d3d12.dll",
        .symbol_name = "D3D12CreateDevice",
        .reason = @errorName(err),
    };
    defer libffi.close();

    var d3d12 = dynamic_ffi.DynamicLibrary.open(allocator, "d3d12.dll") catch |err| return .{
        .backend = .directx12,
        .supported = false,
        .library_name = "d3d12.dll",
        .symbol_name = "D3D12CreateDevice",
        .reason = @errorName(err),
    };
    defer d3d12.close();

    const function = d3d12.lookup(*const anyopaque, "D3D12CreateDevice") catch |err| return .{
        .backend = .directx12,
        .supported = false,
        .library_name = "d3d12.dll",
        .symbol_name = "D3D12CreateDevice",
        .reason = @errorName(err),
    };

    const params = [_]dynamic_ffi.Parameter{
        .{ .name = "pAdapter", .ty = .{ .pointer = .{ .ownership = .borrowed } } },
        .{ .name = "MinimumFeatureLevel", .ty = .u32 },
        .{ .name = "riid", .ty = .{ .pointer = .{ .ownership = .borrowed } } },
        .{ .name = "ppDevice", .ty = .{ .pointer = .{ .mutable = true, .ownership = .borrowed } } },
    };
    var prepared = try libffi.prepare(allocator, .{
        .symbol = "D3D12CreateDevice",
        .abi = .system,
        .parameters = &params,
        .result = .i32,
    });
    defer prepared.deinit();

    var adapter: usize = 0;
    var minimum_feature_level: u32 = 0xb000;
    const iid_device = DirectXGuid.init(0x189819f1, 0x1db6, 0x4b57, .{ 0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7 });
    var iid_device_ptr: usize = @intFromPtr(&iid_device);
    var device_ptr: usize = 0;
    var hresult: i32 = 0;
    var args = [_]?*anyopaque{
        @ptrCast(&adapter),
        @ptrCast(&minimum_feature_level),
        @ptrCast(&iid_device_ptr),
        @ptrCast(&device_ptr),
    };
    try prepared.invoke(function, &hresult, &args);

    return .{
        .backend = .directx12,
        .supported = hresult >= 0,
        .library_name = "d3d12.dll",
        .symbol_name = "D3D12CreateDevice",
        .reason = if (hresult >= 0) "DirectX 12 feature-level validation responded through LibFFI" else "D3D12CreateDevice returned a failing HRESULT",
        .ffi_invoked = true,
        .native_status = hresult,
    };
}

fn vulkanLoaderName() ?[]const u8 {
    return switch (builtin.os.tag) {
        .windows => "vulkan-1.dll",
        .linux => "libvulkan.so.1",
        .macos => "libvulkan.1.dylib",
        else => null,
    };
}

const DirectXGuid = extern struct {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [8]u8,

    fn init(data1: u32, data2: u16, data3: u16, data4: [8]u8) DirectXGuid {
        return .{ .data1 = data1, .data2 = data2, .data3 = data3, .data4 = data4 };
    }
};

test "graphics backend probe reports deterministic platform support" {
    const vulkan = try probeBackend(std.testing.allocator, .vulkan);
    try std.testing.expectEqual(GraphicsBackend.vulkan, vulkan.backend);
    try std.testing.expect(vulkan.library_name.len > 0 or builtin.os.tag != .windows);

    const d3d12 = try probeBackend(std.testing.allocator, .directx12);
    try std.testing.expectEqual(GraphicsBackend.directx12, d3d12.backend);
    if (builtin.os.tag == .windows) {
        try std.testing.expectEqualStrings("d3d12.dll", d3d12.library_name);
        try std.testing.expectEqualStrings("D3D12CreateDevice", d3d12.symbol_name);
    } else {
        try std.testing.expect(!d3d12.supported);
    }
}
