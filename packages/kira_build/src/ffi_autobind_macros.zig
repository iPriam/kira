const std = @import("std");

pub const CMacro = struct {
    name: []const u8,
    value: []const u8,
};

pub fn collectConstants(allocator: std.mem.Allocator, headers: []const []const u8, macros: *std.StringHashMapUnmanaged(CMacro)) !void {
    for (headers) |header_path| {
        // d3d12.h alone is over 5 MB; size the cap for real-world API headers.
        const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, header_path, allocator, .limited(64 * 1024 * 1024));
        var lines = std.mem.splitScalar(u8, text, '\n');
        while (lines.next()) |raw_line| {
            const line = std.mem.trim(u8, raw_line, " \t\r");
            if (!std.mem.startsWith(u8, line, "#define ")) continue;
            const rest = std.mem.trimStart(u8, line["#define ".len..], " \t");
            var parts = std.mem.tokenizeAny(u8, rest, " \t");
            const name = parts.next() orelse continue;
            if (name.len == 0 or name[0] == '_' or std.mem.indexOfScalar(u8, name, '(') != null) continue;
            const value_text = std.mem.trim(u8, rest[name.len..], " \t");
            if (normalizeIntegerValue(allocator, value_text)) |value| {
                try macros.put(allocator, try allocator.dupe(u8, name), .{
                    .name = try allocator.dupe(u8, name),
                    .value = value,
                });
            }
        }
    }
}

fn normalizeIntegerValue(allocator: std.mem.Allocator, text: []const u8) ?[]const u8 {
    if (text.len == 0) return null;
    const trimmed = std.mem.trim(u8, text, " \t()");
    if (trimmed.len == 0) return null;
    var end = trimmed.len;
    while (end > 0) {
        const ch = trimmed[end - 1];
        if (ch == 'u' or ch == 'U' or ch == 'l' or ch == 'L') {
            end -= 1;
            continue;
        }
        break;
    }
    const candidate = trimmed[0..end];
    const signed_value = std.fmt.parseInt(i64, candidate, 0) catch {
        const unsigned_value = std.fmt.parseInt(u64, candidate, 0) catch return null;
        return std.fmt.allocPrint(allocator, "{d}", .{unsigned_value}) catch null;
    };
    return std.fmt.allocPrint(allocator, "{d}", .{signed_value}) catch null;
}
