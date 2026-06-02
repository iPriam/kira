const std = @import("std");
const builtin = @import("builtin");
const build_def = @import("kira_build_definition");
const llvm_backend = @import("kira_llvm_backend");
const BuildSystem = @import("build_system.zig").BuildSystem;

test "wasm32 emscripten build runs real Kira entrypoint through node" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    const process_allocator = std.heap.smp_allocator;
    try ensureRuntimeToolingAvailable(process_allocator);

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "App/app");
    try tmp.dir.createDirPath(std.testing.io, "out");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/app/main.kira",
        .data =
        \\@Main
        \\function main() {
        \\    print("wasm-entrypoint-ok");
        \\    return;
        \\}
        ,
    });

    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app/main.kira", allocator);
    const output_root = try tmp.dir.realPathFileAlloc(std.testing.io, "out", allocator);
    const output_path = try std.fs.path.join(allocator, &.{ output_root, "main.js" });

    var system = BuildSystem.init(allocator);
    system.use_cache = false;
    const outcome = try system.build(.{
        .source_path = source_path,
        .output_path = output_path,
        .target = build_def.BuildTarget{ .execution = .wasm32_emscripten },
    });

    try std.testing.expect(!outcome.failed());
    try std.testing.expect(hasArtifact(outcome.artifacts, output_path));
    try std.testing.expect(hasArtifact(outcome.artifacts, try replaceExtension(allocator, output_path, ".wasm")));

    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(process_allocator, io_impl.io(), .{
        .argv = &.{ "node", output_path },
        .expand_arg0 = .expand,
        .stdout_limit = .limited(64 * 1024),
        .stderr_limit = .limited(64 * 1024),
    });
    defer process_allocator.free(result.stdout);
    defer process_allocator.free(result.stderr);

    try std.testing.expectEqual(@as(std.process.Child.Term, .{ .exited = 0 }), result.term);
    try std.testing.expect(std.mem.indexOf(u8, result.stdout, "wasm-entrypoint-ok") != null);
}

fn ensureRuntimeToolingAvailable(allocator: std.mem.Allocator) !void {
    llvm_backend.emscripten.validateAvailable(allocator) catch |err| switch (err) {
        error.EmscriptenUnavailable => return error.SkipZigTest,
        else => return err,
    };
    validateNodeAvailable(allocator) catch |err| switch (err) {
        error.NodeUnavailable => return error.SkipZigTest,
        else => return err,
    };
}

test "wasm32 emscripten reports host native library target exclusion" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "App/app");
    try tmp.dir.createDirPath(std.testing.io, "App/NativeLibs");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/kira.toml",
        .data =
        \\[package]
        \\name = "App"
        \\version = "0.1.0"
        \\kind = "app"
        \\kira = "0.1.0"
        \\native_libraries = ["NativeLibs/host_only.toml"]
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/app/main.kira",
        .data =
        \\@Main
        \\function main() {
        \\    print("host-only-native-lib");
        \\    return;
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/NativeLibs/host_only.toml",
        .data =
        \\[library]
        \\name = "host_only"
        \\link_mode = "static"
        \\abi = "c"
        \\
        \\[target.aarch64-macos-none]
        \\static_lib = "libhost_only.a"
        ,
    });

    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app/main.kira", allocator);
    var system = BuildSystem.init(allocator);
    system.use_cache = false;
    const result = try system.checkForBuildTarget(source_path, .{ .execution = .wasm32_emscripten });

    try std.testing.expect(result.failed());
    try std.testing.expectEqualStrings("KTC003", result.diagnostics[0].code.?);
    try std.testing.expectEqualStrings("unsupported native library target", result.diagnostics[0].title);
    try std.testing.expect(std.mem.indexOf(u8, result.diagnostics[0].message, "wasm32-emscripten-unknown") != null);
}

fn validateNodeAvailable(allocator: std.mem.Allocator) !void {
    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = std.process.run(allocator, io_impl.io(), .{
        .argv = &.{ "node", "--version" },
        .expand_arg0 = .expand,
        .stdout_limit = .limited(1024),
        .stderr_limit = .limited(1024),
    }) catch |err| switch (err) {
        error.FileNotFound => return error.NodeUnavailable,
        else => return err,
    };
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);

    if (result.term == .exited and result.term.exited == 0) return;
    return error.NodeUnavailable;
}

fn hasArtifact(artifacts: []const build_def.Artifact, path: []const u8) bool {
    for (artifacts) |artifact| {
        if (std.mem.eql(u8, artifact.path, path)) return true;
    }
    return false;
}

fn replaceExtension(allocator: std.mem.Allocator, path: []const u8, extension: []const u8) ![]const u8 {
    const ext = std.fs.path.extension(path);
    if (ext.len == 0) return std.fmt.allocPrint(allocator, "{s}{s}", .{ path, extension });
    return std.fmt.allocPrint(allocator, "{s}{s}", .{ path[0 .. path.len - ext.len], extension });
}

fn inheritedProcessEnviron() std.process.Environ {
    return switch (builtin.os.tag) {
        .windows => .{ .block = .global },
        .wasi, .emscripten, .freestanding, .other => .empty,
        else => .{ .block = .{ .slice = currentPosixEnvironBlock() } },
    };
}

fn currentPosixEnvironBlock() [:null]const ?[*:0]const u8 {
    if (!builtin.link_libc) return &.{};

    const environ = std.c.environ;
    var len: usize = 0;
    while (environ[len] != null) : (len += 1) {}
    return environ[0..len :null];
}
