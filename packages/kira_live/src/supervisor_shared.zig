const std = @import("std");
const builtin = @import("builtin");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const live = @import("root.zig");
const live_build_options = @import("kira_live_build_options");
const model = @import("model.zig");
const protocol = @import("protocol.zig");

pub fn renderStandaloneDiagnostic(stderr: anytype, item: diagnostics.Diagnostic) !void {
    const items = [_]diagnostics.Diagnostic{item};
    for (&items) |diag| {
        const severity = switch (diag.severity) {
            .@"error" => "error",
            .warning => "warning",
            .note => "note",
        };
        if (diag.code) |code| {
            try stderr.print("{s}[{s}]: {s}\n", .{ severity, code, diag.title });
        } else {
            try stderr.print("{s}: {s}\n", .{ severity, diag.title });
        }
        try stderr.print("  {s}\n", .{diag.message});
        if (diag.domain) |domain| try stderr.print("  domain: {s}\n", .{domain});
        if (diag.phase) |phase| try stderr.print("  phase: {s}\n", .{phase});
        for (diag.notes) |note| try stderr.print("  note: {s}\n", .{note});
        if (diag.help) |help| try stderr.print("  help: {s}\n", .{help});
    }
}

pub const LiveServer = struct {
    allocator: std.mem.Allocator,
    io_impl: std.Io.Threaded,
    server: std.Io.net.Server,
    graph: live.BundleGraph,
    port: u16,

    pub fn listen(allocator: std.mem.Allocator, bind_host: []const u8, port: u16, graph: live.BundleGraph) !LiveServer {
        var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
        const io = io_impl.io();
        const bind_address = try std.Io.net.IpAddress.parse(bind_host, port);
        const server = try std.Io.net.IpAddress.listen(&bind_address, io, .{
            .reuse_address = true,
            .mode = .stream,
            .protocol = .tcp,
        });
        return .{
            .allocator = allocator,
            .io_impl = io_impl,
            .server = server,
            .graph = graph,
            .port = port,
        };
    }

    pub fn deinit(self: *LiveServer) void {
        self.server.deinit(self.io_impl.io());
        self.io_impl.deinit();
    }

    pub fn accept(self: *LiveServer) !LiveConnection {
        const stream = try self.server.accept(self.io_impl.io());
        return LiveConnection.init(self.allocator, self.graph, self.io_impl.io(), stream);
    }
};

pub const LiveConnection = struct {
    allocator: std.mem.Allocator,
    graph: live.BundleGraph,
    io: std.Io,
    stream: std.Io.net.Stream,
    reader: std.Io.net.Stream.Reader,
    writer: std.Io.net.Stream.Writer,
    reader_buffer: [4096]u8,
    writer_buffer: [4096]u8,

    fn init(allocator: std.mem.Allocator, graph: live.BundleGraph, io: std.Io, stream: std.Io.net.Stream) LiveConnection {
        var connection = LiveConnection{
            .allocator = allocator,
            .graph = graph,
            .io = io,
            .stream = stream,
            .reader = undefined,
            .writer = undefined,
            .reader_buffer = undefined,
            .writer_buffer = undefined,
        };
        connection.reader = std.Io.net.Stream.Reader.init(connection.stream, io, &connection.reader_buffer);
        connection.writer = std.Io.net.Stream.Writer.init(connection.stream, io, &connection.writer_buffer);
        return connection;
    }

    pub fn close(self: *LiveConnection) void {
        self.stream.close(self.io);
    }

    pub fn sendGraphAndBundles(self: *LiveConnection) !void {
        var graph_buffer: [8192]u8 = undefined;
        var writer = std.Io.Writer.fixed(&graph_buffer);
        try self.graph.writeToml(&writer);
        try protocol.writeFrame(&self.writer.interface, .bundle_graph, writer.buffered());
        try self.writer.interface.flush();

        for (self.graph.bundles) |bundle| {
            if (std.mem.eql(u8, bundle.id, self.graph.main_bundle_id)) continue;
            try self.sendBundle(bundle);
        }
        for (self.graph.bundles) |bundle| {
            if (!std.mem.eql(u8, bundle.id, self.graph.main_bundle_id)) continue;
            try self.sendBundle(bundle);
        }
    }

    fn sendBundle(self: *LiveConnection, bundle: model.BundleSpec) !void {
        const bundle_dir = try std.fs.path.join(self.allocator, &.{ std.fs.path.dirname(std.fs.path.dirname(bundle.manifest_rel_path) orelse ".") orelse ".", "" });
        _ = bundle_dir;
        const manifest_path = try std.fs.path.join(self.allocator, &.{ self.graph.target_path, ".kira-build", "live", bundle.manifest_rel_path });
        const bundle_root = std.fs.path.dirname(manifest_path) orelse return error.LiveBundleBuildFailed;
        const files = try collectBundleFiles(self.allocator, bundle_root, bundle_root);
        const payload = try protocol.encodeReplaceBundlePayload(self.allocator, bundle.id, files);
        try protocol.writeFrame(&self.writer.interface, .replace_bundle, payload);
        try self.writer.interface.flush();
    }

    pub fn waitForHealthMarkers(self: *LiveConnection, stdout: anytype, timeout_ns: u64, require_frame: bool) !bool {
        const markers = [_][]const u8{
            "live.bundle.graph.received",
            "live.client.bundle.received",
            "live.bundle.loaded",
            "live.bundle.linked",
            "live.entrypoint.started",
            "live.frame.presented",
        };
        var seen = [_]bool{false} ** markers.len;
        const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
        while (elapsedSince(start) < timeout_ns) {
            if (self.reader.interface.bufferedLen() == 0 and !try waitReadable(self.stream.socket.handle, 250)) continue;
            const frame = protocol.readFrame(self.allocator, &self.reader.interface) catch return false;
            if (frame.kind == .log_line) {
                try stdout.print("{s}\n", .{frame.payload});
                for (markers, 0..) |marker, index| {
                    if (std.mem.eql(u8, frame.payload, marker)) seen[index] = true;
                }
                if (allSeen(seen, require_frame)) return true;
            }
        }
        return false;
    }

    pub fn waitForShutdownAck(self: *LiveConnection, stdout: anytype, timeout_ns: u64) !bool {
        const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
        while (elapsedSince(start) < timeout_ns) {
            if (self.reader.interface.bufferedLen() == 0 and !try waitReadable(self.stream.socket.handle, 100)) continue;
            const frame = protocol.readFrame(self.allocator, &self.reader.interface) catch return false;
            if (frame.kind == .log_line) {
                try stdout.print("{s}\n", .{frame.payload});
            }
            if (frame.kind == .shutdown_ack) {
                try emitEvent(stdout, "live.shutdown.ack", "client=desktop", .{});
                return true;
            }
        }
        return false;
    }
};

pub fn acceptClientOrDiagnose(
    allocator: std.mem.Allocator,
    server: *LiveServer,
    child: *std.process.Child,
    io: std.Io,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !?LiveConnection {
    const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
    while (elapsedSince(start) < 30 * std.time.ns_per_s) {
        if (try pollChildExited(child)) {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveRunnerExitedEarly(allocator, target.target_root));
            return null;
        }
        if (try waitReadable(server.server.socket.handle, 250)) {
            try emitEvent(stdout, "live.client.connecting", "target={s}", .{target.target_root});
            return try server.accept();
        }
    }
    killAndWait(child, io);
    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveClientFailedToConnect(allocator, target.target_root));
    return null;
}

pub fn killAndWait(child: *std.process.Child, io: std.Io) void {
    if (child.id == null) return;
    child.kill(io);
}

pub fn waitReadable(fd: anytype, timeout_ms: i32) !bool {
    if (builtin.os.tag == .windows) {
        var pollfd = [_]WindowsPollFd{.{
            .fd = fd,
            .events = windows_poll_in,
            .revents = 0,
        }};
        const ready = WSAPoll(&pollfd, pollfd.len, timeout_ms);
        if (ready < 0) return error.PollFailed;
        return ready > 0 and (pollfd[0].revents & windows_poll_in) != 0;
    }
    var pollfd = [_]std.posix.pollfd{.{
        .fd = fd,
        .events = std.posix.POLL.IN,
        .revents = 0,
    }};
    const ready = try std.posix.poll(&pollfd, timeout_ms);
    return ready > 0 and (pollfd[0].revents & std.posix.POLL.IN) != 0;
}

const windows_poll_in: c_short = 0x0300;

const WindowsPollFd = extern struct {
    fd: *anyopaque,
    events: c_short,
    revents: c_short,
};

extern "ws2_32" fn WSAPoll(fd_array: [*]WindowsPollFd, fds: u32, timeout: c_int) callconv(.winapi) c_int;

pub fn pollChildExited(child: *std.process.Child) !bool {
    const pid = child.id orelse return true;
    switch (builtin.os.tag) {
        .windows, .wasi => return false,
        else => {
            var status: c_int = 0;
            const result = std.c.waitpid(pid, &status, @intCast(std.c.W.NOHANG));
            if (result == 0) return false;
            if (result == pid) {
                child.id = null;
                return true;
            }
            return false;
        },
    }
}

pub fn waitChildExitBefore(child: *std.process.Child, timeout_ns: u64) !bool {
    const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
    while (elapsedSince(start) < timeout_ns) {
        if (try pollChildExited(child)) return true;
        try std.Options.debug_io.sleep(.fromNanoseconds(100 * std.time.ns_per_ms), .awake);
    }
    return try pollChildExited(child);
}

/// Like `pollChildExited`, but preserves the child's `Term` (exit code / signal)
/// instead of discarding it. Returns `null` while the child is still running (or
/// when status cannot be reaped here, e.g. windows/wasi). On exit it reaps the
/// child (clearing `child.id`, so a later `kill` is a no-op) and decodes the
/// wait status into a `Term`.
pub fn pollChildTerm(child: *std.process.Child) !?std.process.Child.Term {
    const pid = child.id orelse return null;
    switch (builtin.os.tag) {
        .windows, .wasi => return null,
        else => {
            var status: c_int = 0;
            const result = std.c.waitpid(pid, &status, @intCast(std.c.W.NOHANG));
            if (result == 0) return null;
            if (result == pid) {
                child.id = null;
                return std.Io.Threaded.statusToTerm(@bitCast(status));
            }
            return null;
        },
    }
}

/// Poll for the child's `Term` up to `timeout_ns`. Returns the `Term` if the
/// child stopped on its own before the deadline, or `null` if it is still alive
/// (the caller is then expected to terminate it deliberately).
pub fn waitChildTermBefore(child: *std.process.Child, timeout_ns: u64) !?std.process.Child.Term {
    const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
    while (elapsedSince(start) < timeout_ns) {
        if (try pollChildTerm(child)) |term| return term;
        try std.Options.debug_io.sleep(.fromNanoseconds(100 * std.time.ns_per_ms), .awake);
    }
    return try pollChildTerm(child);
}

pub const SourceSnapshot = struct {
    mtime_ns: i96,
    size: u64,

    pub fn capture(allocator: std.mem.Allocator, path: []const u8) !SourceSnapshot {
        _ = allocator;
        const stat = try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{});
        return .{
            .mtime_ns = stat.mtime.nanoseconds,
            .size = stat.size,
        };
    }

    pub fn changed(self: *SourceSnapshot, path: []const u8) !bool {
        const stat = try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{});
        return stat.mtime.nanoseconds != self.mtime_ns or stat.size != self.size;
    }

    pub fn refresh(self: *SourceSnapshot, path: []const u8) !void {
        const stat = try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{});
        self.mtime_ns = stat.mtime.nanoseconds;
        self.size = stat.size;
    }
};

fn collectBundleFiles(allocator: std.mem.Allocator, root: []const u8, current: []const u8) ![]const protocol.ReplaceBundlePayload.FilePayload {
    var files = std.array_list.Managed(protocol.ReplaceBundlePayload.FilePayload).init(allocator);
    try appendBundleFiles(allocator, &files, root, current);
    return files.toOwnedSlice();
}

fn appendBundleFiles(
    allocator: std.mem.Allocator,
    files: *std.array_list.Managed(protocol.ReplaceBundlePayload.FilePayload),
    root: []const u8,
    current: []const u8,
) !void {
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, current, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        const child = try std.fs.path.join(allocator, &.{ current, entry.name });
        switch (entry.kind) {
            .directory => try appendBundleFiles(allocator, files, root, child),
            .file => {
                const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, child, allocator, .limited(16 * 1024 * 1024));
                const relative = try std.fs.path.relative(allocator, root, null, root, child);
                try files.append(.{ .relative_path = relative, .bytes = bytes });
            },
            else => {},
        }
    }
}

fn allSeen(values: [6]bool, require_frame: bool) bool {
    for (values, 0..) |value, index| {
        if (!require_frame and index == values.len - 1) continue;
        if (!value) return false;
    }
    return true;
}

pub fn rewriteRunnerManifestPort(allocator: std.mem.Allocator, manifest_path: []const u8, port: u16) !void {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, manifest_path, allocator, .limited(1024 * 1024));
    var parsed = try model.RunnerManifest.parse(allocator, text);
    parsed.server_port = port;
    try writeTomlFile(manifest_path, parsed);
}

pub fn rewriteRunnerManifestEndpoint(allocator: std.mem.Allocator, manifest_path: []const u8, host: []const u8, port: u16) !void {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, manifest_path, allocator, .limited(1024 * 1024));
    var parsed = try model.RunnerManifest.parse(allocator, text);
    parsed.server_host = host;
    parsed.server_port = port;
    try writeTomlFile(manifest_path, parsed);
}

pub fn writeTomlFile(path: []const u8, value: anytype) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    var buffer: [4096]u8 = undefined;
    var writer = file.writer(std.Options.debug_io, &buffer);
    try value.writeToml(&writer.interface);
    try writer.interface.flush();
}

pub fn writeFile(path: []const u8, data: []const u8) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

pub fn writeRawBytes(path: []const u8, data: []const u8) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

pub fn runTool(allocator: std.mem.Allocator, argv: []const []const u8) !void {
    return runToolWithDeveloperDir(allocator, null, null, argv);
}

pub fn runToolInCwd(allocator: std.mem.Allocator, cwd: []const u8, argv: []const []const u8) !void {
    return runToolWithDeveloperDir(allocator, null, cwd, argv);
}

pub fn runToolWithDeveloperDir(allocator: std.mem.Allocator, developer_dir: ?[]const u8, cwd: ?[]const u8, argv: []const []const u8) !void {
    const process_environ: std.process.Environ = switch (builtin.os.tag) {
        .windows => .{ .block = .global },
        .wasi, .emscripten, .freestanding, .other => .empty,
        else => .{ .block = .{ .slice = std.c.environ[0..blk: {
            var len: usize = 0;
            while (std.c.environ[len] != null) : (len += 1) {}
            break :blk len;
        } :null] } },
    };
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    var environ_map = if (developer_dir != null) try std.process.Environ.createMap(process_environ, allocator) else null;
    defer if (environ_map) |*map| map.deinit();
    if (environ_map) |*map| {
        try map.put("DEVELOPER_DIR", developer_dir.?);
    }
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = argv,
        .expand_arg0 = .expand,
        .cwd = if (cwd) |path| .{ .path = path } else .inherit,
        .environ_map = if (environ_map) |*map| map else null,
        .stdout_limit = .limited(256 * 1024),
        .stderr_limit = .limited(256 * 1024),
    });
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);
    if (result.term == .exited and result.term.exited == 0) return;
    if (result.stdout.len != 0) std.debug.print("{s}", .{result.stdout});
    if (result.stderr.len != 0) std.debug.print("{s}", .{result.stderr});
    return error.ExternalCommandFailed;
}

pub fn runToolCapture(allocator: std.mem.Allocator, argv: []const []const u8) ![]u8 {
    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = argv,
        .expand_arg0 = .expand,
        .stdout_limit = .limited(512 * 1024),
        .stderr_limit = .limited(512 * 1024),
    });
    defer allocator.free(result.stderr);
    if (result.term == .exited and result.term.exited == 0) return result.stdout;
    allocator.free(result.stdout);
    return error.ExternalCommandFailed;
}

pub fn toolAvailable(allocator: std.mem.Allocator, name: []const u8) bool {
    if (findToolPath(allocator, name) catch null) |path| {
        allocator.free(path);
        return true;
    }
    return false;
}

pub fn findToolPath(allocator: std.mem.Allocator, name: []const u8) !?[]const u8 {
    const candidates = [_][]const u8{ "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/opt/homebrew/share/android-commandlinetools/platform-tools" };
    for (candidates) |dir| {
        const path = std.fs.path.join(allocator, &.{ dir, name }) catch continue;
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch {
            allocator.free(path);
            continue;
        };
        file.close(std.Options.debug_io);
        return path;
    }
    if (try findAndroidSdkToolPath(allocator, name)) |path| return path;
    return null;
}

fn findAndroidSdkToolPath(allocator: std.mem.Allocator, name: []const u8) !?[]const u8 {
    if (envVarOwned(allocator, "ANDROID_HOME")) |root| {
        defer allocator.free(root);
        if (try findAndroidSdkToolPathUnderRoot(allocator, root, name)) |path| return path;
    } else |_| {}
    if (envVarOwned(allocator, "ANDROID_SDK_ROOT")) |root| {
        defer allocator.free(root);
        if (try findAndroidSdkToolPathUnderRoot(allocator, root, name)) |path| return path;
    } else |_| {}
    if (envVarOwned(allocator, "HOME")) |home| {
        defer allocator.free(home);
        const root = try std.fs.path.join(allocator, &.{ home, "Library", "Android", "sdk" });
        defer allocator.free(root);
        if (try findAndroidSdkToolPathUnderRoot(allocator, root, name)) |path| return path;
    } else |_| {}
    return null;
}

fn findAndroidSdkToolPathUnderRoot(allocator: std.mem.Allocator, root: []const u8, name: []const u8) !?[]const u8 {
    const candidates = [_][]const u8{
        "platform-tools",
        "cmdline-tools/latest/bin",
        "emulator",
    };
    for (candidates) |relative| {
        const path = try std.fs.path.join(allocator, &.{ root, relative, name });
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch {
            allocator.free(path);
            continue;
        };
        file.close(std.Options.debug_io);
        return path;
    }
    return null;
}

pub fn envVarOwned(allocator: std.mem.Allocator, name: []const u8) ![]u8 {
    if (builtin.link_libc) {
        const name_z = try allocator.dupeZ(u8, name);
        defer allocator.free(name_z);
        const value = std.c.getenv(name_z.ptr) orelse return error.EnvironmentVariableNotFound;
        return allocator.dupe(u8, std.mem.span(value));
    }
    return error.EnvironmentVariableNotFound;
}

pub fn resolveLiveRunnerBuildRoot(allocator: std.mem.Allocator) ![]const u8 {
    if (isLiveRunnerBuildRoot(live_build_options.repo_root)) return allocator.dupe(u8, live_build_options.repo_root);
    if (try findRepoRootFromSelfExe(allocator)) |root| return root;
    if (try findRepoRootFromCwd(allocator)) |root| return root;
    return error.ExternalCommandFailed;
}

fn findRepoRootFromCwd(allocator: std.mem.Allocator) !?[]u8 {
    const cwd = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, ".", allocator);
    defer allocator.free(cwd);
    return findRepoRootFromPath(allocator, cwd);
}

fn findRepoRootFromSelfExe(allocator: std.mem.Allocator) !?[]u8 {
    const exe_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    defer allocator.free(exe_path);
    const exe_dir = std.fs.path.dirname(exe_path) orelse return null;
    return findRepoRootFromPath(allocator, exe_dir);
}

fn findRepoRootFromPath(allocator: std.mem.Allocator, start_path: []const u8) !?[]u8 {
    var current = try allocator.dupe(u8, start_path);
    errdefer allocator.free(current);
    while (true) {
        if (isLiveRunnerBuildRoot(current)) return current;
        const parent = std.fs.path.dirname(current) orelse break;
        if (std.mem.eql(u8, parent, current)) break;
        const parent_copy = try allocator.dupe(u8, parent);
        allocator.free(current);
        current = parent_copy;
    }
    allocator.free(current);
    return null;
}

fn isLiveRunnerBuildRoot(path: []const u8) bool {
    const build_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "build.zig" }) catch return false;
    defer std.heap.page_allocator.free(build_path);
    if (!fileExists(build_path)) return false;
    const runner_source = std.fs.path.join(std.heap.page_allocator, &.{ path, "packages", "kira_live", "src", "desktop_main.zig" }) catch return false;
    defer std.heap.page_allocator.free(runner_source);
    return fileExists(runner_source);
}

fn fileExists(path: []const u8) bool {
    const file = std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

pub fn inheritedProcessEnviron() std.process.Environ {
    return switch (builtin.os.tag) {
        .windows => .{ .block = .global },
        .wasi, .emscripten, .freestanding, .other => .empty,
        else => .{ .block = .{ .slice = std.c.environ[0..blk: {
            var len: usize = 0;
            while (std.c.environ[len] != null) : (len += 1) {}
            break :blk len;
        } :null] } },
    };
}

pub fn emitEvent(writer: anytype, name: []const u8, comptime fmt: []const u8, args: anytype) !void {
    try writer.print("event: {s}", .{name});
    if (fmt.len != 0) {
        try writer.writeAll(" ");
        try writer.print(fmt, args);
    }
    try writer.writeAll("\n");
    try writer.flush();
}

pub fn emitStderrEvent(writer: anytype, name: []const u8, comptime fmt: []const u8, args: anytype) !void {
    try emitEvent(writer, name, fmt, args);
}

pub fn elapsedSince(start: std.Io.Clock.Timestamp) u64 {
    const duration_ns = start.durationTo(std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake)).raw.toNanoseconds();
    return @intCast(@max(duration_ns, 0));
}
