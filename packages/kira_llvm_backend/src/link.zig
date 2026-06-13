const std = @import("std");
const builtin = @import("builtin");
const native = @import("kira_native_lib_definition");
const build_options = @import("kira_llvm_build_options");
const backend_utils = @import("backend_utils.zig");
const clang_driver = @import("clang_driver.zig");
const emscripten = @import("emscripten.zig");
const toolchain = @import("toolchain.zig");

pub fn buildRuntimeHelpersObject(
    allocator: std.mem.Allocator,
    object_path: []const u8,
    pic: bool,
    selector: ?native.TargetSelector,
) ![]const u8 {
    const helper_object = try helperObjectPath(allocator, object_path);
    const helper_source = try std.fs.path.join(allocator, &.{ build_options.repo_root, "packages", "kira_native_bridge", "src", "runtime_helpers.c" });
    try compileHelperSource(allocator, helper_source, helper_object, pic, selector);
    return helper_object;
}

pub fn buildDynamicFfiHelpersObject(
    allocator: std.mem.Allocator,
    object_path: []const u8,
    pic: bool,
    selector: ?native.TargetSelector,
) ![]const u8 {
    const helper_object = try siblingObjectPath(allocator, object_path, ".dynamic_ffi");
    const helper_source = try std.fs.path.join(allocator, &.{ build_options.repo_root, "packages", "kira_native_bridge", "src", "dynamic_ffi_helpers.c" });
    try compileHelperSource(allocator, helper_source, helper_object, pic, selector);
    return helper_object;
}

fn compileHelperSource(
    allocator: std.mem.Allocator,
    helper_source: []const u8,
    helper_object: []const u8,
    pic: bool,
    selector: ?native.TargetSelector,
) !void {
    const driver_path = try compilerDriverPathForSelector(allocator, selector);
    try ensureParentDir(helper_object);
    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.append(driver_path);
    try clang_driver.appendClangDriverArgs(allocator, &argv, selector);
    if (pic and builtin.os.tag != .windows) try argv.append("-fPIC");
    try argv.appendSlice(&.{ "-c", helper_source, "-o", helper_object });
    try runCommand(allocator, argv.items);
}

pub fn linkExecutable(
    allocator: std.mem.Allocator,
    executable_path: []const u8,
    object_paths: []const []const u8,
    native_libraries: []const native.ResolvedNativeLibrary,
    selector: ?native.TargetSelector,
) !void {
    try ensureParentDir(executable_path);
    const driver_path = try compilerDriverPathForSelector(allocator, selector);
    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.append(driver_path);
    try clang_driver.appendClangDriverArgs(allocator, &argv, selector);
    try argv.appendSlice(&.{ "-o", executable_path });
    if (builtin.os.tag == .windows) {
        try argv.append("-Wl,/subsystem:console");
    }
    try appendNativeLibraryPaths(allocator, &argv);
    for (object_paths) |path| try argv.append(path);

    for (native_libraries) |library| {
        if (library.artifact_path.len != 0) try argv.append(library.artifact_path);
        for (library.link.system_libs) |system_lib| {
            try argv.append(try std.fmt.allocPrint(allocator, "-l{s}", .{system_lib}));
        }
        for (library.link.frameworks) |framework| {
            try argv.appendSlice(&.{ "-framework", framework });
        }
    }
    try appendDefaultSystemLibraries(&argv, selector);

    try runCommand(allocator, argv.items);
}

pub fn linkSharedLibrary(
    allocator: std.mem.Allocator,
    library_path: []const u8,
    object_paths: []const []const u8,
    native_libraries: []const native.ResolvedNativeLibrary,
    selector: ?native.TargetSelector,
) !void {
    try ensureParentDir(library_path);
    const driver_path = try compilerDriverPathForSelector(allocator, selector);
    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.append(driver_path);
    try clang_driver.appendClangDriverArgs(allocator, &argv, selector);
    try argv.appendSlice(&.{ "-shared", "-o", library_path });
    try appendNativeLibraryPaths(allocator, &argv);
    for (object_paths) |path| try argv.append(path);

    for (native_libraries) |library| {
        if (library.artifact_path.len != 0) try argv.append(library.artifact_path);
        for (library.link.system_libs) |system_lib| {
            try argv.append(try std.fmt.allocPrint(allocator, "-l{s}", .{system_lib}));
        }
        for (library.link.frameworks) |framework| {
            try argv.appendSlice(&.{ "-framework", framework });
        }
    }
    try appendDefaultSystemLibraries(&argv, selector);

    try runCommand(allocator, argv.items);
}

fn appendDefaultSystemLibraries(argv: *std.array_list.Managed([]const u8), selector: ?native.TargetSelector) !void {
    if (emscripten.isSelector(selector)) return;
    if (builtin.os.tag == .linux) try argv.append("-lm");
}

fn appendNativeLibraryPaths(allocator: std.mem.Allocator, argv: *std.array_list.Managed([]const u8)) !void {
    var environ_map = try std.process.Environ.createMap(backend_utils.inheritedProcessEnviron(), allocator);
    defer environ_map.deinit();
    const value = environ_map.get("KIRA_NATIVE_LIBRARY_PATH") orelse return;
    var paths = std.mem.splitScalar(u8, value, std.fs.path.delimiter);
    while (paths.next()) |path| {
        if (path.len != 0) try argv.append(try std.fmt.allocPrint(allocator, "-L{s}", .{path}));
    }
}

fn compilerDriverPathForSelector(allocator: std.mem.Allocator, selector: ?native.TargetSelector) ![]const u8 {
    if (emscripten.isSelector(selector)) return emscripten.emccPath(allocator);
    if (try clang_driver.appleClangPathForSelector(allocator, selector)) |path| return path;
    const llvm_toolchain = try toolchain.Toolchain.discover(allocator);
    return llvm_toolchain.compilerDriverPath(allocator);
}

fn helperObjectPath(allocator: std.mem.Allocator, object_path: []const u8) ![]const u8 {
    return siblingObjectPath(allocator, object_path, ".bridge");
}

fn siblingObjectPath(allocator: std.mem.Allocator, object_path: []const u8, suffix: []const u8) ![]const u8 {
    const ext = std.fs.path.extension(object_path);
    if (ext.len == 0) return std.fmt.allocPrint(allocator, "{s}{s}.o", .{ object_path, suffix });
    const stem = object_path[0 .. object_path.len - ext.len];
    return std.fmt.allocPrint(allocator, "{s}{s}{s}", .{ stem, suffix, ext });
}

fn runCommand(allocator: std.mem.Allocator, argv: []const []const u8) !void {
    const llvm_toolchain = try toolchain.Toolchain.discover(allocator);
    var environ_map = try llvm_toolchain.processEnvironMap(allocator);
    defer environ_map.deinit();
    const process_environ = backend_utils.inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = argv,
        .expand_arg0 = .expand,
        .environ_map = &environ_map,
        .stdout_limit = .limited(512 * 1024),
        .stderr_limit = .limited(512 * 1024),
    });
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);

    if (result.term == .exited and result.term.exited == 0) return;
    if (result.stdout.len != 0) std.debug.print("{s}", .{result.stdout});
    if (result.stderr.len != 0) std.debug.print("{s}", .{result.stderr});
    return error.ExternalCommandFailed;
}

fn ensureParentDir(path: []const u8) !void {
    const maybe_dir = std.fs.path.dirname(path) orelse return;
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, maybe_dir);
}
