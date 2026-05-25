const std = @import("std");

nanoseconds: u64,

const Duration = @This();

pub fn parse(text: []const u8) ?Duration {
    if (text.len == 0) return null;
    if (std.mem.endsWith(u8, text, "ms")) {
        return parseScaled(text[0 .. text.len - 2], std.time.ns_per_ms);
    }
    if (std.mem.endsWith(u8, text, "s")) {
        return parseScaled(text[0 .. text.len - 1], std.time.ns_per_s);
    }
    return parseScaled(text, std.time.ns_per_s);
}

fn parseScaled(number: []const u8, scale: u64) ?Duration {
    if (number.len == 0) return null;
    const parsed = std.fmt.parseInt(u64, number, 10) catch return null;
    if (parsed == 0) return null;
    return .{ .nanoseconds = std.math.mul(u64, parsed, scale) catch return null };
}

pub fn appendArgs(self: Duration, allocator: std.mem.Allocator, list: *std.array_list.Managed([]const u8)) !void {
    const millis = self.nanoseconds / std.time.ns_per_ms;
    try list.append(try std.fmt.allocPrint(allocator, "{d}ms", .{millis}));
}

test "parse duration accepts seconds milliseconds and plain seconds" {
    try std.testing.expectEqual(@as(u64, 5 * std.time.ns_per_s), parse("5s").?.nanoseconds);
    try std.testing.expectEqual(@as(u64, 5000 * std.time.ns_per_ms), parse("5000ms").?.nanoseconds);
    try std.testing.expectEqual(@as(u64, 5 * std.time.ns_per_s), parse("5").?.nanoseconds);
    try std.testing.expect(parse("0s") == null);
    try std.testing.expect(parse("soon") == null);
}
