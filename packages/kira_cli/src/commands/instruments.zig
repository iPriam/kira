const std = @import("std");
const builtin = @import("builtin");
const bytecode = @import("kira_bytecode");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const diagnostics = @import("kira_diagnostics");
const hybrid_runtime = @import("kira_hybrid_runtime");
const instruments = @import("kira_instruments");
const package_manager = @import("kira_package_manager");
const runtime_abi = @import("kira_runtime_abi");
const vm_runtime = @import("kira_vm_runtime");
const support = @import("../support.zig");

const InstrumentKind = instruments.InstrumentKind;
const InstrumentBackend = instruments.InstrumentBackend;

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    if (args.len == 0 or !std.mem.eql(u8, args[0], "run")) {
        try stderr.writeAll("error: expected `kira instruments run <target>`\n");
        return error.InvalidArguments;
    }

    const run_args = args[1..];
    const parsed = parseRunArgs(allocator, run_args) catch |err| {
        if (err == error.InvalidArguments) try diagnoseRunArgError(run_args, stderr);
        return err;
    };
    defer allocator.free(parsed.tracks);

    if (parsed.tracks.len == 0) {
        try stderr.writeAll("error: at least one --track memory|cpu option is required\n");
        return error.InvalidArguments;
    }
    if (builtin.os.tag != .windows and hasTrack(parsed.tracks, .memory)) {
        try stderr.writeAll("error: memory instrumentation is not implemented on this platform\n");
        return error.CommandFailed;
    }

    const report = try runInstrumented(allocator, parsed, stderr);
    try report.writeHuman(stdout);
    if (parsed.json_out) |path| try writeJsonReport(path, report);

    if (report.status == .fail) return error.CommandFailed;
}

pub fn executeArtifact(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    _ = allocator;
    const parsed = try parseArtifactArgs(args);
    var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
    defer {
        if (parsed.cwd) |_| std.process.setCurrentDir(std.Options.debug_io, original_cwd) catch {};
        original_cwd.close(std.Options.debug_io);
    }
    if (parsed.cwd) |cwd| {
        var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, cwd, .{});
        defer dir.close(std.Options.debug_io);
        try std.process.setCurrentDir(std.Options.debug_io, dir);
    }

    switch (parsed.backend) {
        .runtime => {
            const runtime_allocator = std.heap.smp_allocator;
            const module = try bytecode.Module.readFromFile(runtime_allocator, parsed.artifact_path);
            var vm = vm_runtime.Vm.init(runtime_allocator);
            defer vm.deinit();
            try vm.runMain(&module, stdout);
        },
        .hybrid => {
            const runtime_allocator = std.heap.smp_allocator;
            const manifest = try hybrid_runtime.loadHybridModule(runtime_allocator, parsed.artifact_path);
            var runtime = try hybrid_runtime.HybridRuntime.init(runtime_allocator, manifest);
            defer runtime.deinit();
            runtime.run() catch |err| {
                if (err == error.RuntimeFailure) {
                    if (runtime.vm.lastError()) |message| {
                        try stderr.print("hybrid runtime failure: {s}\n", .{message});
                        return error.CommandFailed;
                    }
                }
                return err;
            };
        },
        .llvm => return error.InvalidArguments,
    }
}

const ParsedRunArgs = struct {
    target: []const u8,
    backend: InstrumentBackend,
    tracks: []InstrumentKind,
    duration_ns: u64,
    sample_rate_hz: f64,
    fail_on_growth_bytes: ?u64 = null,
    json_out: ?[]const u8 = null,
};

fn parseRunArgs(allocator: std.mem.Allocator, args: []const []const u8) !ParsedRunArgs {
    var target: ?[]const u8 = null;
    var backend: ?InstrumentBackend = null;
    var track_list = std.array_list.Managed(InstrumentKind).init(allocator);
    errdefer track_list.deinit();
    var duration_ns: ?u64 = null;
    var sample_rate_hz: ?f64 = null;
    var fail_on_growth_bytes: ?u64 = null;
    var json_out: ?[]const u8 = null;

    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            backend = parseBackend(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--track")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            const track = parseTrack(args[index]) orelse return error.InvalidArguments;
            if (!hasTrack(track_list.items, track)) try track_list.append(track);
            continue;
        }
        if (std.mem.eql(u8, arg, "--duration")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            duration_ns = parseDurationNs(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--sample-rate")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            sample_rate_hz = parseSampleRateHz(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--fail-on-growth")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            fail_on_growth_bytes = parseByteSize(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--json-out")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            json_out = args[index];
            continue;
        }
        if (std.mem.startsWith(u8, arg, "--")) return error.InvalidArguments;
        if (target != null) return error.InvalidArguments;
        target = arg;
    }

    return .{
        .target = target orelse return error.InvalidArguments,
        .backend = backend orelse .runtime,
        .tracks = try track_list.toOwnedSlice(),
        .duration_ns = duration_ns orelse return error.InvalidArguments,
        .sample_rate_hz = sample_rate_hz orelse return error.InvalidArguments,
        .fail_on_growth_bytes = fail_on_growth_bytes,
        .json_out = json_out,
    };
}

fn diagnoseRunArgError(args: []const []const u8, stderr: anytype) !void {
    var saw_target = false;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return stderr.writeAll("error: --backend expects runtime, llvm, or hybrid\n");
            if (parseBackend(args[index]) == null) {
                return stderr.print("error: invalid backend `{s}`; expected runtime, llvm, or hybrid\n", .{args[index]});
            }
            continue;
        }
        if (std.mem.eql(u8, arg, "--track")) {
            index += 1;
            if (index >= args.len) return stderr.writeAll("error: --track expects memory or cpu\n");
            if (parseTrack(args[index]) == null) {
                return stderr.print("error: invalid track `{s}`; expected memory or cpu\n", .{args[index]});
            }
            continue;
        }
        if (std.mem.eql(u8, arg, "--duration")) {
            index += 1;
            if (index >= args.len) return stderr.writeAll("error: --duration expects a value such as 30s, 1m, or 500ms\n");
            if (parseDurationNs(args[index]) == null) {
                return stderr.print("error: invalid duration `{s}`; expected values like 30s, 1m, or 500ms\n", .{args[index]});
            }
            continue;
        }
        if (std.mem.eql(u8, arg, "--sample-rate")) {
            index += 1;
            if (index >= args.len) return stderr.writeAll("error: --sample-rate expects a value such as 10hz or 2.5hz\n");
            if (parseSampleRateHz(args[index]) == null) {
                return stderr.print("error: invalid sample rate `{s}`; expected values like 10hz or 2.5hz\n", .{args[index]});
            }
            continue;
        }
        if (std.mem.eql(u8, arg, "--fail-on-growth")) {
            index += 1;
            if (index >= args.len) return stderr.writeAll("error: --fail-on-growth expects a byte size such as 10mb, 512kb, or 1048576\n");
            if (parseByteSize(args[index]) == null) {
                return stderr.print("error: invalid byte size `{s}`; expected values like 10mb, 512kb, or 1048576\n", .{args[index]});
            }
            continue;
        }
        if (std.mem.eql(u8, arg, "--json-out")) {
            index += 1;
            if (index >= args.len) return stderr.writeAll("error: --json-out expects an output path\n");
            continue;
        }
        if (std.mem.startsWith(u8, arg, "--")) {
            return stderr.print("error: unknown instruments option `{s}`\n", .{arg});
        }
        if (saw_target) {
            return stderr.print("error: unexpected extra target `{s}`\n", .{arg});
        }
        saw_target = true;
    }

    if (!saw_target) return stderr.writeAll("error: missing target for `kira instruments run <target>`\n");
    return stderr.writeAll("error: invalid instruments arguments\n");
}

fn parseBackend(value: []const u8) ?InstrumentBackend {
    if (std.mem.eql(u8, value, "runtime")) return .runtime;
    if (std.mem.eql(u8, value, "llvm")) return .llvm;
    if (std.mem.eql(u8, value, "hybrid")) return .hybrid;
    return null;
}

fn parseTrack(value: []const u8) ?InstrumentKind {
    if (std.mem.eql(u8, value, "memory")) return .memory;
    if (std.mem.eql(u8, value, "cpu")) return .cpu;
    return null;
}

fn parseDurationNs(value: []const u8) ?u64 {
    return parseUnitValue(value, &.{
        .{ .suffix = "ms", .scale = std.time.ns_per_ms },
        .{ .suffix = "s", .scale = std.time.ns_per_s },
        .{ .suffix = "m", .scale = std.time.ns_per_min },
    });
}

fn parseSampleRateHz(value: []const u8) ?f64 {
    if (!std.mem.endsWith(u8, value, "hz")) return null;
    const number = value[0 .. value.len - 2];
    if (number.len == 0) return null;
    const parsed = std.fmt.parseFloat(f64, number) catch return null;
    if (parsed <= 0 or !std.math.isFinite(parsed)) return null;
    return parsed;
}

fn parseByteSize(value: []const u8) ?u64 {
    return parseUnitValue(value, &.{
        .{ .suffix = "kb", .scale = 1024 },
        .{ .suffix = "mb", .scale = 1024 * 1024 },
        .{ .suffix = "gb", .scale = 1024 * 1024 * 1024 },
        .{ .suffix = "b", .scale = 1 },
        .{ .suffix = "", .scale = 1 },
    });
}

const Unit = struct {
    suffix: []const u8,
    scale: u64,
};

fn parseUnitValue(value: []const u8, units: []const Unit) ?u64 {
    for (units) |unit| {
        if (!std.mem.endsWith(u8, value, unit.suffix)) continue;
        const number = value[0 .. value.len - unit.suffix.len];
        if (number.len == 0) return null;
        const parsed = std.fmt.parseFloat(f64, number) catch return null;
        if (parsed < 0 or !std.math.isFinite(parsed)) return null;
        const scaled = parsed * @as(f64, @floatFromInt(unit.scale));
        if (scaled > @as(f64, @floatFromInt(std.math.maxInt(u64)))) return null;
        return @intFromFloat(scaled);
    }
    return null;
}

fn runInstrumented(allocator: std.mem.Allocator, parsed: ParsedRunArgs, stderr: anytype) !instruments.Report {
    const input = try support.resolveCommandInput(allocator, parsed.target);
    const backend = toExecutionTarget(parsed.backend);

    if (input.project_root) |project_root| {
        var package_diagnostics = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        _ = package_manager.syncProject(allocator, project_root, support.versionString(), .{
            .offline = false,
            .locked = false,
        }, &package_diagnostics) catch |err| {
            if (err == error.DiagnosticsEmitted) {
                try support.renderStandaloneDiagnostics(stderr, package_diagnostics.items);
                return error.CommandFailed;
            }
            return err;
        };
    }

    try support.logFrontendStarted(stderr, "instruments run", input.source_path);
    var system = build.BuildSystem.init(allocator);
    const output_root = try support.outputRoot(allocator, input.project_root);
    defer allocator.free(output_root);
    try support.ensurePath(output_root);

    const output_path = try instrumentOutputPath(
        allocator,
        output_root,
        input.project_name orelse std.fs.path.stem(input.source_path),
        backend,
    );
    const result = try system.build(.{
        .source_path = input.source_path,
        .output_path = output_path,
        .target = .{ .execution = backend },
    });
    if (result.failed()) {
        try support.logBuildAborted(stderr, "instruments run", result.failure_kind.?, input.source_path);
        if (result.source) |source| try support.renderDiagnostics(stderr, &source, result.diagnostics);
        return error.CommandFailed;
    }

    const artifact_path = switch (backend) {
        .vm => findArtifact(result.artifacts, .bytecode) orelse return error.MissingBytecodeArtifact,
        .llvm_native => findArtifact(result.artifacts, .executable) orelse return error.MissingExecutableArtifact,
        .hybrid => findArtifact(result.artifacts, .hybrid_manifest) orelse return error.MissingHybridManifestArtifact,
    }.path;

    const argv = try childArgv(allocator, parsed.backend, artifact_path);
    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const io = io_impl.io();
    var child = try std.process.spawn(io, .{
        .argv = argv,
        .cwd = if (input.project_root) |root| .{ .path = root } else .inherit,
        .stdin = .ignore,
        .stdout = .ignore,
        .stderr = .ignore,
    });

    const run_result = try sampleChild(allocator, io, &child, parsed);
    const memory_report = if (hasTrack(parsed.tracks, .memory))
        run_result.memory.finish(parsed.fail_on_growth_bytes)
    else
        null;
    const cpu_report = if (hasTrack(parsed.tracks, .cpu)) run_result.cpu.finish() else null;
    const reasons = try instruments.appendFailureReasons(allocator, memory_report, run_result.process);
    const status: instruments.InstrumentStatus = if (reasons.len == 0) .pass else .fail;

    return .{
        .target = parsed.target,
        .backend = parsed.backend,
        .tracks = parsed.tracks,
        .duration_seconds = @as(f64, @floatFromInt(parsed.duration_ns)) / @as(f64, @floatFromInt(std.time.ns_per_s)),
        .sample_rate_hz = parsed.sample_rate_hz,
        .samples = run_result.samples,
        .process = run_result.process,
        .memory = memory_report,
        .cpu = cpu_report,
        .status = status,
        .failure_reasons = reasons,
    };
}

const SampleResult = struct {
    memory: instruments.MemoryAccumulator = .{},
    cpu: instruments.CpuAccumulator = .{},
    samples: usize = 0,
    process: instruments.ProcessReport,
};

fn sampleChild(allocator: std.mem.Allocator, io: std.Io, child: *std.process.Child, parsed: ParsedRunArgs) !SampleResult {
    _ = allocator;
    var sampler = instruments.ProcessSampler.init(child.*);
    var memory: instruments.MemoryAccumulator = .{};
    var cpu: instruments.CpuAccumulator = .{};
    var previous_cpu_total: ?u64 = null;
    var previous_sample_ns: ?u64 = null;
    const sample_interval_ns = @max(1, @as(u64, @intFromFloat(@as(f64, @floatFromInt(std.time.ns_per_s)) / parsed.sample_rate_hz)));
    const memory_baseline_delay_ns = memoryBaselineDelayNs(parsed.duration_ns);
    var samples_taken: usize = 0;
    var next_sample_ns = sample_interval_ns;
    const cpu_count = std.Thread.getCpuCount() catch 1;
    const start = std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
    var process_report = instruments.ProcessReport{ .pid = sampler.processId(), .end_reason = .duration_completed, .exit_code = null };

    while (true) {
        const elapsed_ns = elapsedSince(start);
        if (elapsed_ns > parsed.duration_ns) break;

        if (samples_taken > 0 and try sampler.hasExited()) {
            const term = try child.wait(io);
            process_report.end_reason = .exited;
            process_report.exit_code = exitCode(term);
            return .{
                .memory = memory,
                .cpu = cpu,
                .samples = samples_taken,
                .process = process_report,
            };
        }

        if (next_sample_ns > elapsed_ns) {
            try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(next_sample_ns - elapsed_ns)), .awake);
        }

        const sample_elapsed_ns = elapsedSince(start);
        if (sample_elapsed_ns > parsed.duration_ns) break;

        if (try sampler.hasExited()) {
            const term = try child.wait(io);
            process_report.end_reason = .exited;
            process_report.exit_code = exitCode(term);
            return .{ .memory = memory, .cpu = cpu, .samples = samples_taken, .process = process_report };
        }

        const sample = try sampler.sample();
        samples_taken += 1;
        if (hasTrack(parsed.tracks, .memory)) {
            if (sample_elapsed_ns >= memory_baseline_delay_ns) {
                if (sample.rss_bytes) |rss| memory.add(rss);
            }
        }
        if (hasTrack(parsed.tracks, .cpu)) {
            cpu.addSample();
            if (sample.cpu_total_100ns) |cpu_total| {
                if (previous_cpu_total) |prev_cpu| {
                    if (previous_sample_ns) |prev_ns| {
                        const wall_delta_ns = sample_elapsed_ns - prev_ns;
                        if (wall_delta_ns > 0 and cpu_total >= prev_cpu) {
                            const cpu_delta_ns = (cpu_total - prev_cpu) * 100;
                            const percent = (@as(f64, @floatFromInt(cpu_delta_ns)) * 100.0) /
                                (@as(f64, @floatFromInt(wall_delta_ns)) * @as(f64, @floatFromInt(cpu_count)));
                            cpu.addPercent(percent);
                        }
                    }
                }
                previous_cpu_total = cpu_total;
                previous_sample_ns = sample_elapsed_ns;
            }
        }

        if (try sampler.hasExited()) {
            const term = try child.wait(io);
            process_report.end_reason = .exited;
            process_report.exit_code = exitCode(term);
            return .{ .memory = memory, .cpu = cpu, .samples = samples_taken, .process = process_report };
        }

        const after_sample_ns = elapsedSince(start);
        if (after_sample_ns >= parsed.duration_ns) break;
        next_sample_ns = @min(parsed.duration_ns, (samples_taken + 1) * sample_interval_ns);
        if (next_sample_ns > after_sample_ns) {
            try std.Options.debug_io.sleep(.fromNanoseconds(@intCast(next_sample_ns - after_sample_ns)), .awake);
        }
    }

    child.kill(io);
    return .{
        .memory = memory,
        .cpu = cpu,
        .samples = samples_taken,
        .process = process_report,
    };
}

fn memoryBaselineDelayNs(duration_ns: u64) u64 {
    if (duration_ns < 2 * std.time.ns_per_s) return 0;
    return std.time.ns_per_s;
}

fn childArgv(allocator: std.mem.Allocator, backend: InstrumentBackend, artifact_path: []const u8) ![]const []const u8 {
    if (backend == .llvm) return allocator.dupe([]const u8, &.{artifact_path});
    const exe_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    return allocator.dupe([]const u8, &.{
        exe_path,
        "__instrument-artifact",
        "--backend",
        backend.label(),
        "--artifact",
        artifact_path,
    });
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

fn toExecutionTarget(backend: InstrumentBackend) build_def.ExecutionTarget {
    return switch (backend) {
        .runtime => .vm,
        .llvm => .llvm_native,
        .hybrid => .hybrid,
    };
}

fn instrumentOutputPath(allocator: std.mem.Allocator, output_root: []const u8, stem: []const u8, backend: build_def.ExecutionTarget) ![]const u8 {
    return switch (backend) {
        .vm => std.fmt.allocPrint(allocator, "{s}/{s}.instruments.kbc", .{ output_root, stem }),
        .llvm_native => std.fmt.allocPrint(allocator, "{s}/{s}.instruments{s}", .{ output_root, stem, build.executableExtension() }),
        .hybrid => std.fmt.allocPrint(allocator, "{s}/{s}.instruments.khm", .{ output_root, stem }),
    };
}

fn findArtifact(artifacts: []const build_def.Artifact, kind: build_def.ArtifactKind) ?build_def.Artifact {
    for (artifacts) |artifact| {
        if (artifact.kind == kind) return artifact;
    }
    return null;
}

fn writeJsonReport(path: []const u8, report: instruments.Report) !void {
    if (std.fs.path.dirname(path)) |dir| {
        if (dir.len > 0) try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, dir);
    }
    var file = try std.Io.Dir.cwd().createFile(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    var buffer: [4096]u8 = undefined;
    var writer = file.writer(std.Options.debug_io, &buffer);
    try report.writeJson(&writer.interface);
    try writer.interface.flush();
}

fn hasTrack(tracks: []const InstrumentKind, needle: InstrumentKind) bool {
    for (tracks) |track| {
        if (track == needle) return true;
    }
    return false;
}

fn elapsedSince(start: std.Io.Clock.Timestamp) u64 {
    const duration_ns = start.durationTo(std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake)).raw.toNanoseconds();
    return @intCast(@max(duration_ns, 0));
}

fn exitCode(term: std.process.Child.Term) ?u8 {
    return switch (term) {
        .exited => |code| code,
        else => 1,
    };
}

const ParsedArtifactArgs = struct {
    backend: InstrumentBackend,
    artifact_path: []const u8,
    cwd: ?[]const u8 = null,
};

fn parseArtifactArgs(args: []const []const u8) !ParsedArtifactArgs {
    var backend: ?InstrumentBackend = null;
    var artifact_path: ?[]const u8 = null;
    var cwd: ?[]const u8 = null;

    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            backend = parseBackend(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (std.mem.eql(u8, arg, "--artifact")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            artifact_path = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--cwd")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            cwd = args[index];
            continue;
        }
        return error.InvalidArguments;
    }
    return .{
        .backend = backend orelse return error.InvalidArguments,
        .artifact_path = artifact_path orelse return error.InvalidArguments,
        .cwd = cwd,
    };
}

test "parse instruments run options" {
    const parsed = try parseRunArgs(std.testing.allocator, &.{
        "../kira-graphics/examples/basic_3d_cube",
        "--backend",
        "hybrid",
        "--track",
        "memory",
        "--track",
        "cpu",
        "--duration",
        "30s",
        "--sample-rate",
        "10hz",
        "--fail-on-growth",
        "10mb",
        "--json-out",
        ".kira/instruments/out.json",
    });
    defer std.testing.allocator.free(parsed.tracks);

    try std.testing.expectEqual(InstrumentBackend.hybrid, parsed.backend);
    try std.testing.expectEqualStrings("../kira-graphics/examples/basic_3d_cube", parsed.target);
    try std.testing.expectEqual(@as(usize, 2), parsed.tracks.len);
    try std.testing.expectEqual(InstrumentKind.memory, parsed.tracks[0]);
    try std.testing.expectEqual(InstrumentKind.cpu, parsed.tracks[1]);
    try std.testing.expectEqual(@as(u64, 30 * std.time.ns_per_s), parsed.duration_ns);
    try std.testing.expectEqual(@as(f64, 10.0), parsed.sample_rate_hz);
    try std.testing.expectEqual(@as(u64, 10 * 1024 * 1024), parsed.fail_on_growth_bytes.?);
    try std.testing.expectEqualStrings(".kira/instruments/out.json", parsed.json_out.?);
}

test "parse durations rates and byte sizes" {
    try std.testing.expectEqual(@as(u64, 500 * std.time.ns_per_ms), parseDurationNs("500ms").?);
    try std.testing.expectEqual(@as(u64, 60 * std.time.ns_per_s), parseDurationNs("1m").?);
    try std.testing.expectEqual(@as(f64, 2.5), parseSampleRateHz("2.5hz").?);
    try std.testing.expectEqual(@as(u64, 512 * 1024), parseByteSize("512kb").?);
    try std.testing.expectEqual(@as(u64, 1048576), parseByteSize("1048576").?);
}

test "invalid instruments values are rejected" {
    try std.testing.expectError(error.InvalidArguments, parseRunArgs(std.testing.allocator, &.{ "target", "--backend", "native", "--track", "memory", "--duration", "1s", "--sample-rate", "1hz" }));
    try std.testing.expectError(error.InvalidArguments, parseRunArgs(std.testing.allocator, &.{ "target", "--backend", "hybrid", "--track", "io", "--duration", "1s", "--sample-rate", "1hz" }));
    try std.testing.expectError(error.InvalidArguments, parseRunArgs(std.testing.allocator, &.{ "target", "--backend", "hybrid", "--track", "memory", "--duration", "soon", "--sample-rate", "1hz" }));
    try std.testing.expectError(error.InvalidArguments, parseRunArgs(std.testing.allocator, &.{ "target", "--backend", "hybrid", "--track", "memory", "--duration", "1s", "--sample-rate", "fast" }));
    try std.testing.expectError(error.InvalidArguments, parseRunArgs(std.testing.allocator, &.{ "target", "--backend", "hybrid", "--track", "memory", "--duration", "1s", "--sample-rate", "1hz", "--fail-on-growth", "large" }));
}
