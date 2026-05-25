const std = @import("std");
const kira_live = @import("kira_live");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    try kira_live.execute(allocator, args, stdout, stderr);
}

pub fn executeRunner(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    _ = stdout;
    _ = stderr;
    if (args.len != 1) return error.InvalidArguments;
    try kira_live.runFromManifestPath(allocator, args[0]);
}
