const std = @import("std");
const native = @import("native_library.zig");
const LinkExtras = @import("link_extras.zig").LinkExtras;

pub const TargetSelector = struct {
    architecture: []const u8,
    operating_system: []const u8,
    abi: []const u8,

    pub fn parse(allocator: std.mem.Allocator, triple: []const u8) !TargetSelector {
        var parts = std.mem.splitScalar(u8, triple, '-');
        const arch = parts.next() orelse return error.InvalidManifest;
        const os = parts.next() orelse return error.InvalidManifest;
        const abi = parts.next() orelse return error.InvalidManifest;
        return .{
            .architecture = try allocator.dupe(u8, arch),
            .operating_system = try allocator.dupe(u8, os),
            .abi = try allocator.dupe(u8, abi),
        };
    }

    pub fn eql(self: TargetSelector, other: TargetSelector) bool {
        return std.mem.eql(u8, self.architecture, other.architecture) and
            std.mem.eql(u8, self.operating_system, other.operating_system) and
            std.mem.eql(u8, self.abi, other.abi);
    }
};

pub const ResolvedNativeLibrary = struct {
    manifest_path: ?[]const u8 = null,
    name: []const u8,
    link_mode: native.LinkMode,
    abi: native.LibraryAbi,
    artifact_path: []const u8,
    target: TargetSelector,
    headers: native.HeaderSpec,
    autobinding: ?native.AutobindingSpec = null,
    build: native.BuildRecipe = .{},
    link: LinkExtras,
};

pub fn resolveLibrary(allocator: std.mem.Allocator, spec: native.NativeLibrarySpec, active_target: TargetSelector) !ResolvedNativeLibrary {
    for (spec.targets) |target_spec| {
        if (target_spec.selector.eql(active_target)) {
            const artifact_path = if (spec.link_mode == .static)
                (target_spec.static_lib orelse return error.UnsupportedTarget)
            else
                (target_spec.dynamic_lib orelse return error.UnsupportedTarget);

            return .{
                .name = spec.name,
                .link_mode = spec.link_mode,
                .abi = spec.abi,
                .artifact_path = try allocator.dupe(u8, artifact_path),
                .target = .{
                    .architecture = try allocator.dupe(u8, active_target.architecture),
                    .operating_system = try allocator.dupe(u8, active_target.operating_system),
                    .abi = try allocator.dupe(u8, active_target.abi),
                },
                .headers = try cloneHeaders(allocator, spec.headers),
                .autobinding = if (spec.autobinding) |autobinding| try cloneAutobinding(allocator, autobinding) else null,
                .build = try cloneBuildRecipe(allocator, spec.build),
                .link = try LinkExtras.clone(allocator, target_spec.link),
            };
        }
    }
    return error.UnsupportedTarget;
}

fn cloneHeaders(allocator: std.mem.Allocator, headers: native.HeaderSpec) !native.HeaderSpec {
    return .{
        .entrypoint = if (headers.entrypoint) |value| try allocator.dupe(u8, value) else null,
        .include_dirs = try cloneStrings(allocator, headers.include_dirs),
        .defines = try cloneStrings(allocator, headers.defines),
        .frameworks = try cloneStrings(allocator, headers.frameworks),
        .system_libs = try cloneStrings(allocator, headers.system_libs),
    };
}

fn cloneAutobinding(allocator: std.mem.Allocator, autobinding: native.AutobindingSpec) !native.AutobindingSpec {
    return .{
        .module_name = try allocator.dupe(u8, autobinding.module_name),
        .output_path = try allocator.dupe(u8, autobinding.output_path),
        .headers = try cloneStrings(allocator, autobinding.headers),
        .bindings = .{
            .mode = autobinding.bindings.mode,
            .profile = autobinding.bindings.profile,
            .functions = try cloneStrings(allocator, autobinding.bindings.functions),
            .structs = try cloneStrings(allocator, autobinding.bindings.structs),
            .callbacks = try cloneStrings(allocator, autobinding.bindings.callbacks),
        },
    };
}

fn cloneBuildRecipe(allocator: std.mem.Allocator, build: native.BuildRecipe) !native.BuildRecipe {
    return .{
        .sources = try cloneStrings(allocator, build.sources),
        .include_dirs = try cloneStrings(allocator, build.include_dirs),
        .defines = try cloneStrings(allocator, build.defines),
    };
}

fn cloneStrings(allocator: std.mem.Allocator, values: []const []const u8) ![]const []const u8 {
    var list = std.array_list.Managed([]const u8).init(allocator);
    for (values) |value| {
        try list.append(try allocator.dupe(u8, value));
    }
    return list.toOwnedSlice();
}

test "resolves native library for target" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const selector = try TargetSelector.parse(allocator, "x86_64-linux-gnu");
    const spec = native.NativeLibrarySpec{
        .name = "glfw",
        .link_mode = .static,
        .abi = .c,
        .targets = &.{.{
            .selector = selector,
            .static_lib = "vendor/glfw/linux/x86_64/libglfw3.a",
        }},
    };

    const resolved = try resolveLibrary(allocator, spec, selector);
    try std.testing.expectEqualStrings("vendor/glfw/linux/x86_64/libglfw3.a", resolved.artifact_path);
}
