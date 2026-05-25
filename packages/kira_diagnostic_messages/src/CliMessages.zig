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

pub fn invalidFlagValue(allocator: std.mem.Allocator, flag: []const u8, value: []const u8, expected: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL003_InvalidFlagValue,
        .domain = .cli,
        .phase = .cli_argument_parsing,
        .title = "invalid flag value",
        .message = try std.fmt.allocPrint(
            allocator,
            "Option `{s}` does not accept value `{s}`.",
            .{ flag, value },
        ),
        .help = try std.fmt.allocPrint(allocator, "Expected {s}.", .{expected}),
    });
}

pub fn invalidDurationFlag(allocator: std.mem.Allocator, flag: []const u8, value: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL030_InvalidDurationFlag,
        .domain = .cli,
        .phase = .cli_argument_parsing,
        .title = "invalid duration flag",
        .message = try std.fmt.allocPrint(
            allocator,
            "Option `{s}` does not accept duration `{s}`.",
            .{ flag, value },
        ),
        .help = "Use a positive duration like `5s`, `5000ms`, or plain integer seconds.",
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

pub fn liveSmokeUnsupportedTarget(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL031_LiveSmokeUnsupportedTarget,
        .domain = .cli,
        .phase = .target_selection,
        .title = "live target is not bundle-compatible",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira cannot start a live session for `{s}` because the target or one of its packages is not currently compatible with live bundle generation.",
            .{target},
        ),
        .help = "Use `kira check` and `kira build` for this target, or update the package so it can be lowered into a live bundle before retrying `kira live`.",
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

pub fn invalidLivePlatform(allocator: std.mem.Allocator, value: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL041_InvalidLivePlatform,
        .domain = .cli,
        .phase = .cli_argument_parsing,
        .title = "invalid live platform",
        .message = try std.fmt.allocPrint(
            allocator,
            "`{s}` is not a supported live platform.",
            .{value},
        ),
        .help = "Use `desktop`, `ios`, `ios-simulator`, or `ios-device`. Use `kira live <target>` when omitting the platform.",
    });
}

pub fn liveRunnerBuildRootMissing(allocator: std.mem.Allocator, attempted_cwd: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL032_LiveRunnerBuildRootMissing,
        .domain = .cli,
        .phase = .backend_prepare,
        .title = "live runner build root is unavailable",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira could not locate a runner build root while preparing the desktop live runner. The user invocation cwd was `{s}`.",
            .{attempted_cwd},
        ),
        .help = "Run `zig build` in the Kira toolchain repository so the live runner can be built or installed, then retry `kira live`.",
    });
}

pub fn liveServerFailedToStart(allocator: std.mem.Allocator, host: []const u8, port: u16) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL033_LiveServerFailedToStart,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live server failed to start",
        .message = try std.fmt.allocPrint(allocator, "Kira could not bind the live server at {s}:{d}.", .{ host, port }),
        .help = "Check for another live server using the same port, then retry the command.",
    });
}

pub fn liveClientFailedToConnect(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL034_LiveClientFailedToConnect,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live client failed to connect",
        .message = try std.fmt.allocPrint(allocator, "The desktop live runner for `{s}` did not connect to the live server in time.", .{target}),
        .help = "Re-run the command and inspect runner output; if this repeats, rebuild the Kira toolchain with `zig build`.",
    });
}

pub fn liveEntrypointDidNotStart(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL037_LiveEntrypointDidNotStart,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live entrypoint did not start",
        .message = try std.fmt.allocPrint(allocator, "The live client connected for `{s}`, but the app entrypoint did not start.", .{target}),
        .help = "Run `kira build` for the same target and inspect runtime or native bridge diagnostics.",
    });
}

pub fn liveFrameNotPresented(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL038_LiveFrameNotPresented,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live frame was not presented",
        .message = try std.fmt.allocPrint(allocator, "The live client started `{s}`, but no rendered frame was acknowledged.", .{target}),
        .help = "Use a renderable desktop target, or run a non-rendering target with an explicit headless/smoke mode once one is available.",
    });
}

pub fn liveReloadTimedOut(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL039_LiveReloadTimedOut,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live reload timed out",
        .message = try std.fmt.allocPrint(allocator, "A source change was detected for `{s}`, but the client did not complete hot restart.", .{target}),
        .help = "Check the source edit for compiler diagnostics, then retry the live session.",
    });
}

pub fn liveRunnerExitedEarly(allocator: std.mem.Allocator, target: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL044_LiveRunnerExitedEarly,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "live runner exited early",
        .message = try std.fmt.allocPrint(allocator, "The live runner for `{s}` exited before the live session became ready.", .{target}),
        .help = "Inspect runner logs above for the runtime failure and rerun `kira build` for the same target.",
    });
}

pub fn iosLiveUnsupported(allocator: std.mem.Allocator, platform: []const u8, detail: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KCL046_IosLiveUnsupported,
        .domain = .cli,
        .phase = .runtime_execution,
        .title = "iOS live runner is not implemented yet",
        .message = try std.fmt.allocPrint(allocator, "Kira recognized platform `{s}`, but this build cannot launch an iOS live client yet. {s}", .{ platform, detail }),
        .help = "Use `kira live desktop <target>` today. iOS simulator support needs a simulator runner build, install, launch, and protocol connection path.",
    });
}

pub fn missingXcodeTools(allocator: std.mem.Allocator, tool: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KTC020_MissingXcodeTools,
        .domain = .toolchain,
        .phase = .toolchain_activation,
        .title = "missing Xcode command line tool",
        .message = try std.fmt.allocPrint(allocator, "Kira could not find `{s}` while auditing iOS live support.", .{tool}),
        .help = "Install Xcode and select it with `xcode-select`, then retry `kira live ios-simulator <target>`.",
    });
}

pub fn missingIosSimulatorRuntime(allocator: std.mem.Allocator, detail: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KTC021_MissingIosSimulatorRuntime,
        .domain = .toolchain,
        .phase = .toolchain_activation,
        .title = "missing iOS simulator runtime",
        .message = try std.fmt.allocPrint(allocator, "Kira could not find an available iOS simulator runtime. {s}", .{detail}),
        .help = "Install an iOS simulator runtime in Xcode Settings > Platforms, then retry.",
    });
}
