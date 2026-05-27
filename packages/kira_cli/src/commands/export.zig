const std = @import("std");
const diag_messages = @import("kira_diagnostic_messages");
const manifest = @import("kira_manifest");
const kira_project = @import("kira_project");
const kira_toolchain = @import("kira_toolchain");
const kira_wasm_runtime = @import("kira_wasm_runtime");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
    const target = kira_project.resolveTargetFromPath(allocator, parsed.input_path) catch |err| switch (err) {
        error.InvalidProjectPath => {
            try support.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.invalidProjectPath(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        error.ProjectManifestNotFound => {
            try support.renderStandaloneDiagnostic(stderr, try diag_messages.PackageMessages.missingProjectManifest(allocator, parsed.input_path));
            return error.CommandFailed;
        },
        else => return err,
    };
    const root = target.root_path orelse std.fs.path.dirname(target.source_path orelse ".") orelse ".";
    const project_name = target.project_name orelse "KiraApp";
    const exports_root = try std.fs.path.join(allocator, &.{ root, "exports" });
    const selected_app_path = try allocator.dupe(u8, root);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, exports_root);

    switch (parsed.family) {
        .apple => try exportApple(allocator, stdout, exports_root, project_name, selected_app_path, null),
        .macos => try exportApple(allocator, stdout, exports_root, project_name, selected_app_path, .macos),
        .ios => try exportApple(allocator, stdout, exports_root, project_name, selected_app_path, .ios),
        .tvos => try exportApple(allocator, stdout, exports_root, project_name, selected_app_path, .tvos),
        .visionos => try exportApple(allocator, stdout, exports_root, project_name, selected_app_path, .visionos),
        .windows => try exportWindows(allocator, stdout, stderr, exports_root, project_name),
        .android => try exportAndroid(allocator, stdout, stderr, exports_root, project_name, selected_app_path),
        .web => try exportWeb(allocator, stdout, stderr, exports_root, project_name, parsed.surface),
        .linux => try exportLinux(allocator, stdout, stderr, exports_root, project_name),
    }
}

const ParsedArgs = struct {
    family: manifest.ExportFamily,
    input_path: []const u8 = ".",
    profile: manifest.BuildProfile = .debug,
    surface: manifest.WebSurface = .dom,
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    if (args.len == 0) return error.InvalidArguments;
    var parsed = ParsedArgs{ .family = manifest.ExportFamily.parse(args[0]) orelse return error.InvalidArguments };
    var input_path: ?[]const u8 = null;
    var index: usize = 1;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--profile")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.profile = manifest.BuildProfile.parse(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--surface")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            parsed.surface = manifest.WebSurface.parse(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return error.InvalidArguments;
        if (input_path != null) return error.InvalidArguments;
        input_path = arg;
    }
    parsed.input_path = input_path orelse ".";
    return parsed;
}

fn exportApple(
    allocator: std.mem.Allocator,
    stdout: anytype,
    exports_root: []const u8,
    project_name: []const u8,
    selected_app_path: []const u8,
    focus: ?manifest.ApplePlatform,
) !void {
    const apple_root = try std.fs.path.join(allocator, &.{ exports_root, "apple" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, apple_root);
    const workspace = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcworkspace" });
    const project = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcodeproj" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, workspace);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, project);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraRuntime" }));
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraLiveClient" }));
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraBundleLoader" }));
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ apple_root, "Shared", "Assets.xcassets" }));

    try writeTextFile(try std.fs.path.join(allocator, &.{ workspace, "contents.xcworkspacedata" }),
        \\<?xml version="1.0" encoding="UTF-8"?>
        \\<Workspace version="1.0">
        \\  <FileRef location="group:KiraApp.xcodeproj"></FileRef>
        \\</Workspace>
        \\
    );
    try writeTextFile(try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraRuntime", "main.m" }), appleMainSource());
    try writeTextFile(try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraRuntime", "KiraRunner.toml" }), try runnerConfigToml(allocator, "apple", project_name, selected_app_path));
    try writeTextFile(try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraLiveClient", "KiraLiveClient.swift" }), appleSwiftSource());
    try writeTextFile(try std.fs.path.join(allocator, &.{ apple_root, "Shared", "KiraBundleLoader", "KiraBundleLoader.swift" }), appleBundleLoaderSource());

    const platforms = [_]manifest.ApplePlatform{ .macos, .ios, .tvos, .visionos };
    for (platforms) |platform| {
        const dir_name = applePlatformDir(platform);
        const platform_dir = try std.fs.path.join(allocator, &.{ apple_root, dir_name });
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, platform_dir);
        try writeTextFile(try std.fs.path.join(allocator, &.{ platform_dir, "Info.plist" }), try appleInfoPlist(allocator, platform, project_name));
        try writeTextFile(try std.fs.path.join(allocator, &.{ platform_dir, "Entitlements.plist" }), entitlementsPlist());
    }
    try writeTextFile(try std.fs.path.join(allocator, &.{ project, "project.pbxproj" }), try applePbxproj(allocator, project_name));
    try writeAppleSchemes(allocator, apple_root);
    if (focus) |platform| {
        try stdout.print("exported {s} Apple target at {s}\n", .{ platform.label(), apple_root });
    } else {
        try stdout.print("exported merged Apple workspace at {s}\n", .{apple_root});
    }
}

fn exportWindows(allocator: std.mem.Allocator, stdout: anytype, stderr: anytype, exports_root: []const u8, project_name: []const u8) !void {
    const root = try std.fs.path.join(allocator, &.{ exports_root, "windows" });
    try writeCmakeScaffold(allocator, root, project_name, "windows");
    try stdout.print("exported Windows Visual Studio/CMake project at {s}\n", .{root});
    if (!commandExists(allocator, "cmake")) {
        try support.renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingVisualStudioTools(allocator, "`cmake` was not found on PATH in this environment."));
    }
}

fn exportLinux(allocator: std.mem.Allocator, stdout: anytype, stderr: anytype, exports_root: []const u8, project_name: []const u8) !void {
    const root = try std.fs.path.join(allocator, &.{ exports_root, "linux" });
    try writeCmakeScaffold(allocator, root, project_name, "linux");
    try stdout.print("exported Linux CMake/Ninja project at {s}\n", .{root});
    if (!commandExists(allocator, "cmake") or !commandExists(allocator, "ninja")) {
        try support.renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingLinuxBuildTools(allocator, "`cmake` and `ninja` should both be available for a full local Linux export build."));
    }
}

fn exportAndroid(allocator: std.mem.Allocator, stdout: anytype, stderr: anytype, exports_root: []const u8, project_name: []const u8, selected_app_path: []const u8) !void {
    const root = try std.fs.path.join(allocator, &.{ exports_root, "android" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "java", "com", "kira", "app" }));
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "assets" }));
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "res", "values" }));
    const application_id = try androidApplicationId(allocator, project_name);
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "settings.gradle" }), "pluginManagement { repositories { google(); mavenCentral(); gradlePluginPortal() } }\ndependencyResolutionManagement { repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS); repositories { google(); mavenCentral() } }\nrootProject.name = 'KiraApp'\ninclude ':app'\n");
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "build.gradle" }), "plugins {\n    id 'com.android.application' version '8.7.3' apply false\n}\n");
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "app", "build.gradle" }), try std.fmt.allocPrint(allocator, "plugins {{ id 'com.android.application' }}\n\nandroid {{ namespace 'com.kira.app'; compileSdk 35\n    defaultConfig {{ applicationId '{s}'; minSdk 26; targetSdk 35; versionCode 1; versionName '0.1.0' }}\n    compileOptions {{ sourceCompatibility JavaVersion.VERSION_17; targetCompatibility JavaVersion.VERSION_17 }}\n}}\n", .{application_id}));
    if (try androidSdkRoot(allocator)) |sdk_root| {
        defer allocator.free(sdk_root);
        try writeTextFile(try std.fs.path.join(allocator, &.{ root, "local.properties" }), try std.fmt.allocPrint(allocator, "sdk.dir={s}\n", .{sdk_root}));
    }
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "AndroidManifest.xml" }), "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\"><application android:theme=\"@style/AppTheme\" android:label=\"KiraApp\"><activity android:name=\"com.kira.app.MainActivity\" android:exported=\"true\"><intent-filter><action android:name=\"android.intent.action.MAIN\"/><category android:name=\"android.intent.category.LAUNCHER\"/></intent-filter></activity></application></manifest>\n");
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "java", "com", "kira", "app", "MainActivity.java" }), androidMainSource());
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "assets", "KiraRunner.toml" }), try runnerConfigToml(allocator, "android", project_name, selected_app_path));
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "app", "src", "main", "res", "values", "styles.xml" }), "<resources><style name=\"AppTheme\" parent=\"android:style/Theme.Material.Light.NoActionBar\"/></resources>\n");
    try stdout.print("exported Android Gradle runner project at {s}\n", .{root});
    if (!commandExists(allocator, "sdkmanager") and !commandExists(allocator, "adb")) {
        try support.renderStandaloneDiagnostic(stderr, try diag_messages.ToolchainMessages.missingAndroidSdk(allocator, "Android Studio installation is intentionally not automated."));
    }
}

fn exportWeb(allocator: std.mem.Allocator, stdout: anytype, stderr: anytype, exports_root: []const u8, project_name: []const u8, surface: manifest.WebSurface) !void {
    if (surface == .hybrid) {
        try support.renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.exportNotImplemented(allocator, surface.label(), "The hybrid web surface is modeled, but it still needs a browser VM/native boundary runner."));
        return error.CommandFailed;
    }
    const requirements = manifest.webSurfaceRequirements(surface);
    const root = try std.fs.path.join(allocator, &.{ exports_root, "web" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, root);
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "index.html" }), try webIndex(allocator, project_name, surface));
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "kira-browser-ffi.generated.js" }), webGeneratedFfiJs());
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "kira-wasm.js" }), webRuntimeJs(surface));
    const wasm = try kira_wasm_runtime.buildModule(allocator, .{ .app_name = project_name, .surface = surface.label() });
    if (!kira_wasm_runtime.validateModule(wasm) or kira_wasm_runtime.isHeaderOnly(wasm)) return error.InvalidWasmArtifact;
    try writeBytesFile(try std.fs.path.join(allocator, &.{ root, "kira-app.wasm" }), wasm);
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "manifest.json" }), try webManifestJson(allocator, project_name, requirements));
    try stdout.print("exported Kira Wasm {s} runtime at {s}\n", .{ surface.label(), root });
}

fn writeCmakeScaffold(allocator: std.mem.Allocator, root: []const u8, project_name: []const u8, platform: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, try std.fs.path.join(allocator, &.{ root, "src" }));
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "CMakeLists.txt" }), try std.fmt.allocPrint(allocator,
        \\cmake_minimum_required(VERSION 3.25)
        \\project({s}_kira_{s} C)
        \\add_executable(KiraApp src/main.c)
        \\
    , .{ try safeIdentifier(allocator, project_name), platform }));
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "CMakePresets.json" }),
        \\{"version":6,"configurePresets":[{"name":"debug","generator":"Ninja","binaryDir":"build/debug","cacheVariables":{"CMAKE_BUILD_TYPE":"Debug"}},{"name":"release","generator":"Ninja","binaryDir":"build/release","cacheVariables":{"CMAKE_BUILD_TYPE":"Release"}}]}
        \\
    );
    try writeTextFile(try std.fs.path.join(allocator, &.{ root, "src", "main.c" }), "#include <stdio.h>\nint main(void) { puts(\"Kira platform export host\"); return 0; }\n");
}

fn writeAppleSchemes(allocator: std.mem.Allocator, apple_root: []const u8) !void {
    const schemes_root = try std.fs.path.join(allocator, &.{ apple_root, "KiraApp.xcodeproj", "xcshareddata", "xcschemes" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, schemes_root);
    const platforms = [_][]const u8{ "macOS", "iOS", "tvOS", "visionOS" };
    const profiles = [_][]const u8{ "Debug", "Profiler", "Release" };
    for (platforms) |platform| {
        for (profiles) |profile| {
            const name = try std.fmt.allocPrint(allocator, "KiraApp-{s}-{s}", .{ platform, profile });
            try writeTextFile(try std.fs.path.join(allocator, &.{ schemes_root, try std.fmt.allocPrint(allocator, "{s}.xcscheme", .{name}) }), try schemeXml(allocator, name, profile));
        }
    }
}

fn schemeXml(allocator: std.mem.Allocator, name: []const u8, profile: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator,
        \\<?xml version="1.0" encoding="UTF-8"?>
        \\<Scheme LastUpgradeVersion="1600" version="1.7">
        \\  <BuildAction parallelizeBuildables="YES" buildImplicitDependencies="YES">
        \\    <BuildActionEntries><BuildActionEntry buildForTesting="YES" buildForRunning="YES" buildForProfiling="YES" buildForArchiving="YES" buildForAnalyzing="YES"><BuildableReference BuildableIdentifier="primary" BlueprintIdentifier="{s}" BuildableName="{s}.app" BlueprintName="{s}" ReferencedContainer="container:KiraApp.xcodeproj"></BuildableReference></BuildActionEntry></BuildActionEntries>
        \\  </BuildAction>
        \\  <LaunchAction buildConfiguration="{s}"></LaunchAction>
        \\</Scheme>
        \\
    , .{ name, name, name, profile });
}

fn applePbxproj(allocator: std.mem.Allocator, project_name: []const u8) ![]const u8 {
    _ = project_name;
    return std.fmt.allocPrint(allocator,
        \\// !$*UTF8*$!
        \\{{
        \\archiveVersion = 1;
        \\classes = {{}};
        \\objectVersion = 56;
        \\objects = {{
        \\A1 = {{isa = PBXProject; buildConfigurationList = C0; compatibilityVersion = "Xcode 14.0"; developmentRegion = en; hasScannedForEncodings = 0; knownRegions = (en, Base, ); mainGroup = A2; productRefGroup = A3; projectDirPath = ""; projectRoot = ""; targets = (TmacDebug, TiosDebug, TtvosDebug, TvisionDebug, ); }};
        \\A2 = {{isa = PBXGroup; children = (A3, FMain, ); sourceTree = "<group>"; }};
        \\A3 = {{isa = PBXGroup; children = (); name = Products; sourceTree = "<group>"; }};
        \\FMain = {{isa = PBXFileReference; lastKnownFileType = sourcecode.c.objc; path = Shared/KiraRuntime/main.m; sourceTree = "<group>"; }};
        \\BFMac = {{isa = PBXBuildFile; fileRef = FMain; }};
        \\BFIos = {{isa = PBXBuildFile; fileRef = FMain; }};
        \\BFTvos = {{isa = PBXBuildFile; fileRef = FMain; }};
        \\BFVision = {{isa = PBXBuildFile; fileRef = FMain; }};
        \\SMac = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (BFMac, ); runOnlyForDeploymentPostprocessing = 0; }};
        \\SIos = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (BFIos, ); runOnlyForDeploymentPostprocessing = 0; }};
        \\STvos = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (BFTvos, ); runOnlyForDeploymentPostprocessing = 0; }};
        \\SVision = {{isa = PBXSourcesBuildPhase; buildActionMask = 2147483647; files = (BFVision, ); runOnlyForDeploymentPostprocessing = 0; }};
        \\TmacDebug = {{isa = PBXNativeTarget; buildConfigurationList = C1; buildPhases = (SMac, ); buildRules = (); dependencies = (); name = "KiraApp-macOS-Debug"; productName = "KiraApp-macOS-Debug"; productType = "com.apple.product-type.application"; }};
        \\TiosDebug = {{isa = PBXNativeTarget; buildConfigurationList = C2; buildPhases = (SIos, ); buildRules = (); dependencies = (); name = "KiraApp-iOS-Debug"; productName = "KiraApp-iOS-Debug"; productType = "com.apple.product-type.application"; }};
        \\TtvosDebug = {{isa = PBXNativeTarget; buildConfigurationList = C3; buildPhases = (STvos, ); buildRules = (); dependencies = (); name = "KiraApp-tvOS-Debug"; productName = "KiraApp-tvOS-Debug"; productType = "com.apple.product-type.application"; }};
        \\TvisionDebug = {{isa = PBXNativeTarget; buildConfigurationList = C4; buildPhases = (SVision, ); buildRules = (); dependencies = (); name = "KiraApp-visionOS-Debug"; productName = "KiraApp-visionOS-Debug"; productType = "com.apple.product-type.application"; }};
        \\C0 = {{isa = XCConfigurationList; buildConfigurations = (PDebug, PProfiler, PRelease, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; }};
        \\C1 = {{isa = XCConfigurationList; buildConfigurations = (MDebug, MProfiler, MRelease, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; }};
        \\C2 = {{isa = XCConfigurationList; buildConfigurations = (IDebug, IProfiler, IRelease, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; }};
        \\C3 = {{isa = XCConfigurationList; buildConfigurations = (TDebug, TProfiler, TRelease, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; }};
        \\C4 = {{isa = XCConfigurationList; buildConfigurations = (VDebug, VProfiler, VRelease, ); defaultConfigurationIsVisible = 0; defaultConfigurationName = Debug; }};
        \\PDebug = {{isa = XCBuildConfiguration; buildSettings = {{}}; name = Debug; }};
        \\PProfiler = {{isa = XCBuildConfiguration; buildSettings = {{}}; name = Profiler; }};
        \\PRelease = {{isa = XCBuildConfiguration; buildSettings = {{}}; name = Release; }};
        \\MDebug = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-macOS-Debug"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.macos.debug"; SDKROOT = macosx; INFOPLIST_FILE = macOS/Info.plist; CODE_SIGNING_ALLOWED = NO;}}; name = Debug; }};
        \\MProfiler = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-macOS-Profiler"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.macos.profiler"; SDKROOT = macosx; INFOPLIST_FILE = macOS/Info.plist; CODE_SIGNING_ALLOWED = NO;}}; name = Profiler; }};
        \\MRelease = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-macOS-Release"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.macos.release"; SDKROOT = macosx; INFOPLIST_FILE = macOS/Info.plist; CODE_SIGNING_ALLOWED = NO;}}; name = Release; }};
        \\IDebug = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-iOS-Debug"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.live.dev"; SDKROOT = iphoneos; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; TARGETED_DEVICE_FAMILY = "1,2"; INFOPLIST_FILE = iOS/Info.plist; OTHER_LDFLAGS = "-framework UIKit"; ALWAYS_SEARCH_USER_PATHS = NO; DEVELOPMENT_TEAM = AKD4RFY7LU; CODE_SIGN_STYLE = Automatic; CODE_SIGNING_ALLOWED = YES;}}; name = Debug; }};
        \\IProfiler = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-iOS-Profiler"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.live.dev.profiler"; SDKROOT = iphoneos; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; TARGETED_DEVICE_FAMILY = "1,2"; INFOPLIST_FILE = iOS/Info.plist; OTHER_LDFLAGS = "-framework UIKit"; ALWAYS_SEARCH_USER_PATHS = NO; DEVELOPMENT_TEAM = AKD4RFY7LU; CODE_SIGN_STYLE = Automatic; CODE_SIGNING_ALLOWED = YES;}}; name = Profiler; }};
        \\IRelease = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-iOS-Release"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.live.dev.release"; SDKROOT = iphoneos; SUPPORTED_PLATFORMS = "iphoneos iphonesimulator"; TARGETED_DEVICE_FAMILY = "1,2"; INFOPLIST_FILE = iOS/Info.plist; OTHER_LDFLAGS = "-framework UIKit"; ALWAYS_SEARCH_USER_PATHS = NO; DEVELOPMENT_TEAM = AKD4RFY7LU; CODE_SIGN_STYLE = Automatic; CODE_SIGNING_ALLOWED = YES;}}; name = Release; }};
        \\TDebug = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-tvOS-Debug"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.tvos.debug"; SDKROOT = appletvos; INFOPLIST_FILE = tvOS/Info.plist; CODE_SIGNING_ALLOWED = YES;}}; name = Debug; }};
        \\TProfiler = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-tvOS-Profiler"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.tvos.profiler"; SDKROOT = appletvos; INFOPLIST_FILE = tvOS/Info.plist; CODE_SIGNING_ALLOWED = YES;}}; name = Profiler; }};
        \\TRelease = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-tvOS-Release"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.tvos.release"; SDKROOT = appletvos; INFOPLIST_FILE = tvOS/Info.plist; CODE_SIGNING_ALLOWED = YES;}}; name = Release; }};
        \\VDebug = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-visionOS-Debug"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.visionos.debug"; SDKROOT = xros; INFOPLIST_FILE = visionOS/Info.plist; CODE_SIGNING_ALLOWED = YES;}}; name = Debug; }};
        \\VProfiler = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-visionOS-Profiler"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.visionos.profiler"; SDKROOT = xros; INFOPLIST_FILE = visionOS/Info.plist; CODE_SIGNING_ALLOWED = YES;}}; name = Profiler; }};
        \\VRelease = {{isa = XCBuildConfiguration; buildSettings = {{PRODUCT_NAME = "KiraApp-visionOS-Release"; PRODUCT_BUNDLE_IDENTIFIER = "com.kira.app.visionos.release"; SDKROOT = xros; INFOPLIST_FILE = visionOS/Info.plist; CODE_SIGNING_ALLOWED = YES;}}; name = Release; }};
        \\}};
        \\rootObject = A1;
        \\}}
        \\
    , .{});
}

fn appleMainSource() []const u8 {
    return
    \\#import <TargetConditionals.h>
    \\#import <Foundation/Foundation.h>
    \\#if TARGET_OS_IPHONE
    \\#import <UIKit/UIKit.h>
    \\static NSString *KiraRunnerConfig(void) {
    \\    NSString *path = [[NSBundle mainBundle] pathForResource:@"KiraRunner" ofType:@"toml"];
    \\    if (path == nil) {
    \\        return @"KiraRunner.toml not bundled";
    \\    }
    \\    NSError *error = nil;
    \\    NSString *config = [NSString stringWithContentsOfFile:path encoding:NSUTF8StringEncoding error:&error];
    \\    if (config == nil) {
    \\        return [NSString stringWithFormat:@"KiraRunner.toml unreadable: %@", error.localizedDescription];
    \\    }
    \\    return config;
    \\}
    \\@interface KiraSceneDelegate : UIResponder <UIWindowSceneDelegate>
    \\@property (strong, nonatomic) UIWindow *window;
    \\@end
    \\@implementation KiraSceneDelegate
    \\- (void)scene:(UIScene *)scene willConnectToSession:(UISceneSession *)session options:(UISceneConnectionOptions *)connectionOptions {
    \\    (void)session;
    \\    (void)connectionOptions;
    \\    if (![scene isKindOfClass:[UIWindowScene class]]) {
    \\        return;
    \\    }
    \\    NSString *config = KiraRunnerConfig();
    \\    NSLog(@"Kira Apple runner host launched");
    \\    NSLog(@"Kira runner config loaded: %@", config);
    \\    UIWindowScene *windowScene = (UIWindowScene *)scene;
    \\    self.window = [[UIWindow alloc] initWithWindowScene:windowScene];
    \\    UIViewController *controller = [UIViewController new];
    \\    controller.view.backgroundColor = [UIColor systemBackgroundColor];
    \\    UILabel *label = [[UILabel alloc] initWithFrame:controller.view.bounds];
    \\    label.autoresizingMask = UIViewAutoresizingFlexibleWidth | UIViewAutoresizingFlexibleHeight;
    \\    label.textAlignment = NSTextAlignmentCenter;
    \\    label.numberOfLines = 0;
    \\    label.text = @"Kira runtime configured";
    \\    [controller.view addSubview:label];
    \\    self.window.rootViewController = controller;
    \\    [self.window makeKeyAndVisible];
    \\}
    \\@end
    \\@interface KiraAppDelegate : UIResponder <UIApplicationDelegate>
    \\@end
    \\@implementation KiraAppDelegate
    \\- (UISceneConfiguration *)application:(UIApplication *)application configurationForConnectingSceneSession:(UISceneSession *)connectingSceneSession options:(UISceneConnectionOptions *)options {
    \\    (void)application;
    \\    (void)options;
    \\    UISceneConfiguration *configuration = [[UISceneConfiguration alloc] initWithName:@"Default Configuration" sessionRole:connectingSceneSession.role];
    \\    configuration.delegateClass = [KiraSceneDelegate class];
    \\    return configuration;
    \\}
    \\@end
    \\int main(int argc, char **argv) {
    \\    @autoreleasepool {
    \\        return UIApplicationMain(argc, argv, nil, NSStringFromClass([KiraAppDelegate class]));
    \\    }
    \\}
    \\#else
    \\int main(int argc, char **argv) {
    \\    (void)argc;
    \\    (void)argv;
    \\    @autoreleasepool {
    \\        NSLog(@"Kira Apple runner host launched");
    \\        NSString *path = [[NSBundle mainBundle] pathForResource:@"KiraRunner" ofType:@"toml"];
    \\        NSLog(@"Kira runner config path: %@", path);
    \\    }
    \\    return 0;
    \\}
    \\#endif
    \\
    ;
}

fn androidMainSource() []const u8 {
    return
    \\package com.kira.app;
    \\
    \\import android.app.Activity;
    \\import android.os.Bundle;
    \\import android.util.Log;
    \\import android.widget.TextView;
    \\import java.io.BufferedReader;
    \\import java.io.InputStream;
    \\import java.io.InputStreamReader;
    \\import java.nio.charset.StandardCharsets;
    \\
    \\public final class MainActivity extends Activity {
    \\  private String runnerConfig() {
    \\    StringBuilder builder = new StringBuilder();
    \\    try (InputStream input = getAssets().open("KiraRunner.toml");
    \\         BufferedReader reader = new BufferedReader(new InputStreamReader(input, StandardCharsets.UTF_8))) {
    \\      String line;
    \\      while ((line = reader.readLine()) != null) {
    \\        builder.append(line).append('\n');
    \\      }
    \\      return builder.toString();
    \\    } catch (Exception error) {
    \\      return "KiraRunner.toml unreadable: " + error.getMessage();
    \\    }
    \\  }
    \\
    \\  public void onCreate(Bundle state) {
    \\    super.onCreate(state);
    \\    String config = runnerConfig();
    \\    Log.i("KiraRunner", "Kira Android runner host launched");
    \\    Log.i("KiraRunner", "Kira runner config loaded: " + config);
    \\    TextView label = new TextView(this);
    \\    label.setText("Kira runtime configured");
    \\    setContentView(label);
    \\  }
    \\}
    \\
    ;
}

fn runnerConfigToml(allocator: std.mem.Allocator, runner: []const u8, project_name: []const u8, selected_app_path: []const u8) ![]const u8 {
    return std.fmt.allocPrint(
        allocator,
        \\runner = "{s}"
        \\payload_kind = "kira-runtime-config"
        \\app_name = "{s}"
        \\selected_example = "{s}"
        \\bundle_identifier = "com.kira.live.dev"
        \\required_markers = [
        \\  "KIRA_UI_FOUNDATION_APP_STARTED",
        \\  "KIRA_UI_TREE_BUILT",
        \\  "KIRA_UI_RETAINED_TREE_READY",
        \\  "KIRA_UI_LAYOUT_NON_EMPTY",
        \\  "KIRA_UI_DRAW_COMMANDS_SUBMITTED",
        \\  "KIRA_APP_RENDERED_VISIBLE_CONTENT",
        \\]
        \\
    ,
        .{ runner, project_name, selected_app_path },
    );
}

fn appleSwiftSource() []const u8 {
    return "import Foundation\n\npublic struct KiraLiveClient { public let serverURL: URL }\n";
}

fn appleBundleLoaderSource() []const u8 {
    return "import Foundation\n\npublic struct KiraBundleLoader { public let bundleRoot: URL }\n";
}

fn applePlatformDir(platform: manifest.ApplePlatform) []const u8 {
    return switch (platform) {
        .macos => "macOS",
        .ios => "iOS",
        .tvos => "tvOS",
        .visionos => "visionOS",
    };
}

fn appleInfoPlist(allocator: std.mem.Allocator, platform: manifest.ApplePlatform, project_name: []const u8) ![]const u8 {
    const requires_ios = if (platform == .ios or platform == .tvos or platform == .visionos) "<key>LSRequiresIPhoneOS</key><true/>" else "";
    const scene_manifest = if (platform == .ios or platform == .tvos or platform == .visionos) "<key>UIApplicationSceneManifest</key><dict><key>UIApplicationSupportsMultipleScenes</key><false/><key>UISceneConfigurations</key><dict><key>UIWindowSceneSessionRoleApplication</key><array><dict><key>UISceneConfigurationName</key><string>Default Configuration</string><key>UISceneDelegateClassName</key><string>KiraSceneDelegate</string></dict></array></dict></dict>" else "";
    const launch_metadata = if (platform == .ios) "<key>UISupportedInterfaceOrientations</key><array><string>UIInterfaceOrientationPortrait</string><string>UIInterfaceOrientationLandscapeLeft</string><string>UIInterfaceOrientationLandscapeRight</string></array><key>UISupportedInterfaceOrientations~ipad</key><array><string>UIInterfaceOrientationPortrait</string><string>UIInterfaceOrientationPortraitUpsideDown</string><string>UIInterfaceOrientationLandscapeLeft</string><string>UIInterfaceOrientationLandscapeRight</string></array><key>UILaunchScreen</key><dict/>" else "";
    return std.fmt.allocPrint(allocator,
        \\<?xml version="1.0" encoding="UTF-8"?>
        \\<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
        \\<plist version="1.0"><dict><key>CFBundleName</key><string>{s}</string><key>CFBundleDisplayName</key><string>{s}</string><key>CFBundleIdentifier</key><string>$(PRODUCT_BUNDLE_IDENTIFIER)</string><key>CFBundleExecutable</key><string>$(EXECUTABLE_NAME)</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleVersion</key><string>1</string><key>CFBundleShortVersionString</key><string>0.1.0</string>{s}{s}{s}</dict></plist>
        \\
    , .{ project_name, project_name, requires_ios, scene_manifest, launch_metadata });
}

fn entitlementsPlist() []const u8 {
    return "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict/></plist>\n";
}

fn webIndex(allocator: std.mem.Allocator, project_name: []const u8, surface: manifest.WebSurface) ![]const u8 {
    return std.fmt.allocPrint(allocator,
        \\<!doctype html>
        \\<html><head><meta charset="utf-8"><title>{s}</title></head>
        \\<body data-kira-runner="web" data-kira-surface="{s}"><script src="./kira-browser-ffi.generated.js"></script><script src="./kira-wasm.js"></script></body></html>
        \\
    , .{ project_name, surface.label() });
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

fn webRuntimeJs(surface: manifest.WebSurface) []const u8 {
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

fn webManifestJson(allocator: std.mem.Allocator, project_name: []const u8, requirements: manifest.WebSurfaceRequirements) ![]const u8 {
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

fn writeTextFile(path: []const u8, data: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, std.fs.path.dirname(path) orelse ".");
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn writeBytesFile(path: []const u8, data: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, std.fs.path.dirname(path) orelse ".");
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn safeIdentifier(allocator: std.mem.Allocator, name: []const u8) ![]const u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    for (name) |ch| {
        if (std.ascii.isAlphanumeric(ch)) {
            try out.append(std.ascii.toLower(ch));
        } else {
            try out.append('_');
        }
    }
    return out.toOwnedSlice();
}

fn androidApplicationId(allocator: std.mem.Allocator, project_name: []const u8) ![]const u8 {
    const segment = try safeJavaPackageSegment(allocator, project_name);
    return std.fmt.allocPrint(allocator, "com.kira.{s}", .{segment});
}

fn safeJavaPackageSegment(allocator: std.mem.Allocator, name: []const u8) ![]const u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    for (name) |ch| {
        if (std.ascii.isAlphanumeric(ch) or ch == '_') {
            try out.append(std.ascii.toLower(ch));
        } else {
            try out.append('_');
        }
    }
    if (out.items.len == 0 or !std.ascii.isAlphabetic(out.items[0])) {
        try out.insertSlice(0, "app_");
    }
    return out.toOwnedSlice();
}

fn commandExists(allocator: std.mem.Allocator, name: []const u8) bool {
    const candidates = [_][]const u8{ "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin" };
    for (candidates) |dir| {
        const path = std.fs.path.join(allocator, &.{ dir, name }) catch continue;
        defer allocator.free(path);
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch continue;
        file.close(std.Options.debug_io);
        return true;
    }
    if (androidSdkToolExists(allocator, name)) return true;
    return false;
}

fn androidSdkToolExists(allocator: std.mem.Allocator, name: []const u8) bool {
    if (androidSdkRoot(allocator) catch null) |root| {
        defer allocator.free(root);
        return androidSdkToolExistsUnderRoot(allocator, root, name);
    }
    return false;
}

fn androidSdkRoot(allocator: std.mem.Allocator) !?[]const u8 {
    if (kira_toolchain.envVarOwned(allocator, "ANDROID_HOME")) |root| {
        if (directoryExistsAbsolute(root)) return root;
        allocator.free(root);
    } else |_| {}
    if (kira_toolchain.envVarOwned(allocator, "ANDROID_SDK_ROOT")) |root| {
        if (directoryExistsAbsolute(root)) return root;
        allocator.free(root);
    } else |_| {}
    if (kira_toolchain.envVarOwned(allocator, "HOME")) |home| {
        defer allocator.free(home);
        const root = try std.fs.path.join(allocator, &.{ home, "Library", "Android", "sdk" });
        if (directoryExistsAbsolute(root)) return root;
        allocator.free(root);
    } else |_| {}
    return null;
}

fn directoryExistsAbsolute(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn androidSdkToolExistsUnderRoot(allocator: std.mem.Allocator, root: []const u8, name: []const u8) bool {
    const candidates = [_][]const u8{
        "platform-tools",
        "cmdline-tools/latest/bin",
        "emulator",
    };
    for (candidates) |relative| {
        const path = std.fs.path.join(allocator, &.{ root, relative, name }) catch continue;
        defer allocator.free(path);
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch continue;
        file.close(std.Options.debug_io);
        return true;
    }
    return false;
}

test "web export FFI uses stable tracked callback handles" {
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

test "web export manifest models WebGPU canvas requirements" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const requirements = manifest.webSurfaceRequirements(.webgpu);
    const json = try webManifestJson(arena.allocator(), "KiraApp", requirements);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"surface\":\"webgpu\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"rendering_model\":\"graphics-canvas\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"graphics_capability\":\"webgpu\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"requires_canvas\":true") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"requires_browser_detection\":true") != null);
}

test "Android application ids are valid without manual replacement" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    try std.testing.expectEqualStrings("com.kira.kira_app", try androidApplicationId(arena.allocator(), "Kira App"));
    try std.testing.expectEqualStrings("com.kira.app_123_demo", try androidApplicationId(arena.allocator(), "123 Demo"));
}

test "Apple iOS export uses stable automatic signing and complete bundle metadata" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const project = try applePbxproj(arena.allocator(), "Kira App");
    try std.testing.expect(std.mem.indexOf(u8, project, "PRODUCT_BUNDLE_IDENTIFIER = \"com.kira.live.dev\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, project, "DEVELOPMENT_TEAM = AKD4RFY7LU") != null);
    try std.testing.expect(std.mem.indexOf(u8, project, "CODE_SIGN_STYLE = Automatic") != null);
    try std.testing.expect(std.mem.indexOf(u8, project, "SUPPORTED_PLATFORMS = \"iphoneos iphonesimulator\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, project, "OTHER_LDFLAGS = \"-framework UIKit\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, project, "ALWAYS_SEARCH_USER_PATHS = NO") != null);

    const plist = try appleInfoPlist(arena.allocator(), .ios, "Kira App");
    try std.testing.expect(std.mem.indexOf(u8, plist, "CFBundleIdentifier") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "CFBundleExecutable") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "CFBundleName") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "CFBundleDisplayName") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "<key>CFBundlePackageType</key><string>APPL</string>") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "UIApplicationSceneManifest") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "KiraSceneDelegate") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "<key>UILaunchScreen</key><dict/>") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "UISupportedInterfaceOrientations~ipad") != null);
    try std.testing.expect(std.mem.indexOf(u8, plist, "UIRequiresFullScreen") == null);
}
