const std = @import("std");

pub fn makePath(path: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, path);
}

pub fn fileExists(path: []const u8) bool {
    var file = if (std.fs.path.isAbsolute(path))
        std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false
    else
        std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

pub fn readFileAlloc(path: []const u8, allocator: std.mem.Allocator, limit: usize) ![]u8 {
    if (std.fs.path.isAbsolute(path)) {
        const parent_path = std.fs.path.dirname(path) orelse return error.FileNotFound;
        const base_name = std.fs.path.basename(path);
        var parent_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, parent_path, .{});
        defer parent_dir.close(std.Options.debug_io);
        return parent_dir.readFileAlloc(std.Options.debug_io, base_name, allocator, .limited(limit));
    }
    return std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(limit));
}

pub fn writeFile(path: []const u8, data: []const u8) !void {
    const maybe_dir = std.fs.path.dirname(path) orelse ".";
    try makePath(maybe_dir);
    if (std.fs.path.isAbsolute(path)) {
        const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
        defer file.close(std.Options.debug_io);
        try file.writeStreamingAll(std.Options.debug_io, data);
        return;
    }
    try std.Io.Dir.cwd().writeFile(std.Options.debug_io, .{
        .sub_path = path,
        .data = data,
    });
}

pub fn statFile(path: []const u8) !std.Io.File.Stat {
    var file = if (std.fs.path.isAbsolute(path))
        try std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{})
    else
        try std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{});
    defer file.close(std.Options.debug_io);
    return file.stat(std.Options.debug_io);
}
