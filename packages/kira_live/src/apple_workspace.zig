const std = @import("std");
const builtin = @import("builtin");
const live = @import("root.zig");
const native = @import("kira_native_lib_definition");
const model = @import("model.zig");
const shared = @import("supervisor_shared.zig");
const pbx = @import("apple_pbxproj.zig");
const live_build_options = @import("kira_live_build_options");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const llvm_backend = @import("kira_llvm_backend");
const manifest = @import("kira_manifest");
const apple_app_sources = @import("apple_app_sources.zig");

pub const Platform = enum { macos, ios, tvos, visionos };

pub const Mode = enum { standalone, live };

// Apple Developer team used for automatic signing of device-capable targets.
// This must be a team Xcode recognizes under the signed-in Apple ID; the Apple
// Development certificate team (`security find-identity`) is not necessarily the
// same as the team Xcode uses for managed provisioning.
pub const default_development_team = "F3U5976KWH";

pub const Options = struct {
    apple_root: []const u8,
    mode: Mode = .standalone,
    server_host: []const u8 = "127.0.0.1",
    server_port: u16 = 0,
    platforms: []const Platform = &.{ .macos, .ios, .tvos, .visionos },
};

pub const Generated = struct {
    apple_root: []const u8,
    workspace_path: []const u8,
    project_path: []const u8,
    scheme_names: []const []const u8,
    unavailable: []const PlatformStatus,
    native_execution: bool,
};

pub const PlatformStatus = struct {
    platform: Platform,
    reason: []const u8,
};

const Arch = struct {
    native_selector: []const u8,
    zig_triple: []const u8,
    apple_sdk: []const u8,
    sdk_condition: ?[]const u8,
    label: []const u8,
};

const ArchResult = struct {
    // null for native (llvm) execution — there is no hybrid bundle, the whole
    // program is compiled to a native object that provides its own `main`.
    bundles: ?live.BundleBuildArtifacts,
    ldflags: []const u8,
    output_root: []const u8,
};

fn appExecutionIsNative(allocator: std.mem.Allocator, base_target: live.ResolvedLiveTarget) bool {
    const text = std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, base_target.validation_manifest_path, allocator, .limited(2 * 1024 * 1024)) catch return false;
    const parsed = manifest.parseProjectManifest(allocator, text) catch return false;
    return std.mem.eql(u8, parsed.execution_mode, "llvm") or std.mem.eql(u8, parsed.execution_mode, "llvm_native");
}

const PlatformMeta = struct {
    product_suffix: []const u8,
    bundle_id: []const u8,
    sdkroot: []const u8,
    supported_platforms: []const u8,
    deployment_key: []const u8,
    deployment_value: []const u8,
    device_family: ?[]const u8,
    plist_basename: []const u8,
};

fn platformMeta(platform: Platform) PlatformMeta {
    return switch (platform) {
        .macos => .{
            .product_suffix = "macOS",
            .bundle_id = "com.kira.live.dev",
            .sdkroot = "macosx",
            .supported_platforms = "macosx",
            .deployment_key = "MACOSX_DEPLOYMENT_TARGET",
            .deployment_value = "13.0",
            .device_family = null,
            .plist_basename = "macOS-Info.plist",
        },
        .ios => .{
            .product_suffix = "iOS",
            .bundle_id = "com.kira.live.dev",
            .sdkroot = "iphoneos",
            .supported_platforms = "iphoneos iphonesimulator",
            .deployment_key = "IPHONEOS_DEPLOYMENT_TARGET",
            .deployment_value = "17.0",
            .device_family = "1,2",
            .plist_basename = "iOS-Info.plist",
        },
        .tvos => .{
            .product_suffix = "tvOS",
            .bundle_id = "com.kira.live.dev",
            .sdkroot = "appletvos",
            .supported_platforms = "appletvos appletvsimulator",
            .deployment_key = "TVOS_DEPLOYMENT_TARGET",
            .deployment_value = "15.0",
            .device_family = "3",
            .plist_basename = "tvOS-Info.plist",
        },
        .visionos => .{
            .product_suffix = "visionOS",
            .bundle_id = "com.kira.live.dev",
            .sdkroot = "xros",
            .supported_platforms = "xros xrsimulator",
            .deployment_key = "XROS_DEPLOYMENT_TARGET",
            .deployment_value = "1.0",
            .device_family = "7",
            .plist_basename = "visionOS-Info.plist",
        },
    };
}

fn hostArch() []const u8 {
    return switch (builtin.cpu.arch) {
        .aarch64 => "arm64",
        .x86_64 => "x86_64",
        else => "arm64",
    };
}

fn macHostSelector() []const u8 {
    return switch (builtin.cpu.arch) {
        .aarch64 => "aarch64-macos-none",
        .x86_64 => "x86_64-macos-none",
        else => "aarch64-macos-none",
    };
}

fn macHostZigTriple() []const u8 {
    return switch (builtin.cpu.arch) {
        .aarch64 => "aarch64-macos-none",
        .x86_64 => "x86_64-macos-none",
        else => "aarch64-macos-none",
    };
}

fn archesFor(allocator: std.mem.Allocator, platform: Platform) ![]const Arch {
    return switch (platform) {
        .macos => try allocator.dupe(Arch, &.{
            .{ .native_selector = macHostSelector(), .zig_triple = macHostZigTriple(), .apple_sdk = "macosx", .sdk_condition = null, .label = "macos" },
        }),
        .ios => try allocator.dupe(Arch, &.{
            .{ .native_selector = "aarch64-ios-none", .zig_triple = "aarch64-ios-none", .apple_sdk = "iphoneos", .sdk_condition = "iphoneos*", .label = "ios-device" },
            .{ .native_selector = "aarch64-ios-simulator", .zig_triple = "aarch64-ios-simulator", .apple_sdk = "iphonesimulator", .sdk_condition = "iphonesimulator*", .label = "ios-sim" },
        }),
        .tvos => try allocator.dupe(Arch, &.{
            .{ .native_selector = "aarch64-tvos-none", .zig_triple = "aarch64-tvos-none", .apple_sdk = "appletvos", .sdk_condition = "appletvos*", .label = "tvos-device" },
            .{ .native_selector = "aarch64-tvos-simulator", .zig_triple = "aarch64-tvos-simulator", .apple_sdk = "appletvsimulator", .sdk_condition = "appletvsimulator*", .label = "tvos-sim" },
        }),
        .visionos => try allocator.dupe(Arch, &.{
            .{ .native_selector = "aarch64-xros-none", .zig_triple = "aarch64-visionos-none", .apple_sdk = "xros", .sdk_condition = "xros*", .label = "visionos-device" },
            .{ .native_selector = "aarch64-xros-simulator", .zig_triple = "aarch64-visionos-simulator", .apple_sdk = "xrsimulator", .sdk_condition = "xrsimulator*", .label = "visionos-sim" },
        }),
    };
}

pub fn generate(
    allocator: std.mem.Allocator,
    base_target: live.ResolvedLiveTarget,
    options: Options,
) !Generated {
    const apple_root = options.apple_root;
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, apple_root);
    const sources_dir = try std.fs.path.join(allocator, &.{ apple_root, "Sources" });
    const resources_dir = try std.fs.path.join(allocator, &.{ apple_root, "Resources" });
    const work_root = try std.fs.path.join(allocator, &.{ apple_root, "build" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, sources_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, resources_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, work_root);

    try shared.writeFile(try std.fs.path.join(allocator, &.{ sources_dir, "main.m" }), apple_app_sources.unifiedMainSource());
    // Native (llvm) targets compile this trivial TU so Xcode runs the linker; `main`
    // is provided by kira_native_app.o (OTHER_LDFLAGS). Both source files are always
    // written; only the one wired into the active target's Sources phase is compiled.
    try shared.writeFile(
        try std.fs.path.join(allocator, &.{ sources_dir, "native_link_stub.c" }),
        "/* Forces Xcode to link native (llvm) targets; main lives in kira_native_app.o. */\ntypedef int kira_native_link_stub_t;\n",
    );

    var specs = std.array_list.Managed(pbx.TargetSpec).init(allocator);
    var schemes = std.array_list.Managed([]const u8).init(allocator);
    var unavailable = std.array_list.Managed(PlatformStatus).init(allocator);
    var embed_source: ?ArchResult = null;
    var main_bundle_id: []const u8 = "";

    // Honor the project's execution_mode. "llvm" builds a whole-program native object
    // (its own `main` → Kira main → sapp_run) linked directly — no HybridRuntime, no
    // VM, no bytecode. Otherwise the hybrid-runner bundle path is used.
    const native_execution = appExecutionIsNative(allocator, base_target);

    // Standalone (exported) projects get a build phase that rebuilds the active
    // platform's Kira artifacts before linking, so editing Kira source and pressing
    // Build in Xcode regenerates the native object + embedded bundles for that SDK.
    // Live projects are driven by `kira live`, which rebuilds itself, so they don't.
    const rebuild_script: ?[]const u8 = if (options.mode == .standalone) blk: {
        const kira_exe = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
        break :blk try std.fmt.allocPrint(
            allocator,
            "set -e\nif [ \"${{PLATFORM_NAME}}\" != \"\" ]; then \"{s}\" export apple \"{s}\" --xcode-rebuild \"${{PLATFORM_NAME}}\"; fi\n",
            .{ kira_exe, base_target.target_root },
        );
    } else null;

    for (options.platforms) |platform| {
        const meta = platformMeta(platform);
        const arches = try archesFor(allocator, platform);
        const product_name = try std.fmt.allocPrint(allocator, "KiraApp-{s}", .{meta.product_suffix});

        var blocks = std.array_list.Managed(pbx.LdflagsBlock).init(allocator);
        var platform_failed: ?[]const u8 = null;
        for (arches) |arch| {
            const result = (if (native_execution)
                buildNativeArch(allocator, base_target, work_root, arch)
            else
                buildArch(allocator, base_target, work_root, arch)) catch |err| {
                platform_failed = try std.fmt.allocPrint(allocator, "arch {s} failed: {s}", .{ arch.label, @errorName(err) });
                break;
            };
            if (embed_source == null) embed_source = result;
            if (main_bundle_id.len == 0) {
                if (result.bundles) |b| main_bundle_id = b.graph.main_bundle_id;
            }
            try blocks.append(.{ .sdk_condition = arch.sdk_condition, .value = result.ldflags });
        }

        // Per-platform Info.plist.
        try shared.writeFile(
            try std.fs.path.join(allocator, &.{ resources_dir, meta.plist_basename }),
            try apple_app_sources.infoPlist(allocator, platform, product_name, meta.bundle_id),
        );

        try specs.append(.{
            .product_name = product_name,
            .bundle_id = meta.bundle_id,
            .info_plist_rel = try std.fmt.allocPrint(allocator, "Resources/{s}", .{meta.plist_basename}),
            .sdkroot = meta.sdkroot,
            .supported_platforms = meta.supported_platforms,
            .deployment_key = meta.deployment_key,
            .deployment_value = meta.deployment_value,
            .device_family = meta.device_family,
            .archs = if (platform == .macos) hostArch() else "arm64",
            .ldflags_blocks = try blocks.toOwnedSlice(),
            .development_team = default_development_team,
            .unavailable_reason = platform_failed,
            .rebuild_script = if (platform_failed == null) rebuild_script else null,
            .native_entry = native_execution,
        });
        try schemes.append(product_name);
        if (platform_failed) |reason| try unavailable.append(.{ .platform = platform, .reason = reason });
    }

    if (embed_source == null) return error.NoApplePlatformBuilt;

    // Native (llvm) targets link a self-contained native object with its own `main`,
    // so there's no bytecode to embed and no runner manifest to write.
    if (!native_execution) {
        // Embed bytecode bundles (arch-independent) once.
        try embedBytecodeBundles(allocator, resources_dir, embed_source.?.output_root);

        // KiraRunner.toml with the mode flag — the single thing that differs export vs live.
        try writeRunnerManifest(allocator, resources_dir, base_target, options, main_bundle_id, embed_source.?.bundles.?.native_contract_hash);
    }

    // project.pbxproj
    const project_dir = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcodeproj" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, project_dir);
    const project_path = try std.fs.path.join(allocator, &.{ project_dir, "project.pbxproj" });
    try shared.writeFile(project_path, try pbx.render(allocator, specs.items));

    // Shared schemes so `xcodebuild -scheme` and Xcode's scheme picker work.
    const schemes_dir = try std.fs.path.join(allocator, &.{ project_dir, "xcshareddata", "xcschemes" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, schemes_dir);
    for (schemes.items, 0..) |scheme_name, index| {
        try shared.writeFile(
            try std.fs.path.join(allocator, &.{ schemes_dir, try std.fmt.allocPrint(allocator, "{s}.xcscheme", .{scheme_name}) }),
            try pbx.schemeXml(allocator, scheme_name, index),
        );
    }

    // Workspace wrapping the single project.
    const workspace_dir = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcworkspace" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, workspace_dir);
    try shared.writeFile(try std.fs.path.join(allocator, &.{ workspace_dir, "contents.xcworkspacedata" }),
        \\<?xml version="1.0" encoding="UTF-8"?>
        \\<Workspace version="1.0">
        \\  <FileRef location="group:KiraApp.xcodeproj"></FileRef>
        \\</Workspace>
        \\
    );

    return .{
        .apple_root = apple_root,
        .workspace_path = workspace_dir,
        .project_path = project_dir,
        .scheme_names = try schemes.toOwnedSlice(),
        .unavailable = try unavailable.toOwnedSlice(),
        .native_execution = native_execution,
    };
}

// Rebuild only the Kira artifacts for the SDK Xcode is currently building, into the
// same deterministic paths the generated project links against. Invoked by the
// per-target Run Script build phase with $PLATFORM_NAME.
pub fn rebuildPlatform(
    allocator: std.mem.Allocator,
    base_target: live.ResolvedLiveTarget,
    apple_root: []const u8,
    platform_name: []const u8,
) !void {
    const resources_dir = try std.fs.path.join(allocator, &.{ apple_root, "Resources" });
    const work_root = try std.fs.path.join(allocator, &.{ apple_root, "build" });
    const arch = try archForPlatformName(allocator, platform_name);
    if (appExecutionIsNative(allocator, base_target)) {
        _ = try buildNativeArch(allocator, base_target, work_root, arch);
        return;
    }
    const result = try buildArch(allocator, base_target, work_root, arch);
    try embedBytecodeBundles(allocator, resources_dir, result.output_root);
}

fn archForPlatformName(allocator: std.mem.Allocator, platform_name: []const u8) !Arch {
    inline for (.{ Platform.macos, Platform.ios, Platform.tvos, Platform.visionos }) |platform| {
        const arches = try archesFor(allocator, platform);
        for (arches) |arch| {
            if (std.mem.eql(u8, arch.apple_sdk, platform_name)) return arch;
        }
    }
    return error.UnsupportedTarget;
}

// Native (llvm) per-arch build: compile the whole program to a single native object
// (which provides its own `main`) for the arch, and link it directly. No VM bundle.
fn buildNativeArch(allocator: std.mem.Allocator, base_target: live.ResolvedLiveTarget, work_root: []const u8, arch: Arch) !ArchResult {
    const selector = try native.TargetSelector.parse(allocator, arch.native_selector);
    const arch_out = try std.fs.path.join(allocator, &.{ work_root, arch.label });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, arch_out);
    const compiled = try build.compileFileForBackendWithSelector(allocator, base_target.validation_entrypoint_path, .llvm_native, selector, &.{});
    if (compiled.failed() or compiled.verified_program == null) return error.LiveBundleBuildFailed;
    const object_dir = try std.fs.path.join(allocator, &.{ arch_out, "native" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, object_dir);
    const object_path = try std.fs.path.join(allocator, &.{ object_dir, "kira_native_app.o" });
    _ = llvm_backend.compile(allocator, .{
        .mode = .llvm_native,
        .program = &compiled.verified_program.?,
        .module_name = std.fs.path.stem(base_target.validation_entrypoint_path),
        .emit = .{ .object_path = object_path },
        .target_selector = selector,
        .resolved_native_libraries = compiled.native_libraries,
    }) catch return error.LiveBundleBuildFailed;
    // The native object calls Kira runtime bridge helpers (state alloc/recover, first
    // frame) defined in runtime_helpers.c. Compile them for this arch and link them in,
    // mirroring linkExecutable's whole-program native path.
    const helper_object = llvm_backend.link.buildRuntimeHelpersObject(allocator, object_path, false, selector) catch return error.LiveBundleBuildFailed;
    const ldflags = try buildNativeLdflags(allocator, object_path, helper_object, compiled.native_libraries);
    return .{ .bundles = null, .ldflags = ldflags, .output_root = arch_out };
}

fn buildNativeLdflags(
    allocator: std.mem.Allocator,
    object_path: []const u8,
    helper_object: []const u8,
    native_libraries: []const native.ResolvedNativeLibrary,
) ![]const u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    try out.appendSlice("\"");
    try out.appendSlice(object_path);
    try out.appendSlice("\", \"");
    try out.appendSlice(helper_object);
    try out.appendSlice("\"");
    for (native_libraries) |library| {
        try out.appendSlice(", \"");
        try out.appendSlice(library.artifact_path);
        try out.appendSlice("\"");
        for (library.link.frameworks) |framework| {
            try out.appendSlice(", \"-framework\", \"");
            try out.appendSlice(framework);
            try out.appendSlice("\"");
        }
        for (library.link.system_libs) |system_lib| {
            try out.appendSlice(", \"-l");
            try out.appendSlice(system_lib);
            try out.appendSlice("\"");
        }
    }
    return out.toOwnedSlice();
}

fn buildArch(allocator: std.mem.Allocator, base_target: live.ResolvedLiveTarget, work_root: []const u8, arch: Arch) !ArchResult {
    var arch_target = base_target;
    arch_target.output_root = try std.fs.path.join(allocator, &.{ work_root, arch.label });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, arch_target.output_root);
    const selector = try native.TargetSelector.parse(allocator, arch.native_selector);
    const bundles = try live.buildBundles(allocator, arch_target, selector, true);
    const support_lib = try buildSupportArchive(allocator, work_root, arch);
    const ldflags = try buildLdflags(allocator, support_lib, bundles.main_native_object_path, bundles.main_native_libraries);
    return .{ .bundles = bundles, .ldflags = ldflags, .output_root = arch_target.output_root };
}

fn buildSupportArchive(allocator: std.mem.Allocator, work_root: []const u8, arch: Arch) ![]const u8 {
    const runner_build_root = try shared.resolveLiveRunnerBuildRoot(allocator);
    const sdk_capture = try shared.runToolCapture(allocator, &.{ "xcrun", "--sdk", arch.apple_sdk, "--show-sdk-path" });
    defer allocator.free(sdk_capture);
    const sdk_path = std.mem.trim(u8, sdk_capture, " \t\r\n");
    try shared.runToolInCwd(allocator, runner_build_root, &.{
        live_build_options.zig_exe,
        "build",
        "live-runner-support",
        "-Doptimize=ReleaseFast",
        try std.fmt.allocPrint(allocator, "-Dtarget={s}", .{arch.zig_triple}),
        try std.fmt.allocPrint(allocator, "-Dapple-sdk={s}", .{sdk_path}),
    });
    const source_archive = try std.fs.path.join(allocator, &.{ runner_build_root, "zig-out", "lib", "libkira_live_runner_support.a" });
    const support_dir = try std.fs.path.join(allocator, &.{ work_root, "support", arch.label });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, support_dir);
    const dest_archive = try std.fs.path.join(allocator, &.{ support_dir, "libkira_live_runner_support_xcode.a" });
    const object_dir = try std.fs.path.join(allocator, &.{ support_dir, "support-objects" });
    _ = std.Io.Dir.cwd().deleteTree(std.Options.debug_io, object_dir) catch {};
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, object_dir);
    try shared.runToolInCwd(allocator, object_dir, &.{ "/usr/bin/ar", "-x", source_archive });

    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.appendSlice(&.{ "/usr/bin/libtool", "-static", "-o", dest_archive });
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, object_dir, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file or !std.mem.endsWith(u8, entry.name, ".o")) continue;
        const object_path = try std.fs.path.join(allocator, &.{ object_dir, entry.name });
        try shared.runTool(allocator, &.{ "/bin/chmod", "0644", object_path });
        try argv.append(object_path);
    }
    if (argv.items.len == 4) return error.ExternalCommandFailed;
    try shared.runTool(allocator, argv.items);
    return dest_archive;
}

fn buildLdflags(
    allocator: std.mem.Allocator,
    support_lib: []const u8,
    native_object: []const u8,
    native_libraries: []const native.ResolvedNativeLibrary,
) ![]const u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    try out.appendSlice("\"-Wl,-force_load,");
    try out.appendSlice(support_lib);
    try out.appendSlice("\", \"");
    try out.appendSlice(native_object);
    try out.appendSlice("\"");
    for (native_libraries) |library| {
        try out.appendSlice(", \"");
        try out.appendSlice(library.artifact_path);
        try out.appendSlice("\"");
        for (library.link.frameworks) |framework| {
            try out.appendSlice(", \"-framework\", \"");
            try out.appendSlice(framework);
            try out.appendSlice("\"");
        }
        for (library.link.system_libs) |system_lib| {
            try out.appendSlice(", \"-l");
            try out.appendSlice(system_lib);
            try out.appendSlice("\"");
        }
    }
    try out.appendSlice(", \"-Wl,-export_dynamic\"");
    return out.toOwnedSlice();
}

fn embedBytecodeBundles(allocator: std.mem.Allocator, resources_dir: []const u8, arch_output_root: []const u8) !void {
    const dest = try std.fs.path.join(allocator, &.{ resources_dir, "Bundles" });
    _ = std.Io.Dir.cwd().deleteTree(std.Options.debug_io, dest) catch {};
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, dest);
    const source = try std.fs.path.join(allocator, &.{ arch_output_root, "bundles" });
    var source_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, source, .{ .iterate = true });
    defer source_dir.close(std.Options.debug_io);
    var iter = source_dir.iterate();
    while (try iter.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .directory or !std.mem.endsWith(u8, entry.name, ".klbundle")) continue;
        try copyTree(
            allocator,
            try std.fs.path.join(allocator, &.{ source, entry.name }),
            try std.fs.path.join(allocator, &.{ dest, entry.name }),
        );
    }
}

fn copyTree(allocator: std.mem.Allocator, source: []const u8, dest: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, dest);
    var src_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, source, .{ .iterate = true });
    defer src_dir.close(std.Options.debug_io);
    var iterator = src_dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        const child_src = try std.fs.path.join(allocator, &.{ source, entry.name });
        const child_dst = try std.fs.path.join(allocator, &.{ dest, entry.name });
        switch (entry.kind) {
            .directory => try copyTree(allocator, child_src, child_dst),
            .file => {
                const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, child_src, allocator, .limited(64 * 1024 * 1024));
                defer allocator.free(bytes);
                try shared.writeFile(child_dst, bytes);
            },
            else => {},
        }
    }
}

fn writeRunnerManifest(
    allocator: std.mem.Allocator,
    resources_dir: []const u8,
    base_target: live.ResolvedLiveTarget,
    options: Options,
    main_bundle_id: []const u8,
    native_contract_hash: []const u8,
) !void {
    const runner_manifest = model.RunnerManifest{
        .kind = .xcode_ios,
        .name = base_target.runner_display_name,
        .bundle_id = "com.kira.live.dev",
        .version = "0.1.0",
        .target_path = base_target.target_root,
        .package_name = base_target.target_package_name,
        .validation_app_path = base_target.validation_app_root,
        .bundles_path = "Bundles",
        .local_cache_path = "app-support/KiraExport",
        .main_bundle_id = main_bundle_id,
        .server_host = options.server_host,
        .server_port = options.server_port,
        .native_contract_hash = native_contract_hash,
        .runtime_mode = switch (options.mode) {
            .standalone => .standalone,
            .live => .live,
        },
        .embedded_bundles_path = "Bundles",
    };
    try shared.writeTomlFile(try std.fs.path.join(allocator, &.{ resources_dir, "KiraRunner.toml" }), runner_manifest);
}

test "platform metadata covers every Apple platform" {
    inline for (.{ Platform.macos, Platform.ios, Platform.tvos, Platform.visionos }) |p| {
        const meta = platformMeta(p);
        try std.testing.expect(meta.product_suffix.len > 0);
        try std.testing.expect(meta.sdkroot.len > 0);
    }
}

test "visionOS arch uses zig visionos triple but xros clang selector" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const arches = try archesFor(arena.allocator(), .visionos);
    try std.testing.expectEqualStrings("aarch64-xros-none", arches[0].native_selector);
    try std.testing.expectEqualStrings("aarch64-visionos-none", arches[0].zig_triple);
    try std.testing.expectEqualStrings("xros", arches[0].apple_sdk);
}
