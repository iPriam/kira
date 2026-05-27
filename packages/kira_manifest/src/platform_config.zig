const std = @import("std");

pub const RunnerId = enum {
    desktop,
    macos,
    ios,
    tvos,
    visionos,
    windows,
    android,
    web,
    linux,

    pub fn parse(value: []const u8) ?RunnerId {
        inline for (@typeInfo(RunnerId).@"enum".fields) |field| {
            if (std.mem.eql(u8, value, field.name)) return @enumFromInt(field.value);
        }
        if (std.mem.eql(u8, value, "ios-simulator") or std.mem.eql(u8, value, "ios-device")) return .ios;
        return null;
    }

    pub fn label(self: RunnerId) []const u8 {
        return @tagName(self);
    }
};

pub const BuildSystem = enum {
    kira,
    xcode,
    visual_studio,
    android_studio,
    kira_wasm,
    cmake,

    pub fn parse(value: []const u8) ?BuildSystem {
        if (std.mem.eql(u8, value, "kira")) return .kira;
        if (std.mem.eql(u8, value, "xcode")) return .xcode;
        if (std.mem.eql(u8, value, "visual-studio") or std.mem.eql(u8, value, "visual_studio")) return .visual_studio;
        if (std.mem.eql(u8, value, "android-studio") or std.mem.eql(u8, value, "android_studio")) return .android_studio;
        if (std.mem.eql(u8, value, "kira-wasm") or std.mem.eql(u8, value, "kira_wasm")) return .kira_wasm;
        if (std.mem.eql(u8, value, "cmake")) return .cmake;
        return null;
    }

    pub fn label(self: BuildSystem) []const u8 {
        return switch (self) {
            .kira => "kira",
            .xcode => "xcode",
            .visual_studio => "visual-studio",
            .android_studio => "android-studio",
            .kira_wasm => "kira-wasm",
            .cmake => "cmake",
        };
    }
};

pub const BuildProfile = enum {
    debug,
    profiler,
    release,

    pub fn parse(value: []const u8) ?BuildProfile {
        if (std.mem.eql(u8, value, "debug")) return .debug;
        if (std.mem.eql(u8, value, "profiler")) return .profiler;
        if (std.mem.eql(u8, value, "release")) return .release;
        return null;
    }

    pub fn label(self: BuildProfile) []const u8 {
        return @tagName(self);
    }
};

pub const Backend = enum {
    vm,
    llvm,
    hybrid,

    pub fn parse(value: []const u8) ?Backend {
        if (std.mem.eql(u8, value, "vm")) return .vm;
        if (std.mem.eql(u8, value, "llvm")) return .llvm;
        if (std.mem.eql(u8, value, "hybrid")) return .hybrid;
        return null;
    }

    pub fn label(self: Backend) []const u8 {
        return @tagName(self);
    }
};

pub const ExecutionBackend = enum {
    vm,
    llvm,
    hybrid,
    wasm_runtime,
    wasm_aot,

    pub fn parse(value: []const u8) ?ExecutionBackend {
        if (std.mem.eql(u8, value, "vm")) return .vm;
        if (std.mem.eql(u8, value, "llvm") or std.mem.eql(u8, value, "llvm_native")) return .llvm;
        if (std.mem.eql(u8, value, "hybrid")) return .hybrid;
        if (std.mem.eql(u8, value, "wasm_runtime") or std.mem.eql(u8, value, "wasm-runtime")) return .wasm_runtime;
        if (std.mem.eql(u8, value, "wasm_aot") or std.mem.eql(u8, value, "wasm-aot")) return .wasm_aot;
        return null;
    }

    pub fn label(self: ExecutionBackend) []const u8 {
        return switch (self) {
            .vm => "vm",
            .llvm => "llvm",
            .hybrid => "hybrid",
            .wasm_runtime => "wasm_runtime",
            .wasm_aot => "wasm_aot",
        };
    }
};

pub const HybridSelectionMode = enum {
    annotation_driven,
    native_except_runtime,
    vm_except_native,
    explicit_only,

    pub fn parse(value: []const u8) ?HybridSelectionMode {
        if (std.mem.eql(u8, value, "annotation_driven") or std.mem.eql(u8, value, "annotation-driven")) return .annotation_driven;
        if (std.mem.eql(u8, value, "native_except_runtime") or std.mem.eql(u8, value, "native-except-runtime")) return .native_except_runtime;
        if (std.mem.eql(u8, value, "vm_except_native") or std.mem.eql(u8, value, "vm-except-native")) return .vm_except_native;
        if (std.mem.eql(u8, value, "explicit_only") or std.mem.eql(u8, value, "explicit-only")) return .explicit_only;
        return null;
    }

    pub fn label(self: HybridSelectionMode) []const u8 {
        return switch (self) {
            .annotation_driven => "annotation_driven",
            .native_except_runtime => "native_except_runtime",
            .vm_except_native => "vm_except_native",
            .explicit_only => "explicit_only",
        };
    }
};

pub const BackendSelectionSource = enum {
    platform_default,
    profile,
    app_manifest,
    cli,

    pub fn label(self: BackendSelectionSource) []const u8 {
        return switch (self) {
            .platform_default => "platform-default",
            .profile => "profile",
            .app_manifest => "app-manifest",
            .cli => "cli",
        };
    }
};

pub const WebGraphicsBridge = enum {
    none,
    webgpu,

    pub fn parse(value: []const u8) ?WebGraphicsBridge {
        if (std.mem.eql(u8, value, "none")) return .none;
        if (std.mem.eql(u8, value, "webgpu")) return .webgpu;
        return null;
    }

    pub fn label(self: WebGraphicsBridge) []const u8 {
        return switch (self) {
            .none => "none",
            .webgpu => "webgpu",
        };
    }
};

pub const LibraryExecutionPolicy = struct {
    package: []const u8,
    backend: ExecutionBackend = .hybrid,
    source: BackendSelectionSource = .app_manifest,
    native_required: bool = false,
    ffi_allowed: bool = false,
    hybrid_selection: ?HybridSelectionMode = null,
};

pub const WebExecutionPolicy = struct {
    backend: ExecutionBackend = .wasm_runtime,
    graphics_bridge: WebGraphicsBridge = .none,
};

pub const ExecutionPolicy = struct {
    default_backend: ExecutionBackend = .vm,
    default_source: BackendSelectionSource = .platform_default,
    hybrid_selection: HybridSelectionMode = .annotation_driven,
    libraries: []const LibraryExecutionPolicy = &.{},
    web: WebExecutionPolicy = .{},
};

pub const WebSurface = enum {
    dom,
    webgpu,
    hybrid,

    pub fn parse(value: []const u8) ?WebSurface {
        if (std.mem.eql(u8, value, "dom")) return .dom;
        if (std.mem.eql(u8, value, "webgpu")) return .webgpu;
        if (std.mem.eql(u8, value, "hybrid")) return .hybrid;
        return null;
    }

    pub fn label(self: WebSurface) []const u8 {
        return @tagName(self);
    }
};

pub const WebRenderingModel = enum {
    dom,
    graphics_canvas,
    hybrid,

    pub fn label(self: WebRenderingModel) []const u8 {
        return switch (self) {
            .dom => "dom",
            .graphics_canvas => "graphics-canvas",
            .hybrid => "hybrid",
        };
    }
};

pub const WebGraphicsCapability = enum {
    webgpu,

    pub fn label(self: WebGraphicsCapability) []const u8 {
        return switch (self) {
            .webgpu => "webgpu",
        };
    }
};

pub const WebSurfaceRequirements = struct {
    surface: WebSurface,
    rendering_model: WebRenderingModel,
    graphics_capability: ?WebGraphicsCapability = null,
    requires_canvas: bool = false,
    requires_browser_detection: bool = false,
};

pub fn webSurfaceRequirements(surface: WebSurface) WebSurfaceRequirements {
    return switch (surface) {
        .dom => .{
            .surface = .dom,
            .rendering_model = .dom,
        },
        .webgpu => .{
            .surface = .webgpu,
            .rendering_model = .graphics_canvas,
            .graphics_capability = .webgpu,
            .requires_canvas = true,
            .requires_browser_detection = true,
        },
        .hybrid => .{
            .surface = .hybrid,
            .rendering_model = .hybrid,
            .graphics_capability = .webgpu,
            .requires_canvas = true,
            .requires_browser_detection = true,
        },
    };
}

pub const ExportFamily = enum {
    apple,
    macos,
    ios,
    tvos,
    visionos,
    windows,
    android,
    web,
    linux,

    pub fn parse(value: []const u8) ?ExportFamily {
        inline for (@typeInfo(ExportFamily).@"enum".fields) |field| {
            if (std.mem.eql(u8, value, field.name)) return @enumFromInt(field.value);
        }
        return null;
    }

    pub fn label(self: ExportFamily) []const u8 {
        return @tagName(self);
    }
};

pub const ApplePlatform = enum {
    macos,
    ios,
    tvos,
    visionos,

    pub fn label(self: ApplePlatform) []const u8 {
        return switch (self) {
            .macos => "macOS",
            .ios => "iOS",
            .tvos => "tvOS",
            .visionos => "visionOS",
        };
    }
};

pub const LiveProtocolMode = enum {
    full_bundle,
};

pub const ProfileConfig = struct {
    id: BuildProfile,
    backend: Backend,
    optimization: []const u8,
    debug_symbols: bool,
    profiling: bool = false,
    strip: bool = false,
    lto: bool = false,
};

pub const RunnerConfig = struct {
    id: RunnerId,
    build_system: BuildSystem,
    default_profile: BuildProfile,
    default_surface: ?WebSurface = null,
};

pub const ResolvedConfig = struct {
    profiles: [3]ProfileConfig,
    runners: [9]RunnerConfig,

    pub fn profile(self: ResolvedConfig, id: BuildProfile) ProfileConfig {
        for (self.profiles) |item| if (item.id == id) return item;
        unreachable;
    }

    pub fn runner(self: ResolvedConfig, id: RunnerId) RunnerConfig {
        for (self.runners) |item| if (item.id == id) return item;
        unreachable;
    }
};

pub fn defaultResolvedConfig() ResolvedConfig {
    return .{
        .profiles = .{
            .{
                .id = .debug,
                .backend = .vm,
                .optimization = "none",
                .debug_symbols = true,
            },
            .{
                .id = .profiler,
                .backend = .llvm,
                .optimization = "speed-lite",
                .debug_symbols = true,
                .profiling = true,
            },
            .{
                .id = .release,
                .backend = .llvm,
                .optimization = "speed",
                .debug_symbols = false,
                .strip = true,
                .lto = true,
            },
        },
        .runners = .{
            .{ .id = .desktop, .build_system = .kira, .default_profile = .debug },
            .{ .id = .macos, .build_system = .xcode, .default_profile = .debug },
            .{ .id = .ios, .build_system = .xcode, .default_profile = .debug },
            .{ .id = .tvos, .build_system = .xcode, .default_profile = .debug },
            .{ .id = .visionos, .build_system = .xcode, .default_profile = .debug },
            .{ .id = .windows, .build_system = .visual_studio, .default_profile = .debug },
            .{ .id = .android, .build_system = .android_studio, .default_profile = .debug },
            .{ .id = .web, .build_system = .kira_wasm, .default_profile = .debug, .default_surface = .dom },
            .{ .id = .linux, .build_system = .cmake, .default_profile = .debug },
        },
    };
}

pub fn validateProfileSection(section: []const u8) !void {
    if (std.mem.eql(u8, section, "profiles.profile")) return error.ReservedProfileName;
    if (!std.mem.startsWith(u8, section, "profiles.")) return;
    const name = section["profiles.".len..];
    if (BuildProfile.parse(name) == null) return error.UnknownProfile;
}

test "default platform config synthesizes profiles and runners" {
    const config = defaultResolvedConfig();
    try std.testing.expectEqual(Backend.vm, config.profile(.debug).backend);
    try std.testing.expectEqual(Backend.llvm, config.profile(.profiler).backend);
    try std.testing.expect(config.profile(.profiler).profiling);
    try std.testing.expect(config.profile(.release).lto);
    try std.testing.expectEqual(BuildSystem.kira_wasm, config.runner(.web).build_system);
    try std.testing.expectEqual(WebSurface.dom, config.runner(.web).default_surface.?);
    try std.testing.expectEqual(BuildSystem.android_studio, config.runner(.android).build_system);
}

test "profile is reserved; profiler is the supported profile" {
    try validateProfileSection("profiles.profiler");
    try std.testing.expectError(error.ReservedProfileName, validateProfileSection("profiles.profile"));
}

test "web surface requirements distinguish DOM and WebGPU canvas rendering" {
    const dom_requirements = webSurfaceRequirements(.dom);
    try std.testing.expectEqual(WebRenderingModel.dom, dom_requirements.rendering_model);
    try std.testing.expect(!dom_requirements.requires_canvas);
    try std.testing.expect(dom_requirements.graphics_capability == null);

    const webgpu_requirements = webSurfaceRequirements(.webgpu);
    try std.testing.expectEqual(WebRenderingModel.graphics_canvas, webgpu_requirements.rendering_model);
    try std.testing.expectEqual(WebGraphicsCapability.webgpu, webgpu_requirements.graphics_capability.?);
    try std.testing.expect(webgpu_requirements.requires_canvas);
    try std.testing.expect(webgpu_requirements.requires_browser_detection);
}

test "execution backend policy uses typed internal values" {
    try std.testing.expectEqual(ExecutionBackend.wasm_runtime, ExecutionBackend.parse("wasm_runtime").?);
    try std.testing.expectEqual(ExecutionBackend.wasm_aot, ExecutionBackend.parse("wasm-aot").?);
    try std.testing.expectEqual(HybridSelectionMode.native_except_runtime, HybridSelectionMode.parse("native_except_runtime").?);
    try std.testing.expectEqualStrings("app-manifest", BackendSelectionSource.app_manifest.label());
}
