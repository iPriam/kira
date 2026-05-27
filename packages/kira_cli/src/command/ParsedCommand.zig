const std = @import("std");
const build_def = @import("kira_build_definition");
const manifest = @import("kira_manifest");
const app_generation = @import("kira_app_generation");
const Duration = @import("Duration.zig");
const CommandKind = @import("CommandKind.zig").CommandKind;

pub const HelpOptions = struct {
    command: ?CommandKind = null,
};

pub const VersionOptions = struct {};

pub const ProjectOptions = struct {
    backend: ?build_def.ExecutionTarget = null,
    profile: ?manifest.BuildProfile = null,
    offline: bool = false,
    locked: bool = false,
    timings: bool = false,
    print_backend_policy: bool = false,
    input_path: []const u8 = ".",
};

pub const RunOptions = struct {
    runner: ?manifest.RunnerId = null,
    backend: ?build_def.ExecutionTarget = null,
    surface: manifest.WebSurface = .dom,
    offline: bool = false,
    locked: bool = false,
    trace_execution: bool = false,
    timings: bool = false,
    quit_after: ?Duration = null,
    input_path: []const u8 = ".",
};

pub const LiveMode = enum { run, runners_list, runners_build, runners_clean };
pub const LiveRunnerKind = enum {
    desktop,
    macos,
    ios,
    tvos,
    visionos,
    windows,
    android,
    web,
    linux,

    pub fn legacyLabel(self: LiveRunnerKind) []const u8 {
        return switch (self) {
            .desktop => "desktop",
            .macos => "macos",
            .ios => "ios",
            .tvos => "tvos",
            .visionos => "visionos",
            .windows => "windows",
            .android => "android",
            .web => "web",
            .linux => "linux",
        };
    }
};

pub const LiveOptions = struct {
    mode: LiveMode = .run,
    runner: LiveRunnerKind = .desktop,
    input_path: []const u8,
    run_for: ?Duration = null,
    quit_after: ?Duration = null,
    profile: ?manifest.BuildProfile = null,
    surface: manifest.WebSurface = .dom,
    host: ?[]const u8 = null,
    port: ?u16 = null,
    server_url: ?[]const u8 = null,
    kill_after: bool = false,
    headless: bool = false,
    device: []const u8 = "auto",
};

pub const ExportOptions = struct {
    family: manifest.ExportFamily,
    input_path: []const u8 = ".",
    profile: manifest.BuildProfile = .debug,
    surface: manifest.WebSurface = .dom,
};

pub const NewOptions = struct {
    kind: app_generation.TemplateKind = .app,
    name: []const u8,
    destination: []const u8,
};

pub const FetchLlvmMode = enum { download_and_install, ci_metadata_json, install_archive };
pub const FetchLlvmOptions = struct {
    mode: FetchLlvmMode = .download_and_install,
    archive_path: ?[]const u8 = null,
};

pub const SyncOptions = struct {
    offline: bool = false,
    locked: bool = false,
    input_path: ?[]const u8 = null,
};

pub const AddOptions = struct {
    package_name: []const u8,
    git_url: ?[]const u8 = null,
    rev: ?[]const u8 = null,
    tag: ?[]const u8 = null,
};

pub const SinglePackageOptions = struct {
    package_name: []const u8,
};

pub const UpdateOptions = struct {
    input_path: ?[]const u8 = null,
};

pub const PackageMode = enum { pack, inspect };
pub const PackageOptions = struct {
    mode: PackageMode,
    input_path: ?[]const u8 = null,
};

pub const ShaderMode = enum { check, ast, build };
pub const ShaderOptions = struct {
    mode: ShaderMode,
    input_path: ?[]const u8 = null,
    out_dir: ?[]const u8 = null,
    target: ?[]const u8 = null,
};

pub const InstrumentBackend = enum { runtime, llvm, hybrid };
pub const InstrumentTrack = enum { memory, cpu };
pub const InstrumentsOptions = struct {
    input_path: []const u8,
    backend: InstrumentBackend = .runtime,
    tracks: []const InstrumentTrack,
    duration: Duration,
    sample_rate: []const u8,
    fail_on_growth: ?[]const u8 = null,
    json_out: ?[]const u8 = null,
};

pub const InstrumentArtifactOptions = struct {
    backend: InstrumentBackend,
    artifact_path: []const u8,
    cwd: ?[]const u8 = null,
};

pub const RunHybridArtifactOptions = struct {
    manifest_path: []const u8,
    cwd: ?[]const u8 = null,
};

pub const LiveRunnerOptions = struct {
    manifest_path: []const u8,
};

pub const ParsedCommand = union(CommandKind) {
    run: RunOptions,
    fetch_llvm: FetchLlvmOptions,
    tokens: UpdateOptions,
    ast: UpdateOptions,
    check: ProjectOptions,
    build: ProjectOptions,
    instruments: InstrumentsOptions,
    instrument_artifact: InstrumentArtifactOptions,
    run_hybrid_artifact: RunHybridArtifactOptions,
    live_runner: LiveRunnerOptions,
    shader: ShaderOptions,
    new: NewOptions,
    sync: SyncOptions,
    add: AddOptions,
    remove: SinglePackageOptions,
    update: UpdateOptions,
    package: PackageOptions,
    live: LiveOptions,
    export_cmd: ExportOptions,
    help: HelpOptions,
    version: VersionOptions,

    pub fn kind(self: ParsedCommand) CommandKind {
        return std.meta.activeTag(self);
    }
};
