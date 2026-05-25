const std = @import("std");
const builtin = @import("builtin");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const live = @import("root.zig");
const model = @import("model.zig");
const native = @import("kira_native_lib_definition");
const protocol = @import("protocol.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
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
    const developer_dir = if (parsed.kind == .xcode_macos or parsed.kind == .xcode_ios) try discoverXcodeDeveloperDir(allocator) else null;
    const selector = try runnerSelector(allocator, parsed.kind);
    const bundles = live.buildBundles(allocator, target, selector, parsed.kind != .desktop_dynamic_host) catch |err| switch (err) {
        error.LiveBundleBuildFailed => {
            const diagnostic = if (parsed.kill_after)
                try diag_messages.CliMessages.liveSmokeUnsupportedTarget(allocator, parsed.input_path)
            else
                try diag_messages.CliMessages.liveBundleBuildFailed(allocator, parsed.input_path);
            try renderStandaloneDiagnostic(stderr, diagnostic);
            return error.CommandFailed;
        },
        error.IPhoneOSSdkUnavailable => {
            if (parsed.kind == .xcode_ios) {
                const blocked = try generateBlockedAppleRunnerArtifacts(allocator, .ios, target);
                if (developer_dir) |dir| {
                    try runToolWithDeveloperDir(allocator, dir, &.{ "plutil", "-lint", try std.fs.path.join(allocator, &.{ blocked.runner_dir, try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{target.runner_display_name}), "project.pbxproj" }) });
                    try runToolWithDeveloperDir(allocator, dir, &.{ "xcodebuild", "-list", "-project", try std.fs.path.join(allocator, &.{ blocked.runner_dir, try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{target.runner_display_name}) }) });
                }
                return failIPhoneOSSdk(stderr);
            }
            return err;
        },
        error.MacOSSdkUnavailable => {
            if (parsed.kind == .xcode_macos) {
                const blocked = try generateBlockedAppleRunnerArtifacts(allocator, .macos, target);
                if (developer_dir) |dir| {
                    try runToolWithDeveloperDir(allocator, dir, &.{ "plutil", "-lint", try std.fs.path.join(allocator, &.{ blocked.runner_dir, try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{target.runner_display_name}), "project.pbxproj" }) });
                    try runToolWithDeveloperDir(allocator, dir, &.{ "xcodebuild", "-list", "-project", try std.fs.path.join(allocator, &.{ blocked.runner_dir, try std.fmt.allocPrint(allocator, "{s}.xcodeproj", .{target.runner_display_name}) }) });
                }
                return failMacOSSdk(stderr);
            }
            return err;
        },
        else => {
            if (parsed.kind == .xcode_ios or parsed.kind == .xcode_macos) {
                _ = try generateBlockedAppleRunnerArtifacts(allocator, if (parsed.kind == .xcode_ios) .ios else .macos, target);
            }
            return err;
        },
    };
    const runner = generateRunnerArtifacts(allocator, parsed.kind, target, bundles, parsed) catch |err| switch (err) {
        error.ExternalCommandFailed => {
            try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.liveBundleBuildFailed(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        else => return err,
    };
    if (parsed.mode == .runners_build) {
        try stdout.print("built {s}\n", .{runner.runner_dir});
        return;
    }

    switch (parsed.kind) {
        .desktop_dynamic_host => try runDesktop(allocator, parsed, bundles, runner, stdout, stderr),
        .xcode_macos => {
            const dir = developer_dir orelse return failMissingXcode(stderr, "macOS runner generation completed but building requires the full Xcode app, not Command Line Tools.");
            try validateAppleRunnerProject(allocator, dir, .macos, runner, target.runner_display_name);
            try runMacOSApp(allocator, parsed, bundles, runner, target.runner_display_name, stdout, stderr);
        },
        .xcode_ios => {
            const dir = developer_dir orelse return failMissingXcode(stderr, "iOS runner generation completed but building/installing requires the full Xcode app, not Command Line Tools.");
            validateAppleRunnerProject(allocator, dir, .ios, runner, target.runner_display_name) catch {
                try stderr.writeAll("error[KLIVE004]: iOS runner build validation failed\n");
                try stderr.writeAll("  Xcode can read the generated iOS project, but the build destination/platform setup is not usable on this machine.\n");
                try stderr.writeAll("  help: Install the required iOS platform components in Xcode and ensure a valid generic iOS destination or connected device is available.\n");
                return error.CommandFailed;
            };
            return error.CommandFailed;
        },
    }
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
    kind: live.RunnerKind = .desktop_dynamic_host,
    input_path: []const u8,
    run_for_ns: u64 = 5 * std.time.ns_per_s,
    kill_after: bool = false,
    device: []const u8 = "auto",
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    if (args.len == 0) return error.InvalidArguments;
    if (std.mem.eql(u8, args[0], "runners")) {
        if (args.len < 3) return error.InvalidArguments;
        if (std.mem.eql(u8, args[1], "list")) return .{ .mode = .runners_list, .input_path = args[2] };
        if (std.mem.eql(u8, args[1], "build")) return .{ .mode = .runners_build, .input_path = args[2], .kind = .desktop_dynamic_host };
        if (std.mem.eql(u8, args[1], "clean")) return .{ .mode = .runners_clean, .input_path = args[2] };
        return error.InvalidArguments;
    }

    const kind = live.RunnerKind.parse(args[0]) orelse return error.InvalidArguments;
    var parsed = ParsedArgs{
        .mode = .run,
        .kind = kind,
        .input_path = "",
    };
    var index: usize = 1;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
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
    if (parsed.input_path.len == 0) return error.InvalidArguments;
    return parsed;
}

const PreparedRunner = struct {
    runner_dir: []const u8,
    manifest_path: []const u8,
    executable_path: ?[]const u8 = null,
};

fn generateRunnerArtifacts(
    allocator: std.mem.Allocator,
    kind: live.RunnerKind,
    target: live.ResolvedLiveTarget,
    bundles: live.BundleBuildArtifacts,
    parsed: ParsedArgs,
) !PreparedRunner {
    const runners_root = try std.fs.path.join(allocator, &.{ target.output_root, "runners" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, runners_root);
    const runner_dir = try std.fs.path.join(allocator, &.{ runners_root, kind.deterministicDirectoryName() });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, runner_dir);
    const cache_rel = switch (kind) {
        .desktop_dynamic_host => "cache",
        else => "Resources/live-cache",
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
            try runTool(allocator, &.{ "zig", "build", "live-desktop-runner" });
            const bin_dir = try std.fs.path.join(allocator, &.{ runner_dir, "bin" });
            try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, bin_dir);
            const source_exe = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, "zig-out/bin/kira-live-desktop-runner", allocator);
            const dest_exe = try std.fs.path.join(allocator, &.{ bin_dir, "kira-live-desktop-runner" });
            try copyFile(source_exe, dest_exe);
            return .{
                .runner_dir = runner_dir,
                .manifest_path = manifest_path,
                .executable_path = dest_exe,
            };
        },
        .xcode_macos => {
            try generateXcodeProject(allocator, .macos, runner_dir, target, bundles);
        },
        .xcode_ios => {
            try generateXcodeProject(allocator, .ios, runner_dir, target, bundles);
        },
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
    try runTool(allocator, &.{ "zig", "build", "live-runner-support", "-Doptimize=ReleaseFast" });
    const support_library_source = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, "zig-out/lib/libkira_live_runner_support.a", allocator);
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
    try runTool(allocator, &.{ "/usr/bin/libtool", "-static", "-o", dest_archive, source_archive });
    return dest_archive;
}

fn runDesktop(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    bundles: live.BundleBuildArtifacts,
    runner: PreparedRunner,
    stdout: anytype,
    stderr: anytype,
) !void {
    var server = try LiveServer.listen(allocator, "127.0.0.1", 42111, bundles.graph);
    defer server.deinit();
    try rewriteRunnerManifestPort(allocator, runner.manifest_path, server.port);

    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    const io = io_impl.io();
    var child = try std.process.spawn(io, .{
        .argv = &.{ runner.executable_path.?, runner.manifest_path },
        .cwd = .{ .path = std.fs.path.dirname(runner.manifest_path) orelse "." },
        .stdin = .ignore,
        .stdout = .inherit,
        .stderr = .inherit,
    });

    if (parsed.kill_after) {
        if (parsed.run_for_ns != 0) {
            try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(parsed.run_for_ns)), .awake);
        }
        child.kill(io);
        try stderr.print("live runner quit-after elapsed: {s}\n", .{runner.manifest_path});
        return;
    }

    var connection = try server.accept();
    defer connection.close();
    try connection.sendGraphAndBundles();
    const health_ok = try connection.waitForHealthMarkers(stdout, 30 * std.time.ns_per_s);
    if (!health_ok) {
        child.kill(io);
        return error.LiveHealthCheckFailed;
    }
    if (parsed.run_for_ns != 0) {
        try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(parsed.run_for_ns)), .awake);
    }
    _ = try child.wait(io);
    try stderr.print("live runner completed: {s}\n", .{runner.manifest_path});
}

fn runMacOSApp(
    allocator: std.mem.Allocator,
    parsed: ParsedArgs,
    bundles: live.BundleBuildArtifacts,
    runner: PreparedRunner,
    product_name: []const u8,
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

    var server = try LiveServer.listen(allocator, "127.0.0.1", 42111, bundles.graph);
    defer server.deinit();
    try rewriteRunnerManifestPort(allocator, runner.manifest_path, server.port);
    const manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, runner.manifest_path, allocator, .limited(1024 * 1024));
    try writeFile(bundled_manifest, manifest_text);

    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    const io = io_impl.io();
    var child = try std.process.spawn(io, .{
        .argv = &.{app_exec},
        .cwd = .{ .path = std.fs.path.dirname(app_exec) orelse "." },
        .stdin = .ignore,
        .stdout = .inherit,
        .stderr = .inherit,
    });

    if (parsed.kill_after) {
        if (parsed.run_for_ns != 0) {
            try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(parsed.run_for_ns)), .awake);
        }
        child.kill(io);
        try stderr.print("live runner quit-after elapsed: {s}\n", .{bundled_manifest});
        return;
    }

    var connection = try server.accept();
    defer connection.close();
    try connection.sendGraphAndBundles();
    const health_ok = try connection.waitForHealthMarkers(stdout, 30 * std.time.ns_per_s);
    if (!health_ok) {
        child.kill(io);
        return error.LiveHealthCheckFailed;
    }
    if (parsed.run_for_ns != 0) {
        try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(parsed.run_for_ns)), .awake);
    }
    _ = try child.wait(io);
    try stderr.print("live runner completed: {s}\n", .{bundled_manifest});
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
    const pbxproj_path = try std.fs.path.join(allocator, &.{ project_path, "project.pbxproj" });
    const derived_data_path = try std.fs.path.join(allocator, &.{ runner.runner_dir, "DerivedData" });
    try runToolWithDeveloperDir(allocator, developer_dir, &.{ "plutil", "-lint", pbxproj_path });
    try runToolWithDeveloperDir(allocator, developer_dir, &.{ "xcodebuild", "-list", "-project", project_path });
    try runToolWithDeveloperDir(allocator, developer_dir, &.{ "xcodebuild", "-showBuildSettings", "-project", project_path });
    switch (platform) {
        .macos => try runToolWithDeveloperDir(allocator, developer_dir, &.{
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
        .ios => try runToolWithDeveloperDir(allocator, developer_dir, &.{
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

    fn waitForHealthMarkers(self: *LiveConnection, stdout: anytype, timeout_ns: u64) !bool {
        const markers = [_][]const u8{
            "KIRA_LIVE_CONNECTED",
            "KIRA_BUNDLE_GRAPH_RECEIVED",
            "KIRA_BUNDLE_LOADED",
            "KIRA_BUNDLE_LINKED",
            "KIRA_ENTRYPOINT_STARTED",
            "KIRA_APP_RENDERED_FIRST_FRAME",
        };
        var seen = [_]bool{false} ** markers.len;
        const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
        while (elapsedSince(start) < timeout_ns) {
            if (!try waitReadable(self.stream.socket.handle, 250)) continue;
            const frame = try protocol.readFrame(self.allocator, &self.reader.interface);
            if (frame.kind == .log_line) {
                try stdout.print("{s}\n", .{frame.payload});
                for (markers, 0..) |marker, index| {
                    if (std.mem.eql(u8, frame.payload, marker)) seen[index] = true;
                }
                if (allSeen(seen)) return true;
            }
        }
        return false;
    }
};

fn waitReadable(fd: anytype, timeout_ms: i32) !bool {
    var pollfd = [_]std.posix.pollfd{.{
        .fd = fd,
        .events = std.posix.POLL.IN,
        .revents = 0,
    }};
    const ready = try std.posix.poll(&pollfd, timeout_ms);
    return ready > 0 and (pollfd[0].revents & std.posix.POLL.IN) != 0;
}

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
                const cwd = try std.process.currentPathAlloc(std.Options.debug_io, allocator);
                defer allocator.free(cwd);
                const relative = try std.fs.path.relative(allocator, cwd, null, root, child);
                try files.append(.{ .relative_path = relative, .bytes = bytes });
            },
            else => {},
        }
    }
}

fn allSeen(values: [6]bool) bool {
    for (values) |value| if (!value) return false;
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
    return null;
}

fn runnerSelector(allocator: std.mem.Allocator, kind: live.RunnerKind) !?native.TargetSelector {
    return switch (kind) {
        .desktop_dynamic_host, .xcode_macos => try native.TargetSelector.parse(allocator, switch (builtin.cpu.arch) {
            .aarch64 => "aarch64-macos-none",
            .x86_64 => "x86_64-macos-none",
            else => return error.UnsupportedTarget,
        }),
        .xcode_ios => try native.TargetSelector.parse(allocator, "aarch64-ios-none"),
    };
}

fn rewriteRunnerManifestPort(allocator: std.mem.Allocator, manifest_path: []const u8, port: u16) !void {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, manifest_path, allocator, .limited(1024 * 1024));
    var parsed = try model.RunnerManifest.parse(allocator, text);
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

fn runTool(allocator: std.mem.Allocator, argv: []const []const u8) !void {
    return runToolWithDeveloperDir(allocator, null, argv);
}

fn runToolWithDeveloperDir(allocator: std.mem.Allocator, developer_dir: ?[]const u8, argv: []const []const u8) !void {
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
    const base = std.fs.path.basename(target.target_root);
    const suffix = switch (kind) {
        .desktop_dynamic_host => "desktop",
        .xcode_macos => "macos",
        .xcode_ios => "ios",
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
