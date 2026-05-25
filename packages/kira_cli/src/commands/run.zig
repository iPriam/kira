const std = @import("std");
const builtin = @import("builtin");
const bytecode = @import("kira_bytecode");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const hybrid_runtime = @import("kira_hybrid_runtime");
const package_manager = @import("kira_package_manager");
const runtime_abi = @import("kira_runtime_abi");
const vm_runtime = @import("kira_vm_runtime");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
    build.setTimingsEnabled(parsed.timings or timingsEnvEnabled());
    const previous_trace = runtime_abi.executionTraceEnabled();
    runtime_abi.setExecutionTraceEnabled(parsed.trace_execution);
    defer runtime_abi.setExecutionTraceEnabled(previous_trace);
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
    try support.validateTargetSelection(allocator, stderr, .run, input);
    const backend = parsed.backend orelse input.default_backend orelse .vm;
    const source_path = input.target.source_path.?;

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

    try support.logFrontendStarted(stderr, "run", source_path);
    var system = build.BuildSystem.init(allocator);
    const output_root = try support.outputRoot(allocator, input.target.root_path);
    defer allocator.free(output_root);
    try support.ensurePath(output_root);
    const stem = input.target.project_name orelse std.fs.path.stem(source_path);
    const output_path = try runOutputPath(allocator, output_root, stem, backend);
    const result = try system.build(.{
        .source_path = source_path,
        .output_path = output_path,
        .target = .{ .execution = backend },
    });
    if (result.failed()) {
        try support.logBuildAborted(stderr, "run", result.failure_kind.?, source_path);
        if (result.source) |source| {
            try support.renderDiagnostics(stderr, &source, result.diagnostics);
        }
        return error.CommandFailed;
    }

    switch (backend) {
        .vm => {
            const bytecode_artifact = findBytecode(result.artifacts) orelse return error.MissingBytecodeArtifact;
            const runtime_allocator = std.heap.smp_allocator;
            const module = try bytecode.Module.readFromFile(runtime_allocator, bytecode_artifact.path);
            var vm = vm_runtime.Vm.init(runtime_allocator);
            defer vm.deinit();
            var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
            defer {
                if (input.target.root_path) |_| std.process.setCurrentDir(std.Options.debug_io, original_cwd) catch {};
                original_cwd.close(std.Options.debug_io);
            }
            if (input.target.root_path) |root| {
                var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, root, .{});
                defer dir.close(std.Options.debug_io);
                try std.process.setCurrentDir(std.Options.debug_io, dir);
            }
            try vm.runMain(&module, stdout);
            if (runtimeMemoryReportEnabled()) vm.emitMemoryReport("vm");
            if (runtimeMemoryDetailEnabled()) vm.emitMemoryDetail();
        },
        .llvm_native => {
            const executable = findExecutable(result.artifacts) orelse return error.MissingExecutableArtifact;
            try runExecutable(allocator, executable.path, input.target.root_path, parsed.trace_execution, stdout, stderr);
        },
        .hybrid => {
            const manifest_artifact = findHybridManifest(result.artifacts) orelse return error.MissingHybridManifestArtifact;
            const runtime_allocator = std.heap.smp_allocator;
            const manifest = try hybrid_runtime.loadHybridModule(runtime_allocator, manifest_artifact.path);
            var runtime = try hybrid_runtime.HybridRuntime.init(runtime_allocator, manifest);
            defer runtime.deinit();
            var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
            defer {
                if (input.target.root_path) |_| std.process.setCurrentDir(std.Options.debug_io, original_cwd) catch {};
                original_cwd.close(std.Options.debug_io);
            }
            if (input.target.root_path) |root| {
                var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, root, .{});
                defer dir.close(std.Options.debug_io);
                try std.process.setCurrentDir(std.Options.debug_io, dir);
            }
            runtime.run() catch |err| {
                if (err == error.RuntimeFailure) {
                    if (runtime.vm.lastError()) |message| {
                        try stderr.print("hybrid runtime failure: {s}\n", .{message});
                        return error.CommandFailed;
                    }
                }
                return err;
            };
            if (runtimeMemoryReportEnabled()) runtime.vm.emitMemoryReport("hybrid");
            if (runtimeMemoryDetailEnabled()) runtime.vm.emitMemoryDetail();
        },
    }
}

const ParsedArgs = struct {
    backend: ?build_def.ExecutionTarget = null,
    offline: bool = false,
    locked: bool = false,
    trace_execution: bool = false,
    timings: bool = false,
    input_path: []const u8,
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    var backend: ?build_def.ExecutionTarget = null;
    var offline = false;
    var locked = false;
    var trace_execution = false;
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
        if (std.mem.eql(u8, arg, "--trace-execution")) {
            trace_execution = true;
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
        .trace_execution = trace_execution,
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

fn runtimeMemoryReportEnabled() bool {
    if (!builtin.link_libc) return false;
    const raw = std.c.getenv("KIRA_RUNTIME_MEMORY_REPORT") orelse return false;
    const value = std.mem.span(raw);
    return value.len != 0 and !std.mem.eql(u8, value, "0") and !std.mem.eql(u8, value, "false");
}

fn runtimeMemoryDetailEnabled() bool {
    if (!builtin.link_libc) return false;
    const raw = std.c.getenv("KIRA_RUNTIME_MEMORY_DETAIL") orelse return false;
    const value = std.mem.span(raw);
    return value.len != 0 and !std.mem.eql(u8, value, "0") and !std.mem.eql(u8, value, "false");
}

fn parseBackend(arg: []const u8) ?build_def.ExecutionTarget {
    if (std.mem.eql(u8, arg, "vm")) return .vm;
    if (std.mem.eql(u8, arg, "llvm")) return .llvm_native;
    if (std.mem.eql(u8, arg, "hybrid")) return .hybrid;
    return null;
}

fn findExecutable(artifacts: []const build_def.Artifact) ?build_def.Artifact {
    for (artifacts) |artifact| {
        if (artifact.kind == .executable) return artifact;
    }
    return null;
}

fn findBytecode(artifacts: []const build_def.Artifact) ?build_def.Artifact {
    for (artifacts) |artifact| {
        if (artifact.kind == .bytecode) return artifact;
    }
    return null;
}

fn findHybridManifest(artifacts: []const build_def.Artifact) ?build_def.Artifact {
    for (artifacts) |artifact| {
        if (artifact.kind == .hybrid_manifest) return artifact;
    }
    return null;
}

fn runOutputPath(allocator: std.mem.Allocator, output_root: []const u8, stem: []const u8, backend: build_def.ExecutionTarget) ![]const u8 {
    return switch (backend) {
        .vm => std.fmt.allocPrint(allocator, "{s}/{s}.run.kbc", .{ output_root, stem }),
        .llvm_native => std.fmt.allocPrint(allocator, "{s}/{s}.run{s}", .{ output_root, stem, build.executableExtension() }),
        .hybrid => std.fmt.allocPrint(allocator, "{s}/{s}.run.khm", .{ output_root, stem }),
    };
}

fn runExecutable(
    allocator: std.mem.Allocator,
    path: []const u8,
    project_root: ?[]const u8,
    trace_execution: bool,
    stdout: anytype,
    stderr: anytype,
) !void {
    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    var environ_map = if (trace_execution) try std.process.Environ.createMap(process_environ, allocator) else null;
    defer if (environ_map) |*map| map.deinit();
    if (environ_map) |*map| {
        try map.put("KIRA_TRACE_EXECUTION", "1");
    }
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = &.{path},
        .cwd = if (project_root) |root| .{ .path = root } else .inherit,
        .environ_map = if (environ_map) |*map| map else null,
    });
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);

    if (result.stdout.len > 0) try stdout.writeAll(result.stdout);
    if (result.term == .exited and result.term.exited == 0) {
        if (result.stderr.len > 0) try stderr.writeAll(result.stderr);
        return;
    }

    try stderr.print("native executable failed: {s}\n", .{path});
    switch (result.term) {
        .exited => |code| try stderr.print("  exit code: {d}\n", .{code}),
        .signal => |signal| try stderr.print("  signal: {d}\n", .{signal}),
        .stopped => |signal| try stderr.print("  stopped by signal: {d}\n", .{signal}),
        .unknown => |code| try stderr.print("  status: {d}\n", .{code}),
    }
    if (result.stderr.len > 0) {
        try stderr.writeAll("  stderr:\n");
        try stderr.writeAll(result.stderr);
        if (result.stderr[result.stderr.len - 1] != '\n') try stderr.writeAll("\n");
    } else {
        try stderr.writeAll("  stderr: <empty>\n");
    }
    if (result.stdout.len > 0) {
        try stderr.writeAll("  stdout:\n");
        try stderr.writeAll(result.stdout);
        if (result.stdout[result.stdout.len - 1] != '\n') try stderr.writeAll("\n");
    }
    if (project_root) |cwd| {
        try stderr.print("  cwd: {s}\n", .{cwd});
    }
    try stderr.print("  command: {s}\n", .{path});
    if (builtin.os.tag == .windows and result.term == .exited and result.term.exited == 9) {
        try stderr.writeAll("  note: Windows may report native fail-fast statuses through the low exit byte; running the executable directly can reveal the full NTSTATUS.\n");
    }
    return error.NativeRunFailed;
}

fn inheritedProcessEnviron() std.process.Environ {
    return switch (builtin.os.tag) {
        .windows => .{ .block = .global },
        .wasi, .emscripten, .freestanding, .other => .empty,
        else => .{ .block = .{ .slice = currentPosixEnvironBlock() } },
    };
}

fn currentPosixEnvironBlock() [:null]const ?[*:0]const u8 {
    if (!builtin.link_libc) return &.{};

    const environ = std.c.environ;
    var len: usize = 0;
    while (environ[len] != null) : (len += 1) {}
    return environ[0..len :null];
}

test "parseArgs recognizes trace execution" {
    const parsed = try parseArgs(&.{ "--trace-execution", "--backend", "hybrid", "examples/hello.kira" });
    try std.testing.expect(parsed.trace_execution);
    try std.testing.expectEqual(build_def.ExecutionTarget.hybrid, parsed.backend.?);
    try std.testing.expectEqualStrings("examples/hello.kira", parsed.input_path);
}
