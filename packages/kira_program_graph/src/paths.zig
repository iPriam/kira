const std = @import("std");
const builtin = @import("builtin");

pub fn canonicalizeExistingPath(allocator: std.mem.Allocator, path: []const u8) ![]u8 {
    if (std.fs.path.isAbsolute(path)) {
        var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, std.fs.path.dirname(path) orelse ".", .{}) catch return allocator.dupe(u8, path);
        defer dir.close(std.Options.debug_io);
        return realPathFileAlloc(allocator, dir, std.fs.path.basename(path));
    }
    return realPathFileAlloc(allocator, std.Io.Dir.cwd(), path);
}

/// Generated FFI bindings live in `<package>/bindings`, a sibling of the `app/`
/// source root, so hand-written and generated sources never share a directory.
pub fn bindingsRootForSourceRoot(allocator: std.mem.Allocator, source_root: []const u8) ![]u8 {
    const package_root = std.fs.path.dirname(source_root) orelse return allocator.dupe(u8, source_root);
    const joined = try std.fs.path.join(allocator, &.{ package_root, "bindings" });
    defer allocator.free(joined);
    return canonicalizeSourceRoot(allocator, joined);
}

pub fn canonicalizeSourceRoot(allocator: std.mem.Allocator, root_path: []const u8) ![]u8 {
    if (dirExists(root_path)) return canonicalizeDirectory(allocator, root_path);
    return absolutizeLexical(allocator, root_path);
}

pub fn canonicalizeDirectory(allocator: std.mem.Allocator, path: []const u8) ![]u8 {
    if (std.fs.path.isAbsolute(path)) {
        var parent = std.Io.Dir.openDirAbsolute(std.Options.debug_io, std.fs.path.dirname(path) orelse ".", .{}) catch return allocator.dupe(u8, path);
        defer parent.close(std.Options.debug_io);
        return realPathFileAlloc(allocator, parent, std.fs.path.basename(path));
    }
    return realPathFileAlloc(allocator, std.Io.Dir.cwd(), path);
}

pub fn absolutizeLexical(allocator: std.mem.Allocator, path: []const u8) ![]u8 {
    if (std.fs.path.isAbsolute(path)) return allocator.dupe(u8, path);
    const cwd = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, ".", allocator);
    defer allocator.free(cwd);
    return std.fs.path.join(allocator, &.{ cwd, path });
}

fn realPathFileAlloc(allocator: std.mem.Allocator, dir: std.Io.Dir, path: []const u8) ![]u8 {
    const canonical_z = try dir.realPathFileAlloc(std.Options.debug_io, path, allocator);
    defer allocator.free(canonical_z);
    return allocator.dupe(u8, canonical_z);
}

pub fn fileExists(path: []const u8) bool {
    if (std.fs.path.isAbsolute(path)) {
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false;
        file.close(std.Options.debug_io);
        return true;
    }

    var file = std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

pub fn dirExists(path: []const u8) bool {
    if (std.fs.path.isAbsolute(path)) {
        var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
        dir.close(std.Options.debug_io);
        return true;
    }

    var dir = std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

pub fn pathWithinRoot(path: []const u8, root: []const u8) bool {
    if (pathEql(path, root)) return true;
    if (path.len <= root.len) return false;
    if (!pathStartsWith(path, root)) return false;
    return isSeparator(path[root.len]);
}

pub fn pathEql(left: []const u8, right: []const u8) bool {
    if (builtin.os.tag == .windows) return std.ascii.eqlIgnoreCase(left, right);
    return std.mem.eql(u8, left, right);
}

fn pathStartsWith(path: []const u8, prefix: []const u8) bool {
    if (path.len < prefix.len) return false;
    const head = path[0..prefix.len];
    if (builtin.os.tag == .windows) return std.ascii.eqlIgnoreCase(head, prefix);
    return std.mem.eql(u8, head, prefix);
}

fn isSeparator(ch: u8) bool {
    return ch == '/' or ch == '\\';
}

test "pathWithinRoot requires a component boundary" {
    try std.testing.expect(pathWithinRoot("C:\\pkg\\app\\main.kira", "C:\\pkg\\app"));
    try std.testing.expect(!pathWithinRoot("C:\\pkg\\application\\main.kira", "C:\\pkg\\app"));
}
