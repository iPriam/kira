const std = @import("std");
const builtin = @import("builtin");
const kira_toolchain = @import("kira_toolchain");
const toolchain_layout = @import("kira_llvm_toolchain_layout");
const libffi_metadata = @import("libffi_metadata.zig");
const github_release_fetch = @import("github_release_fetch.zig");
const archive_extract = @import("archive_extract.zig");

const install_marker_name = ".kira-libffi-toolchain.json";

pub const FetchPlan = struct {
    version: []const u8,
    source_tag: []const u8,
    source_commit: []const u8,
    release_tag: []const u8,
    repository: []const u8,
    asset_name: []const u8,
    host_target: []const u8,
    install_dir: []const u8,
    archive_format: libffi_metadata.ArchiveFormat,

    pub fn deinit(self: FetchPlan, allocator: std.mem.Allocator) void {
        allocator.free(self.version);
        allocator.free(self.source_tag);
        allocator.free(self.source_commit);
        allocator.free(self.release_tag);
        allocator.free(self.repository);
        allocator.free(self.asset_name);
        allocator.free(self.host_target);
        allocator.free(self.install_dir);
    }
};

const CiMetadataJson = struct {
    version: []const u8,
    source_tag: []const u8,
    source_commit: []const u8,
    release_tag: []const u8,
    repository: []const u8,
    asset_name: []const u8,
    host_target: []const u8,
    install_dir: []const u8,
};

pub const InstallMarker = struct {
    version: []const u8,
    host_key: []const u8,
    source_tag: []const u8,
    source_commit: []const u8,
    release_tag: []const u8,
    repository: []const u8,
    asset_name: []const u8,
};

pub fn run(
    allocator: std.mem.Allocator,
    out: anytype,
    err: anytype,
    metadata_root: []const u8,
) !void {
    const plan = try planFetch(allocator, err, metadata_root);
    defer plan.deinit(allocator);

    try out.print(
        "LibFFI {s}\nrelease tag: {s}\nrepository: {s}\nhost target: {s}\nasset: {s}\ninstall dir: {s}\n",
        .{ plan.version, plan.release_tag, plan.repository, plan.host_target, plan.asset_name, plan.install_dir },
    );

    if (try isInstalledAndValid(allocator, plan.install_dir, markerFromPlan(plan))) {
        try out.print("LibFFI toolchain is already installed and matches the pinned metadata. Skipping.\n", .{});
        return;
    }

    const resolved_asset = github_release_fetch.resolveReleaseAssetInRepository(
        allocator,
        try allocator.dupe(u8, plan.repository),
        plan.release_tag,
        plan.asset_name,
    ) catch |fetch_err| {
        switch (fetch_err) {
            error.GitHubReleaseTagNotFound => try err.print("GitHub release tag {s} was not found in repository {s}.\n", .{ plan.release_tag, plan.repository }),
            error.GitHubReleaseAssetNotFound => try err.print("GitHub release {s} exists, but asset {s} was not found.\n", .{ plan.release_tag, plan.asset_name }),
            else => try err.print("Failed to resolve GitHub release asset {s} from tag {s}: {s}\n", .{ plan.asset_name, plan.release_tag, @errorName(fetch_err) }),
        }
        return fetch_err;
    };
    defer resolved_asset.deinit(allocator);

    const temp_root_dir = try temporaryInstallDir(allocator, plan.version, plan.host_target);
    defer allocator.free(temp_root_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, temp_root_dir);
    errdefer std.Io.Dir.cwd().deleteTree(std.Options.debug_io, temp_root_dir) catch {};

    const archive_path = try std.fs.path.join(allocator, &.{ temp_root_dir, plan.asset_name });
    defer allocator.free(archive_path);

    try out.print("Downloading {s}\n", .{resolved_asset.download_url});
    github_release_fetch.downloadAssetToFile(allocator, resolved_asset.download_url, archive_path) catch |download_err| {
        try err.print("Failed to download LibFFI archive from {s}: {s}\n", .{ resolved_asset.download_url, @errorName(download_err) });
        return download_err;
    };

    try installArchiveWithPlan(allocator, out, err, plan, archive_path);
}

pub fn printCiMetadataJson(
    allocator: std.mem.Allocator,
    out: anytype,
    err: anytype,
    metadata_root: []const u8,
) !void {
    const plan = try planFetch(allocator, err, metadata_root);
    defer plan.deinit(allocator);
    try std.json.Stringify.value(CiMetadataJson{
        .version = plan.version,
        .source_tag = plan.source_tag,
        .source_commit = plan.source_commit,
        .release_tag = plan.release_tag,
        .repository = plan.repository,
        .asset_name = plan.asset_name,
        .host_target = plan.host_target,
        .install_dir = plan.install_dir,
    }, .{ .whitespace = .indent_2 }, out);
    try out.writeByte('\n');
}

pub fn installArchive(
    allocator: std.mem.Allocator,
    out: anytype,
    err: anytype,
    metadata_root: []const u8,
    archive_path: []const u8,
) !void {
    const plan = try planFetch(allocator, err, metadata_root);
    defer plan.deinit(allocator);
    try installArchiveWithPlan(allocator, out, err, plan, archive_path);
}

fn planFetch(allocator: std.mem.Allocator, err: anytype, metadata_root: []const u8) !FetchPlan {
    const metadata_path = try std.fs.path.join(allocator, &.{ metadata_root, "libffi-metadata.toml" });
    defer allocator.free(metadata_path);
    const metadata = libffi_metadata.parseFile(allocator, metadata_path) catch {
        try err.print("Failed to parse libffi-metadata.toml at {s}.\n", .{metadata_path});
        return error.InvalidLibffiMetadata;
    };
    defer metadata.deinit(allocator);

    const host_key = toolchain_layout.hostLlvmBundleKey(builtin.target) orelse {
        try err.print("Unsupported host target for Kira LibFFI bundles: {s}-{s}.\n", .{ @tagName(builtin.cpu.arch), @tagName(builtin.os.tag) });
        return error.UnsupportedLibffiHost;
    };
    const target = metadata.findTarget(host_key) orelse {
        try err.print("libffi-metadata.toml does not publish a bundle for host target {s}.\n", .{host_key});
        return error.LibffiTargetNotPublished;
    };

    const install_home = try kira_toolchain.managedLibffiHome(allocator, metadata.version, host_key);
    errdefer allocator.free(install_home);
    return .{
        .version = try allocator.dupe(u8, metadata.version),
        .source_tag = try allocator.dupe(u8, metadata.source_tag),
        .source_commit = try allocator.dupe(u8, metadata.source_commit),
        .release_tag = try allocator.dupe(u8, metadata.release_tag),
        .repository = try allocator.dupe(u8, metadata.repository),
        .asset_name = try allocator.dupe(u8, target.asset),
        .host_target = try allocator.dupe(u8, host_key),
        .install_dir = install_home,
        .archive_format = target.archive,
    };
}

fn installArchiveWithPlan(
    allocator: std.mem.Allocator,
    out: anytype,
    err: anytype,
    plan: FetchPlan,
    archive_path: []const u8,
) !void {
    if (!fileExistsAbsolute(archive_path)) {
        try err.print("LibFFI archive path does not exist: {s}\n", .{archive_path});
        return error.LibffiArchiveNotFound;
    }
    if (try isInstalledAndValid(allocator, plan.install_dir, markerFromPlan(plan))) {
        try out.print("LibFFI toolchain is already installed and matches the pinned metadata. Skipping.\n", .{});
        return;
    }

    const version_root = try kira_toolchain.managedLibffiVersionRoot(allocator, plan.version);
    defer allocator.free(version_root);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, version_root);
    if (dirExistsAbsolute(plan.install_dir)) {
        try out.print("Removing stale LibFFI install at {s} before reinstalling.\n", .{plan.install_dir});
        try std.Io.Dir.cwd().deleteTree(std.Options.debug_io, plan.install_dir);
    }

    const temp_root_dir = try temporaryInstallDir(allocator, plan.version, plan.host_target);
    defer allocator.free(temp_root_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, temp_root_dir);
    errdefer std.Io.Dir.cwd().deleteTree(std.Options.debug_io, temp_root_dir) catch {};

    const temp_install_dir = try std.fs.path.join(allocator, &.{ temp_root_dir, "payload" });
    defer allocator.free(temp_install_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, temp_install_dir);
    try out.print("Extracting {s}\n", .{archive_path});
    archive_extract.extractArchive(allocator, archive_path, plan.archive_format, temp_install_dir) catch |extract_err| {
        try err.print("Failed to extract {s}: {s}\n", .{ archive_path, @errorName(extract_err) });
        return extract_err;
    };
    const package_install_dir = try findLibffiInstallRoot(allocator, temp_install_dir);
    defer allocator.free(package_install_dir);
    if (!looksLikeLibffiInstall(allocator, package_install_dir)) {
        try err.print("Extracted LibFFI archive did not produce a usable install tree at {s}.\n", .{temp_install_dir});
        return error.InvalidInstalledToolchain;
    }
    try writeInstallMarker(allocator, package_install_dir, markerFromPlan(plan));
    try std.Io.Dir.renameAbsolute(package_install_dir, plan.install_dir, std.Options.debug_io);
    if (dirExistsAbsolute(temp_root_dir)) std.Io.Dir.cwd().deleteTree(std.Options.debug_io, temp_root_dir) catch {};
    try out.print("Installed LibFFI toolchain into {s}\n", .{plan.install_dir});
}

fn markerFromPlan(plan: FetchPlan) InstallMarker {
    return .{
        .version = plan.version,
        .host_key = plan.host_target,
        .source_tag = plan.source_tag,
        .source_commit = plan.source_commit,
        .release_tag = plan.release_tag,
        .repository = plan.repository,
        .asset_name = plan.asset_name,
    };
}

fn isInstalledAndValid(allocator: std.mem.Allocator, install_home: []const u8, expected: InstallMarker) !bool {
    if (!dirExistsAbsolute(install_home)) return false;
    if (!looksLikeLibffiInstall(allocator, install_home)) return false;
    const marker_path = try installMarkerPath(allocator, install_home);
    defer allocator.free(marker_path);
    if (!fileExistsAbsolute(marker_path)) return false;
    const marker = readInstallMarker(allocator, marker_path) catch return false;
    defer marker.deinit(allocator);
    return std.mem.eql(u8, marker.version, expected.version) and
        std.mem.eql(u8, marker.host_key, expected.host_key) and
        std.mem.eql(u8, marker.source_tag, expected.source_tag) and
        std.mem.eql(u8, marker.source_commit, expected.source_commit) and
        std.mem.eql(u8, marker.release_tag, expected.release_tag) and
        std.mem.eql(u8, marker.repository, expected.repository) and
        std.mem.eql(u8, marker.asset_name, expected.asset_name);
}

fn writeInstallMarker(allocator: std.mem.Allocator, install_home: []const u8, marker: InstallMarker) !void {
    const marker_path = try installMarkerPath(allocator, install_home);
    defer allocator.free(marker_path);
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, marker_path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    var buffer: [4096]u8 = undefined;
    var writer = file.writer(std.Options.debug_io, &buffer);
    try std.json.Stringify.value(marker, .{ .whitespace = .indent_2 }, &writer.interface);
    try writer.interface.writeByte('\n');
    try writer.interface.flush();
}

fn readInstallMarker(allocator: std.mem.Allocator, marker_path: []const u8) !OwnedMarker {
    const contents = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, marker_path, allocator, .limited(4 * 1024));
    defer allocator.free(contents);
    var parsed = try std.json.parseFromSlice(InstallMarker, allocator, contents, .{
        .ignore_unknown_fields = true,
        .allocate = .alloc_always,
    });
    defer parsed.deinit();
    return .{
        .version = try allocator.dupe(u8, parsed.value.version),
        .host_key = try allocator.dupe(u8, parsed.value.host_key),
        .source_tag = try allocator.dupe(u8, parsed.value.source_tag),
        .source_commit = try allocator.dupe(u8, parsed.value.source_commit),
        .release_tag = try allocator.dupe(u8, parsed.value.release_tag),
        .repository = try allocator.dupe(u8, parsed.value.repository),
        .asset_name = try allocator.dupe(u8, parsed.value.asset_name),
    };
}

const OwnedMarker = struct {
    version: []const u8,
    host_key: []const u8,
    source_tag: []const u8,
    source_commit: []const u8,
    release_tag: []const u8,
    repository: []const u8,
    asset_name: []const u8,

    fn deinit(self: OwnedMarker, allocator: std.mem.Allocator) void {
        allocator.free(self.version);
        allocator.free(self.host_key);
        allocator.free(self.source_tag);
        allocator.free(self.source_commit);
        allocator.free(self.release_tag);
        allocator.free(self.repository);
        allocator.free(self.asset_name);
    }
};

fn installMarkerPath(allocator: std.mem.Allocator, install_home: []const u8) ![]const u8 {
    return std.fs.path.join(allocator, &.{ install_home, install_marker_name });
}

fn temporaryInstallDir(allocator: std.mem.Allocator, version: []const u8, host_key: []const u8) ![]const u8 {
    const version_root = try kira_toolchain.managedLibffiVersionRoot(allocator, version);
    defer allocator.free(version_root);
    var stamp_bytes: [8]u8 = undefined;
    std.Options.debug_io.random(&stamp_bytes);
    const stamp = std.mem.readInt(u64, &stamp_bytes, .little);
    const dirname = try std.fmt.allocPrint(allocator, ".{s}.tmp-{d}", .{ host_key, stamp });
    defer allocator.free(dirname);
    return std.fs.path.join(allocator, &.{ version_root, dirname });
}

fn looksLikeLibffiInstall(allocator: std.mem.Allocator, install_home: []const u8) bool {
    const ffi_h = std.fs.path.join(allocator, &.{ install_home, "include", "ffi.h" }) catch return false;
    defer allocator.free(ffi_h);
    const ffitarget_h = std.fs.path.join(allocator, &.{ install_home, "include", "ffitarget.h" }) catch return false;
    defer allocator.free(ffitarget_h);
    if (!fileExistsAbsolute(ffi_h) or !fileExistsAbsolute(ffitarget_h)) return false;

    return switch (builtin.os.tag) {
        .windows => hasLibffiLibrary(allocator, install_home, "lib", &.{ ".dll", ".lib" }) or
            hasLibffiLibrary(allocator, install_home, "bin", &.{".dll"}),
        .linux => hasLibffiLibrary(allocator, install_home, "lib", &.{".so"}) or
            hasLibffiLibrary(allocator, install_home, "lib64", &.{".so"}),
        .macos => hasLibffiLibrary(allocator, install_home, "lib", &.{".dylib"}),
        else => false,
    };
}

fn hasLibffiLibrary(
    allocator: std.mem.Allocator,
    install_home: []const u8,
    relative_dir: []const u8,
    allowed_suffixes: []const []const u8,
) bool {
    const lib_dir_path = std.fs.path.join(allocator, &.{ install_home, relative_dir }) catch return false;
    defer allocator.free(lib_dir_path);

    var lib_dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, lib_dir_path, .{ .iterate = true }) catch return false;
    defer lib_dir.close(std.Options.debug_io);

    var iterator = lib_dir.iterate();
    while (iterator.next(std.Options.debug_io) catch null) |entry| {
        if (entry.kind != .file and entry.kind != .sym_link) continue;
        if (!std.mem.startsWith(u8, entry.name, "libffi") and !std.mem.eql(u8, entry.name, "ffi.lib")) continue;
        for (allowed_suffixes) |suffix| {
            if (std.mem.endsWith(u8, entry.name, suffix)) return true;
        }
    }
    return false;
}

fn findLibffiInstallRoot(allocator: std.mem.Allocator, extracted_root: []const u8) ![]const u8 {
    if (looksLikeLibffiInstall(allocator, extracted_root)) {
        return allocator.dupe(u8, extracted_root);
    }

    var root_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, extracted_root, .{ .iterate = true });
    defer root_dir.close(std.Options.debug_io);

    var iterator = root_dir.iterate();
    var candidate: ?[]u8 = null;
    errdefer if (candidate) |path| allocator.free(path);
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .directory) continue;
        const path = try std.fs.path.join(allocator, &.{ extracted_root, entry.name });
        if (looksLikeLibffiInstall(allocator, path)) {
            if (candidate != null) {
                allocator.free(path);
                return error.InvalidInstalledToolchain;
            }
            candidate = path;
        } else {
            allocator.free(path);
        }
    }

    return candidate orelse error.InvalidInstalledToolchain;
}

fn dirExistsAbsolute(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn fileExistsAbsolute(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

test "validates managed libffi marker and install path" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    const install_home = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(install_home);
    try tmp.dir.createDirPath(std.testing.io, "include");
    try writeFile(tmp.dir, "include/ffi.h", "");
    try writeFile(tmp.dir, "include/ffitarget.h", "");
    const library_relative_path = switch (builtin.os.tag) {
        .windows => "bin/libffi.dll",
        .linux => "lib/libffi.so",
        .macos => "lib/libffi.dylib",
        else => return error.UnsupportedLibffiHost,
    };
    if (std.fs.path.dirname(library_relative_path)) |parent| try tmp.dir.createDirPath(std.testing.io, parent);
    try writeFile(tmp.dir, library_relative_path, "");

    try writeInstallMarker(std.testing.allocator, install_home, .{
        .version = "3.5.2",
        .host_key = "x86_64-windows-msvc",
        .source_tag = "v3.5.2",
        .source_commit = "250e4b8d55918f3f0380608e7f2f6cfe02a8c3ee",
        .release_tag = "v3.5.2",
        .repository = "kira-lang-com/libffi",
        .asset_name = "libffi-3.5.2-windows-x64-shared.zip",
    });
    try std.testing.expect(try isInstalledAndValid(std.testing.allocator, install_home, .{
        .version = "3.5.2",
        .host_key = "x86_64-windows-msvc",
        .source_tag = "v3.5.2",
        .source_commit = "250e4b8d55918f3f0380608e7f2f6cfe02a8c3ee",
        .release_tag = "v3.5.2",
        .repository = "kira-lang-com/libffi",
        .asset_name = "libffi-3.5.2-windows-x64-shared.zip",
    }));
}

test "plans libffi CI metadata from repo metadata" {
    var stderr_buffer: [512]u8 = undefined;
    var stderr = std.Io.Writer.fixed(&stderr_buffer);
    const plan = try planFetch(std.testing.allocator, &stderr, ".");
    defer plan.deinit(std.testing.allocator);
    const expected_host = toolchain_layout.hostLlvmBundleKey(builtin.target) orelse return error.UnsupportedLibffiHost;
    try std.testing.expectEqualStrings("3.5.2", plan.version);
    try std.testing.expectEqualStrings("v3.5.2", plan.source_tag);
    try std.testing.expectEqualStrings("250e4b8d55918f3f0380608e7f2f6cfe02a8c3ee", plan.source_commit);
    try std.testing.expectEqualStrings("kira-lang-com/libffi", plan.repository);
    try std.testing.expectEqualStrings(expected_host, plan.host_target);
    if (std.mem.eql(u8, expected_host, "x86_64-windows-msvc")) {
        try std.testing.expectEqualStrings("libffi-3.5.2-windows-x64-shared.zip", plan.asset_name);
    }
}

test "finds libffi install root inside packaged artifact directory" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "libffi-3.5.2-windows-x64-shared/include");
    try tmp.dir.createDirPath(std.testing.io, "libffi-3.5.2-windows-x64-shared/lib");
    try writeFile(tmp.dir, "libffi-3.5.2-windows-x64-shared/include/ffi.h", "");
    try writeFile(tmp.dir, "libffi-3.5.2-windows-x64-shared/include/ffitarget.h", "");
    const library_relative_path = switch (builtin.os.tag) {
        .windows => "libffi-3.5.2-windows-x64-shared/lib/libffi-8.dll",
        .linux => "libffi-3.5.2-windows-x64-shared/lib/libffi.so",
        .macos => "libffi-3.5.2-windows-x64-shared/lib/libffi.dylib",
        else => return error.UnsupportedLibffiHost,
    };
    try writeFile(tmp.dir, library_relative_path, "");

    const root = try tmp.dir.realPathFileAlloc(std.testing.io, ".", std.testing.allocator);
    defer std.testing.allocator.free(root);
    const found = try findLibffiInstallRoot(std.testing.allocator, root);
    defer std.testing.allocator.free(found);
    try std.testing.expect(std.mem.endsWith(u8, found, "libffi-3.5.2-windows-x64-shared"));
}

fn writeFile(dir: std.fs.Dir, relative_path: []const u8, contents: []const u8) !void {
    if (std.fs.path.dirname(relative_path)) |parent| try dir.createDirPath(std.Options.debug_io, parent);
    const file = try dir.createFile(relative_path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, contents);
}
