const std = @import("std");

pub fn main() !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    try verify(arena.allocator(), true);
}

test "repository truth guards reject Python, root Zig clutter, and fake markers" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    try verify(arena.allocator(), false);
}

fn verify(allocator: std.mem.Allocator, print_success: bool) !void {
    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var walker = try dir.walk(allocator);
    defer walker.deinit();

    var violations = std.array_list.Managed([]const u8).init(allocator);
    while (try walker.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file) continue;
        if (skipPath(entry.path)) continue;
        try checkPythonFile(allocator, &violations, entry.path);
        try checkRootZig(allocator, &violations, entry.path);
        if (shouldScanText(entry.path)) {
            const text = try dir.readFileAlloc(std.Options.debug_io, entry.path, allocator, .limited(16 * 1024 * 1024));
            try checkPythonCommand(allocator, &violations, entry.path, text);
            try checkFakeMarkers(allocator, &violations, entry.path, text);
        }
    }

    if (violations.items.len != 0) {
        for (violations.items) |violation| std.debug.print("{s}\n", .{violation});
        return error.RepositoryTruthViolation;
    }
    if (print_success) std.debug.print("repository truth checks passed\n", .{});
}

fn skipPath(path: []const u8) bool {
    return startsWithPath(path, ".git/") or
        startsWithPath(path, ".zig-cache/") or
        startsWithPath(path, "zig-out/") or
        startsWithPath(path, "generated/") or
        startsWithPath(path, "third_party/") or
        startsWithPath(path, "zig-pkg/") or
        startsWithPath(path, ".codex/") or
        startsWithPath(path, ".github/") or
        startsWithPath(path, ".opencode/");
}

fn checkPythonFile(allocator: std.mem.Allocator, violations: *std.array_list.Managed([]const u8), path: []const u8) !void {
    if (!std.mem.endsWith(u8, path, ".py")) return;
    if (pathsEqual(path, "scripts/llvm/llvm_release.py")) return;
    try addViolation(allocator, violations, "python file is forbidden outside explicit CI allowlist: {s}", .{path});
}

fn checkRootZig(allocator: std.mem.Allocator, violations: *std.array_list.Managed([]const u8), path: []const u8) !void {
    if (!std.mem.endsWith(u8, path, ".zig")) return;
    if (hasPathSeparator(path)) return;
    if (pathsEqual(path, "build.zig")) return;
    try addViolation(allocator, violations, "unexpected root-level Zig file: {s}", .{path});
}

fn checkPythonCommand(allocator: std.mem.Allocator, violations: *std.array_list.Managed([]const u8), path: []const u8, text: []const u8) !void {
    if (pathsEqual(path, "tests/repository_truth.zig")) return;
    if (pathsEqual(path, "scripts/llvm/llvm_release.py")) return;
    const forbidden = [_][]const u8{ "python3", "python -m", "http.server", "pytest", "unittest", "#!/usr/bin/env python", "#!/usr/bin/python" };
    for (forbidden) |token| {
        if (std.mem.indexOf(u8, text, token) != null) {
            try addViolation(allocator, violations, "forbidden Python command token `{s}` in {s}", .{ token, path });
        }
    }
}

fn checkFakeMarkers(allocator: std.mem.Allocator, violations: *std.array_list.Managed([]const u8), path: []const u8, text: []const u8) !void {
    if (pathsEqual(path, "tests/repository_truth.zig")) return;
    const forbidden_anywhere = [_][]const u8{
        "KiraWebGpuSmoke",
        "KiraWebSmoke",
        "KIRA_WEBGPU_FRAME_RENDERED",
        "KIRA_WEBGPU_PIPELINE_CREATED",
    };
    for (forbidden_anywhere) |token| {
        if (std.mem.indexOf(u8, text, token) != null) {
            try addViolation(allocator, violations, "fake host/Kira marker token `{s}` in {s}", .{ token, path });
        }
    }
    if (pathsEqual(path, "packages/kira_live/src/supervisor.zig") and
        std.mem.indexOf(u8, text, "KIRA_APP_RENDERED_VISIBLE_CONTENT") != null)
    {
        try addViolation(allocator, violations, "live supervisor must not translate Kira visible-content markers into host frame success", .{});
    }
}

fn shouldScanText(path: []const u8) bool {
    return std.mem.endsWith(u8, path, ".zig") or
        std.mem.endsWith(u8, path, ".zon") or
        std.mem.endsWith(u8, path, ".js") or
        std.mem.endsWith(u8, path, ".sh") or
        std.mem.endsWith(u8, path, ".toml") or
        std.mem.endsWith(u8, path, ".kira");
}

fn startsWithPath(path: []const u8, prefix: []const u8) bool {
    if (path.len < prefix.len) return false;
    for (prefix, 0..) |char, index| {
        if (normalizePathChar(path[index]) != normalizePathChar(char)) return false;
    }
    return true;
}

fn pathsEqual(lhs: []const u8, rhs: []const u8) bool {
    if (lhs.len != rhs.len) return false;
    for (lhs, rhs) |lhs_char, rhs_char| {
        if (normalizePathChar(lhs_char) != normalizePathChar(rhs_char)) return false;
    }
    return true;
}

fn hasPathSeparator(path: []const u8) bool {
    return std.mem.indexOfScalar(u8, path, '/') != null or
        std.mem.indexOfScalar(u8, path, '\\') != null;
}

fn normalizePathChar(char: u8) u8 {
    return if (char == '\\') '/' else char;
}

fn addViolation(
    allocator: std.mem.Allocator,
    violations: *std.array_list.Managed([]const u8),
    comptime fmt: []const u8,
    args: anytype,
) !void {
    try violations.append(try std.fmt.allocPrint(allocator, fmt, args));
}
