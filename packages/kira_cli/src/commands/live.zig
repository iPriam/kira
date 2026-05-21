const std = @import("std");
const kira_live = @import("kira_live");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    try kira_live.execute(allocator, args, stdout, stderr);
}
