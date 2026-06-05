const builtin = @import("builtin");
const std = @import("std");
const discovery = @import("discovery.zig");
const execute = @import("execute.zig");

pub fn main(init: std.process.Init.Minimal) !void {
    var arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const raw_args = try init.args.toSlice(allocator);
    const args = try allocator.alloc([]const u8, raw_args.len);
    for (raw_args, 0..) |arg, index| args[index] = arg;
    if (args.len != 2) return error.InvalidArguments;

    const cases = try discovery.discoverCases(allocator);
    if (cases.len == 0) return error.NoCorpusCases;

    const jobs = try buildJobs(allocator, cases);
    const profile_enabled = try envFlag(allocator, init.environ, "KIRA_CORPUS_PROFILE");
    // Stability mode: native (llvm) cases build and link real binaries, which contend
    // on the toolchain when many run at once and can fail transiently under load. Stable
    // mode runs every job serially and retries a failing llvm job a few times so a
    // transient build/link hiccup does not fail the suite. Real (deterministic) failures
    // still fail after the retries are exhausted. Enable with KIRA_CORPUS_STABLE=1 or
    // `zig build test -Dstable-tests`.
    const stable = try envFlag(allocator, init.environ, "KIRA_CORPUS_STABLE");
    const worker_count = if (stable) @as(usize, @intCast(@min(jobs.len, 1))) else resolveWorkerCount(allocator, init.environ, jobs.len);
    const retries = resolveRetries(allocator, init.environ, stable);
    const options: execute.Options = .{
        .hybrid_runner_path = args[1],
        .profile = profile_enabled,
    };

    const start = nowTimestamp();
    const results = try allocator.alloc(JobResult, jobs.len);
    for (results) |*result| result.* = .{};

    var shared = Shared{
        .jobs = jobs,
        .results = results,
        .options = options,
        .retries = retries,
    };

    if (worker_count <= 1 or jobs.len <= 1) {
        shared.runUntilDone();
    } else {
        const extra_workers = worker_count - 1;
        const threads = try allocator.alloc(std.Thread, extra_workers);
        for (threads) |*thread| thread.* = try std.Thread.spawn(.{}, workerMain, .{&shared});
        shared.runUntilDone();
        for (threads) |thread| thread.join();
    }

    var passed: usize = 0;
    var failed: usize = 0;
    for (results) |result| {
        std.debug.print("{s}", .{result.report.output});
        passed += result.report.passed;
        failed += result.report.failed;
    }

    std.debug.print("Corpus summary: {d} passed, {d} failed\n", .{ passed, failed });
    if (profile_enabled) {
        std.debug.print(
            "Corpus timing: wall={s}, workers={d}, jobs={d}\n",
            .{ try formatDuration(allocator, elapsedNs(start)), worker_count, jobs.len },
        );
    }

    for (results) |result| result.deinit();
    if (failed != 0) return error.CorpusFailures;
}

const Job = struct {
    case: discovery.Case,
    backend: discovery.Backend,
};

const JobResult = struct {
    arena: ?*std.heap.ArenaAllocator = null,
    report: execute.JobReport = .{
        .output = "",
        .passed = 0,
        .failed = 0,
    },

    fn deinit(self: JobResult) void {
        if (self.arena) |arena_ptr| {
            arena_ptr.deinit();
            std.heap.smp_allocator.destroy(arena_ptr);
        }
    }
};

const Shared = struct {
    jobs: []const Job,
    results: []JobResult,
    options: execute.Options,
    retries: usize = 0,
    next_index: std.atomic.Value(usize) = .init(0),

    fn runUntilDone(self: *Shared) void {
        while (true) {
            const index = self.next_index.fetchAdd(1, .monotonic);
            if (index >= self.jobs.len) break;
            self.runJob(index);
        }
    }

    fn runJob(self: *Shared, index: usize) void {
        const job = self.jobs[index];
        // Only native (llvm) jobs are retried: they build/link real binaries and so are
        // the only jobs subject to transient toolchain contention. vm/hybrid run in-process.
        const max_attempts: usize = if (job.backend == .llvm) self.retries + 1 else 1;
        var attempt: usize = 0;
        while (true) {
            attempt += 1;
            const arena_ptr = std.heap.smp_allocator.create(std.heap.ArenaAllocator) catch {
                self.results[index] = .{
                    .report = .{
                        .output = "FAIL <internal>: OutOfMemory\n",
                        .passed = 0,
                        .failed = 1,
                    },
                };
                return;
            };
            arena_ptr.* = std.heap.ArenaAllocator.init(std.heap.smp_allocator);
            const allocator = arena_ptr.allocator();

            const report = execute.runBackendJob(allocator, job.case, job.backend, self.options) catch |err| blk: {
                const label = std.fmt.allocPrint(
                    allocator,
                    "{s} [{s}]",
                    .{ job.case.name, backendName(job.backend) },
                ) catch "internal";
                const output = std.fmt.allocPrint(allocator, "FAIL {s}: {s}\n", .{ label, @errorName(err) }) catch "FAIL <internal>: execution failed\n";
                break :blk execute.JobReport{
                    .output = output,
                    .passed = 0,
                    .failed = 1,
                };
            };

            // Retry a failing llvm job (transient build/link flake) until it passes or the
            // attempts are exhausted; a deterministic failure still fails after the retries.
            if (report.failed != 0 and attempt < max_attempts) {
                arena_ptr.deinit();
                std.heap.smp_allocator.destroy(arena_ptr);
                continue;
            }

            self.results[index] = .{
                .arena = arena_ptr,
                .report = report,
            };
            return;
        }
    }
};

fn workerMain(shared: *Shared) void {
    shared.runUntilDone();
}

fn buildJobs(allocator: std.mem.Allocator, cases: []const discovery.Case) ![]Job {
    var jobs = std.array_list.Managed(Job).init(allocator);
    for (cases) |case| {
        for (case.expectation.backends) |backend| {
            try jobs.append(.{
                .case = case,
                .backend = backend,
            });
        }
    }
    return jobs.toOwnedSlice();
}

fn resolveWorkerCount(allocator: std.mem.Allocator, environ: std.process.Environ, job_count: usize) usize {
    if (job_count == 0) return 0;
    if (environ.getAlloc(allocator, "KIRA_CORPUS_JOBS")) |value| {
        defer allocator.free(value);
        if (std.fmt.parseInt(usize, std.mem.trim(u8, value, " \t\r\n"), 10)) |parsed| {
            return clampWorkerCount(parsed, job_count);
        } else |_| {}
    } else |_| {}

    const cpu_count = std.Thread.getCpuCount() catch 1;
    const cap = switch (builtin.os.tag) {
        .windows => @min(cpu_count, 4),
        else => @min(cpu_count, 8),
    };
    return clampWorkerCount(cap, job_count);
}

fn clampWorkerCount(count: usize, job_count: usize) usize {
    const resolved = if (count == 0) 1 else count;
    return @max(@min(resolved, job_count), 1);
}

// Number of EXTRA attempts for a failing llvm job. Defaults to 2 in stable mode, 0
// otherwise; KIRA_CORPUS_RETRIES overrides either way (capped to keep reruns bounded).
fn resolveRetries(allocator: std.mem.Allocator, environ: std.process.Environ, stable: bool) usize {
    var retries: usize = if (stable) 2 else 0;
    if (environ.getAlloc(allocator, "KIRA_CORPUS_RETRIES")) |value| {
        defer allocator.free(value);
        if (std.fmt.parseInt(usize, std.mem.trim(u8, value, " \t\r\n"), 10)) |parsed| {
            retries = parsed;
        } else |_| {}
    } else |_| {}
    return @min(retries, 5);
}

fn envFlag(allocator: std.mem.Allocator, environ: std.process.Environ, name: []const u8) !bool {
    const value = environ.getAlloc(allocator, name) catch return false;
    defer allocator.free(value);
    const trimmed = std.mem.trim(u8, value, " \t\r\n");
    if (trimmed.len == 0) return false;
    if (std.ascii.eqlIgnoreCase(trimmed, "0")) return false;
    if (std.ascii.eqlIgnoreCase(trimmed, "false")) return false;
    if (std.ascii.eqlIgnoreCase(trimmed, "no")) return false;
    if (std.ascii.eqlIgnoreCase(trimmed, "off")) return false;
    return true;
}

fn backendName(backend: discovery.Backend) []const u8 {
    return switch (backend) {
        .vm => "vm",
        .llvm => "llvm",
        .hybrid => "hybrid",
    };
}

fn nowTimestamp() std.Io.Clock.Timestamp {
    return std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
}

fn elapsedNs(start: std.Io.Clock.Timestamp) u64 {
    const duration_ns = start.durationTo(std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake)).raw.toNanoseconds();
    return @intCast(@max(duration_ns, 0));
}

fn formatDuration(allocator: std.mem.Allocator, ns: u64) ![]const u8 {
    if (ns >= std.time.ns_per_ms) {
        const whole = ns / std.time.ns_per_ms;
        const tenths = (ns % std.time.ns_per_ms) / (std.time.ns_per_ms / 10);
        if (tenths == 0) return std.fmt.allocPrint(allocator, "{d}ms", .{whole});
        return std.fmt.allocPrint(allocator, "{d}.{d}ms", .{ whole, tenths });
    }
    if (ns >= std.time.ns_per_us) {
        return std.fmt.allocPrint(allocator, "{d}us", .{ns / std.time.ns_per_us});
    }
    return std.fmt.allocPrint(allocator, "{d}ns", .{ns});
}
