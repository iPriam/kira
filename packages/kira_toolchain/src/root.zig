const std = @import("std");
const builtin = @import("builtin");

pub const CurrentToolchain = struct {
    channel: Channel,
    version: []const u8,
    primary: []const u8,

    pub fn deinit(self: CurrentToolchain, allocator: std.mem.Allocator) void {
        allocator.free(self.version);
        allocator.free(self.primary);
    }
};

pub const Channel = enum {
    release,
    dev,

    pub fn parse(text: []const u8) ?Channel {
        if (std.mem.eql(u8, text, "release")) return .release;
        if (std.mem.eql(u8, text, "dev")) return .dev;
        return null;
    }

    pub fn dirName(self: Channel) []const u8 {
        return @tagName(self);
    }
};

pub fn homeDir(allocator: std.mem.Allocator) ![]u8 {
    if (envVarOwned(allocator, "HOME")) |home| {
        return home;
    } else |_| {}

    if (envVarOwned(allocator, "USERPROFILE")) |home| {
        return home;
    } else |_| {}

    if (builtin.link_libc and builtin.os.tag != .windows and builtin.os.tag != .wasi) {
        if (std.c.getpwuid(std.c.getuid())) |entry| {
            if (entry.dir) |dir| {
                const value = std.mem.span(dir);
                if (value.len != 0) return allocator.dupe(u8, value);
            }
        }
    }

    return error.HomeDirectoryUnavailable;
}

pub fn envVarOwned(allocator: std.mem.Allocator, name: []const u8) ![]u8 {
    if (builtin.os.tag == .windows or (builtin.os.tag == .wasi and !builtin.link_libc)) {
        var environ = try std.process.Environ.createMap(.{ .block = .global }, allocator);
        defer environ.deinit();
        const value = environ.get(name) orelse return error.EnvironmentVariableNotFound;
        return allocator.dupe(u8, value);
    }

    if (builtin.link_libc) {
        const name_z = try allocator.dupeZ(u8, name);
        defer allocator.free(name_z);
        const value = std.c.getenv(name_z.ptr) orelse return error.EnvironmentVariableNotFound;
        return allocator.dupe(u8, std.mem.span(value));
    }

    return error.EnvironmentVariableNotFound;
}

pub fn kiraHome(allocator: std.mem.Allocator) ![]u8 {
    const home = try homeDir(allocator);
    defer allocator.free(home);
    return std.fs.path.join(allocator, &.{ home, ".kira" });
}

pub fn toolchainsRoot(allocator: std.mem.Allocator) ![]u8 {
    const home = try kiraHome(allocator);
    defer allocator.free(home);
    return std.fs.path.join(allocator, &.{ home, "toolchains" });
}

pub fn currentStateRoot(allocator: std.mem.Allocator) ![]u8 {
    return toolchainsRoot(allocator);
}

pub fn currentToolchainPath(allocator: std.mem.Allocator) ![]u8 {
    const root = try currentStateRoot(allocator);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, "current.toml" });
}

pub fn managedToolchainRoot(
    allocator: std.mem.Allocator,
    channel: Channel,
    version: []const u8,
) ![]u8 {
    const root = try toolchainsRoot(allocator);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, channel.dirName(), version });
}

pub fn managedBinaryDir(
    allocator: std.mem.Allocator,
    channel: Channel,
    version: []const u8,
) ![]u8 {
    const root = try managedToolchainRoot(allocator, channel, version);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, "bin" });
}

pub fn managedPrimaryBinaryPath(
    allocator: std.mem.Allocator,
    channel: Channel,
    version: []const u8,
    primary: []const u8,
) ![]u8 {
    const bin_dir = try managedBinaryDir(allocator, channel, version);
    defer allocator.free(bin_dir);
    const executable = try executableName(allocator, primary);
    defer allocator.free(executable);
    return std.fs.path.join(allocator, &.{ bin_dir, executable });
}

pub fn managedLlvmRoot(allocator: std.mem.Allocator) ![]u8 {
    const root = try toolchainsRoot(allocator);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, "llvm" });
}

pub fn managedLlvmVersionRoot(
    allocator: std.mem.Allocator,
    llvm_version: []const u8,
) ![]u8 {
    const root = try managedLlvmRoot(allocator);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, llvm_version });
}

pub fn managedLlvmHome(
    allocator: std.mem.Allocator,
    llvm_version: []const u8,
    host_key: []const u8,
) ![]u8 {
    const root = try managedLlvmVersionRoot(allocator, llvm_version);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, host_key });
}

pub fn managedLibffiRoot(allocator: std.mem.Allocator) ![]u8 {
    const root = try toolchainsRoot(allocator);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, "libffi" });
}

pub fn managedLibffiVersionRoot(
    allocator: std.mem.Allocator,
    libffi_version: []const u8,
) ![]u8 {
    const root = try managedLibffiRoot(allocator);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, libffi_version });
}

pub fn managedLibffiHome(
    allocator: std.mem.Allocator,
    libffi_version: []const u8,
    host_key: []const u8,
) ![]u8 {
    const root = try managedLibffiVersionRoot(allocator, libffi_version);
    defer allocator.free(root);
    return std.fs.path.join(allocator, &.{ root, host_key });
}

pub fn executableName(allocator: std.mem.Allocator, base: []const u8) ![]u8 {
    if (@import("builtin").os.tag == .windows) {
        return std.fmt.allocPrint(allocator, "{s}.exe", .{base});
    }
    return allocator.dupe(u8, base);
}

pub fn managedLlvmToolPath(allocator: std.mem.Allocator, llvm_home: []const u8, tool_name: []const u8) ![]u8 {
    const bin_dir = try std.fs.path.join(allocator, &.{ llvm_home, "bin" });
    defer allocator.free(bin_dir);
    const executable = try executableName(allocator, tool_name);
    defer allocator.free(executable);
    const path = try std.fs.path.join(allocator, &.{ bin_dir, executable });
    if (fileExists(path)) return path;

    std.debug.print(
        "LLVM toolchain at '{s}' is missing required tool '{s}'.\nExpected to find: {s}\n",
        .{ llvm_home, executable, path },
    );
    return error.LlvmToolUnavailable;
}

pub fn managedLlvmClangPath(allocator: std.mem.Allocator, llvm_home: []const u8) ![]u8 {
    return managedLlvmToolPath(allocator, llvm_home, "clang");
}

pub fn managedLlvmArPath(allocator: std.mem.Allocator, llvm_home: []const u8) ![]u8 {
    return managedLlvmToolPath(allocator, llvm_home, "llvm-ar");
}

pub fn toolchainRootFromExecutablePath(allocator: std.mem.Allocator, exe_path: []const u8) !?[]u8 {
    const exe_dir = std.fs.path.dirname(exe_path) orelse return null;
    if (!std.mem.eql(u8, std.fs.path.basename(exe_dir), "bin")) return null;

    const toolchain_root = std.fs.path.dirname(exe_dir) orelse return null;
    const metadata_path = try std.fs.path.join(allocator, &.{ toolchain_root, "llvm-metadata.toml" });
    defer allocator.free(metadata_path);
    if (!fileExists(metadata_path)) return null;

    return @as(?[]u8, try allocator.dupe(u8, toolchain_root));
}

pub fn toolchainRootFromSelfExecutable(allocator: std.mem.Allocator) !?[]u8 {
    const exe_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    defer allocator.free(exe_path);
    return toolchainRootFromExecutablePath(allocator, exe_path);
}

pub fn writeCurrentToolchainToml(
    writer: anytype,
    channel: Channel,
    version: []const u8,
    primary: []const u8,
) !void {
    try writer.print(
        \\channel = "{s}"
        \\version = "{s}"
        \\primary = "{s}"
        \\
    , .{ channel.dirName(), version, primary });
}

pub fn parseCurrentToolchainToml(allocator: std.mem.Allocator, contents: []const u8) !CurrentToolchain {
    const channel_value = try parseQuotedField(allocator, contents, "channel");
    defer allocator.free(channel_value);
    const version = try parseQuotedField(allocator, contents, "version");
    const primary = try parseQuotedField(allocator, contents, "primary");

    return .{
        .channel = Channel.parse(channel_value) orelse return error.InvalidCurrentToolchain,
        .version = version,
        .primary = primary,
    };
}

fn parseQuotedField(allocator: std.mem.Allocator, contents: []const u8, field_name: []const u8) ![]u8 {
    const field_index = std.mem.indexOf(u8, contents, field_name) orelse return error.InvalidCurrentToolchain;
    const after_field = contents[field_index + field_name.len ..];
    const equals_index = std.mem.indexOfScalar(u8, after_field, '=') orelse return error.InvalidCurrentToolchain;
    const after_equals = std.mem.trimStart(u8, after_field[equals_index + 1 ..], " \t\r\n");
    if (after_equals.len == 0 or after_equals[0] != '"') return error.InvalidCurrentToolchain;

    const closing_quote = std.mem.indexOfScalarPos(u8, after_equals, 1, '"') orelse return error.InvalidCurrentToolchain;
    return allocator.dupe(u8, after_equals[1..closing_quote]);
}

fn fileExists(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

test "writes and parses current toolchain toml" {
    var buffer: [256]u8 = undefined;
    var stream = std.Io.Writer.fixed(&buffer);

    try writeCurrentToolchainToml(&stream, .release, "0.1.0", "kirac");
    const parsed = try parseCurrentToolchainToml(std.testing.allocator, stream.buffered());
    defer parsed.deinit(std.testing.allocator);

    try std.testing.expectEqual(Channel.release, parsed.channel);
    try std.testing.expectEqualStrings("0.1.0", parsed.version);
    try std.testing.expectEqualStrings("kirac", parsed.primary);
}

test "builds managed toolchain layout" {
    const llvm_version = try pinnedLlvmVersionForTests(std.testing.allocator);
    defer std.testing.allocator.free(llvm_version);

    const maybe_home = homeDir(std.testing.allocator) catch |err| {
        try std.testing.expectEqual(error.HomeDirectoryUnavailable, err);
        try std.testing.expectError(error.HomeDirectoryUnavailable, managedToolchainRoot(std.testing.allocator, .dev, "0.1.0"));
        try std.testing.expectError(error.HomeDirectoryUnavailable, managedLlvmHome(std.testing.allocator, llvm_version, "x86_64-linux-gnu"));
        return;
    };
    defer std.testing.allocator.free(maybe_home);

    const root = try managedToolchainRoot(std.testing.allocator, .dev, "0.1.0");
    defer std.testing.allocator.free(root);
    const expected_root_suffix = try std.fs.path.join(std.testing.allocator, &.{ ".kira", "toolchains", "dev", "0.1.0" });
    defer std.testing.allocator.free(expected_root_suffix);
    try std.testing.expect(std.mem.endsWith(u8, root, expected_root_suffix));

    const llvm_home = try managedLlvmHome(std.testing.allocator, llvm_version, "x86_64-linux-gnu");
    defer std.testing.allocator.free(llvm_home);
    const expected_llvm_suffix = try std.fs.path.join(
        std.testing.allocator,
        &.{ ".kira", "toolchains", "llvm", llvm_version, "x86_64-linux-gnu" },
    );
    defer std.testing.allocator.free(expected_llvm_suffix);
    try std.testing.expect(std.mem.endsWith(u8, llvm_home, expected_llvm_suffix));

    const libffi_home = try managedLibffiHome(std.testing.allocator, "3.5.2", "x86_64-linux-gnu");
    defer std.testing.allocator.free(libffi_home);
    const expected_libffi_suffix = try std.fs.path.join(
        std.testing.allocator,
        &.{ ".kira", "toolchains", "libffi", "3.5.2", "x86_64-linux-gnu" },
    );
    defer std.testing.allocator.free(expected_libffi_suffix);
    try std.testing.expect(std.mem.endsWith(u8, libffi_home, expected_libffi_suffix));
}

fn pinnedLlvmVersionForTests(allocator: std.mem.Allocator) ![]u8 {
    const contents = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, "llvm-metadata.toml", allocator, .limited(16 * 1024));
    defer allocator.free(contents);

    const llvm_section = std.mem.indexOf(u8, contents, "[llvm]") orelse return error.InvalidLlvmMetadata;
    const version_key = std.mem.indexOfPos(u8, contents, llvm_section, "version") orelse return error.InvalidLlvmMetadata;
    const after_key = contents[version_key + "version".len ..];
    const equals_index = std.mem.indexOfScalar(u8, after_key, '=') orelse return error.InvalidLlvmMetadata;
    const after_equals = std.mem.trimStart(u8, after_key[equals_index + 1 ..], " \t\r\n");
    if (after_equals.len < 2 or after_equals[0] != '"') return error.InvalidLlvmMetadata;
    const closing_quote = std.mem.indexOfScalarPos(u8, after_equals, 1, '"') orelse return error.InvalidLlvmMetadata;
    return allocator.dupe(u8, after_equals[1..closing_quote]);
}
