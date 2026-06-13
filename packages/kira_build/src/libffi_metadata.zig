const std = @import("std");
const llvm_metadata = @import("llvm_metadata.zig");

pub const Platform = llvm_metadata.Platform;
pub const ArchiveFormat = llvm_metadata.ArchiveFormat;

pub const Target = struct {
    key: []const u8,
    runner: []const u8,
    platform: Platform,
    archive: ArchiveFormat,
    asset: []const u8,
};

pub const Metadata = struct {
    schema_version: u32,
    version: []const u8,
    source_tag: []const u8,
    source_commit: []const u8,
    release_tag: []const u8,
    repository: []const u8,
    build_type: []const u8,
    targets_to_build: []const u8,
    targets: []Target,

    pub fn deinit(self: Metadata, allocator: std.mem.Allocator) void {
        allocator.free(self.version);
        allocator.free(self.source_tag);
        allocator.free(self.source_commit);
        allocator.free(self.release_tag);
        allocator.free(self.repository);
        allocator.free(self.build_type);
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
    libffi,
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
    var version: ?[]const u8 = null;
    var source_tag: ?[]const u8 = null;
    var source_commit: ?[]const u8 = null;
    var release_tag: ?[]const u8 = null;
    var repository: ?[]const u8 = null;
    var build_type: ?[]const u8 = null;
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
            if (std.mem.eql(u8, header, "libffi")) {
                section = .libffi;
            } else if (std.mem.eql(u8, header, "build")) {
                section = .build;
            } else if (std.mem.startsWith(u8, header, "target.")) {
                const target_key = header["target.".len..];
                if (target_key.len == 0) return error.InvalidLibffiMetadata;
                section = .{ .target = target_key };
                _ = try findOrCreateTarget(allocator, &targets, target_key);
            } else {
                return error.InvalidLibffiMetadata;
            }
            continue;
        }

        switch (section) {
            .root => if (std.mem.startsWith(u8, line, "schema_version")) {
                schema_version = try parseU32(line);
            },
            .libffi => {
                if (assignString(line, "version")) |value| version = try allocator.dupe(u8, value);
                if (assignString(line, "source_tag")) |value| source_tag = try allocator.dupe(u8, value);
                if (assignString(line, "source_commit")) |value| source_commit = try allocator.dupe(u8, value);
                if (assignString(line, "release_tag")) |value| release_tag = try allocator.dupe(u8, value);
                if (assignString(line, "repository")) |value| repository = try allocator.dupe(u8, value);
            },
            .build => {
                if (assignString(line, "build_type")) |value| build_type = try allocator.dupe(u8, value);
                if (assignString(line, "targets_to_build")) |value| targets_to_build = try allocator.dupe(u8, value);
            },
            .target => |target_key| {
                const target = try findOrCreateTarget(allocator, &targets, target_key);
                if (assignString(line, "runner")) |value| target.runner = try allocator.dupe(u8, value);
                if (assignString(line, "platform")) |value| target.platform = parsePlatform(value) orelse return error.InvalidLibffiMetadata;
                if (assignString(line, "archive")) |value| target.archive = ArchiveFormat.fromString(value) orelse return error.InvalidLibffiMetadata;
                if (assignString(line, "asset")) |value| target.asset = try allocator.dupe(u8, value);
            },
        }
    }

    const metadata = Metadata{
        .schema_version = schema_version orelse return error.InvalidLibffiMetadata,
        .version = version orelse return error.InvalidLibffiMetadata,
        .source_tag = source_tag orelse return error.InvalidLibffiMetadata,
        .source_commit = source_commit orelse return error.InvalidLibffiMetadata,
        .release_tag = release_tag orelse return error.InvalidLibffiMetadata,
        .repository = repository orelse return error.InvalidLibffiMetadata,
        .build_type = build_type orelse return error.InvalidLibffiMetadata,
        .targets_to_build = targets_to_build orelse return error.InvalidLibffiMetadata,
        .targets = try targets.toOwnedSlice(),
    };
    try validate(metadata);
    return metadata;
}

fn validate(metadata: Metadata) !void {
    if (metadata.schema_version != 1) return error.InvalidLibffiMetadata;
    if (!std.mem.eql(u8, metadata.repository, "kira-lang-com/libffi")) return error.InvalidLibffiMetadata;
    if (!std.mem.eql(u8, metadata.build_type, "Release")) return error.InvalidLibffiMetadata;
    if (!std.ascii.eqlIgnoreCase(metadata.targets_to_build, "host")) return error.InvalidLibffiMetadata;
    if (!std.mem.startsWith(u8, metadata.source_tag, "v")) return error.InvalidLibffiMetadata;
    if (!std.mem.eql(u8, metadata.release_tag, metadata.source_tag)) return error.InvalidLibffiMetadata;
    if (metadata.source_commit.len != 40) return error.InvalidLibffiMetadata;
    for (metadata.source_commit) |byte| {
        if (!std.ascii.isHex(byte)) return error.InvalidLibffiMetadata;
    }
    if (metadata.targets.len == 0) return error.InvalidLibffiMetadata;
    for (metadata.targets) |target| {
        if (target.key.len == 0 or target.runner.len == 0 or target.asset.len == 0) return error.InvalidLibffiMetadata;
        const expected_prefix = try std.fmt.allocPrint(std.heap.page_allocator, "libffi-{s}-", .{metadata.version});
        defer std.heap.page_allocator.free(expected_prefix);
        if (!std.mem.startsWith(u8, target.asset, expected_prefix)) return error.InvalidLibffiMetadata;
        if (!std.mem.endsWith(u8, target.asset, target.archive.extension())) return error.InvalidLibffiMetadata;
    }
}

fn findOrCreateTarget(allocator: std.mem.Allocator, targets: *std.array_list.Managed(Target), key: []const u8) !*Target {
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
    const equal_index = std.mem.indexOfScalar(u8, line, '=') orelse return error.InvalidLibffiMetadata;
    return std.fmt.parseInt(u32, std.mem.trim(u8, line[equal_index + 1 ..], " \t"), 10);
}

fn parsePlatform(value: []const u8) ?Platform {
    if (std.mem.eql(u8, value, "windows")) return .windows;
    if (std.mem.eql(u8, value, "linux")) return .linux;
    if (std.mem.eql(u8, value, "macos")) return .macos;
    return null;
}

test "parses repo libffi metadata and resolves target" {
    const metadata = try parseFile(std.testing.allocator, "libffi-metadata.toml");
    defer metadata.deinit(std.testing.allocator);

    try std.testing.expectEqualStrings("kira-lang-com/libffi", metadata.repository);
    try std.testing.expectEqualStrings("250e4b8d55918f3f0380608e7f2f6cfe02a8c3ee", metadata.source_commit);
    const windows = metadata.findTarget("x86_64-windows-msvc").?;
    try std.testing.expectEqualStrings("libffi-3.5.2-windows-x64-shared.zip", windows.asset);
    try std.testing.expect(windows.archive == .zip);
}

test "rejects wrong libffi repository" {
    try std.testing.expectError(error.InvalidLibffiMetadata, parse(std.testing.allocator,
        \\schema_version = 1
        \\
        \\[libffi]
        \\version = "3.5.2"
        \\source_tag = "v3.5.2"
        \\source_commit = "250e4b8d55918f3f0380608e7f2f6cfe02a8c3ee"
        \\release_tag = "v3.5.2"
        \\repository = "upstream/libffi"
        \\
        \\[build]
        \\build_type = "Release"
        \\targets_to_build = "host"
        \\
        \\[target.x86_64-linux-gnu]
        \\runner = "ubuntu-24.04"
        \\platform = "linux"
        \\archive = "tar.gz"
        \\asset = "libffi-3.5.2-linux-x86_64-shared.tar.gz"
    ));
}
