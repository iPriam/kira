const builtin = @import("builtin");
const std = @import("std");
const kira_toolchain = @import("../packages/kira_toolchain/src/root.zig");
const toolchain_layout = @import("../packages/kira_llvm_toolchain_layout/src/root.zig");

pub const LlvmHeaderProbe = struct {
    include_dirs: []const []const u8,
    library_dir: ?[]const u8 = null,
    link_name: ?[]const u8 = null,
};

pub fn discoverLlvmHeaders(
    allocator: std.mem.Allocator,
    repo_root: []const u8,
    llvm_version: []const u8,
    llvm_host_key: []const u8,
    env_home: ?[]const u8,
) ?LlvmHeaderProbe {
    if (env_home) |path| {
        if (headerProbeForHome(allocator, path)) |probe| return probe;
    }

    if (llvm_version.len > 0 and !std.mem.eql(u8, llvm_host_key, "unsupported-host")) {
        const shared_home = kira_toolchain.managedLlvmHome(allocator, llvm_version, llvm_host_key) catch return null;
        if (headerProbeForHome(allocator, shared_home)) |probe| return probe;
    }

    if (llvm_version.len > 0 and !std.mem.eql(u8, llvm_host_key, "unsupported-host")) {
        const managed_home = toolchain_layout.managedLlvmHome(allocator, repo_root, llvm_version, llvm_host_key) catch return null;
        if (headerProbeForHome(allocator, managed_home)) |probe| return probe;
    }

    const legacy_current = toolchain_layout.legacyLlvmCurrentHome(allocator, repo_root) catch return null;
    if (headerProbeForHome(allocator, legacy_current)) |probe| return probe;

    if (llvm_version.len > 0 and !std.mem.eql(u8, llvm_host_key, "unsupported-host")) {
        const legacy_versioned = toolchain_layout.legacyLlvmVersionedHome(allocator, repo_root, llvm_version, llvm_host_key) catch return null;
        if (headerProbeForHome(allocator, legacy_versioned)) |probe| return probe;
    }

    return null;
}

fn headerProbeForHome(allocator: std.mem.Allocator, home: []const u8) ?LlvmHeaderProbe {
    const install_include = std.fs.path.join(allocator, &.{ home, "include" }) catch return null;
    if (isValidLlvmIncludeDir(install_include)) {
        const install_bin = std.fs.path.join(allocator, &.{ home, "bin" }) catch return null;
        const install_lib = std.fs.path.join(allocator, &.{ home, "lib" }) catch return null;
        const library = llvmLinkProbe(allocator, install_bin, install_lib);
        return .{
            .include_dirs = allocator.dupe([]const u8, &.{install_include}) catch return null,
            .library_dir = if (library) |value| value.dir else null,
            .link_name = if (library) |value| value.name else null,
        };
    }

    const source_include = std.fs.path.join(allocator, &.{ home, "llvm-project", "llvm", "include" }) catch return null;
    if (!isDir(source_include)) return null;

    const build_variants = [_][]const u8{
        "build/include",
        "build-msvc/include",
        "build-release/include",
        "build-debug/include",
    };
    for (build_variants) |suffix| {
        const build_include = std.fs.path.join(allocator, &.{ home, suffix }) catch continue;
        if (isValidLlvmSplitIncludeDirs(source_include, build_include)) {
            const variant_root = std.fs.path.dirname(build_include) orelse continue;
            const variant_bin = std.fs.path.join(allocator, &.{ variant_root, "bin" }) catch continue;
            const variant_lib = std.fs.path.join(allocator, &.{ variant_root, "lib" }) catch continue;
            const library = llvmLinkProbe(allocator, variant_bin, variant_lib);
            return .{
                .include_dirs = allocator.dupe([]const u8, &.{ source_include, build_include }) catch return null,
                .library_dir = if (library) |value| value.dir else null,
                .link_name = if (library) |value| value.name else null,
            };
        }
    }
    return null;
}

const LlvmLinkProbe = struct {
    dir: []const u8,
    name: []const u8,
};

fn llvmLinkProbe(allocator: std.mem.Allocator, bin_dir: []const u8, lib_dir: []const u8) ?LlvmLinkProbe {
    const candidates = switch (builtin.os.tag) {
        .linux => [_]LlvmLinkProbe{
            .{ .dir = lib_dir, .name = "LLVM-C" },
            .{ .dir = lib_dir, .name = "LLVM" },
            .{ .dir = bin_dir, .name = "LLVM-C" },
            .{ .dir = bin_dir, .name = "LLVM" },
        },
        .macos => [_]LlvmLinkProbe{
            .{ .dir = lib_dir, .name = "LLVM-C" },
            .{ .dir = lib_dir, .name = "LLVM" },
            .{ .dir = bin_dir, .name = "LLVM-C" },
            .{ .dir = bin_dir, .name = "LLVM" },
        },
        else => return null,
    };

    for (candidates) |candidate| {
        const filename = switch (builtin.os.tag) {
            .linux => if (std.mem.eql(u8, candidate.name, "LLVM-C")) "libLLVM-C.so" else "libLLVM.so",
            .macos => if (std.mem.eql(u8, candidate.name, "LLVM-C")) "libLLVM-C.dylib" else "libLLVM.dylib",
            else => unreachable,
        };
        const path = std.fs.path.join(allocator, &.{ candidate.dir, filename }) catch continue;
        if (isFile(path)) return candidate;
    }
    return null;
}

fn isValidLlvmIncludeDir(include_dir: []const u8) bool {
    const core_header = std.fs.path.join(std.heap.page_allocator, &.{ include_dir, "llvm-c", "Core.h" }) catch return false;
    defer std.heap.page_allocator.free(core_header);
    const config_header = std.fs.path.join(std.heap.page_allocator, &.{ include_dir, "llvm", "Config", "llvm-config.h" }) catch return false;
    defer std.heap.page_allocator.free(config_header);
    return isFile(core_header) and isFile(config_header);
}

fn isValidLlvmSplitIncludeDirs(source_include: []const u8, build_include: []const u8) bool {
    const core_header = std.fs.path.join(std.heap.page_allocator, &.{ source_include, "llvm-c", "Core.h" }) catch return false;
    defer std.heap.page_allocator.free(core_header);
    const config_header = std.fs.path.join(std.heap.page_allocator, &.{ build_include, "llvm", "Config", "llvm-config.h" }) catch return false;
    defer std.heap.page_allocator.free(config_header);
    return isFile(core_header) and isFile(config_header);
}

fn isDir(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn isFile(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}
