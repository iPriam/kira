const std = @import("std");
const builtin = @import("builtin");
const kira_toolchain = @import("kira_toolchain");
const build_options = @import("kira_bootstrapper_build_options");
extern "c" fn system(command: [*:0]const u8) c_int;

const default_repository = build_options.release_repository;

const ReleaseTarget = struct {
    asset_suffix: []const u8,
    archive_format: ArchiveFormat,
};

const ArchiveFormat = enum {
    zip,
    tar_gz,

    fn extension(self: ArchiveFormat) []const u8 {
        return switch (self) {
            .zip => ".zip",
            .tar_gz => ".tar.gz",
        };
    }
};

pub fn canAutoInstallManagedToolchain() bool {
    return std.mem.eql(u8, build_options.channel, "release");
}

pub fn managedReleaseVersion() []const u8 {
    return build_options.version;
}

pub fn installManagedReleaseToolchain(allocator: std.mem.Allocator, err: anytype) !void {
    if (!canAutoInstallManagedToolchain()) return error.AutoInstallUnsupported;

    const target = try hostReleaseTarget(builtin.target);
    const repository = try releaseRepository(allocator);
    defer allocator.free(repository);
    const release_tag = try std.fmt.allocPrint(allocator, "v{s}", .{build_options.version});
    defer allocator.free(release_tag);
    const asset_name = try toolchainArchiveAssetName(allocator, target);
    defer allocator.free(asset_name);
    const download_url = try std.fmt.allocPrint(
        allocator,
        "https://github.com/{s}/releases/download/{s}/{s}",
        .{ repository, release_tag, asset_name },
    );
    defer allocator.free(download_url);

    const toolchain_root = try kira_toolchain.managedToolchainRoot(allocator, .release, build_options.version);
    defer allocator.free(toolchain_root);

    if (toolchainLooksInstalled(allocator, toolchain_root)) {
        try activateInstalledReleaseToolchain(allocator);
        return;
    }

    const temp_root = try installTempRoot(allocator);
    defer allocator.free(temp_root);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, temp_root);

    const archive_path = try std.fs.path.join(allocator, &.{ temp_root, asset_name });
    defer allocator.free(archive_path);

    try err.print("kira: installing managed toolchain {s} from {s}\n", .{ build_options.version, asset_name });
    try downloadAssetToFile(allocator, download_url, archive_path);
    try err.writeAll("kira: downloaded managed toolchain archive\n");

    if (dirExistsAbsolute(toolchain_root)) {
        try std.Io.Dir.cwd().deleteTree(std.Options.debug_io, toolchain_root);
    }
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, toolchain_root);

    try extractArchive(allocator, archive_path, target.archive_format, toolchain_root);
    try err.writeAll("kira: extracted managed toolchain archive\n");
    try validateInstalledToolchain(allocator, toolchain_root);
    try activateInstalledReleaseToolchain(allocator);
}

fn hostReleaseTarget(target: std.Target) !ReleaseTarget {
    return hostReleaseTargetParts(target.os.tag, target.cpu.arch);
}

fn hostReleaseTargetParts(os_tag: std.Target.Os.Tag, cpu_arch: std.Target.Cpu.Arch) !ReleaseTarget {
    return switch (os_tag) {
        .linux => switch (cpu_arch) {
            .x86_64 => .{ .asset_suffix = "linux-x64", .archive_format = .tar_gz },
            else => error.UnsupportedReleaseHost,
        },
        .macos => switch (cpu_arch) {
            .aarch64 => .{ .asset_suffix = "macos-arm64", .archive_format = .tar_gz },
            else => error.UnsupportedReleaseHost,
        },
        .windows => switch (cpu_arch) {
            .x86_64 => .{ .asset_suffix = "windows-x64", .archive_format = .zip },
            else => error.UnsupportedReleaseHost,
        },
        else => error.UnsupportedReleaseHost,
    };
}

fn toolchainArchiveAssetName(allocator: std.mem.Allocator, target: ReleaseTarget) ![]u8 {
    return std.fmt.allocPrint(
        allocator,
        "kira-toolchain-{s}{s}",
        .{ target.asset_suffix, target.archive_format.extension() },
    );
}

fn releaseRepository(allocator: std.mem.Allocator) ![]u8 {
    if (kira_toolchain.envVarOwned(allocator, "KIRA_RELEASE_GITHUB_REPOSITORY")) |value| {
        return value;
    } else |_| {}

    return allocator.dupe(u8, default_repository);
}

fn installTempRoot(allocator: std.mem.Allocator) ![]u8 {
    const toolchains_root = try kira_toolchain.toolchainsRoot(allocator);
    defer allocator.free(toolchains_root);
    return std.fs.path.join(allocator, &.{ toolchains_root, ".tmp", "release-install", build_options.version });
}

fn toolchainLooksInstalled(allocator: std.mem.Allocator, toolchain_root: []const u8) bool {
    const kirac_path = kira_toolchain.managedPrimaryBinaryPath(allocator, .release, build_options.version, "kirac") catch return false;
    defer allocator.free(kirac_path);
    if (!fileExists(kirac_path)) return false;

    const metadata_path = std.fs.path.join(allocator, &.{ toolchain_root, "llvm-metadata.toml" }) catch return false;
    defer allocator.free(metadata_path);
    return fileExists(metadata_path);
}

fn validateInstalledToolchain(allocator: std.mem.Allocator, toolchain_root: []const u8) !void {
    const kirac_path = try kira_toolchain.managedPrimaryBinaryPath(allocator, .release, build_options.version, "kirac");
    defer allocator.free(kirac_path);
    if (!fileExists(kirac_path)) return error.ToolchainInstallInvalid;

    const templates_path = try std.fs.path.join(allocator, &.{ toolchain_root, "templates" });
    defer allocator.free(templates_path);
    if (!dirExistsAbsolute(templates_path)) return error.ToolchainInstallInvalid;

    const foundation_path = try std.fs.path.join(allocator, &.{ toolchain_root, "foundation" });
    defer allocator.free(foundation_path);
    if (!dirExistsAbsolute(foundation_path)) return error.ToolchainInstallInvalid;

    const metadata_path = try std.fs.path.join(allocator, &.{ toolchain_root, "llvm-metadata.toml" });
    defer allocator.free(metadata_path);
    if (!fileExists(metadata_path)) return error.ToolchainInstallInvalid;
}

fn activateInstalledReleaseToolchain(allocator: std.mem.Allocator) !void {
    const current_path = try kira_toolchain.currentToolchainPath(allocator);
    defer allocator.free(current_path);
    if (std.fs.path.dirname(current_path)) |parent| {
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, parent);
    }

    const current_file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, current_path, .{ .truncate = true });
    defer current_file.close(std.Options.debug_io);

    var buffer: [512]u8 = undefined;
    var writer = current_file.writer(std.Options.debug_io, &buffer);
    try kira_toolchain.writeCurrentToolchainToml(&writer.interface, .release, build_options.version, "kirac");
    try writer.interface.flush();
}

fn downloadAssetToFile(allocator: std.mem.Allocator, download_url: []const u8, destination_path: []const u8) !void {
    if (std.fs.path.dirname(destination_path)) |parent| {
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, parent);
    }

    if (builtin.os.tag == .windows) {
        runChild(&.{
                "powershell.exe",
                "-NoProfile",
                "-Command",
                "param([string]$uri, [string]$out); Invoke-WebRequest -Uri $uri -OutFile $out",
                download_url,
                destination_path,
        }) catch |err| {
            std.debug.print("kira-bootstrapper release download command failed: {s}\n", .{@errorName(err)});
            return error.ReleaseAssetDownloadFailed;
        };
        return;
    }

    const quoted_destination = try shQuote(allocator, destination_path);
    defer allocator.free(quoted_destination);
    const quoted_url = try shQuote(allocator, download_url);
    defer allocator.free(quoted_url);
    const command = try std.fmt.allocPrint(
        allocator,
        "curl -L -f -sS --retry 3 -A kira-bootstrapper -o {s} {s}",
        .{ quoted_destination, quoted_url },
    );
    defer allocator.free(command);
    runSystemCommand(allocator, command) catch return error.ReleaseAssetDownloadFailed;
}

fn extractArchive(
    allocator: std.mem.Allocator,
    archive_path: []const u8,
    format: ArchiveFormat,
    destination_path: []const u8,
) !void {
    return switch (format) {
        .zip => extractZip(archive_path, destination_path),
        .tar_gz => extractTarGz(allocator, archive_path, destination_path),
    };
}

fn extractZip(archive_path: []const u8, destination_path: []const u8) !void {
    var destination_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, destination_path, .{});
    defer destination_dir.close(std.Options.debug_io);

    const file = try std.Io.Dir.openFileAbsolute(std.Options.debug_io, archive_path, .{});
    defer file.close(std.Options.debug_io);

    var buffer: [16 * 1024]u8 = undefined;
    var reader = file.reader(std.Options.debug_io, &buffer);
    try std.zip.extract(destination_dir, &reader, .{
        .allow_backslashes = true,
        .verify_checksums = false,
    });
}

fn extractTarGz(
    allocator: std.mem.Allocator,
    archive_path: []const u8,
    destination_path: []const u8,
) !void {
    const quoted_archive = try shQuote(allocator, archive_path);
    defer allocator.free(quoted_archive);
    const quoted_destination = try shQuote(allocator, destination_path);
    defer allocator.free(quoted_destination);
    const command = try std.fmt.allocPrint(
        allocator,
        "tar -xzf {s} -C {s}",
        .{ quoted_archive, quoted_destination },
    );
    defer allocator.free(command);
    runSystemCommand(allocator, command) catch return error.ToolchainArchiveExtractionFailed;
}

fn runChild(argv: []const []const u8) !void {
    var child = try std.process.spawn(std.Options.debug_io, .{
        .argv = argv,
        .expand_arg0 = .expand,
        .stdin = .ignore,
        .stdout = .ignore,
        .stderr = .inherit,
    });
    const term = try child.wait(std.Options.debug_io);
    if (term != .exited or term.exited != 0) return error.ChildProcessFailed;
}

fn runSystemCommand(allocator: std.mem.Allocator, command: []const u8) !void {
    if (!builtin.link_libc) return error.SystemCommandUnavailable;

    const command_z = try allocator.dupeZ(u8, command);
    defer allocator.free(command_z);
    if (system(command_z.ptr) != 0) return error.ChildProcessFailed;
}

fn shQuote(allocator: std.mem.Allocator, value: []const u8) ![]u8 {
    var quoted = std.array_list.Managed(u8).init(allocator);
    defer quoted.deinit();

    try quoted.append('\'');
    for (value) |byte| {
        if (byte == '\'') {
            try quoted.appendSlice("'\\''");
        } else {
            try quoted.append(byte);
        }
    }
    try quoted.append('\'');
    return quoted.toOwnedSlice();
}

fn fileExists(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn dirExistsAbsolute(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

test "release toolchain archive names match supported hosts" {
    const linux_name = try toolchainArchiveAssetName(std.testing.allocator, .{ .asset_suffix = "linux-x64", .archive_format = .tar_gz });
    defer std.testing.allocator.free(linux_name);
    try std.testing.expectEqualStrings("kira-toolchain-linux-x64.tar.gz", linux_name);

    const macos_name = try toolchainArchiveAssetName(std.testing.allocator, .{ .asset_suffix = "macos-arm64", .archive_format = .tar_gz });
    defer std.testing.allocator.free(macos_name);
    try std.testing.expectEqualStrings("kira-toolchain-macos-arm64.tar.gz", macos_name);

    const windows_name = try toolchainArchiveAssetName(std.testing.allocator, .{ .asset_suffix = "windows-x64", .archive_format = .zip });
    defer std.testing.allocator.free(windows_name);
    try std.testing.expectEqualStrings("kira-toolchain-windows-x64.zip", windows_name);
}

test "maps supported host targets to release assets" {
    try std.testing.expectEqualDeep(
        ReleaseTarget{ .asset_suffix = "linux-x64", .archive_format = .tar_gz },
        try hostReleaseTargetParts(.linux, .x86_64),
    );
    try std.testing.expectEqualDeep(
        ReleaseTarget{ .asset_suffix = "macos-arm64", .archive_format = .tar_gz },
        try hostReleaseTargetParts(.macos, .aarch64),
    );
    try std.testing.expectEqualDeep(
        ReleaseTarget{ .asset_suffix = "windows-x64", .archive_format = .zip },
        try hostReleaseTargetParts(.windows, .x86_64),
    );
}
