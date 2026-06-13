const std = @import("std");

pub const Platform = enum {
    windows,
    linux,
    macos,
};

pub const ArchiveFormat = enum {
    zip,
    tar_gz,
    tar_xz,

    pub fn extension(self: ArchiveFormat) []const u8 {
        return switch (self) {
            .zip => ".zip",
            .tar_gz => ".tar.gz",
            .tar_xz => ".tar.xz",
        };
    }

    pub fn fromString(value: []const u8) ?ArchiveFormat {
        if (std.mem.eql(u8, value, "zip")) return .zip;
        if (std.mem.eql(u8, value, "tar.gz")) return .tar_gz;
        if (std.mem.eql(u8, value, "tar.xz")) return .tar_xz;
        return null;
    }
};

pub const Target = struct {
    key: []const u8,
    runner: []const u8,
    platform: Platform,
    archive: ArchiveFormat,
    asset: []const u8,

    pub fn platformName(self: Target) []const u8 {
        return switch (self.platform) {
            .windows => "windows",
            .linux => "linux",
            .macos => "macos",
        };
    }

    pub fn archiveName(self: Target) []const u8 {
        return switch (self.archive) {
            .zip => "zip",
            .tar_xz => "tar.xz",
        };
    }
};

pub const Metadata = struct {
    schema_version: u32,
    llvm_version: []const u8,
    llvm_source_tag: []const u8,
    llvm_release_tag: []const u8,
    build_type: []const u8,
    cmake_generator: []const u8,
    targets_to_build: []const u8,
    targets: []Target,

    pub fn deinit(self: Metadata, allocator: std.mem.Allocator) void {
        allocator.free(self.llvm_version);
        allocator.free(self.llvm_source_tag);
        allocator.free(self.llvm_release_tag);
        allocator.free(self.build_type);
        allocator.free(self.cmake_generator);
        allocator.free(self.targets_to_build);
        for (self.targets) |target| {
            allocator.free(target.key);
            allocator.free(target.runner);
            allocator.free(target.asset);
        }
        allocator.free(self.targets);
    }

    pub fn findTarget(self: Metadata, key: []const u8) ?Target {
        for (self.targets) |target| {
            if (std.mem.eql(u8, target.key, key)) return target;
        }
        return null;
    }
};

const Section = union(enum) {
    root,
    llvm,
    build,
    target: []const u8,
};

pub fn parseFile(allocator: std.mem.Allocator, path: []const u8) !Metadata {
    const contents = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(16 * 1024));
    defer allocator.free(contents);
    return parse(allocator, contents);
}

pub fn parse(allocator: std.mem.Allocator, text: []const u8) !Metadata {
    var schema_version: ?u32 = null;
    var llvm_version: ?[]const u8 = null;
    var llvm_source_tag: ?[]const u8 = null;
    var llvm_release_tag: ?[]const u8 = null;
    var build_type: ?[]const u8 = null;
    var cmake_generator: ?[]const u8 = null;
    var targets_to_build: ?[]const u8 = null;
    var targets = std.array_list.Managed(Target).init(allocator);
    errdefer {
        for (targets.items) |target| {
            allocator.free(target.key);
            allocator.free(target.runner);
            allocator.free(target.asset);
        }
        targets.deinit();
    }

    var section: Section = .root;
    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |raw_line| {
        const line = trimComment(raw_line);
        if (line.len == 0) continue;
        if (line[0] == '[' and line[line.len - 1] == ']') {
            const header = line[1 .. line.len - 1];
            if (std.mem.eql(u8, header, "llvm")) {
                section = .llvm;
            } else if (std.mem.eql(u8, header, "build")) {
                section = .build;
            } else if (std.mem.startsWith(u8, header, "target.")) {
                const target_key = header["target.".len..];
                if (target_key.len == 0) return error.InvalidLlvmMetadata;
                section = .{ .target = target_key };
                _ = try findOrCreateTarget(allocator, &targets, target_key);
            } else {
                return error.InvalidLlvmMetadata;
            }
            continue;
        }

        switch (section) {
            .root => {
                if (std.mem.startsWith(u8, line, "schema_version")) {
                    schema_version = try parseU32(line);
                }
            },
            .llvm => {
                if (assignString(line, "version")) |value| llvm_version = try allocator.dupe(u8, value);
                if (assignString(line, "source_tag")) |value| llvm_source_tag = try allocator.dupe(u8, value);
                if (assignString(line, "release_tag")) |value| llvm_release_tag = try allocator.dupe(u8, value);
            },
            .build => {
                if (assignString(line, "build_type")) |value| build_type = try allocator.dupe(u8, value);
                if (assignString(line, "cmake_generator")) |value| cmake_generator = try allocator.dupe(u8, value);
                if (assignString(line, "targets_to_build")) |value| targets_to_build = try allocator.dupe(u8, value);
            },
            .target => |target_key| {
                const target = try findOrCreateTarget(allocator, &targets, target_key);
                if (assignString(line, "runner")) |value| target.runner = try allocator.dupe(u8, value);
                if (assignString(line, "platform")) |value| {
                    target.platform = parsePlatform(value) orelse return error.InvalidLlvmMetadata;
                }
                if (assignString(line, "archive")) |value| {
                    target.archive = ArchiveFormat.fromString(value) orelse return error.InvalidLlvmMetadata;
                }
                if (assignString(line, "asset")) |value| target.asset = try allocator.dupe(u8, value);
            },
        }
    }

    const metadata = Metadata{
        .schema_version = schema_version orelse return error.InvalidLlvmMetadata,
        .llvm_version = llvm_version orelse return error.InvalidLlvmMetadata,
        .llvm_source_tag = llvm_source_tag orelse return error.InvalidLlvmMetadata,
        .llvm_release_tag = llvm_release_tag orelse return error.InvalidLlvmMetadata,
        .build_type = build_type orelse return error.InvalidLlvmMetadata,
        .cmake_generator = cmake_generator orelse return error.InvalidLlvmMetadata,
        .targets_to_build = targets_to_build orelse return error.InvalidLlvmMetadata,
        .targets = try targets.toOwnedSlice(),
    };

    try validate(metadata);
    return metadata;
}

fn validate(metadata: Metadata) !void {
    if (metadata.schema_version != 1) return error.InvalidLlvmMetadata;
    if (!std.mem.eql(u8, metadata.build_type, "Release")) return error.InvalidLlvmMetadata;
    if (!std.mem.eql(u8, metadata.cmake_generator, "Ninja")) return error.InvalidLlvmMetadata;
    if (!std.ascii.eqlIgnoreCase(metadata.targets_to_build, "host")) return error.InvalidLlvmMetadata;

    const expected_prefix = try std.fmt.allocPrint(std.heap.page_allocator, "llvm-v{s}-kira.", .{metadata.llvm_version});
    defer std.heap.page_allocator.free(expected_prefix);
    if (!std.mem.startsWith(u8, metadata.llvm_release_tag, expected_prefix)) return error.InvalidLlvmMetadata;
    if (metadata.targets.len == 0) return error.InvalidLlvmMetadata;

    for (metadata.targets) |target| {
        if (target.key.len == 0 or target.runner.len == 0 or target.asset.len == 0) return error.InvalidLlvmMetadata;
        const expected_asset = try std.fmt.allocPrint(std.heap.page_allocator, "llvm-{s}-{s}{s}", .{
            metadata.llvm_version,
            target.key,
            target.archive.extension(),
        });
        defer std.heap.page_allocator.free(expected_asset);
        if (!std.mem.eql(u8, target.asset, expected_asset)) return error.InvalidLlvmMetadata;
    }
}

fn findOrCreateTarget(
    allocator: std.mem.Allocator,
    targets: *std.array_list.Managed(Target),
    key: []const u8,
) !*Target {
    for (targets.items) |*target| {
        if (std.mem.eql(u8, target.key, key)) return target;
    }

    try targets.append(.{
        .key = try allocator.dupe(u8, key),
        .runner = "",
        .platform = .linux,
        .archive = .tar_xz,
        .asset = "",
    });
    return &targets.items[targets.items.len - 1];
}

fn trimComment(line: []const u8) []const u8 {
    const trimmed = std.mem.trim(u8, line, " \t\r");
    if (trimmed.len == 0 or trimmed[0] == '#') return "";

    var in_string = false;
    var index: usize = 0;
    while (index < trimmed.len) : (index += 1) {
        switch (trimmed[index]) {
            '"' => in_string = !in_string,
            '#' => if (!in_string) return std.mem.trimEnd(u8, trimmed[0..index], " \t"),
            else => {},
        }
    }
    return trimmed;
}

fn assignString(line: []const u8, key: []const u8) ?[]const u8 {
    if (!std.mem.startsWith(u8, line, key)) return null;
    const equal_index = std.mem.indexOfScalar(u8, line, '=') orelse return null;
    const value = std.mem.trim(u8, line[equal_index + 1 ..], " \t");
    if (value.len < 2 or value[0] != '"' or value[value.len - 1] != '"') return null;
    return value[1 .. value.len - 1];
}

fn parseU32(line: []const u8) !u32 {
    const equal_index = std.mem.indexOfScalar(u8, line, '=') orelse return error.InvalidLlvmMetadata;
    return std.fmt.parseInt(u32, std.mem.trim(u8, line[equal_index + 1 ..], " \t"), 10);
}

fn parsePlatform(value: []const u8) ?Platform {
    if (std.mem.eql(u8, value, "windows")) return .windows;
    if (std.mem.eql(u8, value, "linux")) return .linux;
    if (std.mem.eql(u8, value, "macos")) return .macos;
    return null;
}

test "parses repo llvm metadata and resolves target" {
    const metadata = try parseFile(std.testing.allocator, "llvm-metadata.toml");
    defer metadata.deinit(std.testing.allocator);

    try std.testing.expectEqual(@as(u32, 1), metadata.schema_version);

    const linux = metadata.findTarget("x86_64-linux-gnu").?;
    try std.testing.expectEqualStrings("ubuntu-24.04", linux.runner);
    const expected_asset = try std.fmt.allocPrint(std.testing.allocator, "llvm-{s}-x86_64-linux-gnu.tar.xz", .{metadata.llvm_version});
    defer std.testing.allocator.free(expected_asset);
    try std.testing.expectEqualStrings(expected_asset, linux.asset);
    try std.testing.expect(linux.archive == .tar_xz);
}

test "rejects incorrect asset naming" {
    try std.testing.expectError(error.InvalidLlvmMetadata, parse(std.testing.allocator,
        \\schema_version = 1
        \\
        \\[llvm]
        \\version = "0.0.0"
        \\source_tag = "llvmorg-0.0.0"
        \\release_tag = "llvm-v0.0.0-kira.99"
        \\
        \\[build]
        \\build_type = "Release"
        \\cmake_generator = "Ninja"
        \\targets_to_build = "host"
        \\
        \\[target.x86_64-linux-gnu]
        \\runner = "ubuntu-24.04"
        \\platform = "linux"
        \\archive = "tar.xz"
        \\asset = "wrong-name.tar.xz"
    ));
}
