const std = @import("std");
const diag_messages = @import("kira_diagnostic_messages");
const cmd_run = @import("commands/run.zig");
const cmd_build = @import("commands/build.zig");
const cmd_check = @import("commands/check.zig");
const cmd_tokens = @import("commands/tokens.zig");
const cmd_ast = @import("commands/ast.zig");
const cmd_new = @import("commands/new.zig");
const cmd_sync = @import("commands/sync.zig");
const cmd_add = @import("commands/add.zig");
const cmd_remove = @import("commands/remove.zig");
const cmd_update = @import("commands/update.zig");
const cmd_package = @import("commands/package.zig");
const cmd_fetch_llvm = @import("commands/fetch_llvm.zig");
const cmd_shader = @import("commands/shader.zig");
const cmd_instruments = @import("commands/instruments.zig");
const cmd_live = @import("commands/live.zig");
const support = @import("support.zig");
const cli_parser = @import("parse/Parser.zig");
const help_text = @import("command/HelpText.zig");
const CommandKind = @import("command/CommandKind.zig").CommandKind;
const ParsedCommand = @import("command/ParsedCommand.zig").ParsedCommand;
const command_args = @import("dispatch/CommandArgs.zig");

pub fn run(allocator: std.mem.Allocator, args: []const []const u8) !u8 {
    var stdout_buffer: [4096]u8 = undefined;
    var stderr_buffer: [4096]u8 = undefined;
    var stdout = std.Io.File.stdout().writer(std.Options.debug_io, &stdout_buffer);
    defer stdout.interface.flush() catch {};
    var stderr = std.Io.File.stderr().writer(std.Options.debug_io, &stderr_buffer);
    defer stderr.interface.flush() catch {};

    return runWithWriters(allocator, args, &stdout.interface, &stderr.interface);
}

pub fn runWithWriters(allocator: std.mem.Allocator, args: []const []const u8, out: anytype, err: anytype) !u8 {
    const parsed = try cli_parser.parse(allocator, args);
    switch (parsed) {
        .failure => |failure| {
            try support.renderStandaloneDiagnostic(err, failure.diagnostic);
            if (failure.command_for_help) |kind| {
                try err.writeAll("\n");
                try help_text.print(err, kind);
            }
            return 1;
        },
        .command => |command| return dispatchParsedCommand(allocator, command, out, err),
    }
}

fn executeCommand(allocator: std.mem.Allocator, command: []const u8, args: []const []const u8, out: anytype, err: anytype, comptime execute: anytype) !u8 {
    execute(allocator, args, out, err) catch |run_err| {
        if (run_err == error.CommandFailed or run_err == error.InvalidArguments) {
            if (run_err == error.InvalidArguments) try help_text.print(err, null);
            return 1;
        }
        if (run_err == error.ProjectEntrypointNotFound) {
            try support.renderStandaloneDiagnostic(err, try diag_messages.PackageMessages.missingSourceFile(allocator, support.defaultCommandInputPath()));
            return 1;
        }
        if (run_err == error.UnsupportedTarget) {
            try support.renderStandaloneDiagnostic(err, try diag_messages.ToolchainMessages.unsupportedHostTarget(allocator, support.currentHostTargetTriple()));
            return 1;
        }
        if (run_err == error.NativeRunFailed) {
            try support.renderStandaloneDiagnostic(err, diag_messages.CliMessages.nativeExecutableFailed());
            return 1;
        }
        if (run_err == error.MacOSSdkUnavailable) {
            try support.renderStandaloneDiagnostic(err, try diag_messages.ToolchainMessages.invalidToolchainActivation(allocator, @errorName(run_err)));
            return 1;
        }
        if (run_err == error.IPhoneOSSdkUnavailable) {
            try support.renderStandaloneDiagnostic(err, try diag_messages.ToolchainMessages.invalidToolchainActivation(allocator, @errorName(run_err)));
            return 1;
        }
        if (std.mem.eql(u8, command, "live") and
            (run_err == error.EndOfStream or run_err == error.LiveHealthCheckFailed or run_err == error.LiveBundleBuildFailed))
        {
            try support.renderStandaloneDiagnostic(err, try diag_messages.CliMessages.liveSessionEndedUnexpectedly(allocator, @errorName(run_err)));
            return 1;
        }
        try support.logInternalCompilerError(err, @errorName(run_err));
        try support.renderInternalCompilerError(err, @errorName(run_err));
        return 1;
    };
    return 0;
}

fn dispatchParsedCommand(
    allocator: std.mem.Allocator,
    command: ParsedCommand,
    out: anytype,
    err: anytype,
) !u8 {
    switch (command) {
        .help => |options| {
            try help_text.print(out, options.command);
            return 0;
        },
        .version => {
            try out.print("{s} {s}\n", .{ support.binaryName(), support.versionString() });
            return 0;
        },
        else => {},
    }
    const kind = command.kind();
    const args = try command_args.toArgs(allocator, command);
    return dispatchCommand(allocator, kind, kind.label(), args, out, err);
}

fn dispatchCommand(
    allocator: std.mem.Allocator,
    kind: CommandKind,
    command: []const u8,
    args: []const []const u8,
    out: anytype,
    err: anytype,
) !u8 {
    return switch (kind) {
        .run => executeCommand(allocator, command, args, out, err, cmd_run.execute),
        .fetch_llvm => executeCommand(allocator, command, args, out, err, cmd_fetch_llvm.execute),
        .tokens => executeCommand(allocator, command, args, out, err, cmd_tokens.execute),
        .ast => executeCommand(allocator, command, args, out, err, cmd_ast.execute),
        .check => executeCommand(allocator, command, args, out, err, cmd_check.execute),
        .build => executeCommand(allocator, command, args, out, err, cmd_build.execute),
        .instruments => executeCommand(allocator, command, args, out, err, cmd_instruments.execute),
        .instrument_artifact => executeCommand(allocator, command, args, out, err, cmd_instruments.executeArtifact),
        .run_hybrid_artifact => executeCommand(allocator, command, args, out, err, cmd_run.executeHybridArtifact),
        .live_runner => executeCommand(allocator, command, args, out, err, cmd_live.executeRunner),
        .shader => executeCommand(allocator, command, args, out, err, cmd_shader.execute),
        .new => executeCommand(allocator, command, args, out, err, cmd_new.execute),
        .sync => executeCommand(allocator, command, args, out, err, cmd_sync.execute),
        .add => executeCommand(allocator, command, args, out, err, cmd_add.execute),
        .remove => executeCommand(allocator, command, args, out, err, cmd_remove.execute),
        .update => executeCommand(allocator, command, args, out, err, cmd_update.execute),
        .package => executeCommand(allocator, command, args, out, err, cmd_package.execute),
        .live => executeCommand(allocator, command, args, out, err, cmd_live.execute),
        .help, .version => unreachable,
    };
}

test "invalid Kira input exits cleanly with rendered diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "DemoApp/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/project.toml",
        .data = "[project]\n" ++
            "name = \"DemoApp\"\n" ++
            "version = \"0.1.0\"\n\n" ++
            "[defaults]\n" ++
            "execution_mode = \"vm\"\n" ++
            "build_target = \"host\"\n",
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/app/main.kira",
        .data = "@Main\nfunction main() { let x = ; }\n",
    });
    const path = try tmp.dir.realPathFileAlloc(std.testing.io, "DemoApp", arena.allocator());

    var stdout_buffer: [512]u8 = undefined;
    var stderr_buffer: [4096]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    const exit_code = try runWithWriters(
        arena.allocator(),
        &.{ "kirac", "check", path },
        &stdout,
        &stderr,
    );

    try std.testing.expectEqual(@as(u8, 1), exit_code);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "error[KPAR002]: expected expression") != null);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "panic") == null);
}

test "invalid hybrid input exits cleanly without renderer crash" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "DemoApp/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/project.toml",
        .data = "[project]\n" ++
            "name = \"DemoApp\"\n" ++
            "version = \"0.1.0\"\n\n" ++
            "[defaults]\n" ++
            "execution_mode = \"hybrid\"\n" ++
            "build_target = \"host\"\n",
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/app/main.kira",
        .data = "@Main\n" ++
            "@Native\n" ++
            "function main() {\n" ++
            "    print(\"native main\");\n" ++
            "    runtime_helper(\n" ++
            "    return;\n" ++
            "}\n" ++
            "@Runtime\n" ++
            "function runtime_helper() {\n" ++
            "    return;\n" ++
            "}\n",
    });
    const path = try tmp.dir.realPathFileAlloc(std.testing.io, "DemoApp", arena.allocator());

    var stdout_buffer: [512]u8 = undefined;
    var stderr_buffer: [4096]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    const exit_code = try runWithWriters(
        arena.allocator(),
        &.{ "kirac", "run", "--backend", "hybrid", path },
        &stdout,
        &stderr,
    );

    try std.testing.expectEqual(@as(u8, 1), exit_code);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "error[KPAR002]: expected expression") != null);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "panic") == null);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "Segmentation fault") == null);
}

test "version prints standalone binary identity" {
    var stdout_buffer: [128]u8 = undefined;
    var stderr_buffer: [128]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    const exit_code = try runWithWriters(
        std.testing.allocator,
        &.{ "kirac", "--version" },
        &stdout,
        &stderr,
    );

    try std.testing.expectEqual(@as(u8, 0), exit_code);
    try std.testing.expectEqualStrings("kira-bootstrapper 0.1.0\n", stdout.buffered());
    try std.testing.expectEqualStrings("", stderr.buffered());
}

test "run defaults to project.toml in the current directory" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "DemoApp/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/project.toml",
        .data = "[project]\n" ++
            "name = \"DemoApp\"\n" ++
            "version = \"0.1.0\"\n\n" ++
            "[defaults]\n" ++
            "execution_mode = \"vm\"\n" ++
            "build_target = \"host\"\n",
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/app/main.kira",
        .data = "@Main\nfunction main() { let x = ; }\n",
    });

    var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
    defer {
        std.process.setCurrentDir(std.testing.io, original_cwd) catch {};
        original_cwd.close(std.Options.debug_io);
    }

    var app_dir = try tmp.dir.openDir(std.testing.io, "DemoApp", .{});
    defer app_dir.close(std.Options.debug_io);
    try std.process.setCurrentDir(std.testing.io, app_dir);

    var stdout_buffer: [512]u8 = undefined;
    var stderr_buffer: [4096]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    const exit_code = try runWithWriters(
        arena.allocator(),
        &.{ "kirac", "run" },
        &stdout,
        &stderr,
    );

    try std.testing.expectEqual(@as(u8, 1), exit_code);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "error[KPAR002]: expected expression") != null);
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "invalid Kira input") == null);
}

test "vm run launches bytecode produced by the build pipeline" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "DemoApp/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/project.toml",
        .data = "[project]\n" ++
            "name = \"DemoApp\"\n" ++
            "version = \"0.1.0\"\n\n" ++
            "[defaults]\n" ++
            "execution_mode = \"vm\"\n" ++
            "build_target = \"host\"\n",
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/app/main.kira",
        .data = "@Main\nfunction main() { print(\"ok\"); return; }\n",
    });
    const path = try tmp.dir.realPathFileAlloc(std.testing.io, "DemoApp", arena.allocator());

    var stdout_buffer: [512]u8 = undefined;
    var stderr_buffer: [4096]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    const exit_code = try runWithWriters(
        arena.allocator(),
        &.{ "kirac", "run", "--backend", "vm", path },
        &stdout,
        &stderr,
    );

    const output_root = try support.outputRoot(arena.allocator(), path);
    const artifact_path = try std.fs.path.join(arena.allocator(), &.{ output_root, "DemoApp.run.kbc" });
    var artifact = try std.Io.Dir.openFileAbsolute(std.Options.debug_io, artifact_path, .{});
    artifact.close(std.Options.debug_io);

    try std.testing.expectEqual(@as(u8, 0), exit_code);
    try std.testing.expect(std.mem.indexOf(u8, stdout.buffered(), "ok") != null);
}
