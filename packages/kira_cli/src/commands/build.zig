const std = @import("std");
const builtin = @import("builtin");
const build_pkg = @import("kira_build");
const build_def = @import("kira_build_definition");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const package_manager = @import("kira_package_manager");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
    build_pkg.setTimingsEnabled(parsed.timings or timingsEnvEnabled());
    const input = support.resolveCliInput(allocator, parsed.input_path) catch |err| switch (err) {
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
    try support.validateTargetSelection(allocator, stderr, .build, input);
    const backend = parsed.backend orelse input.default_backend orelse .vm;

    if (input.target.root_path) |project_root| {
        var package_diagnostics = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        _ = package_manager.syncProject(allocator, project_root, support.versionString(), .{
            .offline = parsed.offline,
            .locked = parsed.locked,
        }, &package_diagnostics) catch |err| {
            if (err == error.DiagnosticsEmitted) {
                try support.renderStandaloneDiagnostics(stderr, package_diagnostics.items);
                return error.CommandFailed;
            }
            return err;
        };
    }

    if (input.target.target_kind == .library) {
        const source_root = input.target.source_root.?;
        try support.logFrontendStarted(stderr, "build", source_root);
        var system = build_pkg.BuildSystem.init(allocator);
        const result = try system.checkPackageRoot(source_root);
        if (diagnostics.hasErrors(result.diagnostics)) {
            try support.logFrontendFailed(stderr, result.failure_stage, source_root, result.diagnostics.len);
            try support.renderDiagnostics(stderr, &result.source, result.diagnostics);
            return error.CommandFailed;
        }
        try stdout.print("built library {s}\n", .{source_root});
        return;
    }

    const source_path = input.target.source_path.?;
    try support.logFrontendStarted(stderr, "build", source_path);
    const output_root = try support.outputRoot(allocator, input.target.root_path);
    defer allocator.free(output_root);
    try support.ensurePath(output_root);
    const output_path = try defaultOutputPath(
        allocator,
        output_root,
        input.target.project_name orelse std.fs.path.stem(source_path),
        backend,
    );

    var system = build_pkg.BuildSystem.init(allocator);
    const result = try system.build(.{
        .source_path = source_path,
        .output_path = output_path,
        .target = .{ .execution = backend },
    });
    if (result.failed()) {
        try support.logBuildAborted(stderr, "build", result.failure_kind.?, source_path);
        if (result.source) |source| {
            try support.renderDiagnostics(stderr, &source, result.diagnostics);
        }
        return error.CommandFailed;
    }

    for (result.artifacts) |artifact| {
        try stdout.print("wrote {s}\n", .{artifact.path});
    }
}

const ParsedArgs = struct {
    backend: ?build_def.ExecutionTarget = null,
    offline: bool = false,
    locked: bool = false,
    timings: bool = false,
    input_path: []const u8,
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    var backend: ?build_def.ExecutionTarget = null;
    var offline = false;
    var locked = false;
    var timings = false;
    var input_path: ?[]const u8 = null;

    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            backend = parseBackend(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--offline")) {
            offline = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--locked")) {
            locked = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--timings")) {
            timings = true;
            continue;
        }
        if (input_path != null) return error.InvalidArguments;
        input_path = arg;
    }

    return .{
        .backend = backend,
        .offline = offline,
        .locked = locked,
        .timings = timings,
        .input_path = input_path orelse support.defaultCommandInputPath(),
    };
}

fn timingsEnvEnabled() bool {
    if (!builtin.link_libc) return false;
    const raw = std.c.getenv("KIRA_TIMINGS") orelse return false;
    const value = std.mem.span(raw);
    return value.len != 0 and !std.mem.eql(u8, value, "0") and !std.mem.eql(u8, value, "false");
}

fn parseBackend(arg: []const u8) ?build_def.ExecutionTarget {
    if (std.mem.eql(u8, arg, "vm")) return .vm;
    if (std.mem.eql(u8, arg, "llvm")) return .llvm_native;
    if (std.mem.eql(u8, arg, "hybrid")) return .hybrid;
    return null;
}

fn defaultOutputPath(allocator: std.mem.Allocator, output_root: []const u8, stem: []const u8, backend: build_def.ExecutionTarget) ![]const u8 {
    return switch (backend) {
        .vm => std.fmt.allocPrint(allocator, "{s}/{s}.kbc", .{ output_root, stem }),
        .llvm_native => std.fmt.allocPrint(allocator, "{s}/{s}{s}", .{ output_root, stem, build_pkg.executableExtension() }),
        .hybrid => std.fmt.allocPrint(allocator, "{s}/{s}.khm", .{ output_root, stem }),
    };
}
