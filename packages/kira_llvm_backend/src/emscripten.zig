const builtin = @import("builtin");
const std = @import("std");
const native = @import("kira_native_lib_definition");
const backend_utils = @import("backend_utils.zig");

pub fn isSelector(target_selector: ?native.TargetSelector) bool {
    const value = target_selector orelse return false;
    return std.mem.eql(u8, value.architecture, "wasm32") and
        std.mem.eql(u8, value.operating_system, "emscripten");
}

pub fn selector(allocator: std.mem.Allocator) !native.TargetSelector {
    return .{
        .architecture = try allocator.dupe(u8, "wasm32"),
        .operating_system = try allocator.dupe(u8, "emscripten"),
        .abi = try allocator.dupe(u8, "unknown"),
    };
}

pub fn emccPath(allocator: std.mem.Allocator) ![]const u8 {
    if (try envVarOwned(allocator, "EMCC")) |path| return path;
    if (try envVarOwned(allocator, "EMSDK")) |root| {
        defer allocator.free(root);
        const candidate = try std.fs.path.join(allocator, &.{ root, "upstream", "emscripten", emccExecutableName() });
        if (fileExists(candidate)) return candidate;
        allocator.free(candidate);
    }
    return allocator.dupe(u8, emccExecutableName());
}

pub fn validateAvailable(allocator: std.mem.Allocator) !void {
    const emcc = try emccPath(allocator);
    defer allocator.free(emcc);
    const process_environ = backend_utils.inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = std.process.run(allocator, io_impl.io(), .{
        .argv = &.{ emcc, "--version" },
        .expand_arg0 = .expand,
        .stdout_limit = .limited(64 * 1024),
        .stderr_limit = .limited(64 * 1024),
    }) catch |err| switch (err) {
        error.FileNotFound => return error.EmscriptenUnavailable,
        else => return err,
    };
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);
    if (result.term == .exited and result.term.exited == 0) return;
    return error.EmscriptenUnavailable;
}

fn emccExecutableName() []const u8 {
    return if (builtin.os.tag == .windows) "emcc.bat" else "emcc";
}

fn envVarOwned(allocator: std.mem.Allocator, name: []const u8) !?[]const u8 {
    if (!@import("builtin").link_libc) return null;
    const name_z = try allocator.dupeZ(u8, name);
    defer allocator.free(name_z);
    const raw = std.c.getenv(name_z) orelse return null;
    return try allocator.dupe(u8, std.mem.span(raw));
}

fn fileExists(path: []const u8) bool {
    var file = std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

test "wasm32 emscripten selector is explicit" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    try std.testing.expect(isSelector(try selector(arena.allocator())));
}
