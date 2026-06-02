const std = @import("std");

pub const DiagnosticCode = @import("DiagnosticCode.zig").DiagnosticCode;
pub const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
pub const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;
pub const build = @import("DiagnosticMessage.zig").build;
pub const CliMessages = @import("CliMessages.zig");
pub const PackageMessages = @import("PackageMessages.zig");
pub const ToolchainMessages = @import("ToolchainMessages.zig");
pub const CompilerBugMessages = @import("CompilerBugMessages.zig");
pub const BackendMessages = @import("BackendMessages.zig");

test "KIC001 is only used in approved fallback locations" {
    const approved = [_][]const u8{
        "packages/kira_diagnostic_messages/src/DiagnosticCode.zig",
        "packages/kira_diagnostic_messages/src/CompilerBugMessages.zig",
        "packages/kira_diagnostic_messages/src/root.zig",
    };

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var offending = std.array_list.Managed([]const u8).init(arena.allocator());
    const cwd_path = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, ".", arena.allocator());
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, cwd_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var walker = try dir.walk(arena.allocator());
    defer walker.deinit();

    while (try walker.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.path, ".zig")) continue;
        if (isApproved(entry.path, &approved)) continue;

        const text = try dir.readFileAlloc(
            std.Options.debug_io,
            entry.path,
            arena.allocator(),
            .limited(1024 * 1024),
        );
        if (std.mem.indexOf(u8, text, "KIC001") != null or std.mem.indexOf(u8, text, "KICE001") != null) {
            try offending.append(try arena.allocator().dupe(u8, entry.path));
        }
    }

    if (offending.items.len != 0) {
        std.debug.print(
            "Use a specific diagnostic code or the approved legacy fallback helper. Offending files:\\n",
            .{},
        );
        for (offending.items) |path| std.debug.print("  {s}\\n", .{path});
        return error.TestUnexpectedResult;
    }
}

fn isApproved(path: []const u8, approved: []const []const u8) bool {
    for (approved) |candidate| {
        if (pathsEqual(path, candidate)) return true;
    }
    return false;
}

fn pathsEqual(a: []const u8, b: []const u8) bool {
    if (a.len != b.len) return false;
    for (a, b) |lhs, rhs| {
        if (normalizePathChar(lhs) != normalizePathChar(rhs)) return false;
    }
    return true;
}

fn normalizePathChar(char: u8) u8 {
    return if (char == '\\') '/' else char;
}
