const std = @import("std");
const fetch_libffi = @import("fetch_libffi.zig");

pub fn main(init: std.process.Init.Minimal) !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var stdout_buffer: [4096]u8 = undefined;
    var stderr_buffer: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writer(std.Options.debug_io, &stdout_buffer);
    defer stdout.interface.flush() catch {};
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};

    const build_options = @import("fetch_libffi_build_options");
    const raw_args = try init.args.toSlice(allocator);
    const args = try allocator.alloc([]const u8, raw_args.len);
    for (raw_args, 0..) |arg, index| args[index] = arg;

    run(allocator, args[1..], &stdout.interface, &stderr.interface, build_options.repo_root) catch |err| {
        if (!(err == error.InvalidLibffiMetadata or
            err == error.UnsupportedLibffiHost or
            err == error.LibffiTargetNotPublished or
            err == error.GitHubReleaseTagNotFound or
            err == error.GitHubReleaseAssetNotFound or
            err == error.GitHubReleaseLookupFailed or
            err == error.GitHubReleaseTagMismatch or
            err == error.GitHubAssetDownloadFailed or
            err == error.InvalidInstalledToolchain or
            err == error.LibffiArchiveNotFound or
            err == error.InvalidArguments))
        {
            try stderr.interface.print("fetch-libffi failed: {s}\n", .{@errorName(err)});
        }
        return error.FetchLibffiFailed;
    };
}

fn run(
    allocator: std.mem.Allocator,
    args: []const []const u8,
    stdout: anytype,
    stderr: anytype,
    repo_root: []const u8,
) !void {
    if (args.len == 0) return fetch_libffi.run(allocator, stdout, stderr, repo_root);
    if (args.len == 2 and std.mem.eql(u8, args[0], "--ci-metadata") and std.mem.eql(u8, args[1], "--json")) {
        return fetch_libffi.printCiMetadataJson(allocator, stdout, stderr, repo_root);
    }
    if (args.len == 2 and std.mem.eql(u8, args[0], "--archive")) {
        return fetch_libffi.installArchive(allocator, stdout, stderr, repo_root, args[1]);
    }
    try stderr.writeAll("usage: zig build fetch-libffi -- [--ci-metadata --json | --archive <path>]\n");
    return error.InvalidArguments;
}
