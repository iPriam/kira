const std = @import("std");
const hybrid = @import("kira_hybrid_definition");
const hybrid_runtime = @import("kira_hybrid_runtime");
const model = @import("model.zig");
const protocol = @import("protocol.zig");

const RequestQuitFn = *const fn () callconv(.c) void;

var active_client: ?*RunnerClient = null;
var first_frame_sent = false;

pub export fn kira_live_runner_entry(manifest_path: [*:0]const u8) callconv(.c) c_int {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();
    runFromManifestPath(arena.allocator(), std.mem.span(manifest_path)) catch return 1;
    return 0;
}

pub fn runFromManifestPath(allocator: std.mem.Allocator, manifest_path: []const u8) !void {
    const manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, manifest_path, allocator, .limited(1024 * 1024));
    const runner_manifest = try model.RunnerManifest.parse(allocator, manifest_text);
    const manifest_dir = std.fs.path.dirname(manifest_path) orelse ".";
    const local_cache_root = try resolveLocalCacheRoot(allocator, manifest_dir, runner_manifest.local_cache_path);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, local_cache_root);

    var client = try RunnerClient.connect(allocator, runner_manifest.server_host, runner_manifest.server_port);
    defer client.close();
    active_client = &client;
    defer active_client = null;
    first_frame_sent = false;

    try client.sendText(.hello, "kira-live-runner");
    try client.sendText(.runtime_info, runner_manifest.name);
    try client.sendText(.log_line, try std.fmt.allocPrint(allocator, "live.runner.pid={d}", .{std.c.getpid()}));
    try client.sendText(.log_line, "live.client.connected");

    var restart_count: u32 = 0;
    while (true) : (restart_count += 1) {
        switch (try receiveBundleSet(allocator, &client, local_cache_root, runner_manifest.main_bundle_id, restart_count != 0)) {
            .bundle_ready => {},
            .shutdown => return,
        }
        const bundle_root = try std.fs.path.join(allocator, &.{ local_cache_root, "bundles", try std.fmt.allocPrint(allocator, "{s}.klbundle", .{runner_manifest.main_bundle_id}) });
        try runBundle(allocator, &client, bundle_root, runner_manifest.target_path, restart_count);
    }
}

fn resolveLocalCacheRoot(allocator: std.mem.Allocator, manifest_dir: []const u8, local_cache_path: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(local_cache_path)) return allocator.dupe(u8, local_cache_path);
    if (std.mem.startsWith(u8, local_cache_path, "app-support/")) {
        const base = try appContainerPath(allocator, .application_support);
        return std.fs.path.join(allocator, &.{ base, local_cache_path["app-support/".len..] });
    }
    if (std.mem.startsWith(u8, local_cache_path, "app-cache/")) {
        const base = try appContainerPath(allocator, .caches);
        return std.fs.path.join(allocator, &.{ base, local_cache_path["app-cache/".len..] });
    }
    if (std.mem.startsWith(u8, local_cache_path, "tmp/")) {
        const base = try appContainerPath(allocator, .temporary);
        return std.fs.path.join(allocator, &.{ base, local_cache_path["tmp/".len..] });
    }
    return std.fs.path.join(allocator, &.{ manifest_dir, local_cache_path });
}

const AppContainerPathKind = enum {
    application_support,
    caches,
    temporary,
};

fn appContainerPath(allocator: std.mem.Allocator, kind: AppContainerPathKind) ![]const u8 {
    if (std.c.getenv("KIRA_LIVE_APP_CONTAINER_ROOT")) |raw| {
        const root = std.mem.span(raw);
        if (root.len != 0) return appContainerPathFromRoot(allocator, root, kind);
    }
    if (std.c.getenv("HOME")) |raw| {
        const home = std.mem.span(raw);
        if (home.len != 0) return appContainerPathFromRoot(allocator, home, kind);
    }
    if (kind == .temporary) {
        if (std.c.getenv("TMPDIR")) |raw| {
            const tmp = std.mem.span(raw);
            if (tmp.len != 0) return allocator.dupe(u8, tmp);
        }
    }
    return appContainerPathFromRoot(allocator, ".", kind);
}

fn appContainerPathFromRoot(allocator: std.mem.Allocator, root: []const u8, kind: AppContainerPathKind) ![]const u8 {
    return switch (kind) {
        .application_support => std.fs.path.join(allocator, &.{ root, "Library", "Application Support" }),
        .caches => std.fs.path.join(allocator, &.{ root, "Library", "Caches" }),
        .temporary => std.fs.path.join(allocator, &.{ root, "tmp" }),
    };
}

fn runBundle(
    allocator: std.mem.Allocator,
    client: *RunnerClient,
    bundle_root: []const u8,
    target_root: []const u8,
    restart_count: u32,
) !void {
    first_frame_sent = false;
    const bundle_manifest_path = try std.fs.path.join(allocator, &.{ bundle_root, "KiraBundle.toml" });
    const bundle_manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, bundle_manifest_path, allocator, .limited(1024 * 1024));
    const bundle_manifest = try model.BundleManifest.parse(allocator, bundle_manifest_text);
    const hybrid_path = try std.fs.path.join(allocator, &.{ bundle_root, bundle_manifest.hybrid_rel_path });
    const runtime_allocator = std.heap.smp_allocator;
    var hybrid_manifest = try hybrid.HybridModuleManifest.readFromFile(runtime_allocator, hybrid_path);
    hybrid_manifest.bytecode_path = try std.fs.path.join(runtime_allocator, &.{ bundle_root, bundle_manifest.bytecode_rel_path });

    var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
    defer {
        std.process.setCurrentDir(std.Options.debug_io, original_cwd) catch {};
        original_cwd.close(std.Options.debug_io);
    }
    var bundle_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, bundle_root, .{});
    defer bundle_dir.close(std.Options.debug_io);
    try std.process.setCurrentDir(std.Options.debug_io, bundle_dir);
    var runtime = if (std.mem.eql(u8, hybrid_manifest.native_library_path, "__kira_live_self__"))
        try hybrid_runtime.HybridRuntime.initFromCurrentProcess(runtime_allocator, hybrid_manifest)
    else
        try hybrid_runtime.HybridRuntime.init(runtime_allocator, hybrid_manifest);
    defer runtime.deinit();
    try runtime.bridge.installFirstFrameHook(kiraLiveFirstFrameHook);
    try runtime.bridge.installLogHook(kiraLiveLogHook);
    startNativeQuitTimer(&runtime) catch {};
    try client.sendText(.log_line, "live.bundle.linked");
    if (restart_count == 0) {
        try client.sendText(.log_line, "live.entrypoint.started");
    } else {
        try client.sendText(.log_line, "live.entrypoint.restarted");
    }
    var target_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, target_root, .{});
    defer target_dir.close(std.Options.debug_io);
    try std.process.setCurrentDir(std.Options.debug_io, target_dir);
    try runtime.run();
    if (restart_count == 0) {
        try client.sendText(.log_line, "live.entrypoint.finished");
    } else {
        try client.sendText(.log_line, "live.hot_restart.finished");
    }
}

fn startNativeQuitTimer(runtime: *hybrid_runtime.HybridRuntime) !void {
    const duration_raw = std.c.getenv("KIRA_LIVE_QUIT_AFTER_NS") orelse return;
    const duration_ns = std.fmt.parseInt(u64, std.mem.span(duration_raw), 10) catch return;
    if (duration_ns == 0) return;
    if (runtime.bridge.library) |*library| {
        const request_quit = library.lookup(RequestQuitFn, "sapp_request_quit") orelse return;
        var thread = try std.Thread.spawn(.{}, nativeQuitTimerMain, .{ duration_ns, request_quit });
        thread.detach();
    }
}

fn nativeQuitTimerMain(duration_ns: u64, request_quit: RequestQuitFn) void {
    std.Options.debug_io.sleep(.fromNanoseconds(@intCast(duration_ns)), .awake) catch {};
    request_quit();
}

const ReceiveResult = enum { bundle_ready, shutdown };

fn receiveBundleSet(
    allocator: std.mem.Allocator,
    client: *RunnerClient,
    local_cache_root: []const u8,
    main_bundle_id: []const u8,
    is_reload: bool,
) !ReceiveResult {
    while (true) {
        const frame = try client.readFrame(allocator);
        switch (frame.kind) {
            .bundle_graph => {
                try client.sendText(.log_line, "live.bundle.graph.received");
            },
            .replace_bundle => {
                const payload = try protocol.decodeReplaceBundlePayload(allocator, frame.payload);
                try client.sendText(.log_line, "live.client.bundle.received");
                try client.sendText(.log_line, if (is_reload) "live.bundle.update.received" else "live.bundle.received");
                const bundle_dir = try std.fs.path.join(allocator, &.{ local_cache_root, "bundles", try std.fmt.allocPrint(allocator, "{s}.klbundle", .{payload.bundle_id}) });
                try storeBundlePayload(bundle_dir, payload);
                if (std.mem.eql(u8, payload.bundle_id, main_bundle_id)) {
                    try client.sendText(.log_line, "live.bundle.loaded");
                    if (is_reload) {
                        try client.sendText(.log_line, "live.client.hot_restart.started");
                        try client.sendText(.log_line, "live.hot_restart.started");
                    }
                    return .bundle_ready;
                }
            },
            .shutdown => {
                try client.sendText(.log_line, "live.shutdown.started");
                try client.sendText(.log_line, "live.shutdown.received");
                try client.sendText(.shutdown_ack, "ok");
                try client.sendText(.log_line, "live.shutdown.finished");
                return .shutdown;
            },
            else => {},
        }
    }
}

fn storeBundlePayload(bundle_dir: []const u8, payload: protocol.ReplaceBundlePayload) !void {
    for (payload.files) |file| {
        const path = try std.fs.path.join(std.heap.page_allocator, &.{ bundle_dir, file.relative_path });
        defer std.heap.page_allocator.free(path);
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, std.fs.path.dirname(path) orelse ".");
        const out = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
        defer out.close(std.Options.debug_io);
        try out.writeStreamingAll(std.Options.debug_io, file.bytes);
    }
}

fn kiraLiveFirstFrameHook() callconv(.c) void {
    if (first_frame_sent) return;
    first_frame_sent = true;
    if (active_client) |client| {
        client.sendText(.log_line, "live.frame.presented") catch {};
    }
}

fn kiraLiveLogHook(line: [*:0]const u8) callconv(.c) void {
    if (active_client) |client| {
        client.sendText(.log_line, std.mem.span(line)) catch {};
    }
}

const RunnerClient = struct {
    allocator: std.mem.Allocator,
    io_impl: std.Io.Threaded,
    stream: std.Io.net.Stream,
    reader: std.Io.net.Stream.Reader,
    writer: std.Io.net.Stream.Writer,
    reader_buffer: [4096]u8,
    writer_buffer: [4096]u8,

    fn connect(allocator: std.mem.Allocator, host: []const u8, port: u16) !RunnerClient {
        var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
        const io = io_impl.io();
        const address = try std.Io.net.IpAddress.parse(host, port);
        const stream = try std.Io.net.IpAddress.connect(&address, io, .{
            .mode = .stream,
            .protocol = .tcp,
        });
        var client = RunnerClient{
            .allocator = allocator,
            .io_impl = io_impl,
            .stream = stream,
            .reader = undefined,
            .writer = undefined,
            .reader_buffer = undefined,
            .writer_buffer = undefined,
        };
        client.reader = std.Io.net.Stream.Reader.init(client.stream, client.io_impl.io(), &client.reader_buffer);
        client.writer = std.Io.net.Stream.Writer.init(client.stream, client.io_impl.io(), &client.writer_buffer);
        return client;
    }

    fn close(self: *RunnerClient) void {
        self.stream.close(self.io_impl.io());
        self.io_impl.deinit();
    }

    fn sendText(self: *RunnerClient, kind: protocol.LiveMessageKind, text: []const u8) !void {
        try protocol.writeFrame(&self.writer.interface, kind, text);
        try self.writer.interface.flush();
    }

    fn readFrame(self: *RunnerClient, allocator: std.mem.Allocator) !protocol.Frame {
        return protocol.readFrame(allocator, &self.reader.interface);
    }
};

test "live runner cache paths can target app-container writable roots" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const app_support = try appContainerPathFromRoot(arena.allocator(), "/tmp/KiraContainer", .application_support);
    try std.testing.expectEqualStrings("/tmp/KiraContainer/Library/Application Support", app_support);
    const caches = try appContainerPathFromRoot(arena.allocator(), "/tmp/KiraContainer", .caches);
    try std.testing.expectEqualStrings("/tmp/KiraContainer/Library/Caches", caches);
    const temporary = try appContainerPathFromRoot(arena.allocator(), "/tmp/KiraContainer", .temporary);
    try std.testing.expectEqualStrings("/tmp/KiraContainer/tmp", temporary);

    const relative = try resolveLocalCacheRoot(arena.allocator(), "/App.app/Contents/Resources", "cache");
    try std.testing.expectEqualStrings("/App.app/Contents/Resources/cache", relative);
}
