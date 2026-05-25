const std = @import("std");
const builtin = @import("builtin");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const package_manager = @import("kira_package_manager");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
    build.setTimingsEnabled(parsed.timings or timingsEnvEnabled());
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
    try support.validateTargetSelection(allocator, stderr, .check, input);

    if (input.target.root_path) |root| {
        try syncProject(allocator, root, parsed, stderr);
    }

    const result = switch (input.target.target_kind) {
        .library => blk: {
            const source_root = input.target.source_root.?;
            try support.logFrontendStarted(stderr, "check", source_root);
            var system = build.BuildSystem.init(allocator);
            break :blk try system.checkPackageRoot(source_root);
        },
        .executable, .example, .source_file => blk: {
            const source_path = input.target.source_path.?;
            try support.logFrontendStarted(stderr, "check", source_path);
            var system = build.BuildSystem.init(allocator);
            if (parsed.backend) |backend| {
                break :blk try system.checkForBackend(source_path, backend);
            }
            break :blk try system.checkFrontend(source_path);
        },
    };
    if (!diagnostics.hasErrors(result.diagnostics)) {
        try stdout.writeAll("check passed\n");
        return;
    }
    const display_path = input.displayPath();
    try support.logFrontendFailed(stderr, result.failure_stage, display_path, result.diagnostics.len);
    try support.renderDiagnostics(stderr, &result.source, result.diagnostics);
    return error.CommandFailed;
}

fn syncProject(
    allocator: std.mem.Allocator,
    project_root: []const u8,
    parsed: ParsedArgs,
    stderr: anytype,
) !void {
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

const ParsedArgs = struct {
    backend: ?build_def.ExecutionTarget = null,
    offline: bool = false,
    locked: bool = false,
    timings: bool = false,
    input_path: []const u8,
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    var offline = false;
    var locked = false;
    var timings = false;
    var input_path: ?[]const u8 = null;

    var backend: ?build_def.ExecutionTarget = null;
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
