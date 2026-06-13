const builtin = @import("builtin");
const std = @import("std");
const native = @import("kira_native_lib_definition");
const llvm_backend = @import("kira_llvm_backend");

pub fn dumpAst(
    allocator: std.mem.Allocator,
    library: native.ResolvedNativeLibrary,
    headers: []const []const u8,
    filter: ?[]const u8,
) ![]const u8 {
    const llvm_toolchain = try llvm_backend.LlvmToolchain.discover(allocator);
    const clang_path = try llvm_toolchain.clangPath(allocator);
    defer allocator.free(clang_path);
    var environ_map = try llvm_toolchain.processEnvironMap(allocator);
    defer environ_map.deinit();

    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.appendSlice(&.{ clang_path, "-Xclang", "-ast-dump=json", "-fsyntax-only" });
    if (filter) |value| {
        try argv.append("-Xclang");
        try argv.append(try std.fmt.allocPrint(allocator, "-ast-dump-filter={s}", .{value}));
    }
    try llvm_backend.clangDriver.appendHostClangDriverArgs(allocator, &argv);

    if (library.headers.entrypoint) |entrypoint| {
        try argv.append(entrypoint);
    } else if (headers.len > 0) {
        try argv.append(headers[0]);
    } else {
        return error.MissingAutobindingHeader;
    }
    for (library.headers.include_dirs) |include_dir| {
        try argv.append(try std.fmt.allocPrint(allocator, "-I{s}", .{include_dir}));
    }
    for (library.headers.defines) |define| {
        try argv.append(try std.fmt.allocPrint(allocator, "-D{s}", .{define}));
    }

    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = argv.items,
        .expand_arg0 = .expand,
        .environ_map = &environ_map,
        // Whole-API dumps of Vulkan- and D3D12-scale headers produce very large
        // filtered AST JSON streams; the limit must absorb them.
        .stdout_limit = .limited(1024 * 1024 * 1024),
        .stderr_limit = .limited(64 * 1024 * 1024),
    });
    defer allocator.free(result.stderr);

    if (result.term != .exited or result.term.exited != 0) {
        allocator.free(result.stdout);
        return error.ClangAutobindingFailed;
    }

    return result.stdout;
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
