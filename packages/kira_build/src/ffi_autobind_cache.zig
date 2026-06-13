const std = @import("std");
const native = @import("kira_native_lib_definition");
const fs_helpers = @import("ffi_autobind_fs.zig");

pub fn bindingsAreCurrent(allocator: std.mem.Allocator, output_path: []const u8, cache_key: []const u8) !bool {
    if (!fs_helpers.fileExists(output_path)) return false;
    const key_path = try keyPath(allocator, output_path);
    defer allocator.free(key_path);
    const existing = fs_helpers.readFileAlloc(key_path, allocator, 4096) catch return false;
    defer allocator.free(existing);
    return std.mem.eql(u8, existing, cache_key);
}

pub fn writeKey(output_path: []const u8, cache_key: []const u8) !void {
    const path = try keyPath(std.heap.page_allocator, output_path);
    defer std.heap.page_allocator.free(path);
    try fs_helpers.writeFile(path, cache_key);
}

pub fn cacheKey(
    allocator: std.mem.Allocator,
    library: native.ResolvedNativeLibrary,
    autobinding: native.AutobindingSpec,
) ![]const u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update("kira-autobinding-v1\n");
    try hashCompilerIdentity(allocator, &hasher);
    try hashString(&hasher, "library", library.name);
    try hashPath(allocator, &hasher, "artifact", library.artifact_path);
    try hashString(&hasher, "module", autobinding.module_name);
    try hashPath(allocator, &hasher, "output", autobinding.output_path);
    try hashString(&hasher, "mode", @tagName(autobinding.bindings.mode));
    try hashString(&hasher, "profile", @tagName(autobinding.bindings.profile));
    for (library.headers.include_dirs) |path| try hashPath(allocator, &hasher, "include_dir", path);
    for (library.headers.defines) |define| try hashString(&hasher, "header_define", define);
    for (library.build.defines) |define| try hashString(&hasher, "build_define", define);
    for (autobinding.bindings.functions) |name| try hashString(&hasher, "function", name);
    for (autobinding.bindings.structs) |name| try hashString(&hasher, "struct", name);
    for (autobinding.bindings.callbacks) |name| try hashString(&hasher, "callback", name);

    if (library.manifest_path) |path| try hashFileIfPresent(allocator, &hasher, "manifest", path);
    if (library.headers.entrypoint) |path| try hashFileIfPresent(allocator, &hasher, "entrypoint", path);
    for (autobinding.headers) |path| try hashFileIfPresent(allocator, &hasher, "header", path);
    for (library.headers.include_dirs) |dir| try hashHeaderDirectory(allocator, &hasher, dir);

    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    return hexDigest(allocator, &digest);
}

fn keyPath(allocator: std.mem.Allocator, output_path: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}.key", .{output_path});
}

fn hashCompilerIdentity(allocator: std.mem.Allocator, hasher: anytype) !void {
    const exe_path = std.process.executablePathAlloc(std.Options.debug_io, allocator) catch return;
    defer allocator.free(exe_path);
    try hashString(hasher, "compiler_path", exe_path);
    const stat = fs_helpers.statFile(exe_path) catch return;
    var buffer: [128]u8 = undefined;
    const text = try std.fmt.bufPrint(&buffer, "size={d};mtime={d}", .{ stat.size, stat.mtime });
    try hashString(hasher, "compiler_stat", text);
}

fn hashHeaderDirectory(allocator: std.mem.Allocator, hasher: anytype, dir_path: []const u8) !void {
    var files = std.array_list.Managed([]const u8).init(allocator);
    try collectHeaderFiles(allocator, dir_path, &files);
    std.mem.sort([]const u8, files.items, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.lessThan(u8, lhs, rhs);
        }
    }.lessThan);
    for (files.items) |path| try hashFileIfPresent(allocator, hasher, "include_header", path);
}

fn collectHeaderFiles(allocator: std.mem.Allocator, dir_path: []const u8, files: *std.array_list.Managed([]const u8)) !void {
    var dir = if (std.fs.path.isAbsolute(dir_path))
        std.Io.Dir.openDirAbsolute(std.Options.debug_io, dir_path, .{ .iterate = true }) catch return
    else
        std.Io.Dir.cwd().openDir(std.Options.debug_io, dir_path, .{ .iterate = true }) catch return;
    defer dir.close(std.Options.debug_io);

    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind == .directory) {
            const child = try std.fs.path.join(allocator, &.{ dir_path, entry.name });
            try collectHeaderFiles(allocator, child, files);
            continue;
        }
        if (entry.kind != .file or !isHeaderFile(entry.name)) continue;
        try files.append(try std.fs.path.join(allocator, &.{ dir_path, entry.name }));
    }
}

fn isHeaderFile(path: []const u8) bool {
    const ext = std.fs.path.extension(path);
    return std.mem.eql(u8, ext, ".h") or
        std.mem.eql(u8, ext, ".hh") or
        std.mem.eql(u8, ext, ".hpp") or
        std.mem.eql(u8, ext, ".hxx");
}

fn hashFileIfPresent(allocator: std.mem.Allocator, hasher: anytype, label: []const u8, path: []const u8) !void {
    const bytes = fs_helpers.readFileAlloc(path, allocator, 64 * 1024 * 1024) catch return;
    defer allocator.free(bytes);
    try hashPath(allocator, hasher, label, path);
    hasher.update(bytes);
    hasher.update("\n");
}

fn hashPath(allocator: std.mem.Allocator, hasher: anytype, label: []const u8, path: []const u8) !void {
    const canonical = try canonicalPathOrOriginal(allocator, path);
    defer allocator.free(canonical);
    try hashString(hasher, label, canonical);
}

fn canonicalPathOrOriginal(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, path, allocator) catch allocator.dupe(u8, path);
}

fn hashString(hasher: anytype, label: []const u8, value: []const u8) !void {
    hasher.update(label);
    hasher.update("=");
    hasher.update(value);
    hasher.update("\n");
}

fn hexDigest(allocator: std.mem.Allocator, digest: []const u8) ![]const u8 {
    const alphabet = "0123456789abcdef";
    const out = try allocator.alloc(u8, digest.len * 2 + 1);
    for (digest, 0..) |byte, index| {
        out[index * 2] = alphabet[byte >> 4];
        out[index * 2 + 1] = alphabet[byte & 0x0f];
    }
    out[digest.len * 2] = '\n';
    return out;
}
