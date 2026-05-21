const std = @import("std");
const hybrid = @import("kira_hybrid_definition");
const hybrid_runtime = @import("kira_hybrid_runtime");
const model = @import("model.zig");
const protocol = @import("protocol.zig");

extern fn kira_live_install_first_frame_hook(callback: *const fn () callconv(.c) void) callconv(.c) void;

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
    const local_cache_root = if (std.fs.path.isAbsolute(runner_manifest.local_cache_path))
        try allocator.dupe(u8, runner_manifest.local_cache_path)
    else
        try std.fs.path.join(allocator, &.{ manifest_dir, runner_manifest.local_cache_path });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, local_cache_root);

    var client = try RunnerClient.connect(allocator, runner_manifest.server_host, runner_manifest.server_port);
    defer client.close();
    active_client = &client;
    defer active_client = null;
    first_frame_sent = false;
    kira_live_install_first_frame_hook(kiraLiveFirstFrameHook);

    try client.sendText(.hello, "kira-live-runner");
    try client.sendText(.runtime_info, runner_manifest.name);
    try client.sendText(.log_line, "KIRA_LIVE_CONNECTED");

    try receiveInitialBundles(allocator, &client, local_cache_root, runner_manifest.main_bundle_id);
    const bundle_root = try std.fs.path.join(allocator, &.{ local_cache_root, "bundles", try std.fmt.allocPrint(allocator, "{s}.klbundle", .{runner_manifest.main_bundle_id}) });
    const bundle_manifest_path = try std.fs.path.join(allocator, &.{ bundle_root, "KiraBundle.toml" });
    const bundle_manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, bundle_manifest_path, allocator, .limited(1024 * 1024));
    const bundle_manifest = try model.BundleManifest.parse(allocator, bundle_manifest_text);
    const hybrid_path = try std.fs.path.join(allocator, &.{ bundle_root, bundle_manifest.hybrid_rel_path });
    var hybrid_manifest = try hybrid.HybridModuleManifest.readFromFile(allocator, hybrid_path);
    hybrid_manifest.bytecode_path = try std.fs.path.join(allocator, &.{ bundle_root, bundle_manifest.bytecode_rel_path });

    var runtime = if (std.mem.eql(u8, hybrid_manifest.native_library_path, "__kira_live_self__"))
        try hybrid_runtime.HybridRuntime.initFromCurrentProcess(allocator, hybrid_manifest)
    else
        try hybrid_runtime.HybridRuntime.init(allocator, hybrid_manifest);
    defer runtime.deinit();
    try runtime.bridge.installFirstFrameHook(kiraLiveFirstFrameHook);
    try runtime.bridge.installLogHook(kiraLiveLogHook);
    try client.sendText(.log_line, "KIRA_BUNDLE_LINKED");
    try client.sendText(.log_line, "KIRA_ENTRYPOINT_STARTED");
    try runtime.run();
}

fn receiveInitialBundles(
    allocator: std.mem.Allocator,
    client: *RunnerClient,
    local_cache_root: []const u8,
    main_bundle_id: []const u8,
) !void {
    while (true) {
        const frame = try client.readFrame(allocator);
        switch (frame.kind) {
            .bundle_graph => {
                try client.sendText(.log_line, "KIRA_BUNDLE_GRAPH_RECEIVED");
            },
            .replace_bundle => {
                const payload = try protocol.decodeReplaceBundlePayload(allocator, frame.payload);
                const bundle_dir = try std.fs.path.join(allocator, &.{ local_cache_root, "bundles", try std.fmt.allocPrint(allocator, "{s}.klbundle", .{payload.bundle_id}) });
                try storeBundlePayload(bundle_dir, payload);
                if (std.mem.eql(u8, payload.bundle_id, main_bundle_id)) {
                    try client.sendText(.log_line, "KIRA_BUNDLE_LOADED");
                    return;
                }
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
        client.sendText(.log_line, "KIRA_APP_RENDERED_FIRST_FRAME") catch {};
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
