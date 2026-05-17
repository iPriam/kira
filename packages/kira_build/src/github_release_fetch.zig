const std = @import("std");
const kira_toolchain = @import("kira_toolchain");

pub const default_repository = "kira-lang-com/kira";

pub const ResolvedAsset = struct {
    repository: []const u8,
    release_tag: []const u8,
    asset_name: []const u8,
    download_url: []const u8,

    pub fn deinit(self: ResolvedAsset, allocator: std.mem.Allocator) void {
        allocator.free(self.repository);
        allocator.free(self.release_tag);
        allocator.free(self.asset_name);
        allocator.free(self.download_url);
    }
};

const ReleaseResponse = struct {
    tag_name: []const u8,
    assets: []const Asset,

    const Asset = struct {
        name: []const u8,
        browser_download_url: []const u8,
    };
};

pub fn resolveReleaseAsset(
    allocator: std.mem.Allocator,
    repo_root: []const u8,
    release_tag: []const u8,
    asset_name: []const u8,
) !ResolvedAsset {
    const repository = try resolveRepositorySlug(allocator, repo_root);
    errdefer allocator.free(repository);

    const api_url = try std.fmt.allocPrint(
        allocator,
        "https://api.github.com/repos/{s}/releases/tags/{s}",
        .{ repository, release_tag },
    );
    defer allocator.free(api_url);

    const response = try fetchBytes(allocator, api_url, true);
    defer response.deinit(allocator);

    switch (response.status) {
        .ok => {},
        .not_found => return error.GitHubReleaseTagNotFound,
        else => return error.GitHubReleaseLookupFailed,
    }

    var parsed = try std.json.parseFromSlice(ReleaseResponse, allocator, response.body, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    if (!std.mem.eql(u8, parsed.value.tag_name, release_tag)) {
        return error.GitHubReleaseTagMismatch;
    }

    for (parsed.value.assets) |asset| {
        if (std.mem.eql(u8, asset.name, asset_name)) {
            return .{
                .repository = repository,
                .release_tag = try allocator.dupe(u8, release_tag),
                .asset_name = try allocator.dupe(u8, asset_name),
                .download_url = try allocator.dupe(u8, asset.browser_download_url),
            };
        }
    }

    return error.GitHubReleaseAssetNotFound;
}

pub fn downloadAssetToFile(
    allocator: std.mem.Allocator,
    download_url: []const u8,
    destination_path: []const u8,
) !void {
    if (std.fs.path.dirname(destination_path)) |parent| {
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, parent);
    }

    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, destination_path, .{ .read = true, .truncate = true });
    defer file.close(std.Options.debug_io);

    var file_buffer: [16 * 1024]u8 = undefined;
    var file_writer = file.writer(std.Options.debug_io, &file_buffer);

    const response = try fetchWriter(allocator, download_url, false, &file_writer.interface);
    try file_writer.interface.flush();

    switch (response.status) {
        .ok => return,
        .not_found => return error.GitHubReleaseAssetNotFound,
        else => {
            std.debug.print(
                "GitHub asset download failed: status={d} ({s}) url={s}\n",
                .{
                    @intFromEnum(response.status),
                    @tagName(response.status),
                    download_url,
                },
            );
            return error.GitHubAssetDownloadFailed;
        },
    }
}

const FetchResponse = struct {
    status: std.http.Status,
    body: []u8,

    fn deinit(self: FetchResponse, allocator: std.mem.Allocator) void {
        allocator.free(self.body);
    }
};

fn fetchBytes(
    allocator: std.mem.Allocator,
    url: []const u8,
    is_api_request: bool,
) !FetchResponse {
    var writer: std.Io.Writer.Allocating = .init(allocator);
    defer writer.deinit();

    const response = try fetchWriter(allocator, url, is_api_request, &writer.writer);
    return .{
        .status = response.status,
        .body = try writer.toOwnedSlice(),
    };
}

fn fetchWriter(
    allocator: std.mem.Allocator,
    url: []const u8,
    is_api_request: bool,
    writer: *std.Io.Writer,
) !struct { status: std.http.Status } {
    var client: std.http.Client = .{
        .allocator = allocator,
        .io = std.Options.debug_io,
    };
    defer client.deinit();

    const token = githubToken(allocator) catch null;
    defer if (token) |value| allocator.free(value);

    var extra_headers = std.array_list.Managed(std.http.Header).init(allocator);
    defer extra_headers.deinit();

    try extra_headers.append(.{ .name = "User-Agent", .value = "kirac-fetch-llvm" });

    if (is_api_request) {
        try extra_headers.append(.{ .name = "Accept", .value = "application/vnd.github+json" });
        try extra_headers.append(.{ .name = "X-GitHub-Api-Version", .value = "2022-11-28" });
    } else {
        try extra_headers.append(.{ .name = "Accept", .value = "application/octet-stream" });
    }

    var auth_header_value: ?[]const u8 = null;
    defer if (auth_header_value) |value| allocator.free(value);

    if (is_api_request) {
        if (token) |value| {
            auth_header_value = try std.fmt.allocPrint(allocator, "Bearer {s}", .{value});
            try extra_headers.append(.{ .name = "Authorization", .value = auth_header_value.? });
        }
    }

    // GitHub release assets redirect to huge signed URLs.
    // 512 bytes is too small and causes HttpRedirectLocationOversize.
    var redirect_buffer: [64 * 1024]u8 = undefined;

    const result = try client.fetch(.{
        .location = .{ .url = url },
        .response_writer = writer,
        .extra_headers = extra_headers.items,
        .redirect_buffer = &redirect_buffer,
    });

    return .{ .status = result.status };
}

fn githubToken(allocator: std.mem.Allocator) !?[]const u8 {
    return kira_toolchain.envVarOwned(allocator, "GITHUB_TOKEN") catch
        kira_toolchain.envVarOwned(allocator, "GH_TOKEN") catch
        null;
}

pub fn resolveRepositorySlug(allocator: std.mem.Allocator, repo_root: []const u8) ![]const u8 {
    if (kira_toolchain.envVarOwned(allocator, "KIRA_LLVM_GITHUB_REPOSITORY")) |value| {
        return value;
    } else |_| {}

    if (gitRemoteOriginUrl(allocator, repo_root)) |origin_url| {
        defer allocator.free(origin_url);
        if (parseGitHubRepository(origin_url)) |repository| {
            return allocator.dupe(u8, repository);
        }
    }

    return allocator.dupe(u8, default_repository);
}

fn gitRemoteOriginUrl(allocator: std.mem.Allocator, repo_root: []const u8) ?[]const u8 {
    const result = std.process.run(allocator, std.Options.debug_io, .{
        .argv = &.{ "git", "config", "--get", "remote.origin.url" },
        .expand_arg0 = .expand,
        .cwd = .{ .path = repo_root },
        .stdout_limit = .limited(4096),
        .stderr_limit = .limited(4096),
    }) catch return null;
    defer allocator.free(result.stderr);

    if (result.term != .exited or result.term.exited != 0) {
        allocator.free(result.stdout);
        return null;
    }
    return allocator.dupe(u8, std.mem.trim(u8, result.stdout, " \r\n\t")) catch null;
}

fn parseGitHubRepository(remote_url: []const u8) ?[]const u8 {
    const prefixes = [_][]const u8{
        "git@github.com:",
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
    };

    for (prefixes) |prefix| {
        if (!std.mem.startsWith(u8, remote_url, prefix)) continue;
        var trimmed = remote_url[prefix.len..];
        if (std.mem.endsWith(u8, trimmed, ".git")) {
            trimmed = trimmed[0 .. trimmed.len - ".git".len];
        }
        if (std.mem.indexOfScalar(u8, trimmed, '/')) |_| return trimmed;
    }
    return null;
}

test "parses common GitHub remote URLs" {
    try std.testing.expectEqualStrings("kira-lang-com/kira-zig", parseGitHubRepository("git@github.com:kira-lang-com/kira-zig.git").?);
    try std.testing.expectEqualStrings("kira-lang-com/kira-zig", parseGitHubRepository("https://github.com/kira-lang-com/kira-zig.git").?);
    try std.testing.expectEqualStrings("kira-lang-com/kira-zig", parseGitHubRepository("ssh://git@github.com/kira-lang-com/kira-zig").?);
}
