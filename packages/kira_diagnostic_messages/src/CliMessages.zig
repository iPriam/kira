const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const DiagnosticCode = @import("DiagnosticCode.zig").DiagnosticCode;
const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;
const message = @import("DiagnosticMessage.zig");

pub fn unknownCommand(command: []const u8) diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL001_UnknownCommand,
        .domain = .cli,
        .phase = .cli_argument_parsing,
        .title = "unknown command",
        .message = command,
        .help = "Run `kira help` to see the supported commands.",
    });
}

pub fn missingFlagValue(flag: []const u8, expected: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL002_MissingCommandArgument,
        .domain = .cli,
        .phase = .cli_argument_parsing,
        .title = "missing command argument",
        .message = try std.fmt.allocPrint(
            std.heap.page_allocator,
            "Option `{s}` requires {s}.",
            .{ flag, expected },
        ),
        .help = "Pass the required value, or run `kira help` to review the command syntax.",
    });
}

pub fn invalidBackendFlag(allocator: std.mem.Allocator, backend: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL005_InvalidBackendFlag,
        .domain = .cli,
        .phase = .cli_argument_parsing,
        .title = "invalid backend flag",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira does not recognize backend `{s}`.",
            .{backend},
        ),
        .help = "Use `vm`, `llvm`, or `hybrid`.",
    });
}

pub fn invalidProjectPath(allocator: std.mem.Allocator, path: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL006_InvalidProjectPath,
        .domain = .cli,
        .phase = .project_discovery,
        .title = "invalid project path",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira could not open `{s}` as a source file, manifest, or project directory.",
            .{path},
        ),
        .help = "Pass a `.kira` source file, a project root, or a `kira.toml`/`project.toml` path.",
    });
}

pub fn invalidCommandTarget(allocator: std.mem.Allocator, command: []const u8, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL010_InvalidCommandTarget,
        .domain = .cli,
        .phase = .target_selection,
        .title = "invalid command target",
        .message = try std.fmt.allocPrint(
            allocator,
            "Command `{s}` cannot use `{s}` as its target.",
            .{ command, target },
        ),
        .help = "Pick a project root, manifest path, source file, or example target that matches the command.",
    });
}

pub fn libraryTargetCannotBeRun(allocator: std.mem.Allocator, target_root: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL020_LibraryTargetCannotBeRun,
        .domain = .cli,
        .phase = .target_selection,
        .title = "library target cannot be run",
        .message = try std.fmt.allocPrint(
            allocator,
            "The selected target `{s}` is a library, so it can be checked or built but not executed.",
            .{target_root},
        ),
        .help = "Run an example target or executable package instead.",
    });
}

pub fn libraryTargetCannotBeStartedInLiveMode(allocator: std.mem.Allocator, target_root: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL021_LibraryTargetCannotBeStartedInLiveMode,
        .domain = .cli,
        .phase = .target_selection,
        .title = "library target cannot be started in live mode",
        .message = try std.fmt.allocPrint(
            allocator,
            "The selected target `{s}` is a library. Live mode requires an example or executable target.",
            .{target_root},
        ),
        .help = "Run `kira live` against a runnable example or application package.",
    });
}

pub fn commandRequiresRunnableTarget(allocator: std.mem.Allocator, command: []const u8, target_kind: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL022_CommandRequiresRunnableTarget,
        .domain = .cli,
        .phase = .target_selection,
        .title = "command requires a runnable target",
        .message = try std.fmt.allocPrint(
            allocator,
            "Command `{s}` requires a runnable target, but this input resolved to `{s}`.",
            .{ command, target_kind },
        ),
        .help = "Point the command at an application package, example, or source file with an `@Main` entrypoint.",
    });
}

pub fn commandRequiresLiveCapableTarget(allocator: std.mem.Allocator, target_kind: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL023_CommandRequiresLiveCapableTarget,
        .domain = .cli,
        .phase = .target_selection,
        .title = "command requires a live-capable target",
        .message = try std.fmt.allocPrint(
            allocator,
            "Live mode cannot start from target kind `{s}`.",
            .{target_kind},
        ),
        .help = "Use an example or executable application target for `kira live`.",
    });
}

pub fn nativeExecutableFailed() diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL026_NativeExecutableFailed,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "native executable failed",
        .message = "Kira built the native executable, but it exited unsuccessfully while running.",
        .help = "Re-run the generated executable directly to inspect the application/runtime failure.",
    });
}

pub fn liveBundleBuildFailed(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL028_LiveBundleBuildFailed,
        .domain = .cli,
        .phase = .backend_prepare,
        .title = "live bundle build failed",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira could not prepare the live bundle for `{s}`.",
            .{target},
        ),
        .help = "Run `kira check` or `kira build` on the same target first to inspect diagnostics, then retry `kira live`.",
    });
}

pub fn liveSessionEndedUnexpectedly(allocator: std.mem.Allocator, err_name: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL029_LiveSessionEndedUnexpectedly,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live session ended unexpectedly",
        .message = try std.fmt.allocPrint(
            allocator,
            "The live session ended before Kira finished its expected smoke-check flow ({s}).",
            .{err_name},
        ),
        .help = "Retry `kira live` without smoke flags to inspect the runner behavior, or use `kira build`/`kira run` to isolate the target failure first.",
    });
}
