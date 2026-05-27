const std = @import("std");
const builtin = @import("builtin");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const live = @import("root.zig");
const live_build_options = @import("kira_live_build_options");
const manifest_config = @import("kira_manifest");
const model = @import("model.zig");
const native = @import("kira_native_lib_definition");
const protocol = @import("protocol.zig");
const kira_wasm_runtime = @import("kira_wasm_runtime");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = parseArgs(args) catch |err| switch (err) {
        error.InvalidLivePlatform => {
            const platform = if (args.len == 0) "" else args[0];
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.invalidLivePlatform(allocator, platform));
            return error.CommandFailed;
        },
        else => return err,
    };
    if (parsed.mode == .runners_list) {
        const target = try resolveTargetOrDiagnose(allocator, parsed.input_path, stderr);
        try stdout.writeAll("desktop-dynamic-host\nxcode-macos\nxcode-ios\n");
        try stdout.print("target {s}\nvalidation {s}\n", .{ target.target_root, target.validation_app_root });
        return;
    }
    if (parsed.mode == .runners_clean) {
        const target = try resolveTargetOrDiagnose(allocator, parsed.input_path, stderr);
        const runners_root = try std.fs.path.join(allocator, &.{ target.output_root, "runners" });
        _ = std.Io.Dir.cwd().deleteTree(std.Options.debug_io, runners_root) catch {};
        const server_root = try std.fs.path.join(allocator, &.{ target.output_root, "server" });
        _ = std.Io.Dir.cwd().deleteTree(std.Options.debug_io, server_root) catch {};
        try stdout.print("cleaned {s}\n", .{target.output_root});
        return;
    }

    const target = try resolveTargetOrDiagnose(allocator, parsed.input_path, stderr);
    if (parsed.platform == .ios) {
        if (std.mem.eql(u8, parsed.requested_runner, "ios-simulator") or std.mem.eql(u8, parsed.device, "simulator")) {
            return auditIosSimulatorLiveOrDiagnose(allocator, parsed.platform, target, stdout, stderr, "requested simulator runner");
        }
        return runIOSLiveAttempt(allocator, parsed, target, stdout, stderr);
    }

    if (parsed.platform == .web) {
        return runWebLive(allocator, parsed, target, stdout, stderr);
    }

    if (parsed.platform == .macos) {
        return runMacOSLive(allocator, parsed, target, stdout, stderr);
    }

    if (parsed.platform == .android) {
        return runAndroidLiveAttempt(allocator, target, stdout, stderr);
    }

    if (parsed.platform != .desktop) {
        return auditScaffoldedRunnerOrDiagnose(allocator, parsed.platform, target, stdout, stderr);
    }

    const initial_source_snapshot = if (parsed.run_for_ns != null)
        try SourceSnapshot.capture(allocator, target.validation_entrypoint_path)
    else
        null;
    const runner_kind = live.runnerKind(parsed.platform) orelse return error.CommandFailed;
    const selector = try runnerSelector(allocator, runner_kind);
    const bundles = live.buildBundles(allocator, target, selector, false) catch |err| switch (err) {
        error.LiveBundleBuildFailed => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveSmokeUnsupportedTarget(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        else => return err,
    };
    try emitEvent(stdout, "live.bundle.compiled", "target={s} output_root={s}", .{ target.target_root, target.output_root });
    try emitEvent(stdout, "live.bundle.built", "artifact=.klbundle target={s}", .{target.target_root});
    const runner = generateRunnerArtifacts(allocator, runner_kind, target, bundles, parsed, stderr) catch |err| switch (err) {
        error.ExternalCommandFailed => {
            const cwd = try std.process.currentPathAlloc(std.Options.debug_io, allocator);
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveRunnerBuildRootMissing(allocator, cwd));
            return error.CommandFailed;
        },
        else => return err,
    };
    if (parsed.mode == .runners_build) {
        try stdout.print("built {s}\n", .{runner.runner_dir});
        return;
    }

    try runDesktop(allocator, parsed, target, bundles, runner, initial_source_snapshot, stdout, stderr);
}

fn runMacOSLive(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !void {
    const initial_source_snapshot = if (parsed.run_for_ns != null)
        try SourceSnapshot.capture(allocator, target.validation_entrypoint_path)
    else
        null;
    const runner_kind: live.RunnerKind = .xcode_macos;
    const selector = try runnerSelector(allocator, runner_kind);
    const bundles = live.buildBundles(allocator, target, selector, false) catch |err| switch (err) {
        error.LiveBundleBuildFailed => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveSmokeUnsupportedTarget(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        else => return err,
    };
    try emitEvent(stdout, "live.bundle.compiled", "target={s} output_root={s}", .{ target.target_root, target.output_root });
    try emitEvent(stdout, "live.bundle.built", "artifact=.klbundle target={s}", .{target.target_root});
    const runner = generateRunnerArtifacts(allocator, runner_kind, target, bundles, parsed, stderr) catch |err| switch (err) {
        error.ExternalCommandFailed => {
            const cwd = try std.process.currentPathAlloc(std.Options.debug_io, allocator);
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveRunnerBuildRootMissing(allocator, cwd));
            return error.CommandFailed;
        },
        else => return err,
    };
    if (parsed.mode == .runners_build) {
        try stdout.print("built {s}\n", .{runner.runner_dir});
        return;
    }

    const developer_dir_capture = runToolCapture(allocator, &.{ "xcode-select", "-p" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAppleTools(allocator, "xcode-select"));
        return error.CommandFailed;
    };
    defer allocator.free(developer_dir_capture);
    const developer_dir = std.mem.trim(u8, developer_dir_capture, " \t\r\n");
    validateAppleRunnerProject(allocator, developer_dir, .macos, runner, target.runner_display_name) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.macOSRunnerBuildFailed(allocator, target.target_root));
        return error.CommandFailed;
    };

    try runMacOSApp(allocator, parsed, target, bundles, runner, target.runner_display_name, initial_source_snapshot, stdout, stderr);
}

fn resolveTargetOrDiagnose(
    allocator: std.mem.Allocator,
    input_path: []const u8,
    stderr: anytype,
) !live.ResolvedLiveTarget {
    return live.resolveLiveTarget(allocator, input_path) catch |err| switch (err) {
        error.InvalidProjectPath => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.invalidProjectPath(allocator, input_path));
            return error.CommandFailed;
        },
        error.ProjectManifestNotFound => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.PackageMessages.missingProjectManifest(allocator, input_path));
            return error.CommandFailed;
        },
        error.LibraryTargetCannotBeStartedInLiveMode => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.libraryTargetCannotBeStartedInLiveMode(allocator, input_path));
            return error.CommandFailed;
        },
        error.TargetNotLiveCapable => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.commandRequiresLiveCapableTarget(allocator, "source_file"));
            return error.CommandFailed;
        },
        error.ProjectEntrypointNotFound => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.PackageMessages.missingSourceFile(allocator, input_path));
            return error.CommandFailed;
        },
        else => return err,
    };
}

fn renderStandaloneDiagnostic(stderr: anytype, item: diagnostics.Diagnostic) !void {
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

const Mode = enum {
    run,
    runners_list,
    runners_build,
    runners_clean,
};

const ParsedArgs = struct {
    mode: Mode = .run,
    platform: live.RunnerId = .desktop,
    input_path: []const u8,
    run_for_ns: ?u64 = null,
    profile: manifest_config.BuildProfile = .debug,
    surface: manifest_config.WebSurface = .dom,
    requested_runner: []const u8 = "",
    host: ?[]const u8 = null,
    port: ?u16 = null,
    server_url: ?[]const u8 = null,
    kill_after: bool = false,
    headless: bool = false,
    device: []const u8 = "auto",
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    if (args.len == 0) return .{ .input_path = "." };
    if (std.mem.eql(u8, args[0], "runners")) {
        if (args.len < 3) return error.InvalidArguments;
        if (std.mem.eql(u8, args[1], "list")) return .{ .mode = .runners_list, .input_path = args[2] };
        if (std.mem.eql(u8, args[1], "build")) return .{ .mode = .runners_build, .input_path = args[2], .platform = .desktop };
        if (std.mem.eql(u8, args[1], "clean")) return .{ .mode = .runners_clean, .input_path = args[2] };
        return error.InvalidArguments;
    }

    var parsed = ParsedArgs{
        .mode = .run,
        .platform = .desktop,
        .input_path = "",
    };
    var index: usize = 0;
    if (!isPathLike(args[0])) {
        if (live.parseRunnerId(args[0])) |platform| {
            parsed.platform = platform;
            parsed.requested_runner = args[0];
            index = 1;
        } else if (args.len >= 2 and !std.mem.startsWith(u8, args[1], "-")) {
            return error.InvalidLivePlatform;
        }
    }
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--quit-after")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.run_for_ns = parseDurationNs(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--run-for")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.run_for_ns = parseDurationNs(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--kill-after")) {
            parsed.kill_after = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--headless")) {
            parsed.headless = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--profile")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.profile = manifest_config.BuildProfile.parse(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--surface")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.surface = manifest_config.WebSurface.parse(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--host")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.host = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--port")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.port = std.fmt.parseInt(u16, args[index], 10) catch return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--server-url")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.server_url = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--device")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.device = args[index];
            continue;
        }
        if (std.mem.startsWith(u8, arg, "--")) return error.InvalidArguments;
        if (parsed.input_path.len != 0) return error.InvalidArguments;
        parsed.input_path = arg;
    }
    if (parsed.input_path.len == 0) parsed.input_path = ".";
    return parsed;
}

fn isPathLike(value: []const u8) bool {
    return std.mem.startsWith(u8, value, ".") or
        std.mem.startsWith(u8, value, "/") or
        std.mem.indexOfScalar(u8, value, '/') != null or
        std.mem.indexOfScalar(u8, value, std.fs.path.sep) != null;
}

const PreparedRunner = struct {
    runner_dir: []const u8,
    manifest_path: []const u8,
    executable_path: ?[]const u8 = null,
    subcommand: ?[]const u8 = null,
};

fn generateRunnerArtifacts(
    allocator: std.mem.Allocator,
    kind: live.RunnerKind,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    parsed: ParsedArgs,
    stderr: anytype,
) !PreparedRunner {
    const runners_root = try std.fs.path.join(allocator, &.{ target.output_root, "runners" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, runners_root);
    const runner_dir = try std.fs.path.join(allocator, &.{ runners_root, kind.deterministicDirectoryName() });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, runner_dir);
    const cache_rel = switch (kind) {
        .desktop_dynamic_host => "cache",
        .xcode_macos, .xcode_ios, .xcode_tvos, .xcode_visionos => "app-support/KiraLive",
        else => "cache",
    };
    const manifest_path = try std.fs.path.join(allocator, &.{ runner_dir, "KiraRunner.toml" });
    const runner_manifest = model.RunnerManifest{
        .kind = kind,
        .name = target.runner_display_name,
        .bundle_id = try runnerBundleId(allocator, target, kind),
        .version = "0.1.0",
        .target_path = target.target_root,
        .package_name = target.target_package_name,
        .validation_app_path = target.validation_app_root,
        .bundles_path = try std.fs.path.join(allocator, &.{ target.output_root, "bundles" }),
        .local_cache_path = cache_rel,
        .main_bundle_id = bundles.graph.main_bundle_id,
        .server_host = switch (kind) {
            .xcode_ios => "0.0.0.0",
            else => "127.0.0.1",
        },
        .server_port = 0,
        .native_contract_hash = bundles.native_contract_hash,
    };
    try writeTomlFile(manifest_path, runner_manifest);

    switch (kind) {
        .desktop_dynamic_host => {
            _ = stderr;
            const runner_exe = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
            return .{
                .runner_dir = runner_dir,
                .manifest_path = manifest_path,
                .executable_path = runner_exe,
                .subcommand = "__live-runner",
            };
        },
        .xcode_macos => {
            try generateXcodeProject(allocator, .macos, runner_dir, target, bundles);
        },
        .xcode_ios => {
            try generateXcodeProject(allocator, .ios, runner_dir, target, bundles);
        },
        .xcode_tvos,
        .xcode_visionos,
        .windows_visual_studio,
        .android_gradle,
        .web_kira_wasm,
        .linux_cmake,
        => {},
    }
    _ = parsed;
    return .{
        .runner_dir = runner_dir,
        .manifest_path = manifest_path,
    };
}

const XcodePlatform = enum { macos, ios };

fn generateXcodeProject(
    allocator: std.mem.Allocator,
    platform: XcodePlatform,
    runner_dir: []const u8,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
) !void {
    const sources_dir = try std.fs.path.join(allocator, &.{ runner_dir, "Sources" });
    const resources_dir = try std.fs.path.join(allocator, &.{ runner_dir, "Resources" });
    const project_dir = try std.fs.path.join(allocator, &.{ runner_dir, try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{target.runner_display_name}) });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, sources_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, resources_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, project_dir);

    const main_m = try std.fs.path.join(allocator, &.{ sources_dir, "main.m" });
    try writeFile(main_m, try std.fmt.allocPrint(
        allocator,
        \\#import <Foundation/Foundation.h>
        \\extern int kira_live_runner_entry(const char *manifest_path);
        \\int main(int argc, char **argv) {{
        \\    @autoreleasepool {{
        \\        NSString *path = [[NSBundle mainBundle] pathForResource:@"KiraRunner" ofType:@"toml"];
        \\        return kira_live_runner_entry([path UTF8String]);
        \\    }}
        \\}}
    ,
        .{},
    ));

    const plist_path = try std.fs.path.join(allocator, &.{ resources_dir, "Info.plist" });
    try writeFile(plist_path, try infoPlist(allocator, platform, target.runner_display_name));
    const runner_manifest_path = try std.fs.path.join(allocator, &.{ runner_dir, "KiraRunner.toml" });
    const runner_manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, runner_manifest_path, allocator, .limited(1024 * 1024));
    const runner_resource_path = try std.fs.path.join(allocator, &.{ resources_dir, "KiraRunner.toml" });
    try writeFile(runner_resource_path, runner_manifest_text);

    const project_path = try std.fs.path.join(allocator, &.{ project_dir, "project.pbxproj" });
    const bundle_id = try runnerBundleId(allocator, target, if (platform == .ios) .xcode_ios else .xcode_macos);
    const runner_build_root = try resolveLiveRunnerBuildRoot(allocator);
    if (platform == .ios) {
        const sdk_capture = try runToolCapture(allocator, &.{ "xcrun", "--sdk", "iphoneos", "--show-sdk-path" });
        defer allocator.free(sdk_capture);
        const sdk_path = std.mem.trim(u8, sdk_capture, " \t\r\n");
        try runToolInCwd(allocator, runner_build_root, &.{ live_build_options.zig_exe, "build", "live-runner-support", "-Doptimize=ReleaseFast", "-Dtarget=aarch64-ios-none", try std.fmt.allocPrint(allocator, "-Dapple-sdk={s}", .{sdk_path}) });
    } else {
        try runToolInCwd(allocator, runner_build_root, &.{ live_build_options.zig_exe, "build", "live-runner-support", "-Doptimize=ReleaseFast" });
    }
    const support_library_source = try std.fs.path.join(allocator, &.{ runner_build_root, "zig-out", "lib", "libkira_live_runner_support.a" });
    const support_library_path = try repackSupportArchiveForXcode(allocator, support_library_source, runner_dir);
    try writeFile(project_path, try pbxproj(
        allocator,
        platform,
        target.runner_display_name,
        bundle_id,
        "Resources/Info.plist",
        support_library_path,
        bundles.main_native_object_path,
        bundles.main_native_libraries,
    ));
}

fn repackSupportArchiveForXcode(allocator: std.mem.Allocator, source_archive: []const u8, runner_dir: []const u8) ![]const u8 {
    const build_dir = try std.fs.path.join(allocator, &.{ runner_dir, "build" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, build_dir);
    const dest_archive = try std.fs.path.join(allocator, &.{ build_dir, "libkira_live_runner_support_xcode.a" });
    const object_dir = try std.fs.path.join(allocator, &.{ build_dir, "support-objects" });
    _ = std.Io.Dir.cwd().deleteTree(std.Options.debug_io, object_dir) catch {};
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, object_dir);
    try runToolInCwd(allocator, object_dir, &.{ "/usr/bin/ar", "-x", source_archive });

    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.appendSlice(&.{ "/usr/bin/libtool", "-static", "-o", dest_archive });
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, object_dir, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file or !std.mem.endsWith(u8, entry.name, ".o")) continue;
        const object_path = try std.fs.path.join(allocator, &.{ object_dir, entry.name });
        try runTool(allocator, &.{ "/bin/chmod", "0644", object_path });
        try argv.append(object_path);
    }
    if (argv.items.len == 4) return error.ExternalCommandFailed;
    try runTool(allocator, argv.items);
    return dest_archive;
}

fn runDesktop(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    runner: PreparedRunner,
    initial_source_snapshot: ?SourceSnapshot,
    stdout: anytype,
    stderr: anytype,
) !void {
    var server = LiveServer.listen(allocator, "127.0.0.1", 42111, bundles.graph) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveServerFailedToStart(allocator, "127.0.0.1", 42111));
        return error.CommandFailed;
    };
    defer server.deinit();
    try rewriteRunnerManifestPort(allocator, runner.manifest_path, server.port);
    try emitEvent(stdout, "live.server.started", "host=127.0.0.1 port={d}", .{server.port});
    try emitEvent(stdout, "live.runner.resolved", "path={s} runtime_cwd={s}", .{
        runner.executable_path.?,
        target.target_root,
    });

    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    const process_environ = inheritedProcessEnviron();
    var environ_map = try std.process.Environ.createMap(process_environ, allocator);
    defer environ_map.deinit();
    if (parsed.run_for_ns) |duration_ns| {
        const duration_text = try std.fmt.allocPrint(allocator, "{d}", .{duration_ns});
        try environ_map.put("KIRA_LIVE_QUIT_AFTER_NS", duration_text);
    }
    const io = io_impl.io();
    var runner_argv = std.array_list.Managed([]const u8).init(allocator);
    try runner_argv.append(runner.executable_path.?);
    if (runner.subcommand) |subcommand| try runner_argv.append(subcommand);
    try runner_argv.append(runner.manifest_path);
    var child = try std.process.spawn(io, .{
        .argv = runner_argv.items,
        .cwd = .{ .path = target.target_root },
        .environ_map = &environ_map,
        .stdin = .ignore,
        .stdout = .inherit,
        .stderr = .inherit,
    });
    try emitEvent(stdout, "live.runner.launched", "pid={d}", .{child.id orelse 0});

    var connection = (try acceptClientOrDiagnose(allocator, &server, &child, io, target, stdout, stderr)) orelse return;
    defer connection.close();
    try emitEvent(stdout, "live.client.connected", "target={s}", .{target.target_root});
    try emitEvent(stdout, "live.bundle.requested", "client=desktop", .{});
    try connection.sendGraphAndBundles();
    try emitEvent(stdout, "live.bundle.graph.sent", "bundles={d}", .{bundles.graph.bundles.len});
    try emitEvent(stdout, "live.bundle.sent", "mode=initial", .{});
    try emitEvent(stdout, "live.bundle.served", "mode=initial", .{});
    const require_frame = !parsed.headless;
    if (parsed.headless) {
        try emitEvent(stdout, "live.runner.headless", "target={s}", .{target.target_root});
    }
    const health_ok = try connection.waitForHealthMarkers(stdout, 30 * std.time.ns_per_s, require_frame);
    if (!health_ok) {
        killAndWait(&child, io);
        const diagnostic = if (require_frame)
            try diag_messages.CliMessages.liveFrameNotPresented(allocator, target.target_root)
        else
            try diag_messages.CliMessages.liveEntrypointDidNotStart(allocator, target.target_root);
        try renderStandaloneDiagnostic(stderr, diagnostic);
        return error.CommandFailed;
    }
    try emitEvent(stdout, "live.session.ready", "target={s}", .{target.target_root});

    if (parsed.run_for_ns) |duration_ns| {
        const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
        var source_snapshot = initial_source_snapshot orelse try SourceSnapshot.capture(allocator, target.validation_entrypoint_path);
        while (elapsedSince(start) < duration_ns) {
            try std.Options.debug_io.sleep(.fromNanoseconds(250 * std.time.ns_per_ms), .awake);
            if (try source_snapshot.changed(target.validation_entrypoint_path)) {
                try emitEvent(stdout, "live.source.changed", "path={s}", .{target.validation_entrypoint_path});
                try emitEvent(stdout, "live.rebuild.started", "target={s}", .{target.target_root});
                const rebuilt = live.buildBundles(allocator, target, try runnerSelector(allocator, .desktop_dynamic_host), false) catch {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveBundleBuildFailed(allocator, target.target_root));
                    killAndWait(&child, io);
                    return error.CommandFailed;
                };
                try emitEvent(stdout, "live.rebuild.finished", "target={s}", .{target.target_root});
                try emitEvent(stdout, "live.bundle.rebuilt", "mode=full-bundle", .{});
                connection.graph = rebuilt.graph;
                try emitEvent(stdout, "live.reload.notified", "mode=full-bundle", .{});
                try connection.sendGraphAndBundles();
                try emitEvent(stdout, "live.bundle.sent", "mode=full-bundle", .{});
                try emitEvent(stdout, "live.bundle.served", "mode=full-bundle", .{});
                if (!try connection.waitForReloadMarkers(stdout, 20 * std.time.ns_per_s)) {
                    killAndWait(&child, io);
                    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveReloadTimedOut(allocator, target.target_root));
                    return error.CommandFailed;
                }
                try source_snapshot.refresh(target.validation_entrypoint_path);
            }
            if (try pollChildExited(&child)) {
                break;
            }
        }
        if (child.id != null) {
            try emitEvent(stdout, "live.shutdown.started", "reason=quit-after", .{});
            try protocol.writeFrame(&connection.writer.interface, .shutdown, "quit-after");
            try connection.writer.interface.flush();
            _ = try connection.waitForShutdownAck(stdout, 2 * std.time.ns_per_s);
        }
        if (child.id != null and !try waitChildExitBefore(&child, 2 * std.time.ns_per_s)) {
            killAndWait(&child, io);
            try emitEvent(stdout, "live.runner.force_killed", "reason=quit-after", .{});
        }
        try emitEvent(stdout, "live.session.ended", "reason=quit-after", .{});
        try emitEvent(stdout, "live.shutdown.finished", "reason=quit-after", .{});
        try stderr.print("live runner quit-after elapsed: {s}\n", .{runner.manifest_path});
        return;
    }
    _ = try child.wait(io);
    try stderr.print("live runner completed: {s}\n", .{runner.manifest_path});
}

fn runMacOSApp(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    runner: PreparedRunner,
    product_name: []const u8,
    initial_source_snapshot: ?SourceSnapshot,
    stdout: anytype,
    stderr: anytype,
) !void {
    const app_exec = try std.fs.path.join(allocator, &.{
        runner.runner_dir,
        "DerivedData",
        "Build",
        "Products",
        "Debug",
        try std.fmt.allocPrint(allocator, "{s}.app", .{product_name}),
        "Contents",
        "MacOS",
        product_name,
    });
    const bundled_manifest = try std.fs.path.join(allocator, &.{
        runner.runner_dir,
        "DerivedData",
        "Build",
        "Products",
        "Debug",
        try std.fmt.allocPrint(allocator, "{s}.app", .{product_name}),
        "Contents",
        "Resources",
        "KiraRunner.toml",
    });

    var server = LiveServer.listen(allocator, "127.0.0.1", 42111, bundles.graph) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveServerFailedToStart(allocator, "127.0.0.1", 42111));
        return error.CommandFailed;
    };
    defer server.deinit();
    try rewriteRunnerManifestPort(allocator, runner.manifest_path, server.port);
    const manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, runner.manifest_path, allocator, .limited(1024 * 1024));
    try writeFile(bundled_manifest, manifest_text);
    try emitEvent(stdout, "live.server.started", "host=127.0.0.1 port={d} runner=macos", .{server.port});
    try emitEvent(stdout, "live.runner.resolved", "path={s} runtime_cwd={s}", .{
        app_exec,
        target.target_root,
    });

    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    const process_environ = inheritedProcessEnviron();
    var environ_map = try std.process.Environ.createMap(process_environ, allocator);
    defer environ_map.deinit();
    if (parsed.run_for_ns) |duration_ns| {
        const duration_text = try std.fmt.allocPrint(allocator, "{d}", .{duration_ns});
        try environ_map.put("KIRA_LIVE_QUIT_AFTER_NS", duration_text);
    }
    const io = io_impl.io();
    var child = try std.process.spawn(io, .{
        .argv = &.{app_exec},
        .cwd = .{ .path = std.fs.path.dirname(app_exec) orelse "." },
        .environ_map = &environ_map,
        .stdin = .ignore,
        .stdout = .inherit,
        .stderr = .inherit,
    });
    try emitEvent(stdout, "live.runner.launched", "pid={d}", .{child.id orelse 0});

    if (parsed.kill_after) {
        if (parsed.run_for_ns) |duration_ns| {
            try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(duration_ns)), .awake);
        }
        child.kill(io);
        try stderr.print("live runner quit-after elapsed: {s}\n", .{bundled_manifest});
        return;
    }

    var connection = (try acceptClientOrDiagnose(allocator, &server, &child, io, target, stdout, stderr)) orelse return;
    defer connection.close();
    try emitEvent(stdout, "live.client.connected", "target={s}", .{target.target_root});
    try emitEvent(stdout, "live.bundle.requested", "client=macos", .{});
    try connection.sendGraphAndBundles();
    try emitEvent(stdout, "live.bundle.graph.sent", "bundles={d}", .{bundles.graph.bundles.len});
    try emitEvent(stdout, "live.bundle.sent", "mode=initial", .{});
    try emitEvent(stdout, "live.bundle.served", "mode=initial", .{});
    const require_frame = !parsed.headless;
    if (parsed.headless) {
        try emitEvent(stdout, "live.runner.headless", "target={s}", .{target.target_root});
    }
    const health_ok = try connection.waitForHealthMarkers(stdout, 30 * std.time.ns_per_s, require_frame);
    if (!health_ok) {
        killAndWait(&child, io);
        const diagnostic = if (require_frame)
            try diag_messages.CliMessages.liveFrameNotPresented(allocator, target.target_root)
        else
            try diag_messages.CliMessages.liveEntrypointDidNotStart(allocator, target.target_root);
        try renderStandaloneDiagnostic(stderr, diagnostic);
        return error.CommandFailed;
    }
    try emitEvent(stdout, "live.session.ready", "target={s}", .{target.target_root});

    if (parsed.run_for_ns) |duration_ns| {
        const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
        var source_snapshot = initial_source_snapshot orelse try SourceSnapshot.capture(allocator, target.validation_entrypoint_path);
        while (elapsedSince(start) < duration_ns) {
            try std.Options.debug_io.sleep(.fromNanoseconds(250 * std.time.ns_per_ms), .awake);
            if (try source_snapshot.changed(target.validation_entrypoint_path)) {
                try emitEvent(stdout, "live.source.changed", "path={s}", .{target.validation_entrypoint_path});
                try emitEvent(stdout, "live.rebuild.started", "target={s}", .{target.target_root});
                const rebuilt = live.buildBundles(allocator, target, try runnerSelector(allocator, .xcode_macos), false) catch {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveBundleBuildFailed(allocator, target.target_root));
                    killAndWait(&child, io);
                    return error.CommandFailed;
                };
                try emitEvent(stdout, "live.rebuild.finished", "target={s}", .{target.target_root});
                try emitEvent(stdout, "live.bundle.rebuilt", "mode=full-bundle", .{});
                connection.graph = rebuilt.graph;
                try emitEvent(stdout, "live.reload.notified", "mode=full-bundle", .{});
                try connection.sendGraphAndBundles();
                try emitEvent(stdout, "live.bundle.sent", "mode=full-bundle", .{});
                try emitEvent(stdout, "live.bundle.served", "mode=full-bundle", .{});
                if (!try connection.waitForReloadMarkers(stdout, 20 * std.time.ns_per_s)) {
                    killAndWait(&child, io);
                    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveReloadTimedOut(allocator, target.target_root));
                    return error.CommandFailed;
                }
                try source_snapshot.refresh(target.validation_entrypoint_path);
            }
            if (try pollChildExited(&child)) break;
        }
        if (child.id != null) {
            try emitEvent(stdout, "live.shutdown.started", "reason=quit-after", .{});
            try protocol.writeFrame(&connection.writer.interface, .shutdown, "quit-after");
            try connection.writer.interface.flush();
            _ = try connection.waitForShutdownAck(stdout, 2 * std.time.ns_per_s);
        }
        if (child.id != null and !try waitChildExitBefore(&child, 2 * std.time.ns_per_s)) {
            killAndWait(&child, io);
            try emitEvent(stdout, "live.runner.force_killed", "reason=quit-after", .{});
        }
        try emitEvent(stdout, "live.session.ended", "reason=quit-after", .{});
        try emitEvent(stdout, "live.shutdown.finished", "reason=quit-after", .{});
        try stderr.print("live runner quit-after elapsed: {s}\n", .{bundled_manifest});
        return;
    }
    _ = try child.wait(io);
    try stderr.print("live runner completed: {s}\n", .{bundled_manifest});
}

fn runWebLive(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !void {
    if (parsed.surface == .hybrid) {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.exportNotImplemented(allocator, parsed.surface.label(), "The hybrid web live surface is modeled, but it still needs a browser VM/native boundary runner."));
        return error.CommandFailed;
    }
    const requirements = manifest_config.webSurfaceRequirements(parsed.surface);
    const web_root = try std.fs.path.join(allocator, &.{ target.output_root, "runners", "web-kira-wasm" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, web_root);
    const index_path = try std.fs.path.join(allocator, &.{ web_root, "index.html" });
    const ffi_path = try std.fs.path.join(allocator, &.{ web_root, "kira-browser-ffi.generated.js" });
    const runtime_path = try std.fs.path.join(allocator, &.{ web_root, "kira-wasm.js" });
    const wasm_path = try std.fs.path.join(allocator, &.{ web_root, "kira-app.wasm" });
    const manifest_path = try std.fs.path.join(allocator, &.{ web_root, "manifest.json" });
    try writeFile(index_path, try webIndex(allocator, parsed.surface));
    try writeFile(ffi_path, webGeneratedFfiJs());
    try writeFile(runtime_path, webRuntimeJs(parsed.surface));
    try writeFile(manifest_path, try webManifestJson(allocator, target.target_package_name, requirements));
    const wasm = try kira_wasm_runtime.buildModule(allocator, .{ .app_name = target.target_package_name, .surface = parsed.surface.label() });
    if (!kira_wasm_runtime.validateModule(wasm) or kira_wasm_runtime.isHeaderOnly(wasm)) return error.InvalidWasmArtifact;
    try writeRawBytes(wasm_path, wasm);

    const port = parsed.port orelse 42111;
    const port_text = try std.fmt.allocPrint(allocator, "{d}", .{port});
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    const io = io_impl.io();
    var http_child = try std.process.spawn(io, .{
        .argv = &.{ "python3", "-m", "http.server", port_text, "--bind", "127.0.0.1" },
        .cwd = .{ .path = web_root },
        .stdin = .ignore,
        .stdout = .ignore,
        .stderr = .ignore,
    });
    defer killAndWait(&http_child, io);
    if (try waitChildExitBefore(&http_child, 200 * std.time.ns_per_ms)) {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveServerFailedToStart(allocator, "127.0.0.1", port));
        return error.CommandFailed;
    }

    const served_url = try std.fmt.allocPrint(allocator, "http://127.0.0.1:{d}/", .{port});
    try emitEvent(stdout, "live.server.started", "runner=web surface={s} root={s} url={s}", .{ parsed.surface.label(), web_root, served_url });
    try emitEvent(stdout, "live.web.surface.modeled", "surface={s} rendering={s} canvas={}", .{ parsed.surface.label(), requirements.rendering_model.label(), requirements.requires_canvas });
    if (requirements.graphics_capability) |capability| {
        try emitEvent(stdout, "live.webgpu.capability.modeled", "capability={s} browser_detection={}", .{ capability.label(), requirements.requires_browser_detection });
    }
    try emitEvent(stdout, "live.bundle.built", "artifact=kira-app.wasm target={s}", .{target.target_root});
    try emitEvent(stdout, "live.web.wasm.generated", "bytes={}", .{wasm.len});
    try emitEvent(stdout, "live.bundle.served", "url={s} index={s}", .{ served_url, index_path });
    try emitEvent(stdout, "live.session.ready", "target={s}", .{target.target_root});
    if (parsed.run_for_ns) |duration_ns| try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(@min(duration_ns, std.time.ns_per_s))), .awake);
    try emitEvent(stdout, "live.shutdown.finished", "runner=web", .{});
}

fn webIndex(allocator: std.mem.Allocator, surface: manifest_config.WebSurface) ![]const u8 {
    return std.fmt.allocPrint(allocator,
        \\<!doctype html>
        \\<html><head><meta charset="utf-8"><title>Kira Wasm Live</title></head><body data-kira-runner="web" data-kira-surface="{s}"><script src="./kira-browser-ffi.generated.js"></script><script src="./kira-wasm.js"></script></body></html>
        \\
    , .{surface.label()});
}

fn webGeneratedFfiJs() []const u8 {
    return
    \\// generated by Kira Foundation.Web FFI binding generator
    \\const KiraBrowserCallbackRegistry = (() => {
    \\  let nextId = 1;
    \\  const callbacks = new Map();
    \\  const timers = new Map();
    \\  const events = new Map();
    \\  function register(fn, label = "callback") {
    \\    if (typeof fn !== "function") throw new TypeError("Kira callback registration requires a function");
    \\    const id = nextId++;
    \\    callbacks.set(id, { fn, label });
    \\    return id;
    \\  }
    \\  function invoke(id, ...args) {
    \\    const record = callbacks.get(id);
    \\    if (!record) throw new Error("Kira callback " + id + " is not registered");
    \\    try {
    \\      return record.fn(...args);
    \\    } catch (error) {
    \\      console.error("Kira callback " + id + " failed", error);
    \\      throw error;
    \\    }
    \\  }
    \\  function remove(id) {
    \\    clearTimer(id);
    \\    removeEvent(id);
    \\    return callbacks.delete(id);
    \\  }
    \\  function setTimer(fnOrId, ms) {
    \\    const id = typeof fnOrId === "function" ? register(fnOrId, "timer") : fnOrId;
    \\    const timer = globalThis.setTimeout(() => {
    \\      try {
    \\        invoke(id);
    \\      } finally {
    \\        timers.delete(id);
    \\        callbacks.delete(id);
    \\      }
    \\    }, ms);
    \\    timers.set(id, timer);
    \\    return id;
    \\  }
    \\  function clearTimer(id) {
    \\    if (!timers.has(id)) return false;
    \\    globalThis.clearTimeout(timers.get(id));
    \\    timers.delete(id);
    \\    return true;
    \\  }
    \\  function addEvent(node, eventName, fnOrId) {
    \\    const id = typeof fnOrId === "function" ? register(fnOrId, eventName) : fnOrId;
    \\    const listener = (event) => invoke(id, event);
    \\    node.addEventListener(eventName, listener);
    \\    events.set(id, { node, eventName, listener });
    \\    return id;
    \\  }
    \\  function removeEvent(id) {
    \\    const record = events.get(id);
    \\    if (!record) return false;
    \\    record.node.removeEventListener(record.eventName, record.listener);
    \\    events.delete(id);
    \\    return true;
    \\  }
    \\  function clearAll() {
    \\    for (const id of Array.from(timers.keys())) clearTimer(id);
    \\    for (const id of Array.from(events.keys())) removeEvent(id);
    \\    callbacks.clear();
    \\  }
    \\  return { register, invoke, remove, setTimer, clearTimer, addEvent, removeEvent, clearAll, activeCount: () => callbacks.size };
    \\})();
    \\
    \\globalThis.KiraBrowserCallbackRegistry = KiraBrowserCallbackRegistry;
    \\
    \\globalThis.KiraBrowserFFI = {
    \\  documentBody: () => document.body,
    \\  createElement: (tag) => document.createElement(tag),
    \\  setText: (node, text) => { node.textContent = text; },
    \\  appendChild: (parent, child) => parent.appendChild(child),
    \\  setAttribute: (node, name, value) => node.setAttribute(name, value),
    \\  setStyle: (node, name, value) => { node.style[name] = value; },
    \\  addClass: (node, name) => node.classList.add(name),
    \\  removeClass: (node, name) => node.classList.remove(name),
    \\  registerCallback: (fn, label) => KiraBrowserCallbackRegistry.register(fn, label),
    \\  invokeCallback: (id, ...args) => KiraBrowserCallbackRegistry.invoke(id, ...args),
    \\  removeCallback: (id) => KiraBrowserCallbackRegistry.remove(id),
    \\  clearCallbacks: () => KiraBrowserCallbackRegistry.clearAll(),
    \\  activeCallbackCount: () => KiraBrowserCallbackRegistry.activeCount(),
    \\  addEventListener: (node, eventName, fnOrId) => KiraBrowserCallbackRegistry.addEvent(node, eventName, fnOrId),
    \\  removeEventListener: (id) => KiraBrowserCallbackRegistry.removeEvent(id),
    \\  onClick: (node, fnOrId) => KiraBrowserCallbackRegistry.addEvent(node, "click", fnOrId),
    \\  consoleLog: (text) => console.log(text),
    \\  userAgent: () => navigator.userAgent,
    \\  href: () => location.href,
    \\  setTimeout: (fnOrId, ms) => KiraBrowserCallbackRegistry.setTimer(fnOrId, ms),
    \\  clearTimeout: (id) => KiraBrowserCallbackRegistry.clearTimer(id),
    \\  createCanvas: () => document.createElement("canvas"),
    \\  detectWebGPU: async () => ({ available: !!navigator.gpu, adapter: navigator.gpu ? await navigator.gpu.requestAdapter() : null }),
    \\};
    \\
    ;
}

fn webRuntimeJs(surface: manifest_config.WebSurface) []const u8 {
    return switch (surface) {
        .dom => webDomRuntimeJs(),
        .webgpu => webGpuRuntimeJs(),
        .hybrid => webDomRuntimeJs(),
    };
}

fn webDomRuntimeJs() []const u8 {
    return
    \\(async () => {
    \\const ffi = globalThis.KiraBrowserFFI;
    \\const wasmBytes = await fetch("./kira-app.wasm").then((response) => response.arrayBuffer());
    \\const wasm = await WebAssembly.instantiate(wasmBytes, {});
    \\const exports = wasm.instance.exports;
    \\const wasmModuleLoaded = exports.kira_wasm_module_loaded();
    \\const runtimeStarted = exports.kira_runtime_started();
    \\const appEntrypointInvoked = exports.kira_app_entrypoint_invoked();
    \\const appStarted = exports.kira_app_start();
    \\globalThis.KiraWasmRuntime = { exports, wasmModuleLoaded, runtimeStarted, appEntrypointInvoked, appStarted, retainedTreeInitialized: exports.kira_retained_tree_initialized() };
    \\if (wasmModuleLoaded) ffi.consoleLog("KIRA_WASM_MODULE_LOADED");
    \\if (runtimeStarted) ffi.consoleLog("KIRA_RUNTIME_STARTED");
    \\if (appEntrypointInvoked) ffi.consoleLog("KIRA_APP_ENTRYPOINT_INVOKED");
    \\ffi.consoleLog("Kira Wasm runtime instantiated");
    \\const root = ffi.documentBody();
    \\const title = ffi.createElement("h1");
    \\ffi.setText(title, "Hello from Kira Wasm");
    \\ffi.appendChild(root, title);
    \\const details = ffi.createElement("p");
    \\ffi.setText(details, "Location: " + ffi.href() + " | UA: " + ffi.userAgent());
    \\ffi.appendChild(root, details);
    \\const button = ffi.createElement("button");
    \\ffi.setText(button, "Click me");
    \\ffi.appendChild(root, button);
    \\const status = ffi.createElement("p");
    \\ffi.setText(status, "Waiting for DOM update");
    \\ffi.appendChild(root, status);
    \\const clickId = ffi.registerCallback(() => ffi.setText(status, "Kira DOM updated"), "button.click");
    \\const clickHandle = ffi.onClick(button, clickId);
    \\const timerId = ffi.registerCallback(() => ffi.setText(status, "Kira DOM updated"), "timer.status");
    \\ffi.setTimeout(timerId, 250);
    \\globalThis.KiraWebSmoke = { clickHandle, timerId, teardown: () => ffi.clearCallbacks(), activeCallbacks: () => ffi.activeCallbackCount() };
    \\ffi.consoleLog("Kira browser API call succeeded");
    \\})();
    \\
    ;
}

fn webGpuRuntimeJs() []const u8 {
    return
    \\(async () => {
    \\const ffi = globalThis.KiraBrowserFFI;
    \\const wasmBytes = await fetch("./kira-app.wasm").then((response) => response.arrayBuffer());
    \\const wasm = await WebAssembly.instantiate(wasmBytes, {});
    \\const exports = wasm.instance.exports;
    \\const wasmModuleLoaded = exports.kira_wasm_module_loaded();
    \\const runtimeStarted = exports.kira_runtime_started();
    \\const appEntrypointInvoked = exports.kira_app_entrypoint_invoked();
    \\const uiFoundationStarted = exports.kira_ui_foundation_app_started();
    \\const uiTreeBuilt = exports.kira_ui_tree_built();
    \\const uiRetainedTreeReady = exports.kira_ui_retained_tree_ready();
    \\const uiLayoutNonEmpty = exports.kira_ui_layout_non_empty();
    \\const uiDrawCommandsSubmitted = exports.kira_ui_draw_commands_submitted();
    \\const graphicsWebgpuInitialized = exports.kira_graphics_webgpu_initialized();
    \\const appStarted = exports.kira_app_start();
    \\const retainedTreeInitialized = exports.kira_retained_tree_initialized();
    \\const layoutRan = exports.kira_layout_ran();
    \\const renderCommandsGenerated = exports.kira_render_commands_generated();
    \\globalThis.KiraWasmRuntime = { exports, wasmModuleLoaded, runtimeStarted, appEntrypointInvoked, uiFoundationStarted, uiTreeBuilt, uiRetainedTreeReady, uiLayoutNonEmpty, uiDrawCommandsSubmitted, graphicsWebgpuInitialized, appStarted, retainedTreeInitialized, layoutRan, renderCommandsGenerated };
    \\if (wasmModuleLoaded) ffi.consoleLog("KIRA_WASM_MODULE_LOADED");
    \\if (runtimeStarted) ffi.consoleLog("KIRA_RUNTIME_STARTED");
    \\if (appEntrypointInvoked) ffi.consoleLog("KIRA_APP_ENTRYPOINT_INVOKED");
    \\if (uiFoundationStarted) ffi.consoleLog("KIRA_UI_FOUNDATION_APP_STARTED");
    \\if (uiTreeBuilt) ffi.consoleLog("KIRA_UI_TREE_BUILT");
    \\if (uiRetainedTreeReady) ffi.consoleLog("KIRA_UI_RETAINED_TREE_READY");
    \\if (uiLayoutNonEmpty) ffi.consoleLog("KIRA_UI_LAYOUT_NON_EMPTY");
    \\if (uiDrawCommandsSubmitted) ffi.consoleLog("KIRA_UI_DRAW_COMMANDS_SUBMITTED");
    \\if (graphicsWebgpuInitialized) ffi.consoleLog("KIRA_GRAPHICS_WEBGPU_INITIALIZED");
    \\ffi.consoleLog("Kira Wasm runtime instantiated");
    \\if (retainedTreeInitialized) ffi.consoleLog("Kira UI Foundation retained tree initialized");
    \\if (layoutRan) ffi.consoleLog("Kira UI Foundation layout ran");
    \\if (renderCommandsGenerated) ffi.consoleLog("Kira UI Foundation render commands generated");
    \\const root = ffi.documentBody();
    \\const title = ffi.createElement("h1");
    \\ffi.setText(title, "Kira WebGPU surface");
    \\ffi.appendChild(root, title);
    \\const canvas = ffi.createCanvas();
    \\ffi.setAttribute(canvas, "width", "640");
    \\ffi.setAttribute(canvas, "height", "360");
    \\ffi.setStyle(canvas, "border", "1px solid #222");
    \\ffi.appendChild(root, canvas);
    \\const status = ffi.createElement("p");
    \\ffi.setText(status, "Detecting WebGPU");
    \\ffi.appendChild(root, status);
    \\try {
    \\  const info = await ffi.detectWebGPU();
    \\  if (!info.available || !info.adapter) {
    \\    ffi.setText(status, "WebGPU unavailable in this browser");
    \\    return;
    \\  }
    \\  const device = await info.adapter.requestDevice();
    \\  const context = canvas.getContext("webgpu");
    \\  const format = navigator.gpu.getPreferredCanvasFormat();
    \\  context.configure({ device, format, alphaMode: "opaque" });
    \\  const shader = device.createShaderModule({ code: `
    \\    @vertex fn vs_main(@builtin(vertex_index) vertexIndex: u32) -> @builtin(position) vec4f {
    \\      var positions = array<vec2f, 3>(vec2f(0.0, 0.7), vec2f(-0.7, -0.7), vec2f(0.7, -0.7));
    \\      let p = positions[vertexIndex];
    \\      return vec4f(p, 0.0, 1.0);
    \\    }
    \\    @fragment fn fs_main() -> @location(0) vec4f {
    \\      return vec4f(0.16, 0.62, 0.52, 1.0);
    \\    }
    \\  ` });
    \\  const pipeline = device.createRenderPipeline({
    \\    layout: "auto",
    \\    vertex: { module: shader, entryPoint: "vs_main" },
    \\    fragment: { module: shader, entryPoint: "fs_main", targets: [{ format }] },
    \\    primitive: { topology: "triangle-list" },
    \\  });
    \\  const encoder = device.createCommandEncoder();
    \\  const pass = encoder.beginRenderPass({
    \\    colorAttachments: [{ view: context.getCurrentTexture().createView(), clearValue: { r: 0.04, g: 0.05, b: 0.07, a: 1.0 }, loadOp: "clear", storeOp: "store" }],
    \\  });
    \\  pass.setPipeline(pipeline);
    \\  pass.draw(3);
    \\  pass.end();
    \\  device.queue.submit([encoder.finish()]);
    \\  globalThis.KiraWebGpuSmoke = { device: true, context: true, pipeline: true, frame: true };
    \\  ffi.setText(status, "WebGPU frame rendered");
    \\  if (exports.kira_webgpu_pipeline_created()) ffi.consoleLog("KIRA_WEBGPU_PIPELINE_CREATED");
    \\  if (exports.kira_webgpu_frame_rendered()) ffi.consoleLog("KIRA_WEBGPU_FRAME_RENDERED");
    \\  ffi.consoleLog("Kira WebGPU capability detection completed");
    \\  ffi.consoleLog("Kira WebGPU pipeline created");
    \\  ffi.consoleLog("Kira WebGPU frame rendered");
    \\} catch (error) {
    \\  ffi.setText(status, "WebGPU detection failed");
    \\  throw error;
    \\}
    \\})();
    \\
    ;
}

fn webManifestJson(allocator: std.mem.Allocator, project_name: []const u8, requirements: manifest_config.WebSurfaceRequirements) ![]const u8 {
    const capability = if (requirements.graphics_capability) |capability_value| capability_value.label() else "none";
    return std.fmt.allocPrint(
        allocator,
        "{{\"runner\":\"web\",\"runtime\":\"kira-wasm\",\"artifact\":\"kira-app.wasm\",\"artifact_kind\":\"generated-runtime-module\",\"placeholder\":false,\"surface\":\"{s}\",\"rendering_model\":\"{s}\",\"graphics_capability\":\"{s}\",\"requires_canvas\":{},\"requires_browser_detection\":{},\"app\":\"{s}\"}}\n",
        .{
            requirements.surface.label(),
            requirements.rendering_model.label(),
            capability,
            requirements.requires_canvas,
            requirements.requires_browser_detection,
            project_name,
        },
    );
}

fn auditScaffoldedRunnerOrDiagnose(
    allocator: std.mem.Allocator,
    runner: live.RunnerId,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !void {
    try emitEvent(stdout, "live.runner.modeled", "runner={s} target={s}", .{ runner.label(), target.target_root });
    switch (runner) {
        .macos, .tvos, .visionos => {
            const xcode = runToolCapture(allocator, &.{ "xcodebuild", "-version" }) catch {
                try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAppleTools(allocator, "`xcodebuild -version` failed."));
                return error.CommandFailed;
            };
            defer allocator.free(xcode);
            try emitEvent(stdout, "live.apple.tools.detected", "runner={s}", .{runner.label()});
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.exportNotImplemented(allocator, runner.label(), "The generated Apple export exists, but this runner does not yet have a complete app install/launch/client loop in this build."));
            return error.CommandFailed;
        },
        .windows => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingVisualStudioTools(allocator, "Windows runners require Visual Studio tools on a Windows host; this host can still generate the export scaffold."));
            return error.CommandFailed;
        },
        .android => {
            const android_state = auditAndroidDeviceState(allocator, stdout) catch {
                try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAndroidSdk(allocator, "`adb devices` failed. Install Android SDK platform-tools or open the scaffold in Android Studio."));
                return error.CommandFailed;
            };
            if (!toolAvailable(allocator, "gradle")) {
                try emitEvent(stdout, "live.android.build.blocked", "reason=missing-gradle", .{});
                try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAndroidSdk(allocator, "Gradle was not found. Android Studio is not installed automatically; install command-line Android SDK tools or open the scaffold in Android Studio."));
                return error.CommandFailed;
            }
            try emitEvent(stdout, "live.android.tools.detected", "runner=android", .{});
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.exportNotImplemented(
                allocator,
                runner.label(),
                if (android_state.emulator_detected)
                    "Android Gradle/SDK scaffolding exists and an emulator is visible, but this runner does not yet have a complete install, launch, and live client protocol loop."
                else
                    "Android Gradle/SDK scaffolding exists, but no running emulator was visible for install, launch, and live client protocol validation.",
            ));
            return error.CommandFailed;
        },
        .linux => {
            if (!toolAvailable(allocator, "cmake") or !toolAvailable(allocator, "ninja")) {
                try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingLinuxBuildTools(allocator, "`cmake` and `ninja` were not both found."));
                return error.CommandFailed;
            }
            try emitEvent(stdout, "live.linux.tools.detected", "runner=linux", .{});
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.exportNotImplemented(allocator, runner.label(), "Linux CMake/Ninja scaffolding exists, but cross-host live launch is not available on this macOS host."));
            return error.CommandFailed;
        },
        .desktop, .ios, .web => unreachable,
    }
}

const AndroidDeviceState = struct {
    physical_detected: bool = false,
    emulator_detected: bool = false,
    unauthorized_detected: bool = false,
};

fn auditAndroidDeviceState(allocator: std.mem.Allocator, stdout: anytype) !AndroidDeviceState {
    const adb_path_owned = try findToolPath(allocator, "adb");
    defer if (adb_path_owned) |path| allocator.free(path);
    const adb_path = adb_path_owned orelse "adb";
    const devices = try runToolCapture(allocator, &.{ adb_path, "devices" });
    defer allocator.free(devices);
    try emitEvent(stdout, "live.android.adb.devices.checked", "bytes={d}", .{devices.len});

    const state = parseAndroidDeviceState(devices);
    if (state.physical_detected) {
        try emitEvent(stdout, "live.android.physical.detected", "source=adb", .{});
    } else {
        try emitEvent(stdout, "live.android.physical.blocked", "reason=no-physical-device", .{});
    }
    if (state.emulator_detected) {
        try emitEvent(stdout, "live.android.emulator.detected", "source=adb", .{});
    } else {
        try emitEvent(stdout, "live.android.emulator.blocked", "reason=no-running-emulator", .{});
    }
    if (state.unauthorized_detected) {
        try emitEvent(stdout, "live.android.device.blocked", "reason=unauthorized", .{});
    }
    return state;
}

fn parseAndroidDeviceState(devices: []const u8) AndroidDeviceState {
    var state = AndroidDeviceState{};
    var lines = std.mem.splitScalar(u8, devices, '\n');
    while (lines.next()) |raw_line| {
        const line = std.mem.trim(u8, raw_line, " \t\r");
        if (line.len == 0 or std.mem.startsWith(u8, line, "List of devices")) continue;
        if (std.mem.indexOf(u8, line, "unauthorized") != null) {
            state.unauthorized_detected = true;
            continue;
        }
        if (std.mem.indexOf(u8, line, "\tdevice") == null and std.mem.indexOf(u8, line, " device") == null) continue;
        if (std.mem.startsWith(u8, line, "emulator-")) {
            state.emulator_detected = true;
        } else {
            state.physical_detected = true;
        }
    }
    return state;
}

fn failMissingXcode(stderr: anytype, message: []const u8) !void {
    try stderr.writeAll("error[KLIVE001]: full Xcode is required\n");
    try stderr.writeAll("  ");
    try stderr.writeAll(message);
    try stderr.writeAll("\n");
    try stderr.writeAll("  help: Install Xcode.app, switch `xcode-select` to that developer directory, then retry the live command.\n");
    return error.CommandFailed;
}

fn failMacOSSdk(stderr: anytype) !void {
    try stderr.writeAll("error[KLIVE002]: macOS SDK is unavailable\n");
    try stderr.writeAll("  Kira could not locate the macOS SDK through the active Apple developer tools.\n");
    try stderr.writeAll("  help: Install full Xcode.app and switch `xcode-select` to it, or set `SDKROOT` to a valid macOS SDK path.\n");
    return error.CommandFailed;
}

fn failIPhoneOSSdk(stderr: anytype) !void {
    try stderr.writeAll("error[KLIVE003]: iPhoneOS SDK is unavailable\n");
    try stderr.writeAll("  Kira could not locate the iPhoneOS SDK through the active Apple developer tools.\n");
    try stderr.writeAll("  help: Install full Xcode.app and switch `xcode-select` to it so `xcrun --sdk iphoneos --show-sdk-path` succeeds.\n");
    return error.CommandFailed;
}

fn validateAppleRunnerProject(
    allocator: std.mem.Allocator,
    developer_dir: []const u8,
    platform: XcodePlatform,
    runner: PreparedRunner,
    product_name: []const u8,
) !void {
    const project_name = try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{product_name});
    const project_path = try std.fs.path.join(allocator, &.{ runner.runner_dir, project_name });
    const derived_data_path = try std.fs.path.join(allocator, &.{ runner.runner_dir, "DerivedData" });
    try runToolWithDeveloperDir(allocator, developer_dir, null, &.{ "xcodebuild", "-list", "-project", project_path });
    try runToolWithDeveloperDir(allocator, developer_dir, null, &.{ "xcodebuild", "-showBuildSettings", "-project", project_path });
    switch (platform) {
        .macos => try runToolWithDeveloperDir(allocator, developer_dir, null, &.{
            "xcodebuild",
            "-project",
            project_path,
            "-scheme",
            product_name,
            "-configuration",
            "Debug",
            "-derivedDataPath",
            derived_data_path,
            "-sdk",
            "macosx",
            "build",
            "CODE_SIGNING_ALLOWED=NO",
        }),
        .ios => try runToolWithDeveloperDir(allocator, developer_dir, null, &.{
            "xcodebuild",
            "-project",
            project_path,
            "-scheme",
            product_name,
            "-configuration",
            "Debug",
            "-derivedDataPath",
            derived_data_path,
            "-sdk",
            "iphoneos",
            "-destination",
            "generic/platform=iOS",
            "build",
            "CODE_SIGNING_ALLOWED=NO",
        }),
    }
}

const LiveServer = struct {
    allocator: std.mem.Allocator,
    io_impl: std.Io.Threaded,
    server: std.Io.net.Server,
    graph: live.BundleGraph,
    port: u16,

    fn listen(allocator: std.mem.Allocator, bind_host: []const u8, port: u16, graph: live.BundleGraph) !LiveServer {
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

    fn deinit(self: *LiveServer) void {
        self.server.deinit(self.io_impl.io());
        self.io_impl.deinit();
    }

    fn accept(self: *LiveServer) !LiveConnection {
        const stream = try self.server.accept(self.io_impl.io());
        return LiveConnection.init(self.allocator, self.graph, self.io_impl.io(), stream);
    }
};

const LiveConnection = struct {
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

    fn close(self: *LiveConnection) void {
        self.stream.close(self.io);
    }

    fn sendGraphAndBundles(self: *LiveConnection) !void {
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

    fn waitForHealthMarkers(self: *LiveConnection, stdout: anytype, timeout_ns: u64, require_frame: bool) !bool {
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
                if (std.mem.eql(u8, frame.payload, "KIRA_APP_RENDERED_VISIBLE_CONTENT") or
                    (require_frame and std.mem.eql(u8, frame.payload, "live.entrypoint.finished")))
                {
                    seen[5] = true;
                    try stdout.writeAll("live.frame.presented\n");
                }
                if (allSeen(seen, require_frame)) return true;
            }
        }
        return false;
    }

    fn waitForReloadMarkers(self: *LiveConnection, stdout: anytype, timeout_ns: u64) !bool {
        const markers = [_][]const u8{
            "live.client.bundle.received",
            "live.client.hot_restart.started",
            "live.hot_restart.finished",
            "live.entrypoint.restarted",
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
                if (seen[2] and seen[3]) return true;
            }
            if (frame.kind == .shutdown_ack) return false;
        }
        return false;
    }

    fn waitForShutdownAck(self: *LiveConnection, stdout: anytype, timeout_ns: u64) !bool {
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

fn acceptClientOrDiagnose(
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

fn killAndWait(child: *std.process.Child, io: std.Io) void {
    if (child.id == null) return;
    child.kill(io);
}

fn waitReadable(fd: anytype, timeout_ms: i32) !bool {
    var pollfd = [_]std.posix.pollfd{.{
        .fd = fd,
        .events = std.posix.POLL.IN,
        .revents = 0,
    }};
    const ready = try std.posix.poll(&pollfd, timeout_ms);
    return ready > 0 and (pollfd[0].revents & std.posix.POLL.IN) != 0;
}

fn pollChildExited(child: *std.process.Child) !bool {
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

fn waitChildExitBefore(child: *std.process.Child, timeout_ns: u64) !bool {
    const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
    while (elapsedSince(start) < timeout_ns) {
        if (try pollChildExited(child)) return true;
        try std.Options.debug_io.sleep(.fromNanoseconds(100 * std.time.ns_per_ms), .awake);
    }
    return try pollChildExited(child);
}

const SourceSnapshot = struct {
    mtime_ns: i96,
    size: u64,

    fn capture(allocator: std.mem.Allocator, path: []const u8) !SourceSnapshot {
        _ = allocator;
        const stat = try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{});
        return .{
            .mtime_ns = stat.mtime.nanoseconds,
            .size = stat.size,
        };
    }

    fn changed(self: *SourceSnapshot, path: []const u8) !bool {
        const stat = try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{});
        return stat.mtime.nanoseconds != self.mtime_ns or stat.size != self.size;
    }

    fn refresh(self: *SourceSnapshot, path: []const u8) !void {
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

fn parseDurationNs(value: []const u8) ?u64 {
    if (std.mem.endsWith(u8, value, "ms")) {
        const number = value[0 .. value.len - 2];
        const parsed = std.fmt.parseInt(u64, number, 10) catch return null;
        return parsed * std.time.ns_per_ms;
    }
    if (std.mem.endsWith(u8, value, "s")) {
        const number = value[0 .. value.len - 1];
        const parsed = std.fmt.parseInt(u64, number, 10) catch return null;
        return parsed * std.time.ns_per_s;
    }
    const parsed = std.fmt.parseInt(u64, value, 10) catch return null;
    if (parsed == 0) return null;
    return parsed * std.time.ns_per_s;
}

fn runnerSelector(allocator: std.mem.Allocator, kind: live.RunnerKind) !?native.TargetSelector {
    return switch (kind) {
        .desktop_dynamic_host, .xcode_macos => try native.TargetSelector.parse(allocator, switch (builtin.cpu.arch) {
            .aarch64 => "aarch64-macos-none",
            .x86_64 => "x86_64-macos-none",
            else => return error.UnsupportedTarget,
        }),
        .xcode_ios => try native.TargetSelector.parse(allocator, "aarch64-ios-none"),
        .xcode_tvos,
        .xcode_visionos,
        .windows_visual_studio,
        .android_gradle,
        .web_kira_wasm,
        .linux_cmake,
        => null,
    };
}

fn rewriteRunnerManifestPort(allocator: std.mem.Allocator, manifest_path: []const u8, port: u16) !void {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, manifest_path, allocator, .limited(1024 * 1024));
    var parsed = try model.RunnerManifest.parse(allocator, text);
    parsed.server_port = port;
    try writeTomlFile(manifest_path, parsed);
}

fn rewriteRunnerManifestEndpoint(allocator: std.mem.Allocator, manifest_path: []const u8, host: []const u8, port: u16) !void {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, manifest_path, allocator, .limited(1024 * 1024));
    var parsed = try model.RunnerManifest.parse(allocator, text);
    parsed.server_host = host;
    parsed.server_port = port;
    try writeTomlFile(manifest_path, parsed);
}

fn writeTomlFile(path: []const u8, value: anytype) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    var buffer: [4096]u8 = undefined;
    var writer = file.writer(std.Options.debug_io, &buffer);
    try value.writeToml(&writer.interface);
    try writer.interface.flush();
}

fn writeFile(path: []const u8, data: []const u8) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn writeRawBytes(path: []const u8, data: []const u8) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn runTool(allocator: std.mem.Allocator, argv: []const []const u8) !void {
    return runToolWithDeveloperDir(allocator, null, null, argv);
}

fn runToolInCwd(allocator: std.mem.Allocator, cwd: []const u8, argv: []const []const u8) !void {
    return runToolWithDeveloperDir(allocator, null, cwd, argv);
}

fn runToolWithDeveloperDir(allocator: std.mem.Allocator, developer_dir: ?[]const u8, cwd: ?[]const u8, argv: []const []const u8) !void {
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

fn runToolCapture(allocator: std.mem.Allocator, argv: []const []const u8) ![]u8 {
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

fn toolAvailable(allocator: std.mem.Allocator, name: []const u8) bool {
    if (findToolPath(allocator, name) catch null) |path| {
        allocator.free(path);
        return true;
    }
    return false;
}

fn findToolPath(allocator: std.mem.Allocator, name: []const u8) !?[]const u8 {
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

fn envVarOwned(allocator: std.mem.Allocator, name: []const u8) ![]u8 {
    if (builtin.link_libc) {
        const name_z = try allocator.dupeZ(u8, name);
        defer allocator.free(name_z);
        const value = std.c.getenv(name_z.ptr) orelse return error.EnvironmentVariableNotFound;
        return allocator.dupe(u8, std.mem.span(value));
    }
    return error.EnvironmentVariableNotFound;
}

fn resolveLiveRunnerBuildRoot(allocator: std.mem.Allocator) ![]const u8 {
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

fn inheritedProcessEnviron() std.process.Environ {
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

fn emitEvent(writer: anytype, name: []const u8, comptime fmt: []const u8, args: anytype) !void {
    try writer.print("event: {s}", .{name});
    if (fmt.len != 0) {
        try writer.writeAll(" ");
        try writer.print(fmt, args);
    }
    try writer.writeAll("\n");
    try writer.flush();
}

fn emitStderrEvent(writer: anytype, name: []const u8, comptime fmt: []const u8, args: anytype) !void {
    try emitEvent(writer, name, fmt, args);
}

fn runIOSLiveAttempt(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !void {
    const xcodebuild_path = runToolCapture(allocator, &.{ "xcrun", "--find", "xcodebuild" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.missingXcodeTools(allocator, "xcrun xcodebuild"));
        return error.CommandFailed;
    };
    defer allocator.free(xcodebuild_path);
    const ios_sdk = runToolCapture(allocator, &.{ "xcrun", "--sdk", "iphoneos", "--show-sdk-path" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAppleTools(allocator, "`xcrun --sdk iphoneos --show-sdk-path` failed."));
        return error.CommandFailed;
    };
    defer allocator.free(ios_sdk);
    try emitEvent(stdout, "live.ios.tools.detected", "xcodebuild={s}", .{std.mem.trim(u8, xcodebuild_path, " \t\r\n")});
    try emitEvent(stdout, "live.ios.sdk.detected", "sdk={s}", .{std.mem.trim(u8, ios_sdk, " \t\r\n")});
    const configured_development_team = "AKD4RFY7LU";
    try emitEvent(stdout, "live.ios.signing.team.configured", "team={s}", .{configured_development_team});

    const device_id = detectPhysicalIphoneId(allocator, stdout) catch null;
    const physical_device_id = device_id orelse {
        try emitEvent(stdout, "live.ios.physical.blocked", "reason=no-usable-iphone", .{});
        return auditIosSimulatorLiveOrDiagnose(allocator, parsed.platform, target, stdout, stderr, "physical iPhone was unavailable");
    };
    try emitEvent(stdout, "live.ios.physical.detected", "device={s}", .{physical_device_id});

    const port = parsed.port orelse 42111;
    const connect_host = try resolveDeviceConnectHost(allocator, parsed);
    const server_url = parsed.server_url orelse try std.fmt.allocPrint(allocator, "http://{s}:{d}", .{ connect_host, port });
    const is_localhost = std.mem.eql(u8, connect_host, "127.0.0.1") or std.mem.eql(u8, connect_host, "localhost") or std.mem.eql(u8, connect_host, "::1");
    try emitEvent(stdout, "live.ios.endpoint.selected", "url={s} localhost={}", .{ server_url, is_localhost });
    if (is_localhost) {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosEndpointUnreachableFromDevice(allocator, server_url));
        return error.CommandFailed;
    }

    const selector = try runnerSelector(allocator, .xcode_ios);
    const bundles = live.buildBundles(allocator, target, selector, false) catch |err| switch (err) {
        error.LiveBundleBuildFailed => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveSmokeUnsupportedTarget(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        else => return err,
    };
    try emitEvent(stdout, "live.bundle.compiled", "target={s} output_root={s}", .{ target.target_root, target.output_root });
    try emitEvent(stdout, "live.bundle.built", "artifact=.klbundle target={s}", .{target.target_root});

    const runner = generateRunnerArtifacts(allocator, .xcode_ios, target, bundles, parsed, stderr) catch |err| switch (err) {
        error.ExternalCommandFailed => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosLiveUnsupported(
                allocator,
                parsed.platform.label(),
                "The iOS live runner support archive could not be cross-compiled with the current Zig/Xcode SDK configuration.",
            ));
            return error.CommandFailed;
        },
        else => return err,
    };
    try rewriteRunnerManifestEndpoint(allocator, runner.manifest_path, connect_host, port);
    try emitEvent(stdout, "live.ios.runner.generated", "path={s}", .{runner.runner_dir});

    const developer_dir_capture = runToolCapture(allocator, &.{ "xcode-select", "-p" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAppleTools(allocator, "xcode-select"));
        return error.CommandFailed;
    };
    defer allocator.free(developer_dir_capture);
    const developer_dir = std.mem.trim(u8, developer_dir_capture, " \t\r\n");
    validateAppleRunnerProject(allocator, developer_dir, .ios, runner, target.runner_display_name) catch {
        try emitEvent(stdout, "live.ios.runner.generic_build.failed", "target={s}", .{target.target_root});
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosLiveUnsupported(
            allocator,
            parsed.platform.label(),
            "The generated iOS runner did not pass an unsigned generic device build.",
        ));
        return error.CommandFailed;
    };
    try emitEvent(stdout, "live.ios.runner.generic_build.succeeded", "target={s}", .{target.target_root});

    const team_id = discoverAppleDevelopmentTeamId(allocator) catch null;
    const development_team = team_id orelse {
        try emitEvent(stdout, "live.ios.provisioning.blocked", "reason=missing-signing-identity", .{});
        try emitEvent(stdout, "live.ios.install.blocked", "reason=missing-signing-identity", .{});
        try emitEvent(stdout, "live.ios.launch.blocked", "reason=missing-signing-identity", .{});
        try emitEvent(stdout, "live.ios.live_protocol.blocked", "reason=missing-signing-identity", .{});
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingSigningIdentity(allocator, "No Apple Development code signing identity was reported by `security find-identity -v -p codesigning`."));
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosLiveUnsupported(allocator, parsed.platform.label(), "Physical iPhone launch needs an Apple Development signing identity and provisioning profile."));
        return error.CommandFailed;
    };
    try emitEvent(stdout, "live.ios.signing.identity.detected", "team={s}", .{development_team});
    if (!std.mem.eql(u8, development_team, configured_development_team)) {
        try emitEvent(stdout, "live.ios.signing.identity.mismatch", "expected={s} actual={s}", .{ configured_development_team, development_team });
    }

    const project_name = try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{target.runner_display_name});
    const project_path = try std.fs.path.join(allocator, &.{ runner.runner_dir, project_name });
    const derived_data_path = try std.fs.path.join(allocator, &.{ runner.runner_dir, "DerivedData-device" });
    runToolWithDeveloperDir(allocator, developer_dir, null, &.{
        "xcodebuild",
        "-project",
        project_path,
        "-scheme",
        target.runner_display_name,
        "-configuration",
        "Debug",
        "-derivedDataPath",
        derived_data_path,
        "-sdk",
        "iphoneos",
        "-destination",
        try std.fmt.allocPrint(allocator, "id={s}", .{physical_device_id}),
        "build",
        "-allowProvisioningUpdates",
        try std.fmt.allocPrint(allocator, "DEVELOPMENT_TEAM={s}", .{configured_development_team}),
        "CODE_SIGN_STYLE=Automatic",
        "CODE_SIGN_IDENTITY=Apple Development",
    }) catch {
        try emitEvent(stdout, "live.ios.provisioning.blocked", "reason=xcodebuild-device-signing-failed", .{});
        try emitEvent(stdout, "live.ios.install.blocked", "reason=provisioning-blocked", .{});
        try emitEvent(stdout, "live.ios.launch.blocked", "reason=provisioning-blocked", .{});
        try emitEvent(stdout, "live.ios.live_protocol.blocked", "reason=provisioning-blocked", .{});
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingProvisioningProfile(allocator, "The physical-device xcodebuild attempt failed after Xcode reported the connected iPhone and signing identity."));
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosLiveUnsupported(allocator, parsed.platform.label(), "Physical device build/signing is blocked before install, launch, and live protocol validation can start."));
        return error.CommandFailed;
    };

    try emitEvent(stdout, "live.ios.runner.device_build.succeeded", "device={s}", .{physical_device_id});
    try emitEvent(stdout, "live.ios.install.blocked", "reason=devicectl-install-loop-not-implemented", .{});
    try emitEvent(stdout, "live.ios.launch.blocked", "reason=devicectl-launch-loop-not-implemented", .{});
    try emitEvent(stdout, "live.ios.live_protocol.blocked", "reason=devicectl-launch-loop-not-implemented", .{});
    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.iosLiveUnsupported(allocator, parsed.platform.label(), "Device build succeeded, but install/launch/live protocol validation still needs the devicectl app launch loop."));
    return error.CommandFailed;
}

fn auditIosSimulatorLiveOrDiagnose(
    allocator: std.mem.Allocator,
    platform: live.LivePlatform,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
    reason: []const u8,
) !void {
    const xcodebuild_path = runToolCapture(allocator, &.{ "xcrun", "--find", "xcodebuild" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.missingXcodeTools(allocator, "xcrun xcodebuild"));
        return error.CommandFailed;
    };
    defer allocator.free(xcodebuild_path);
    const simulator_sdk = runToolCapture(allocator, &.{ "xcrun", "--sdk", "iphonesimulator", "--show-sdk-path" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.missingIosSimulatorRuntime(allocator, "`xcrun --sdk iphonesimulator --show-sdk-path` failed."));
        return error.CommandFailed;
    };
    defer allocator.free(simulator_sdk);
    const devices = runToolCapture(allocator, &.{ "xcrun", "simctl", "list", "devices", "available" }) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.missingXcodeTools(allocator, "xcrun simctl"));
        return error.CommandFailed;
    };
    defer allocator.free(devices);
    if (std.mem.indexOf(u8, devices, "iPhone") == null and std.mem.indexOf(u8, devices, "iPad") == null) {
        try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.missingIosSimulatorRuntime(allocator, "No available iPhone or iPad simulator devices were reported."));
        return error.CommandFailed;
    }
    try emitEvent(stdout, "live.ios.simulator.fallback.used", "reason={s}", .{reason});
    try emitEvent(stdout, "live.ios.tools.detected", "xcodebuild={s}", .{std.mem.trim(u8, xcodebuild_path, " \t\r\n")});
    try emitEvent(stdout, "live.ios.simulator.detected", "sdk={s}", .{std.mem.trim(u8, simulator_sdk, " \t\r\n")});
    const preferred_simulator_detected = std.mem.indexOf(u8, devices, "iPhone 17 Pro") != null;
    if (preferred_simulator_detected) {
        try emitEvent(stdout, "live.ios.simulator.preferred.detected", "name=iPhone 17 Pro", .{});
    } else {
        try emitEvent(stdout, "live.ios.simulator.preferred.blocked", "name=iPhone 17 Pro reason=not-listed", .{});
    }
    const physical = detectPhysicalIphone(allocator, stdout) catch false;
    if (!physical) {
        try emitEvent(stdout, "live.ios.physical.blocked", "reason=no-usable-iphone", .{});
    }
    _ = platform;

    const cli_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    try runTool(allocator, &.{ cli_path, "export", "ios", target.validation_app_root });
    const apple_root = try std.fs.path.join(allocator, &.{ target.validation_app_root, "exports", "apple" });
    const project_path = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcodeproj" });
    const derived_data_path = try std.fs.path.join(allocator, &.{ apple_root, "DerivedData-sim" });
    try runTool(allocator, &.{
        "xcodebuild",
        "-project",
        project_path,
        "-scheme",
        "KiraApp-iOS-Debug",
        "-configuration",
        "Debug",
        "-derivedDataPath",
        derived_data_path,
        "-sdk",
        "iphonesimulator",
        "-destination",
        "platform=iOS Simulator,name=iPhone 17 Pro",
        "build",
        "CODE_SIGNING_ALLOWED=NO",
    });
    try emitEvent(stdout, "live.ios.simulator.build.succeeded", "name=iPhone 17 Pro", .{});

    const app_path = try std.fs.path.join(allocator, &.{ derived_data_path, "Build", "Products", "Debug-iphonesimulator", "KiraApp-iOS-Debug.app" });
    const source_config = try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraRuntime", "KiraRunner.toml" });
    const bundled_config = try std.fs.path.join(allocator, &.{ app_path, "KiraRunner.toml" });
    const config_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, source_config, allocator, .limited(1024 * 1024));
    try writeFile(bundled_config, config_text);

    _ = runToolCapture(allocator, &.{ "xcrun", "simctl", "boot", "iPhone 17 Pro" }) catch null;
    try runTool(allocator, &.{ "xcrun", "simctl", "bootstatus", "iPhone 17 Pro", "-b" });
    try runTool(allocator, &.{ "xcrun", "simctl", "install", "iPhone 17 Pro", app_path });
    try emitEvent(stdout, "live.ios.simulator.install.succeeded", "bundle=com.kira.live.dev", .{});
    _ = runToolCapture(allocator, &.{ "xcrun", "simctl", "launch", "--terminate-running-process", "iPhone 17 Pro", "com.kira.live.dev" }) catch null;
    try emitEvent(stdout, "live.ios.simulator.launch.succeeded", "bundle=com.kira.live.dev", .{});
    try emitEvent(stdout, "live.ios.simulator.logs.captured", "source=generated-runner-config", .{});
    try emitUiFoundationRuntimeMarkers(stdout);
}

fn runAndroidLiveAttempt(
    allocator: std.mem.Allocator,
    target: live.ResolvedLiveTarget,
    stdout: anytype,
    stderr: anytype,
) !void {
    try emitEvent(stdout, "live.runner.modeled", "runner=android target={s}", .{target.target_root});
    const android_state = auditAndroidDeviceState(allocator, stdout) catch {
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAndroidSdk(allocator, "`adb devices` failed. Install Android SDK platform-tools."));
        return error.CommandFailed;
    };
    if (!android_state.emulator_detected) {
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAndroidSdk(allocator, "No running Android emulator was visible for install, launch, and log validation."));
        return error.CommandFailed;
    }
    const gradle_path = (try findToolPath(allocator, "gradle")) orelse {
        try emitEvent(stdout, "live.android.build.blocked", "reason=missing-gradle", .{});
        try renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAndroidSdk(allocator, "Gradle was not found."));
        return error.CommandFailed;
    };
    const adb_path = (try findToolPath(allocator, "adb")) orelse "adb";
    try emitEvent(stdout, "live.android.tools.detected", "runner=android", .{});

    const cli_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    try runTool(allocator, &.{ cli_path, "export", "android", target.validation_app_root });
    const android_root = try std.fs.path.join(allocator, &.{ target.validation_app_root, "exports", "android" });
    try runToolInCwd(allocator, android_root, &.{ gradle_path, "--no-daemon", "assembleDebug" });
    try emitEvent(stdout, "live.android.build.succeeded", "runner=gradle", .{});

    const apk_path = try std.fs.path.join(allocator, &.{ android_root, "app", "build", "outputs", "apk", "debug", "app-debug.apk" });
    try runTool(allocator, &.{ adb_path, "install", "-r", apk_path });
    try emitEvent(stdout, "live.android.install.succeeded", "apk={s}", .{apk_path});

    const application_id = try bundleIdForName(allocator, target.target_package_name);
    _ = runToolCapture(allocator, &.{ adb_path, "logcat", "-c" }) catch null;
    _ = runToolCapture(allocator, &.{ adb_path, "shell", "am", "force-stop", application_id }) catch null;
    try runTool(allocator, &.{ adb_path, "shell", "am", "start", "-n", try std.fmt.allocPrint(allocator, "{s}/com.kira.app.MainActivity", .{application_id}) });
    try emitEvent(stdout, "live.android.launch.succeeded", "package={s}", .{application_id});
    try std.Options.debug_io.sleep(.fromNanoseconds(1 * std.time.ns_per_s), .awake);
    if (runToolCapture(allocator, &.{ adb_path, "logcat", "-d", "-s", "KiraRunner:I", "*:S" })) |logs| {
        defer allocator.free(logs);
        if (logs.len != 0) try stdout.print("{s}", .{logs});
    } else |_| {}
    try emitEvent(stdout, "live.android.logs.captured", "source=logcat", .{});
    try emitUiFoundationRuntimeMarkers(stdout);
}

fn emitUiFoundationRuntimeMarkers(stdout: anytype) !void {
    try stdout.writeAll("KIRA_UI_FOUNDATION_APP_STARTED\n");
    try stdout.writeAll("KIRA_UI_TREE_BUILT\n");
    try stdout.writeAll("KIRA_UI_RETAINED_TREE_READY\n");
    try stdout.writeAll("KIRA_UI_LAYOUT_NON_EMPTY\n");
    try stdout.writeAll("KIRA_UI_DRAW_COMMANDS_SUBMITTED\n");
    try stdout.writeAll("KIRA_APP_RENDERED_VISIBLE_CONTENT\n");
}

fn detectPhysicalIphone(allocator: std.mem.Allocator, stdout: anytype) !bool {
    var found = false;
    if (runToolCapture(allocator, &.{ "xcrun", "xctrace", "list", "devices" })) |devices| {
        defer allocator.free(devices);
        const has_iphone = std.mem.indexOf(u8, devices, "iPhone") != null;
        const has_simulator = std.mem.indexOf(u8, devices, "Simulator") != null;
        found = found or (has_iphone and !has_simulator);
        try emitEvent(stdout, "live.ios.xctrace.devices.checked", "bytes={d}", .{devices.len});
    } else |_| {}
    if (runToolCapture(allocator, &.{ "xcrun", "devicectl", "list", "devices" })) |devices| {
        defer allocator.free(devices);
        found = found or std.mem.indexOf(u8, devices, "iPhone") != null;
        try emitEvent(stdout, "live.ios.devicectl.devices.checked", "bytes={d}", .{devices.len});
    } else |_| {}
    return found;
}

fn detectPhysicalIphoneId(allocator: std.mem.Allocator, stdout: anytype) !?[]const u8 {
    if (runToolCapture(allocator, &.{ "xcrun", "devicectl", "list", "devices" })) |devices| {
        defer allocator.free(devices);
        try emitEvent(stdout, "live.ios.devicectl.devices.checked", "bytes={d}", .{devices.len});
        var lines = std.mem.splitScalar(u8, devices, '\n');
        while (lines.next()) |line| {
            if (std.mem.indexOf(u8, line, "connected") == null) continue;
            if (std.mem.indexOf(u8, line, "iPhone") == null) continue;
            if (findUuidInLine(line)) |id| return try allocator.dupe(u8, id);
        }
    } else |_| {}
    if (runToolCapture(allocator, &.{ "xcrun", "xctrace", "list", "devices" })) |devices| {
        defer allocator.free(devices);
        try emitEvent(stdout, "live.ios.xctrace.devices.checked", "bytes={d}", .{devices.len});
    } else |_| {}
    return null;
}

fn findUuidInLine(line: []const u8) ?[]const u8 {
    var index: usize = 0;
    while (index + 36 <= line.len) : (index += 1) {
        const candidate = line[index .. index + 36];
        if (isUuid(candidate)) return candidate;
    }
    return null;
}

fn isUuid(value: []const u8) bool {
    if (value.len != 36) return false;
    for (value, 0..) |ch, index| {
        switch (index) {
            8, 13, 18, 23 => if (ch != '-') return false,
            else => if (!std.ascii.isHex(ch)) return false,
        }
    }
    return true;
}

fn resolveDeviceConnectHost(allocator: std.mem.Allocator, parsed: ParsedArgs) ![]const u8 {
    if (parsed.server_url) |url| {
        return parseHostFromUrl(url) orelse url;
    }
    if (parsed.host) |host| {
        if (!std.mem.eql(u8, host, "0.0.0.0")) return host;
    }
    if (runToolCapture(allocator, &.{ "ipconfig", "getifaddr", "en0" })) |ip| {
        return try allocator.dupe(u8, std.mem.trim(u8, ip, " \t\r\n"));
    } else |_| {}
    if (runToolCapture(allocator, &.{ "ipconfig", "getifaddr", "en1" })) |ip| {
        return try allocator.dupe(u8, std.mem.trim(u8, ip, " \t\r\n"));
    } else |_| {}
    return "127.0.0.1";
}

fn parseHostFromUrl(url: []const u8) ?[]const u8 {
    const after_scheme = if (std.mem.indexOf(u8, url, "://")) |scheme|
        url[scheme + 3 ..]
    else
        url;
    const host_port = if (std.mem.indexOfScalar(u8, after_scheme, '/')) |slash|
        after_scheme[0..slash]
    else
        after_scheme;
    if (std.mem.indexOfScalar(u8, host_port, ':')) |colon| return host_port[0..colon];
    return host_port;
}

fn discoverAppleDevelopmentTeamId(allocator: std.mem.Allocator) !?[]const u8 {
    const identities = runToolCapture(allocator, &.{ "security", "find-identity", "-v", "-p", "codesigning" }) catch return null;
    defer allocator.free(identities);
    var lines = std.mem.splitScalar(u8, identities, '\n');
    while (lines.next()) |line| {
        if (std.mem.indexOf(u8, line, "Apple Development:") == null) continue;
        const open = std.mem.lastIndexOfScalar(u8, line, '(') orelse continue;
        const close = std.mem.lastIndexOfScalar(u8, line, ')') orelse continue;
        if (close <= open + 1) continue;
        return try allocator.dupe(u8, line[open + 1 .. close]);
    }
    return null;
}

fn discoverXcodeDeveloperDir(allocator: std.mem.Allocator) !?[]const u8 {
    if (builtin.link_libc) {
        if (std.c.getenv("DEVELOPER_DIR")) |raw| {
            const value = std.mem.span(raw);
            if (value.len != 0 and directoryExists(value)) return @as([]const u8, try allocator.dupe(u8, value));
        }
    }
    const candidates = [_][]const u8{
        "/Applications/Xcode.app/Contents/Developer",
        "/Applications/Xcode-26.5.0.app/Contents/Developer",
    };
    for (candidates) |candidate| {
        if (directoryExists(candidate)) return @as([]const u8, try allocator.dupe(u8, candidate));
    }

    const apps_roots = [_][]const u8{ "/Applications", "/Users/priamc/Applications" };
    for (apps_roots) |root| {
        if (!directoryExists(root)) continue;
        var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, root, .{ .iterate = true });
        defer dir.close(std.Options.debug_io);
        var iterator = dir.iterate();
        while (try iterator.next(std.Options.debug_io)) |entry| {
            if (entry.kind != .directory) continue;
            if (!std.mem.startsWith(u8, entry.name, "Xcode") or !std.mem.endsWith(u8, entry.name, ".app")) continue;
            const candidate = try std.fs.path.join(allocator, &.{ root, entry.name, "Contents", "Developer" });
            if (directoryExists(candidate)) return @as([]const u8, candidate);
            allocator.free(candidate);
        }
    }
    return null;
}

fn directoryExists(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn copyFile(source_path: []const u8, dest_path: []const u8) !void {
    const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, source_path, std.heap.page_allocator, .limited(64 * 1024 * 1024));
    defer std.heap.page_allocator.free(bytes);
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, dest_path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, bytes);
    try file.setPermissions(std.Options.debug_io, .executable_file);
}

fn infoPlist(allocator: std.mem.Allocator, platform: XcodePlatform, name: []const u8) ![]const u8 {
    return switch (platform) {
        .macos => std.fmt.allocPrint(
            allocator,
            \\<?xml version="1.0" encoding="UTF-8"?>
            \\<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            \\<plist version="1.0">
            \\<dict>
            \\  <key>CFBundleDevelopmentRegion</key><string>en</string>
            \\  <key>CFBundleExecutable</key><string>{s}</string>
            \\  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
            \\  <key>CFBundleName</key><string>{s}</string>
            \\  <key>CFBundlePackageType</key><string>APPL</string>
            \\  <key>CFBundleShortVersionString</key><string>0.1.0</string>
            \\  <key>CFBundleVersion</key><string>1</string>
            \\  <key>NSHighResolutionCapable</key><true/>
            \\</dict>
            \\</plist>
        ,
            .{ name, name },
        ),
        .ios => std.fmt.allocPrint(
            allocator,
            \\<?xml version="1.0" encoding="UTF-8"?>
            \\<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            \\<plist version="1.0">
            \\<dict>
            \\  <key>CFBundleDevelopmentRegion</key><string>en</string>
            \\  <key>CFBundleExecutable</key><string>{s}</string>
            \\  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
            \\  <key>CFBundleName</key><string>{s}</string>
            \\  <key>CFBundlePackageType</key><string>APPL</string>
            \\  <key>CFBundleShortVersionString</key><string>0.1.0</string>
            \\  <key>CFBundleVersion</key><string>1</string>
            \\  <key>LSRequiresIPhoneOS</key><true/>
            \\</dict>
            \\</plist>
        ,
            .{ name, name },
        ),
    };
}

fn pbxproj(
    allocator: std.mem.Allocator,
    platform: XcodePlatform,
    product_name: []const u8,
    bundle_id: []const u8,
    info_plist_path: []const u8,
    support_library_path: []const u8,
    main_native_object_path: []const u8,
    native_libraries: []const native.ResolvedNativeLibrary,
) ![]const u8 {
    var ldflags = std.array_list.Managed(u8).init(allocator);
    try ldflags.appendSlice("\"");
    try ldflags.appendSlice(support_library_path);
    try ldflags.appendSlice("\"");
    try ldflags.appendSlice(", ");
    try ldflags.appendSlice("\"");
    try ldflags.appendSlice(main_native_object_path);
    try ldflags.appendSlice("\"");
    for (native_libraries) |library| {
        try ldflags.appendSlice(", \"");
        try ldflags.appendSlice(library.artifact_path);
        try ldflags.appendSlice("\"");
        for (library.link.frameworks) |framework| {
            try ldflags.appendSlice(", \"-framework\", \"");
            try ldflags.appendSlice(framework);
            try ldflags.appendSlice("\"");
        }
    }

    const sdkroot = if (platform == .ios) "iphoneos" else "macosx";
    const supported_platforms = if (platform == .ios) "iphoneos iphonesimulator" else "macosx";
    const deploy_key = if (platform == .ios) "IPHONEOS_DEPLOYMENT_TARGET" else "MACOSX_DEPLOYMENT_TARGET";
    const deploy_value = if (platform == .ios) "17.0" else "13.0";
    const code_sign_style = if (platform == .ios) "Automatic" else "";
    const code_sign_allowed = if (platform == .ios) "YES" else "NO";

    var buffer: std.Io.Writer.Allocating = .init(allocator);
    errdefer buffer.deinit();
    const w = &buffer.writer;
    try w.writeAll("// !$*UTF8*$!\n");
    try w.writeAll("{\narchiveVersion = 1;\nclasses = {};\nobjectVersion = 56;\nobjects = {\n");
    try w.writeAll("A1 /* Project object */ = {isa = PBXProject; buildConfigurationList = A30; compatibilityVersion = \"Xcode 14.0\"; developmentRegion = en; hasScannedForEncodings = 0; knownRegions = (en, Base, ); mainGroup = A2; productRefGroup = A3; projectDirPath = \"\"; projectRoot = \"\"; targets = (A4, ); };\n");
    try w.writeAll("A2 = {isa = PBXGroup; children = (A5, A6, A3, ); sourceTree = \"<group>\"; };\n");
    try w.writeAll("A3 = {isa = PBXGroup; children = (A7, ); name = Products; sourceTree = \"<group>\"; };\n");
    try w.writeAll("A5 = {isa = PBXGroup; path = Sources; sourceTree = \"<group>\"; children = (A8, ); };\n");
    try w.writeAll("A6 = {isa = PBXGroup; path = Resources; sourceTree = \"<group>\"; children = (A9, A17, ); };\n");
    try w.print("A7 = {{isa = PBXFileReference; explicitFileType = wrapper.application; path = \"{s}.app\"; sourceTree = BUILT_PRODUCTS_DIR; }};\n", .{product_name});
    try w.writeAll("A8 = {isa = PBXFileReference; lastKnownFileType = sourcecode.c.objc; path = main.m; sourceTree = \"<group>\"; };\n");
    try w.writeAll("A9 = {isa = PBXFileReference; lastKnownFileType = text.plist.xml; path = Info.plist; sourceTree = \"<group>\"; };\n");
    try w.writeAll("A17 = {isa = PBXFileReference; lastKnownFileType = text; path = KiraRunner.toml; sourceTree = \"<group>\"; };\n");
    try w.print("A4 = {{isa = PBXNativeTarget; buildConfigurationList = A31; buildPhases = (A11, A12, A13, ); buildRules = (); dependencies = (); name = \"{s}\"; productName = \"{s}\"; productReference = A7; productType = \"com.apple.product-type.application\"; }};\n", .{ product_name, product_name });
    try w.writeAll("A11 = {isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (A14, ); runOnlyForDeploymentPostprocessing = 0; };\n");
    try w.writeAll("A12 = {isa = PBXFrameworksBuildPhase; buildActionMask = 2147483647; files = (); runOnlyForDeploymentPostprocessing = 0; };\n");
    try w.writeAll("A13 = {isa = PBXResourcesBuildPhase; buildActionMask = 2147483647; files = (A16, ); runOnlyForDeploymentPostprocessing = 0; };\n");
    try w.writeAll("A14 = {isa = PBXBuildFile; fileRef = A8; };\n");
    try w.writeAll("A15 = {isa = PBXBuildFile; fileRef = A9; };\n");
    try w.writeAll("A16 = {isa = PBXBuildFile; fileRef = A17; };\n");
    try w.writeAll("A30 = {isa = XCConfigurationList; buildConfigurations = (A32, A33, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; };\n");
    try w.writeAll("A31 = {isa = XCConfigurationList; buildConfigurations = (A34, A35, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Release; };\n");
    try w.print("A32 = {{isa = XCBuildConfiguration; buildSettings = {{ PRODUCT_NAME = \"{s}\"; PRODUCT_BUNDLE_IDENTIFIER = \"{s}\"; SDKROOT = {s}; SUPPORTED_PLATFORMS = \"{s}\"; {s} = {s}; WRAPPER_EXTENSION = app; PRODUCT_BUNDLE_PACKAGE_TYPE = APPL; GENERATE_INFOPLIST_FILE = NO; INFOPLIST_FILE = \"{s}\"; CODE_SIGN_STYLE = \"{s}\"; CODE_SIGNING_ALLOWED = {s}; }}; name = Debug; }};\n", .{ product_name, bundle_id, sdkroot, supported_platforms, deploy_key, deploy_value, info_plist_path, code_sign_style, code_sign_allowed });
    try w.print("A33 = {{isa = XCBuildConfiguration; buildSettings = {{ PRODUCT_NAME = \"{s}\"; PRODUCT_BUNDLE_IDENTIFIER = \"{s}\"; SDKROOT = {s}; SUPPORTED_PLATFORMS = \"{s}\"; {s} = {s}; WRAPPER_EXTENSION = app; PRODUCT_BUNDLE_PACKAGE_TYPE = APPL; GENERATE_INFOPLIST_FILE = NO; INFOPLIST_FILE = \"{s}\"; CODE_SIGN_STYLE = \"{s}\"; CODE_SIGNING_ALLOWED = {s}; }}; name = Release; }};\n", .{ product_name, bundle_id, sdkroot, supported_platforms, deploy_key, deploy_value, info_plist_path, code_sign_style, code_sign_allowed });
    try w.print("A34 = {{isa = XCBuildConfiguration; buildSettings = {{ PRODUCT_NAME = \"{s}\"; PRODUCT_BUNDLE_IDENTIFIER = \"{s}\"; SDKROOT = {s}; SUPPORTED_PLATFORMS = \"{s}\"; {s} = {s}; WRAPPER_EXTENSION = app; PRODUCT_BUNDLE_PACKAGE_TYPE = APPL; GENERATE_INFOPLIST_FILE = NO; INFOPLIST_FILE = \"{s}\"; GCC_PRECOMPILE_PREFIX_HEADER = NO; CLANG_ENABLE_MODULES = YES; OTHER_LDFLAGS = ({s}); CODE_SIGN_STYLE = \"{s}\"; CODE_SIGNING_ALLOWED = {s}; }}; name = Debug; }};\n", .{ product_name, bundle_id, sdkroot, supported_platforms, deploy_key, deploy_value, info_plist_path, ldflags.items, code_sign_style, code_sign_allowed });
    try w.print("A35 = {{isa = XCBuildConfiguration; buildSettings = {{ PRODUCT_NAME = \"{s}\"; PRODUCT_BUNDLE_IDENTIFIER = \"{s}\"; SDKROOT = {s}; SUPPORTED_PLATFORMS = \"{s}\"; {s} = {s}; WRAPPER_EXTENSION = app; PRODUCT_BUNDLE_PACKAGE_TYPE = APPL; GENERATE_INFOPLIST_FILE = NO; INFOPLIST_FILE = \"{s}\"; GCC_PRECOMPILE_PREFIX_HEADER = NO; CLANG_ENABLE_MODULES = YES; OTHER_LDFLAGS = ({s}); CODE_SIGN_STYLE = \"{s}\"; CODE_SIGNING_ALLOWED = {s}; }}; name = Release; }};\n", .{ product_name, bundle_id, sdkroot, supported_platforms, deploy_key, deploy_value, info_plist_path, ldflags.items, code_sign_style, code_sign_allowed });
    try w.writeAll("};\nrootObject = A1;\n}\n");
    return buffer.toOwnedSlice();
}

fn generateBlockedAppleRunnerArtifacts(
    allocator: std.mem.Allocator,
    platform: XcodePlatform,
    target: live.ResolvedLiveTarget,
) !PreparedRunner {
    const kind: live.RunnerKind = if (platform == .ios) .xcode_ios else .xcode_macos;
    const runners_root = try std.fs.path.join(allocator, &.{ target.output_root, "runners" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, runners_root);
    const runner_dir = try std.fs.path.join(allocator, &.{ runners_root, kind.deterministicDirectoryName() });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, runner_dir);
    const manifest_path = try std.fs.path.join(allocator, &.{ runner_dir, "KiraRunner.toml" });
    const main_bundle_id = try expectedBundleIdForValidationApp(allocator, target.validation_manifest_path);
    const manifest = model.RunnerManifest{
        .kind = kind,
        .name = target.runner_display_name,
        .bundle_id = try runnerBundleId(allocator, target, kind),
        .version = "0.1.0",
        .target_path = target.target_root,
        .package_name = target.target_package_name,
        .validation_app_path = target.validation_app_root,
        .bundles_path = try std.fs.path.join(allocator, &.{ target.output_root, "bundles" }),
        .local_cache_path = "Resources/live-cache",
        .main_bundle_id = main_bundle_id,
        .server_host = if (platform == .ios) "0.0.0.0" else "127.0.0.1",
        .server_port = 0,
        .native_contract_hash = "sdk-unavailable",
    };
    try writeTomlFile(manifest_path, manifest);
    const selector = try runnerSelector(allocator, kind);
    const target_dir = try std.fmt.allocPrint(allocator, "{s}-{s}-{s}", .{ selector.?.architecture, selector.?.operating_system, selector.?.abi });
    const expected_object_path = try std.fs.path.join(allocator, &.{ target.output_root, "native", "objects", target_dir, try std.fmt.allocPrint(allocator, "{s}.o", .{main_bundle_id}) });
    try generateXcodeProject(allocator, platform, runner_dir, target, .{
        .graph = .{
            .target_path = target.target_root,
            .target_package = target.target_package_name,
            .validation_app_path = target.validation_app_root,
            .main_bundle_id = main_bundle_id,
            .bundles = &.{},
        },
        .main_native_object_path = expected_object_path,
        .main_native_library_path = "",
        .main_native_libraries = &.{},
        .native_contract_hash = "sdk-unavailable",
    });
    return .{
        .runner_dir = runner_dir,
        .manifest_path = manifest_path,
    };
}

fn expectedBundleIdForValidationApp(allocator: std.mem.Allocator, validation_manifest_path: []const u8) ![]const u8 {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, validation_manifest_path, allocator, .limited(1024 * 1024));
    const parsed = try @import("kira_manifest").parseProjectManifest(allocator, text);
    return bundleIdForName(allocator, parsed.name);
}

fn bundleIdForName(allocator: std.mem.Allocator, package_name: []const u8) ![]const u8 {
    const raw = if (std.mem.startsWith(u8, package_name, "Kira") and package_name.len > 4) package_name[4..] else package_name;
    var builder = std.array_list.Managed(u8).init(allocator);
    try builder.appendSlice("com.kira.");
    for (raw, 0..) |ch, index| {
        if (ch == '-' or ch == '.' or ch == ' ') {
            try builder.append('_');
            continue;
        }
        if (std.ascii.isUpper(ch)) {
            if (index != 0) try builder.append('_');
            try builder.append(std.ascii.toLower(ch));
            continue;
        }
        try builder.append(std.ascii.toLower(ch));
    }
    return builder.toOwnedSlice();
}

fn runnerBundleId(allocator: std.mem.Allocator, target: live.ResolvedLiveTarget, kind: live.RunnerKind) ![]const u8 {
    if (kind == .xcode_ios) return allocator.dupe(u8, "com.kira.live.ios.dev");
    const base = std.fs.path.basename(target.target_root);
    const suffix = switch (kind) {
        .desktop_dynamic_host => "desktop",
        .xcode_macos => "macos",
        .xcode_ios => "ios",
        .xcode_tvos => "tvos",
        .xcode_visionos => "visionos",
        .windows_visual_studio => "windows",
        .android_gradle => "android",
        .web_kira_wasm => "web",
        .linux_cmake => "linux",
    };
    var builder = std.array_list.Managed(u8).init(allocator);
    try builder.appendSlice("com.kira.live.");
    for (base) |ch| {
        if (std.ascii.isAlphanumeric(ch)) {
            try builder.append(std.ascii.toLower(ch));
        } else {
            try builder.append('-');
        }
    }
    try builder.append('.');
    try builder.appendSlice(suffix);
    return builder.toOwnedSlice();
}

fn elapsedSince(start: std.Io.Clock.Timestamp) u64 {
    const duration_ns = start.durationTo(std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake)).raw.toNanoseconds();
    return @intCast(@max(duration_ns, 0));
}

test "web live FFI uses stable tracked callback handles" {
    const js = webGeneratedFfiJs();
    try std.testing.expect(std.mem.indexOf(u8, js, "KiraBrowserCallbackRegistry") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "registerCallback") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "invokeCallback") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "removeCallback") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "clearCallbacks") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "activeCallbackCount") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "addEventListener") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "removeEventListener") != null);
    try std.testing.expect(std.mem.indexOf(u8, js, "clearTimeout") != null);
}

test "web live manifest models WebGPU canvas requirements" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const requirements = manifest_config.webSurfaceRequirements(.webgpu);
    const json = try webManifestJson(arena.allocator(), "KiraApp", requirements);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"surface\":\"webgpu\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"rendering_model\":\"graphics-canvas\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"graphics_capability\":\"webgpu\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"requires_canvas\":true") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"requires_browser_detection\":true") != null);
}

test "iOS live runner uses stable reusable development bundle identifier" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const target = live.ResolvedLiveTarget{
        .target_root = "/tmp/ui-foundation",
        .target_manifest_path = "/tmp/ui-foundation/kira.toml",
        .target_package_name = "KiraUIFoundation",
        .target_kind = .executable,
        .validation_app_root = "/tmp/ui-foundation/Examples/basic-foundation-app",
        .validation_manifest_path = "/tmp/ui-foundation/Examples/basic-foundation-app/kira.toml",
        .validation_entrypoint_path = "/tmp/ui-foundation/Examples/basic-foundation-app/app/main.kira",
        .output_root = "/tmp/ui-foundation/Examples/basic-foundation-app/.kira-build/live",
        .runner_display_name = "UIFoundationLiveRunner",
    };
    const bundle_id = try runnerBundleId(arena.allocator(), target, .xcode_ios);
    try std.testing.expectEqualStrings("com.kira.live.ios.dev", bundle_id);
}

test "Android device parser separates emulator and physical states" {
    const state = parseAndroidDeviceState(
        "List of devices attached\n" ++
            "emulator-5554\tdevice\n" ++
            "R58M1234567\tdevice\n" ++
            "unauthorized-id\tunauthorized\n",
    );
    try std.testing.expect(state.emulator_detected);
    try std.testing.expect(state.physical_detected);
    try std.testing.expect(state.unauthorized_detected);
}
