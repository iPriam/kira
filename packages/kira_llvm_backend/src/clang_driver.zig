const std = @import("std");
const builtin = @import("builtin");
const kira_toolchain = @import("kira_toolchain");
const native = @import("kira_native_lib_definition");

pub const AppleSdk = enum {
    macosx,
    iphoneos,
};

const DriverTarget = struct {
    triple: []const u8,
    sdk: ?AppleSdk = null,
};

pub fn appendHostClangDriverArgs(
    allocator: std.mem.Allocator,
    argv: *std.array_list.Managed([]const u8),
) !void {
    return appendClangDriverArgs(allocator, argv, null);
}

pub fn appendClangDriverArgs(
    allocator: std.mem.Allocator,
    argv: *std.array_list.Managed([]const u8),
    selector: ?native.TargetSelector,
) !void {
    const target = try driverTarget(allocator, selector);
    try argv.appendSlice(&.{ "-target", target.triple });

    if (target.sdk) |sdk| {
        const sdk_path = try appleSdkPath(allocator, sdk);
        try argv.appendSlice(&.{ "-isysroot", sdk_path });
    }
}

pub fn hostClangTargetTriple(allocator: std.mem.Allocator) ![]const u8 {
    return clangTargetTripleForSelector(allocator, null);
}

pub fn clangTargetTripleForSelector(allocator: std.mem.Allocator, selector: ?native.TargetSelector) ![]const u8 {
    return (try driverTarget(allocator, selector)).triple;
}

fn driverTarget(allocator: std.mem.Allocator, selector: ?native.TargetSelector) !DriverTarget {
    if (selector) |value| {
        if (std.mem.eql(u8, value.operating_system, "windows")) {
            if (!std.mem.eql(u8, value.architecture, "x86_64")) return error.UnsupportedTarget;
            return .{
                .triple = try allocator.dupe(u8, if (std.mem.eql(u8, value.abi, "gnu")) "x86_64-windows-gnu" else "x86_64-windows-msvc"),
            };
        }
        if (std.mem.eql(u8, value.operating_system, "linux")) {
            if (!std.mem.eql(u8, value.architecture, "x86_64") or !std.mem.eql(u8, value.abi, "gnu")) return error.UnsupportedTarget;
            return .{ .triple = try allocator.dupe(u8, "x86_64-linux-gnu") };
        }
        if (std.mem.eql(u8, value.operating_system, "macos")) {
            if (std.mem.eql(u8, value.architecture, "aarch64")) {
                return .{
                    .triple = try allocator.dupe(u8, "arm64-apple-macosx"),
                    .sdk = .macosx,
                };
            }
            if (std.mem.eql(u8, value.architecture, "x86_64")) {
                return .{
                    .triple = try allocator.dupe(u8, "x86_64-apple-macosx"),
                    .sdk = .macosx,
                };
            }
            return error.UnsupportedTarget;
        }
        if (std.mem.eql(u8, value.operating_system, "ios")) {
            if (!std.mem.eql(u8, value.architecture, "aarch64")) return error.UnsupportedTarget;
            return .{
                .triple = try allocator.dupe(u8, "arm64-apple-ios13.0"),
                .sdk = .iphoneos,
            };
        }
        return error.UnsupportedTarget;
    }

    return switch (builtin.os.tag) {
        .windows => switch (builtin.cpu.arch) {
            .x86_64 => .{ .triple = try allocator.dupe(u8, if (builtin.abi == .gnu) "x86_64-windows-gnu" else "x86_64-windows-msvc") },
            else => error.UnsupportedTarget,
        },
        .macos => switch (builtin.cpu.arch) {
            .aarch64 => .{ .triple = try allocator.dupe(u8, "arm64-apple-macosx"), .sdk = .macosx },
            .x86_64 => .{ .triple = try allocator.dupe(u8, "x86_64-apple-macosx"), .sdk = .macosx },
            else => error.UnsupportedTarget,
        },
        .linux => switch (builtin.cpu.arch) {
            .x86_64 => .{ .triple = try allocator.dupe(u8, "x86_64-linux-gnu") },
            else => error.UnsupportedTarget,
        },
        else => error.UnsupportedTarget,
    };
}

pub fn macOSSdkPath(allocator: std.mem.Allocator) ![]const u8 {
    return appleSdkPath(allocator, .macosx);
}

pub fn appleSdkPath(allocator: std.mem.Allocator, sdk: AppleSdk) ![]const u8 {
    if (builtin.os.tag != .macos) return error.UnsupportedTarget;

    if (kira_toolchain.envVarOwned(allocator, "SDKROOT")) |sdkroot| {
        if (sdkroot.len != 0 and directoryExists(sdkroot)) return sdkroot;
        allocator.free(sdkroot);
    } else |_| {}

    if (kira_toolchain.envVarOwned(allocator, "DEVELOPER_DIR")) |developer_dir| {
        if (try sdkPathFromDeveloperDir(allocator, developer_dir, sdk)) |path| return path;
        allocator.free(developer_dir);
    } else |_| {}

    if (try findBundledXcodeSdk(allocator, sdk)) |path| return path;

    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = &.{ "xcrun", "--sdk", @tagName(sdk), "--show-sdk-path" },
        .expand_arg0 = .expand,
        .stdout_limit = .limited(16 * 1024),
        .stderr_limit = .limited(16 * 1024),
    });
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);

    if (result.term == .exited and result.term.exited == 0) {
        const sdk_path = std.mem.trim(u8, result.stdout, " \t\r\n");
        if (sdk_path.len != 0 and directoryExists(sdk_path)) {
            return allocator.dupe(u8, sdk_path);
        }
    }

    if (result.stderr.len != 0) std.debug.print("{s}", .{result.stderr});
    return switch (sdk) {
        .macosx => error.MacOSSdkUnavailable,
        .iphoneos => error.IPhoneOSSdkUnavailable,
    };
}

fn sdkPathFromDeveloperDir(allocator: std.mem.Allocator, developer_dir: []const u8, sdk: AppleSdk) !?[]const u8 {
    const sdk_dir = try std.fs.path.join(allocator, &.{ developer_dir, "Platforms", if (sdk == .macosx) "MacOSX.platform" else "iPhoneOS.platform", "Developer", "SDKs" });
    defer allocator.free(sdk_dir);
    if (!directoryExists(sdk_dir)) return null;
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, sdk_dir, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .directory or !std.mem.endsWith(u8, entry.name, ".sdk")) continue;
        return @as([]const u8, try std.fs.path.join(allocator, &.{ sdk_dir, entry.name }));
    }
    return null;
}

fn findBundledXcodeSdk(allocator: std.mem.Allocator, sdk: AppleSdk) !?[]const u8 {
    const candidates = [_][]const u8{
        "/Applications/Xcode.app/Contents/Developer",
        "/Applications/Xcode-26.5.0.app/Contents/Developer",
    };
    for (candidates) |candidate| {
        if (!directoryExists(candidate)) continue;
        if (try sdkPathFromDeveloperDir(allocator, candidate, sdk)) |path| return path;
    }
    return null;
}

fn directoryExists(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch
        std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
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
