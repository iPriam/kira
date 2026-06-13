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
        url: []const u8,
        browser_download_url: []const u8,
    };
};

const HttpStatusResult = struct {
    status: std.http.Status,
};

pub fn resolveReleaseAsset(
    allocator: std.mem.Allocator,
    repo_root: []const u8,
    release_tag: []const u8,
    asset_name: []const u8,
) !ResolvedAsset {
    const repository = try resolveRepositorySlug(allocator, repo_root);
    return resolveReleaseAssetInRepository(allocator, repository, release_tag, asset_name);
}

pub fn resolveReleaseAssetInRepository(
    allocator: std.mem.Allocator,
    repository: []const u8,
    release_tag: []const u8,
    asset_name: []const u8,
) !ResolvedAsset {
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

    const response = fetchWriterViaManualRedirect(allocator, download_url, &file_writer.interface) catch |manual_err| blk: {
        std.debug.print("GitHub asset manual redirect download failed: {s}\n", .{@errorName(manual_err)});
        break :blk try fetchWriter(allocator, download_url, false, &file_writer.interface);
    };
    try file_writer.interface.flush();

    switch (response.status) {
        .ok => return,
        .not_found => return error.GitHubReleaseAssetNotFound,
        else => {
            std.debug.print(
                "GitHub asset download failed through Zig HTTP: status={d} ({s}) url={s}\n",
                .{
                    @intFromEnum(response.status),
                    @tagName(response.status),
                    download_url,
                },
            );
            printFailedDownloadBody(allocator, destination_path);
            return error.GitHubAssetDownloadFailed;
        },
    }
}

fn printFailedDownloadBody(allocator: std.mem.Allocator, destination_path: []const u8) void {
    const body = std.Io.Dir.cwd().readFileAlloc(
        std.Options.debug_io,
        destination_path,
        allocator,
        .limited(2048),
    ) catch return;
    defer allocator.free(body);
    const trimmed = std.mem.trim(u8, body, " \t\r\n");
    if (trimmed.len == 0) return;
    std.debug.print("GitHub asset error body: {s}\n", .{trimmed});
}

fn fetchWriterViaManualRedirect(
    allocator: std.mem.Allocator,
    url: []const u8,
    writer: *std.Io.Writer,
) !HttpStatusResult {
    const redirect_url = try fetchSingleRedirectLocation(allocator, url);
    defer allocator.free(redirect_url);
    return fetchWriterMinimal(allocator, redirect_url, writer);
}

fn fetchSingleRedirectLocation(
    allocator: std.mem.Allocator,
    url: []const u8,
) ![]u8 {
    var client: std.http.Client = .{
        .allocator = allocator,
        .io = std.Options.debug_io,
    };
    defer client.deinit();

    const uri = try std.Uri.parse(url);
    var headers = [_]std.http.Header{
        .{ .name = "User-Agent", .value = "kirac-fetch-toolchain" },
    };
    var req = try std.http.Client.request(&client, .GET, uri, .{
        .redirect_behavior = .unhandled,
        .extra_headers = &headers,
    });
    defer req.deinit();

    try req.sendBodiless();
    var response = try req.receiveHead(&.{});
    if (response.head.status.class() != .redirect) return error.GitHubAssetDownloadFailed;
    const location = response.head.location orelse return error.GitHubAssetDownloadFailed;

    const reader = response.reader(&.{});
    _ = reader.discardRemaining() catch |err| switch (err) {
        error.ReadFailed => return response.bodyErr().?,
    };
    return allocator.dupe(u8, location);
}

fn fetchWriterMinimal(
    allocator: std.mem.Allocator,
    url: []const u8,
    writer: *std.Io.Writer,
) !HttpStatusResult {
    var client: std.http.Client = .{
        .allocator = allocator,
        .io = std.Options.debug_io,
    };
    defer client.deinit();

    var redirect_buffer: [64 * 1024]u8 = undefined;
    const result = try client.fetch(.{
        .location = .{ .url = url },
        .response_writer = writer,
        .extra_headers = &.{},
        .redirect_buffer = &redirect_buffer,
    });
    return .{ .status = result.status };
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
) !HttpStatusResult {
    var client: std.http.Client = .{
        .allocator = allocator,
        .io = std.Options.debug_io,
    };
    defer client.deinit();

    const token = githubToken(allocator) catch null;
    defer if (token) |value| allocator.free(value);

    var extra_headers = std.array_list.Managed(std.http.Header).init(allocator);
    defer extra_headers.deinit();
    var privileged_headers = std.array_list.Managed(std.http.Header).init(allocator);
    defer privileged_headers.deinit();

    try extra_headers.append(.{ .name = "User-Agent", .value = "kirac-fetch-toolchain" });

    if (is_api_request) {
        try extra_headers.append(.{ .name = "Accept", .value = "application/vnd.github+json" });
        try extra_headers.append(.{ .name = "X-GitHub-Api-Version", .value = "2022-11-28" });
    } else {
        try privileged_headers.append(.{ .name = "Accept", .value = "application/octet-stream" });
    }

    var auth_header_value: ?[]const u8 = null;
    defer if (auth_header_value) |value| allocator.free(value);

    if (is_api_request) {
        if (token) |value| {
            auth_header_value = try std.fmt.allocPrint(allocator, "Bearer {s}", .{value});
            try privileged_headers.append(.{ .name = "Authorization", .value = auth_header_value.? });
        }
    }

    // GitHub release assets redirect to huge signed URLs.
    // 512 bytes is too small and causes HttpRedirectLocationOversize.
    var redirect_buffer: [64 * 1024]u8 = undefined;

    const result = try client.fetch(.{
        .location = .{ .url = url },
        .response_writer = writer,
        .extra_headers = extra_headers.items,
        .privileged_headers = privileged_headers.items,
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
