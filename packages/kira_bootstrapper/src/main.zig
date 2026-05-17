const std = @import("std");
const kira_toolchain = @import("kira_toolchain");
const release_install = @import("release_install.zig");

pub fn main(init: std.process.Init) !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const raw_args = try init.minimal.args.toSlice(allocator);
    const args = try allocator.alloc([]const u8, raw_args.len);
    for (raw_args, 0..) |arg, index| args[index] = arg;
    const current_path = try kira_toolchain.currentToolchainPath(allocator);
    var current = blk: {
        break :blk loadCurrentToolchain(allocator, current_path) catch |err| switch (err) {
            error.FileNotFound => {
                if (!release_install.canAutoInstallManagedToolchain()) {
                    try printMissingToolchain(current_path);
                    std.process.exit(1);
                }
                try installReleaseToolchainOrExit(allocator, current_path);
                break :blk try loadCurrentToolchain(allocator, current_path);
            },
            error.InvalidCurrentToolchain => {
                if (!release_install.canAutoInstallManagedToolchain()) {
                    try printBrokenToolchain(current_path);
                    std.process.exit(1);
                }
                try installReleaseToolchainOrExit(allocator, current_path);
                break :blk try loadCurrentToolchain(allocator, current_path);
            },
            else => return err,
        };
    };
    defer current.deinit(allocator);

    if (release_install.canAutoInstallManagedToolchain() and
        (current.channel != .release or !std.mem.eql(u8, current.version, release_install.managedReleaseVersion())))
    {
        try installReleaseToolchainOrExit(allocator, current_path);
        current.deinit(allocator);
        current = try loadCurrentToolchain(allocator, current_path);
    }

    var executable_path = try kira_toolchain.managedPrimaryBinaryPath(
        allocator,
        current.channel,
        current.version,
        current.primary,
    );
    if (!managedExecutableExists(executable_path)) {
        if (release_install.canAutoInstallManagedToolchain()) {
            try installReleaseToolchainOrExit(allocator, current_path);
            current.deinit(allocator);
            current = try loadCurrentToolchain(allocator, current_path);
            allocator.free(executable_path);
            executable_path = try kira_toolchain.managedPrimaryBinaryPath(
                allocator,
                current.channel,
                current.version,
                current.primary,
            );
        } else {
            try printMissingExecutable(executable_path);
            std.process.exit(1);
        }
    }

    var child_args = try allocator.alloc([]const u8, args.len);
    child_args[0] = executable_path;
    for (args[1..], 1..) |arg, index| child_args[index] = arg;

    var child = std.process.spawn(init.io, .{
        .argv = child_args,
        .stdin = .inherit,
        .stdout = .inherit,
        .stderr = .inherit,
    }) catch {
        try printMissingExecutable(executable_path);
        std.process.exit(1);
    };
    const term = try child.wait(init.io);

    switch (term) {
        .exited => |code| std.process.exit(code),
        .signal => |signal| {
            std.debug.print("kira-bootstrapper: child terminated by signal {d}\n", .{signal});
            std.process.exit(1);
        },
        else => std.process.exit(1),
    }
}

fn loadCurrentToolchain(allocator: std.mem.Allocator, current_path: []const u8) !kira_toolchain.CurrentToolchain {
    const current_contents = std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, current_path, allocator, .limited(4 * 1024)) catch |err| switch (err) {
        error.FileNotFound => return error.FileNotFound,
        else => return err,
    };
    return kira_toolchain.parseCurrentToolchainToml(allocator, current_contents) catch error.InvalidCurrentToolchain;
}

fn installReleaseToolchainOrExit(allocator: std.mem.Allocator, current_path: []const u8) !void {
    if (!release_install.canAutoInstallManagedToolchain()) {
        try printMissingToolchain(current_path);
        std.process.exit(1);
    }

    var stderr_buffer: [4096]u8 = undefined;
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};

    release_install.installManagedReleaseToolchain(allocator, &stderr.interface) catch |install_err| {
        try stderr.interface.print("kira-bootstrapper could not install the managed release toolchain: {s}\n", .{@errorName(install_err)});
        try stderr.interface.flush();
        try printMissingToolchain(current_path);
        std.process.exit(1);
    };
}

fn managedExecutableExists(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn printMissingToolchain(current_path: []const u8) !void {
    var stderr_buffer: [4096]u8 = undefined;
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};
    try stderr.interface.print(
        "kira-bootstrapper could not find {s}\nhelp: run `zig build install-kirac` to install a Kira toolchain and activate it\n",
        .{current_path},
    );
}

fn printBrokenToolchain(current_path: []const u8) !void {
    var stderr_buffer: [4096]u8 = undefined;
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};
    try stderr.interface.print(
        "kira-bootstrapper found an invalid toolchain manifest at {s}\nhelp: run `zig build install-kirac` to refresh the active toolchain\n",
        .{current_path},
    );
}

fn printBrokenLlvmToolchain() !void {
    var stderr_buffer: [4096]u8 = undefined;
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};
    try stderr.interface.writeAll(
        "kira-bootstrapper found a broken managed LLVM toolchain install\nhelp: run `kira-bootstrapper fetch-llvm` to reinstall the pinned LLVM and Clang bundle\n",
    );
}

fn printMissingExecutable(executable_path: []const u8) !void {
    var stderr_buffer: [4096]u8 = undefined;
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};
    try stderr.interface.print(
        "kira-bootstrapper could not launch the active Kira executable at {s}\nhelp: run `zig build install-kirac` to reinstall the active toolchain\n",
        .{executable_path},
    );
}
