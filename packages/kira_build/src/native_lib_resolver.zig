const std = @import("std");
const manifest = @import("kira_manifest");
const native = @import("kira_native_lib_definition");

pub fn resolveNativeManifestFile(allocator: std.mem.Allocator, path: []const u8, target: native.TargetSelector) !native.ResolvedNativeLibrary {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(1024 * 1024));
    const parsed = try manifest.parseNativeLibManifest(allocator, text);
    var resolved = try native.resolveLibrary(allocator, parsed.library, target);
    resolved.manifest_path = try absolutizePath(allocator, path, path);
    resolved.artifact_path = try absolutizePath(allocator, path, resolved.artifact_path);
    resolved.headers = try resolveHeaders(allocator, path, resolved.headers);
    resolved.autobinding = if (resolved.autobinding) |autobinding| try resolveAutobinding(allocator, path, autobinding) else null;
    resolved.build = try resolveBuildRecipe(allocator, path, resolved.build);
    resolved.link = try resolveLinkExtras(allocator, path, resolved.link);
    return resolved;
}

fn resolveHeaders(allocator: std.mem.Allocator, manifest_path: []const u8, headers: native.HeaderSpec) !native.HeaderSpec {
    return .{
        .entrypoint = if (headers.entrypoint) |value| try absolutizePath(allocator, manifest_path, value) else null,
        .include_dirs = try absolutizePaths(allocator, manifest_path, headers.include_dirs),
        .defines = try cloneStrings(allocator, headers.defines),
        .frameworks = try cloneStrings(allocator, headers.frameworks),
        .system_libs = try cloneStrings(allocator, headers.system_libs),
    };
}

fn resolveAutobinding(allocator: std.mem.Allocator, manifest_path: []const u8, autobinding: native.AutobindingSpec) !native.AutobindingSpec {
    return .{
        .module_name = try allocator.dupe(u8, autobinding.module_name),
        .output_path = try absolutizePath(allocator, manifest_path, autobinding.output_path),
        .headers = try absolutizePaths(allocator, manifest_path, autobinding.headers),
        .bindings = .{
            .mode = autobinding.bindings.mode,
            .profile = autobinding.bindings.profile,
            .functions = try cloneStrings(allocator, autobinding.bindings.functions),
            .structs = try cloneStrings(allocator, autobinding.bindings.structs),
            .callbacks = try cloneStrings(allocator, autobinding.bindings.callbacks),
        },
    };
}

fn resolveBuildRecipe(allocator: std.mem.Allocator, manifest_path: []const u8, build: native.BuildRecipe) !native.BuildRecipe {
    return .{
        .sources = try absolutizePaths(allocator, manifest_path, build.sources),
        .include_dirs = try absolutizePaths(allocator, manifest_path, build.include_dirs),
        .defines = try cloneStrings(allocator, build.defines),
    };
}

fn resolveLinkExtras(allocator: std.mem.Allocator, manifest_path: []const u8, extras: native.LinkExtras) !native.LinkExtras {
    return .{
        .include_dirs = try absolutizePaths(allocator, manifest_path, extras.include_dirs),
        .defines = try cloneStrings(allocator, extras.defines),
        .frameworks = try cloneStrings(allocator, extras.frameworks),
        .system_libs = try cloneStrings(allocator, extras.system_libs),
    };
}

fn absolutizePaths(allocator: std.mem.Allocator, manifest_path: []const u8, values: []const []const u8) ![]const []const u8 {
    var list = std.array_list.Managed([]const u8).init(allocator);
    for (values) |value| {
        try list.append(try absolutizePath(allocator, manifest_path, value));
    }
    return list.toOwnedSlice();
}

fn absolutizePath(allocator: std.mem.Allocator, manifest_path: []const u8, value: []const u8) ![]const u8 {
    if (value.len == 0) return allocator.dupe(u8, value);
    if (try expandEnvPath(allocator, value)) |expanded| return expanded;
    if (std.fs.path.isAbsolute(value)) return allocator.dupe(u8, value);

    const base_dir = std.fs.path.dirname(manifest_path) orelse ".";
    const joined = try std.fs.path.join(allocator, &.{ base_dir, value });
    if (std.fs.path.isAbsolute(joined)) return joined;

    const cwd = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, ".", allocator);
    defer allocator.free(cwd);
    return std.fs.path.join(allocator, &.{ cwd, joined });
}

fn expandEnvPath(allocator: std.mem.Allocator, value: []const u8) !?[]const u8 {
    if (!std.mem.startsWith(u8, value, "${")) return null;
    const close = std.mem.indexOfScalar(u8, value, '}') orelse return null;
    const name = value[2..close];
    const suffix = value[close + 1 ..];
    const root = envVarOwned(allocator, name) catch return null;
    defer allocator.free(root);
    if (suffix.len == 0) return @as(?[]const u8, try allocator.dupe(u8, root));
    const trimmed = std.mem.trim(u8, suffix, "/\\");
    return @as(?[]const u8, try std.fs.path.join(allocator, &.{ root, trimmed }));
}

fn envVarOwned(allocator: std.mem.Allocator, name: []const u8) ![]u8 {
    var environ = try std.process.Environ.createMap(.{ .block = .global }, allocator);
    defer environ.deinit();
    const value = environ.get(name) orelse {
        if (std.mem.eql(u8, name, "WINDOWS_KITS_10_INCLUDE")) return discoverWindowsKitsInclude(allocator);
        return error.EnvironmentVariableNotFound;
    };
    return allocator.dupe(u8, value);
}

fn discoverWindowsKitsInclude(allocator: std.mem.Allocator) ![]u8 {
    const root_path = "C:\\Program Files (x86)\\Windows Kits\\10\\Include";
    var root = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, root_path, .{ .iterate = true });
    defer root.close(std.Options.debug_io);

    var best: ?[]u8 = null;
    var iterator = root.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .directory) continue;
        if (best) |current| {
            if (std.mem.order(u8, entry.name, std.fs.path.basename(current)) != .gt) continue;
            allocator.free(current);
        }
        best = try std.fs.path.join(allocator, &.{ root_path, entry.name });
    }
    return best orelse error.EnvironmentVariableNotFound;
}

fn cloneStrings(allocator: std.mem.Allocator, values: []const []const u8) ![]const []const u8 {
    var list = std.array_list.Managed([]const u8).init(allocator);
    for (values) |value| {
        try list.append(try allocator.dupe(u8, value));
    }
    return list.toOwnedSlice();
}
