const std = @import("std");

pub fn main() !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    try verify(arena.allocator(), false);
}

test "repository truth guards reject Python, root Zig clutter, and fake markers" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    try verify(arena.allocator(), false);
}

fn verify(allocator: std.mem.Allocator, print_success: bool) !void {
    var violations = std.array_list.Managed([]const u8).init(allocator);
    try scanDir(allocator, &violations, ".");

    if (violations.items.len != 0) {
        for (violations.items) |violation| std.debug.print("{s}\n", .{violation});
        return error.RepositoryTruthViolation;
    }
    if (print_success) std.debug.print("repository truth checks passed\n", .{});
}

fn scanDir(
    allocator: std.mem.Allocator,
    violations: *std.array_list.Managed([]const u8),
    dir_path: []const u8,
) !void {
    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, dir_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);

    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        const path = try childPath(allocator, dir_path, entry.name);
        defer allocator.free(path);

        switch (entry.kind) {
            .directory => {
                if (skipPath(path)) continue;
                try scanDir(allocator, violations, path);
            },
            .file => {
                if (skipPath(path)) continue;
                try checkPythonFile(allocator, violations, path);
                try checkRootZig(allocator, violations, path);
                if (shouldScanText(path)) {
                    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(16 * 1024 * 1024));
                    try checkPythonCommand(allocator, violations, path, text);
                    try checkFakeMarkers(allocator, violations, path, text);
                }
            },
            else => {},
        }
    }
}

fn childPath(allocator: std.mem.Allocator, dir_path: []const u8, name: []const u8) ![]const u8 {
    if (std.mem.eql(u8, dir_path, ".")) return allocator.dupe(u8, name);
    return std.fs.path.join(allocator, &.{ dir_path, name });
}

fn skipPath(path: []const u8) bool {
    return startsWithPath(path, ".git/") or
        startsWithPath(path, ".claude/") or
        startsWithPath(path, ".kira/") or
        startsWithPath(path, ".kira-build/") or
        startsWithPath(path, ".zig-cache/") or
        startsWithPath(path, "zig-out/") or
        startsWithPath(path, "generated/") or
        startsWithPath(path, "third_party/") or
        // Downloaded Emscripten SDK (CI `setup-emsdk` actions-cache-folder). It is a
        // vendored toolchain, not repo source, and ships its own Python tooling — the same
        // reason .zig-cache/ and third_party/ are excluded. Without this the repository-truth
        // Python guard trips on emsdk's `tools/ports/*.py` on every CI run that builds WASM.
        startsWithPath(path, "emsdk-cache/") or
        startsWithPath(path, "tests/pass/") or
        startsWithPath(path, "tests/fail/") or
        startsWithPath(path, "zig-pkg/") or
        startsWithPath(path, ".codex/") or
        startsWithPath(path, ".github/") or
        startsWithPath(path, ".opencode/") or
        startsWithPath(path, "Images/");
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
        std.mem.endsWith(u8, path, ".toml");
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
