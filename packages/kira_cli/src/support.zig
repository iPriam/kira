const std = @import("std");
const builtin = @import("builtin");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const kira_log = @import("kira_log");
const kira_project = @import("kira_project");
const source_pkg = @import("kira_source");
const kira_toolchain = @import("kira_toolchain");
const build_options = @import("kira_cli_build_options");

pub fn binaryName() []const u8 {
    return build_options.binary_name;
}

pub fn versionString() []const u8 {
    return build_options.version;
}

pub fn channel() kira_toolchain.Channel {
    return kira_toolchain.Channel.parse(build_options.channel).?;
}

pub fn primaryExecutableName() []const u8 {
    return build_options.primary_executable;
}

pub fn currentHostTargetTriple() []const u8 {
    return comptime std.fmt.comptimePrint(
        "{s}-{s}-{s}",
        .{ @tagName(builtin.cpu.arch), @tagName(builtin.os.tag), @tagName(builtin.abi) },
    );
}

pub fn resolveManagedToolchainRoot(allocator: std.mem.Allocator) ![]u8 {
    if (try kira_toolchain.toolchainRootFromSelfExecutable(allocator)) |toolchain_root| {
        return toolchain_root;
    }
    return kira_toolchain.managedToolchainRoot(allocator, channel(), versionString());
}

pub fn resolveResourceRoot(allocator: std.mem.Allocator) ![]u8 {
    if (try kira_toolchain.toolchainRootFromSelfExecutable(allocator)) |toolchain_root| {
        return toolchain_root;
    }

    if (try findRepoRootFromCwd(allocator)) |repo_root| return repo_root;
    if (try findRepoRootFromSelfExe(allocator)) |repo_root| return repo_root;

    const toolchain_root = try resolveManagedToolchainRoot(allocator);
    if (hasManagedResources(toolchain_root)) return toolchain_root;
    allocator.free(toolchain_root);
    return error.ResourceRootNotFound;
}

pub fn renderDiagnostics(stderr: anytype, source: *const source_pkg.SourceFile, items: []const diagnostics.Diagnostic) !void {
    if (items.len == 0) return;
    try diagnostics.renderer.renderAll(stderr, source, items);
}

pub fn renderStandaloneDiagnostics(stderr: anytype, items: []const diagnostics.Diagnostic) !void {
    for (items) |item| {
        const severity = switch (item.severity) {
            .@"error" => "error",
            .warning => "warning",
            .note => "note",
        };
        if (item.code) |code| {
            try stderr.print("{s}[{s}]: {s}\n", .{ severity, code, item.title });
        } else {
            try stderr.print("{s}: {s}\n", .{ severity, item.title });
        }
        try stderr.print("  {s}\n", .{item.message});
        if (item.domain) |domain| try stderr.print("  domain: {s}\n", .{domain});
        if (item.phase) |phase| try stderr.print("  phase: {s}\n", .{phase});
        for (item.notes) |note| try stderr.print("  note: {s}\n", .{note});
        if (item.help) |help| try stderr.print("  help: {s}\n", .{help});
    }
}

pub fn renderStandaloneDiagnostic(stderr: anytype, item: diagnostics.Diagnostic) !void {
    const items = [_]diagnostics.Diagnostic{item};
    try renderStandaloneDiagnostics(stderr, &items);
}

pub fn logFrontendStarted(stderr: anytype, command: []const u8, path: []const u8) !void {
    try kira_log.write(stderr, .{
        .level = .info,
        .scope = "frontend",
        .event = "started",
        .message = "Frontend compilation started.",
        .fields = &.{
            .{ .key = "command", .value = command },
            .{ .key = "path", .value = path },
        },
    });
}

pub fn logFrontendFailed(stderr: anytype, stage: ?build.FrontendStage, path: []const u8, diagnostics_len: usize) !void {
    var diagnostics_buffer: [32]u8 = undefined;
    const diagnostics_text = try std.fmt.bufPrint(&diagnostics_buffer, "{d}", .{diagnostics_len});
    try kira_log.write(stderr, .{
        .level = .@"error",
        .scope = "frontend",
        .event = "failed",
        .message = "Frontend compilation stopped because Kira emitted diagnostics.",
        .fields = &.{
            .{ .key = "stage", .value = frontendStageName(stage) },
            .{ .key = "path", .value = path },
            .{ .key = "diagnostics", .value = diagnostics_text },
        },
    });
}

pub fn logBuildAborted(stderr: anytype, command: []const u8, kind: build.BuildFailureKind, path: []const u8) !void {
    try kira_log.write(stderr, .{
        .level = .@"error",
        .scope = "build",
        .event = "aborted",
        .message = "Build stopped before producing artifacts.",
        .fields = &.{
            .{ .key = "command", .value = command },
            .{ .key = "reason", .value = buildFailureName(kind) },
            .{ .key = "path", .value = path },
        },
    });
}

pub fn logInternalCompilerError(stderr: anytype, err_name: []const u8) !void {
    try kira_log.write(stderr, .{
        .level = .@"error",
        .scope = "compiler",
        .event = "internal_boundary",
        .message = "The internal compiler error boundary handled an unexpected failure.",
        .fields = &.{
            .{ .key = "error", .value = err_name },
        },
    });
}

pub fn renderInternalCompilerError(stderr: anytype, err_name: []const u8) !void {
    try renderStandaloneDiagnostic(
        stderr,
        try diag_messages.CompilerBugMessages.genericInternalCompilerError(std.heap.page_allocator, err_name),
    );
}

pub const ResolvedCliInput = struct {
    target: kira_project.ResolvedTarget,
    default_backend: ?build_def.ExecutionTarget = null,

    pub fn displayPath(self: ResolvedCliInput) []const u8 {
        return self.target.source_root orelse self.target.displayPath();
    }
};

pub const ResolvedCommandInput = struct {
    source_path: []const u8,
    project_root: ?[]const u8 = null,
    project_name: ?[]const u8 = null,
    default_backend: ?build_def.ExecutionTarget = null,
};

pub fn defaultCommandInputPath() []const u8 {
    return ".";
}

pub fn resolveCliInput(allocator: std.mem.Allocator, path: []const u8) !ResolvedCliInput {
    const target = try kira_project.resolveTargetFromPath(allocator, path);
    const default_backend = if (target.project) |project|
        try parseExecutionTarget(project.manifest.execution_mode)
    else
        null;
    return .{
        .target = target,
        .default_backend = default_backend,
    };
}

pub fn resolveCommandInput(allocator: std.mem.Allocator, path: []const u8) !ResolvedCommandInput {
    const input = try resolveCliInput(allocator, path);
    const source_path = input.target.source_path orelse return error.ProjectEntrypointNotFound;
    return .{
        .source_path = source_path,
        .project_root = input.target.root_path,
        .project_name = input.target.project_name,
        .default_backend = input.default_backend,
    };
}

pub fn validateTargetSelection(
    allocator: std.mem.Allocator,
    stderr: anytype,
    command: kira_project.CommandMode,
    input: ResolvedCliInput,
) !void {
    switch (command) {
        .check, .build => {
            if (input.target.target_kind != .library and input.target.source_path == null) {
                if (input.target.root_path) |root| {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.PackageMessages.missingSourceFile(allocator, root));
                    return error.CommandFailed;
                }
            }
        },
        .run => {
            if (input.target.target_kind == .library) {
                try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.libraryTargetCannotBeRun(allocator, input.target.displayPath()));
                return error.CommandFailed;
            }
            if (!input.target.canRun()) {
                if (input.target.root_path) |root| {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.PackageMessages.missingSourceFile(allocator, root));
                } else {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.commandRequiresRunnableTarget(allocator, "run", input.target.kindName()));
                }
                return error.CommandFailed;
            }
        },
        .live => {
            if (input.target.target_kind == .library) {
                try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.libraryTargetCannotBeStartedInLiveMode(allocator, input.target.displayPath()));
                return error.CommandFailed;
            }
            if (!input.target.canLive()) {
                if (input.target.root_path) |root| {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.PackageMessages.missingSourceFile(allocator, root));
                } else {
                    try renderStandaloneDiagnostic(stderr, try diag_messages.CliMessages.commandRequiresLiveCapableTarget(allocator, input.target.kindName()));
                }
                return error.CommandFailed;
            }
        },
    }
}

pub fn parseExecutionTarget(text: []const u8) !build_def.ExecutionTarget {
    if (std.mem.eql(u8, text, "vm")) return .vm;
    if (std.mem.eql(u8, text, "llvm") or std.mem.eql(u8, text, "llvm_native")) return .llvm_native;
    if (std.mem.eql(u8, text, "hybrid")) return .hybrid;
    return error.InvalidProjectExecutionMode;
}

pub fn outputRoot(allocator: std.mem.Allocator, project_root: ?[]const u8) ![]u8 {
    if (project_root) |root| return std.fs.path.join(allocator, &.{ root, "generated" });
    return allocator.dupe(u8, "generated");
}

pub fn ensurePath(path: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, path);
}

fn frontendStageName(stage: ?build.FrontendStage) []const u8 {
    return switch (stage orelse .ir) {
        .lexer => "lexer",
        .parser => "parser",
        .graph => "graph",
        .semantics => "semantics",
        .ir => "ir",
        .backend_prepare => "backend_prepare",
    };
}

fn buildFailureName(kind: build.BuildFailureKind) []const u8 {
    return switch (kind) {
        .frontend => "frontend_diagnostics",
        .build => "build_diagnostics",
        .toolchain => "toolchain_diagnostics",
    };
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
        if (isRepoRoot(current)) return current;

        const parent = std.fs.path.dirname(current) orelse break;
        if (std.mem.eql(u8, parent, current)) break;

        const parent_copy = try allocator.dupe(u8, parent);
        allocator.free(current);
        current = parent_copy;
    }

    allocator.free(current);
    return null;
}

fn isRepoRoot(path: []const u8) bool {
    const metadata_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "llvm-metadata.toml" }) catch return false;
    defer std.heap.page_allocator.free(metadata_path);
    if (!fileExists(metadata_path)) return false;

    const build_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "build.zig" }) catch return false;
    defer std.heap.page_allocator.free(build_path);
    return fileExists(build_path);
}

fn hasManagedResources(path: []const u8) bool {
    const metadata_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "llvm-metadata.toml" }) catch return false;
    defer std.heap.page_allocator.free(metadata_path);
    if (!fileExists(metadata_path)) return false;

    const templates_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "templates" }) catch return false;
    defer std.heap.page_allocator.free(templates_path);
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, templates_path, .{}) catch std.Io.Dir.cwd().openDir(std.Options.debug_io, templates_path, .{}) catch return false;
    dir.close(std.Options.debug_io);

    const foundation_manifest_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "foundation", "kira.toml" }) catch return false;
    defer std.heap.page_allocator.free(foundation_manifest_path);
    return fileExists(foundation_manifest_path);
}

fn fileExists(path: []const u8) bool {
    if (std.fs.path.isAbsolute(path)) {
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false;
        file.close(std.Options.debug_io);
        return true;
    }

    var file = std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn directoryExists(path: []const u8) bool {
    if (std.fs.path.isAbsolute(path)) {
        var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
        dir.close(std.Options.debug_io);
        return true;
    }

    var dir = std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}
