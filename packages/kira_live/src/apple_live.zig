const std = @import("std");
const diag_messages = @import("kira_diagnostic_messages");
const live = @import("root.zig");
const protocol = @import("protocol.zig");
const live_args = @import("live_args.zig");
const shared = @import("supervisor_shared.zig");
const workspace = @import("apple_workspace.zig");

pub const Platform = enum { macos, ios_simulator };

// Unified Apple live: generate the SAME KiraApp.xcworkspace that `kira export apple`
// produces (runtime mode = live), build the requested platform scheme, launch it, and
// drive it through the live server. The app boots via the shared kira_live_runner_entry.
pub fn run(
    allocator: std.mem.Allocator,
    parsed: live_args.ParsedArgs,
    target: live.ResolvedLiveTarget,
    platform: Platform,
    stdout: anytype,
    stderr: anytype,
) !void {
    const developer_dir_capture = shared.runToolCapture(allocator, &.{ "xcode-select", "-p" }) catch {
        try shared.renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAppleTools(allocator, "xcode-select"));
        return error.CommandFailed;
    };
    defer allocator.free(developer_dir_capture);
    const developer_dir = std.mem.trim(u8, developer_dir_capture, " \t\r\n");

    const apple_root = try std.fs.path.join(allocator, &.{ target.output_root, "apple" });
    _ = workspace.generate(allocator, target, .{
        .apple_root = apple_root,
        .mode = .live,
        .server_host = "127.0.0.1",
        .server_port = 0,
    }) catch {
        try shared.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveSmokeUnsupportedTarget(allocator, parsed.input_path));
        return error.CommandFailed;
    };
    try shared.emitEvent(stdout, "live.export.workspace.generated", "path={s}", .{apple_root});

    // Streamable bundles for the live server. embed_native_in_runner = true so the
    // streamed hybrid manifest binds to the in-process native (the KiraApp executable
    // already links the platform's Kira native code). This keeps a single sokol/Metal
    // instance — streaming a second native dylib would clash on the iOS Metal device.
    const selector = try platformSelector(allocator, platform);
    const bundles = live.buildBundles(allocator, target, selector, true) catch |err| switch (err) {
        error.LiveBundleBuildFailed => {
            try shared.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveSmokeUnsupportedTarget(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        else => return err,
    };

    const scheme = schemeName(platform);
    const product_name = scheme;
    const project_path = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcodeproj" });
    const derived_data = try std.fs.path.join(allocator, &.{ apple_root, "DerivedData" });
    const source_manifest = try std.fs.path.join(allocator, &.{ apple_root, "Resources", "KiraRunner.toml" });

    var server = shared.LiveServer.listen(allocator, "127.0.0.1", 42111, bundles.graph) catch {
        try shared.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveServerFailedToStart(allocator, "127.0.0.1", 42111));
        return error.CommandFailed;
    };
    defer server.deinit();
    try shared.rewriteRunnerManifestPort(allocator, source_manifest, server.port);
    const manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, source_manifest, allocator, .limited(1024 * 1024));

    switch (platform) {
        .macos => try runMacOS(allocator, parsed, target, bundles, developer_dir, project_path, scheme, derived_data, product_name, manifest_text, &server, stdout, stderr),
        .ios_simulator => try runIosSimulator(allocator, parsed, target, bundles, developer_dir, project_path, scheme, derived_data, product_name, manifest_text, &server, stdout, stderr),
    }
}

fn runMacOS(
    allocator: std.mem.Allocator,
    parsed: live_args.ParsedArgs,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    developer_dir: []const u8,
    project_path: []const u8,
    scheme: []const u8,
    derived_data: []const u8,
    product_name: []const u8,
    manifest_text: []const u8,
    server: *shared.LiveServer,
    stdout: anytype,
    stderr: anytype,
) !void {
    shared.runToolWithDeveloperDir(allocator, developer_dir, null, &.{
        "xcodebuild", "-project", project_path, "-scheme", scheme, "-configuration", "Debug",
        "-derivedDataPath", derived_data, "-sdk", "macosx", "build", "CODE_SIGNING_ALLOWED=NO",
    }) catch {
        try shared.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.macOSRunnerBuildFailed(allocator, target.target_root));
        return error.CommandFailed;
    };
    try shared.emitEvent(stdout, "live.macos.build.succeeded", "scheme={s}", .{scheme});

    const app_dir = try std.fs.path.join(allocator, &.{ derived_data, "Build", "Products", "Debug", try std.fmt.allocPrint(allocator, "{s}.app", .{product_name}) });
    const app_exec = try std.fs.path.join(allocator, &.{ app_dir, "Contents", "MacOS", product_name });
    const bundled_manifest = try std.fs.path.join(allocator, &.{ app_dir, "Contents", "Resources", "KiraRunner.toml" });
    try shared.writeFile(bundled_manifest, manifest_text);
    try shared.emitEvent(stdout, "live.server.started", "host=127.0.0.1 port={d} runner=macos", .{server.port});

    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    var environ_map = try std.process.Environ.createMap(shared.inheritedProcessEnviron(), allocator);
    defer environ_map.deinit();
    if (parsed.run_for_ns) |duration_ns| {
        try environ_map.put("KIRA_LIVE_QUIT_AFTER_NS", try std.fmt.allocPrint(allocator, "{d}", .{duration_ns}));
    }
    const io = io_impl.io();
    var child = try std.process.spawn(io, .{
        .argv = &.{app_exec},
        .cwd = .{ .path = app_dir },
        .environ_map = &environ_map,
        .stdin = .ignore,
        .stdout = .inherit,
        .stderr = .inherit,
    });
    try shared.emitEvent(stdout, "live.runner.launched", "pid={any}", .{child.id});

    var connection = (try shared.acceptClientOrDiagnose(allocator, server, &child, io, target, stdout, stderr)) orelse return;
    defer connection.close();
    try driveSession(parsed, target, bundles, &connection, "macos", stdout);
}

fn runIosSimulator(
    allocator: std.mem.Allocator,
    parsed: live_args.ParsedArgs,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    developer_dir: []const u8,
    project_path: []const u8,
    scheme: []const u8,
    derived_data: []const u8,
    product_name: []const u8,
    manifest_text: []const u8,
    server: *shared.LiveServer,
    stdout: anytype,
    stderr: anytype,
) !void {
    const device_name = "iPhone 17 Pro";
    const bundle_id = "com.kira.live.dev";
    shared.runToolWithDeveloperDir(allocator, developer_dir, null, &.{
        "xcodebuild", "-project", project_path, "-scheme", scheme, "-configuration", "Debug",
        "-derivedDataPath", derived_data, "-sdk", "iphonesimulator",
        "-destination", "platform=iOS Simulator,name=iPhone 17 Pro", "build", "CODE_SIGNING_ALLOWED=NO",
    }) catch {
        try shared.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosLiveUnsupported(allocator, parsed.platform.label(), "The unified KiraApp iOS scheme did not build for the simulator."));
        return error.CommandFailed;
    };
    try shared.emitEvent(stdout, "live.ios.simulator.build.succeeded", "scheme={s}", .{scheme});

    const app_path = try std.fs.path.join(allocator, &.{ derived_data, "Build", "Products", "Debug-iphonesimulator", try std.fmt.allocPrint(allocator, "{s}.app", .{product_name}) });
    const bundled_manifest = try std.fs.path.join(allocator, &.{ app_path, "KiraRunner.toml" });
    try shared.writeFile(bundled_manifest, manifest_text);

    _ = shared.runToolCapture(allocator, &.{ "xcrun", "simctl", "boot", device_name }) catch null;
    try shared.runTool(allocator, &.{ "xcrun", "simctl", "bootstatus", device_name, "-b" });
    try shared.runTool(allocator, &.{ "xcrun", "simctl", "install", device_name, app_path });
    try shared.emitEvent(stdout, "live.ios.simulator.install.succeeded", "bundle={s}", .{bundle_id});
    try shared.emitEvent(stdout, "live.server.started", "host=127.0.0.1 port={d} runner=ios-simulator", .{server.port});
    try shared.runTool(allocator, &.{ "xcrun", "simctl", "launch", "--terminate-running-process", device_name, bundle_id });
    try shared.emitEvent(stdout, "live.ios.simulator.launch.succeeded", "bundle={s}", .{bundle_id});

    var connection = (try acceptSimulatorClient(allocator, server, target, stdout, stderr)) orelse return;
    defer connection.close();
    try driveSession(parsed, target, bundles, &connection, "ios-simulator", stdout);

    _ = shared.runToolCapture(allocator, &.{ "xcrun", "simctl", "terminate", device_name, bundle_id }) catch null;
    _ = shared.runToolCapture(allocator, &.{ "xcrun", "simctl", "spawn", device_name, "log", "show", "--last", "2m", "--style", "compact", "--predicate", try std.fmt.allocPrint(allocator, "process == \"{s}\"", .{product_name}) }) catch null;
    try shared.emitEvent(stdout, "live.ios.simulator.logs.captured", "source=simctl-log-show", .{});
}

fn driveSession(
    parsed: live_args.ParsedArgs,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    connection: *shared.LiveConnection,
    runner_label: []const u8,
    stdout: anytype,
) !void {
    try shared.emitEvent(stdout, "live.client.connected", "target={s}", .{target.target_root});
    try shared.emitEvent(stdout, "live.bundle.requested", "client={s}", .{runner_label});
    try connection.sendGraphAndBundles();
    try shared.emitEvent(stdout, "live.bundle.graph.sent", "bundles={d}", .{bundles.graph.bundles.len});
    try shared.emitEvent(stdout, "live.bundle.sent", "mode=initial", .{});
    try shared.emitEvent(stdout, "live.bundle.served", "mode=initial", .{});
    const require_frame = !parsed.headless;
    const health_ok = try connection.waitForHealthMarkers(stdout, 60 * std.time.ns_per_s, require_frame);
    if (!health_ok) return error.CommandFailed;
    try shared.emitEvent(stdout, "live.session.ready", "target={s}", .{target.target_root});

    if (parsed.run_for_ns) |duration_ns| {
        try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(@min(duration_ns, std.time.ns_per_s))), .awake);
    }
    try shared.emitEvent(stdout, "live.shutdown.started", "reason=quit-after", .{});
    try protocol.writeFrame(&connection.writer.interface, .shutdown, "quit-after");
    try connection.writer.interface.flush();
    _ = try connection.waitForShutdownAck(stdout, 2 * std.time.ns_per_s);
    try shared.emitEvent(stdout, "live.shutdown.finished", "reason=quit-after", .{});
}

fn acceptSimulatorClient(
    allocator: std.mem.Allocator,
    server: *shared.LiveServer,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !?shared.LiveConnection {
    const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
    while (shared.elapsedSince(start) < 30 * std.time.ns_per_s) {
        if (try shared.waitReadable(server.server.socket.handle, 250)) {
            try shared.emitEvent(stdout, "live.client.connecting", "target={s}", .{target.target_root});
            return try server.accept();
        }
    }
    try shared.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveClientFailedToConnect(allocator, target.target_root));
    return null;
}

fn platformSelector(allocator: std.mem.Allocator, platform: Platform) !@import("kira_native_lib_definition").TargetSelector {
    const native = @import("kira_native_lib_definition");
    const builtin = @import("builtin");
    return switch (platform) {
        .macos => native.TargetSelector.parse(allocator, switch (builtin.cpu.arch) {
            .aarch64 => "aarch64-macos-none",
            .x86_64 => "x86_64-macos-none",
            else => return error.UnsupportedTarget,
        }),
        .ios_simulator => native.TargetSelector.parse(allocator, "aarch64-ios-simulator"),
    };
}

fn schemeName(platform: Platform) []const u8 {
    return switch (platform) {
        .macos => "KiraApp-macOS",
        .ios_simulator => "KiraApp-iOS",
    };
}
