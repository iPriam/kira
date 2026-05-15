const std = @import("std");
const builtin = @import("builtin");

pub const InstrumentKind = enum {
    memory,
    cpu,

    pub fn label(self: InstrumentKind) []const u8 {
        return @tagName(self);
    }
};

pub const InstrumentBackend = enum {
    runtime,
    llvm,
    hybrid,

    pub fn label(self: InstrumentBackend) []const u8 {
        return @tagName(self);
    }
};

pub const InstrumentStatus = enum {
    pass,
    fail,

    pub fn label(self: InstrumentStatus) []const u8 {
        return @tagName(self);
    }
};

pub const InstrumentFailureKind = enum {
    memory_growth_exceeded,
    process_exit_failed,
    timeout,
    invalid_configuration,

    pub fn label(self: InstrumentFailureKind) []const u8 {
        return @tagName(self);
    }
};

pub const ProcessEndReason = enum {
    exited,
    duration_completed,

    pub fn label(self: ProcessEndReason) []const u8 {
        return @tagName(self);
    }
};

pub const FailureReason = struct {
    kind: InstrumentFailureKind,
    message: []const u8,
};

pub const MemoryReport = struct {
    metric: []const u8 = "private_working_set",
    rss_start_bytes: u64 = 0,
    rss_end_bytes: u64 = 0,
    rss_peak_bytes: u64 = 0,
    rss_growth_bytes: i64 = 0,
    fail_on_growth_bytes: ?u64 = null,
    sample_count: usize = 0,
    status: InstrumentStatus = .pass,
};

pub const CpuReport = struct {
    available: bool = false,
    average_percent: ?f64 = null,
    peak_percent: ?f64 = null,
    sample_count: usize = 0,
};

pub const ProcessReport = struct {
    pid: ?u32 = null,
    end_reason: ProcessEndReason,
    exit_code: ?u8 = null,
};

pub const Report = struct {
    command: []const u8 = "kira instruments run",
    target: []const u8,
    backend: InstrumentBackend,
    tracks: []const InstrumentKind,
    duration_seconds: f64,
    sample_rate_hz: f64,
    samples: usize,
    process: ProcessReport,
    memory: ?MemoryReport = null,
    cpu: ?CpuReport = null,
    status: InstrumentStatus = .pass,
    failure_reasons: []const FailureReason = &.{},

    pub fn writeHuman(self: Report, writer: anytype) !void {
        try writer.writeAll("Kira Instruments Report\n\n");
        try writer.print("target: {s}\n", .{self.target});
        try writer.print("backend: {s}\n", .{self.backend.label()});
        try writer.writeAll("tracks: ");
        for (self.tracks, 0..) |track, index| {
            if (index != 0) try writer.writeAll(", ");
            try writer.writeAll(track.label());
        }
        try writer.writeAll("\n");
        try writer.print("duration: {d:.1}s\n", .{self.duration_seconds});
        try writer.print("sample_rate: {d:.3}hz\n", .{self.sample_rate_hz});
        try writer.print("samples: {d}\n", .{self.samples});
        if (self.process.pid) |pid| try writer.print("process_pid: {d}\n", .{pid});
        try writer.print("process_end: {s}\n", .{self.process.end_reason.label()});
        if (self.process.exit_code) |code| try writer.print("exit_code: {d}\n", .{code});

        if (self.memory) |memory| {
            try writer.writeAll("\nmemory:\n");
            try writer.print("  metric: {s}\n", .{memory.metric});
            try writer.writeAll("  rss_start: ");
            try writeBytes(writer, memory.rss_start_bytes);
            try writer.writeAll("\n  rss_end: ");
            try writeBytes(writer, memory.rss_end_bytes);
            try writer.writeAll("\n  rss_peak: ");
            try writeBytes(writer, memory.rss_peak_bytes);
            try writer.writeAll("\n  rss_growth: ");
            try writeSignedBytes(writer, memory.rss_growth_bytes);
            try writer.writeAll("\n");
            if (memory.fail_on_growth_bytes) |threshold| {
                try writer.writeAll("  threshold: ");
                try writeBytes(writer, threshold);
                try writer.writeAll("\n");
            } else {
                try writer.writeAll("  threshold: unavailable\n");
            }
            try writer.print("  samples: {d}\n", .{memory.sample_count});
            try writer.print("  result: {s}\n", .{upperStatus(memory.status)});
        }

        if (self.cpu) |cpu| {
            try writer.writeAll("\ncpu:\n");
            try writer.print("  available: {s}\n", .{if (cpu.available) "true" else "false"});
            if (cpu.available) {
                try writer.print("  avg: {d:.1}%\n", .{cpu.average_percent orelse 0});
                try writer.print("  peak: {d:.1}%\n", .{cpu.peak_percent orelse 0});
            } else {
                try writer.writeAll("  avg: unavailable\n");
                try writer.writeAll("  peak: unavailable\n");
            }
            try writer.print("  samples: {d}\n", .{cpu.sample_count});
        }

        try writer.print("\nresult: {s}\n", .{upperStatus(self.status)});
        if (self.failure_reasons.len > 0) {
            try writer.writeAll("reason:\n");
            for (self.failure_reasons) |reason| {
                try writer.print("  {s}\n", .{reason.message});
            }
        }
    }

    pub fn writeJson(self: Report, writer: anytype) !void {
        try writer.writeAll("{\n");
        try writer.writeAll("  \"command\": ");
        try writeJsonString(writer, self.command);
        try writer.writeAll(",\n  \"target\": ");
        try writeJsonString(writer, self.target);
        try writer.print(",\n  \"backend\": \"{s}\",\n", .{self.backend.label()});
        try writer.writeAll("  \"tracks\": [");
        for (self.tracks, 0..) |track, index| {
            if (index != 0) try writer.writeAll(", ");
            try writer.print("\"{s}\"", .{track.label()});
        }
        try writer.writeAll("],\n");
        try writer.print("  \"duration_seconds\": {d:.3},\n", .{self.duration_seconds});
        try writer.print("  \"sample_rate_hz\": {d:.3},\n", .{self.sample_rate_hz});
        try writer.print("  \"samples\": {d},\n", .{self.samples});
        try writer.writeAll("  \"process\": {\n");
        try writer.writeAll("    \"pid\": ");
        if (self.process.pid) |pid| {
            try writer.print("{d}", .{pid});
        } else {
            try writer.writeAll("null");
        }
        try writer.print(",\n    \"end_reason\": \"{s}\",\n", .{self.process.end_reason.label()});
        try writer.writeAll("    \"exit_code\": ");
        if (self.process.exit_code) |code| {
            try writer.print("{d}\n", .{code});
        } else {
            try writer.writeAll("null\n");
        }
        try writer.writeAll("  },\n");

        if (self.memory) |memory| {
            try writer.writeAll("  \"memory\": {\n");
            try writer.writeAll("    \"metric\": ");
            try writeJsonString(writer, memory.metric);
            try writer.writeAll(",\n");
            try writer.print("    \"rss_start_bytes\": {d},\n", .{memory.rss_start_bytes});
            try writer.print("    \"rss_end_bytes\": {d},\n", .{memory.rss_end_bytes});
            try writer.print("    \"rss_peak_bytes\": {d},\n", .{memory.rss_peak_bytes});
            try writer.print("    \"rss_growth_bytes\": {d},\n", .{memory.rss_growth_bytes});
            try writer.writeAll("    \"fail_on_growth_bytes\": ");
            if (memory.fail_on_growth_bytes) |threshold| {
                try writer.print("{d}", .{threshold});
            } else {
                try writer.writeAll("null");
            }
            try writer.print(",\n    \"samples\": {d},\n", .{memory.sample_count});
            try writer.print("    \"status\": \"{s}\"\n", .{memory.status.label()});
            try writer.writeAll("  },\n");
        } else {
            try writer.writeAll("  \"memory\": null,\n");
        }

        if (self.cpu) |cpu| {
            try writer.writeAll("  \"cpu\": {\n");
            try writer.print("    \"available\": {s},\n", .{if (cpu.available) "true" else "false"});
            try writer.writeAll("    \"average_percent\": ");
            if (cpu.average_percent) |avg| {
                try writer.print("{d:.3}", .{avg});
            } else {
                try writer.writeAll("null");
            }
            try writer.writeAll(",\n    \"peak_percent\": ");
            if (cpu.peak_percent) |peak| {
                try writer.print("{d:.3}", .{peak});
            } else {
                try writer.writeAll("null");
            }
            try writer.print(",\n    \"samples\": {d}\n", .{cpu.sample_count});
            try writer.writeAll("  },\n");
        } else {
            try writer.writeAll("  \"cpu\": null,\n");
        }

        try writer.print("  \"status\": \"{s}\",\n", .{self.status.label()});
        try writer.writeAll("  \"failure_reasons\": [");
        for (self.failure_reasons, 0..) |reason, index| {
            if (index != 0) try writer.writeAll(",");
            try writer.writeAll("\n    {\n");
            try writer.print("      \"kind\": \"{s}\",\n", .{reason.kind.label()});
            try writer.writeAll("      \"message\": ");
            try writeJsonString(writer, reason.message);
            try writer.writeAll("\n    }");
        }
        if (self.failure_reasons.len > 0) try writer.writeAll("\n  ");
        try writer.writeAll("]\n");
        try writer.writeAll("}\n");
    }
};

pub const MemoryAccumulator = struct {
    sample_count: usize = 0,
    rss_start_bytes: u64 = 0,
    rss_end_bytes: u64 = 0,
    rss_peak_bytes: u64 = 0,

    pub fn add(self: *MemoryAccumulator, rss_bytes: u64) void {
        if (self.sample_count == 0) {
            self.rss_start_bytes = rss_bytes;
            self.rss_peak_bytes = rss_bytes;
        }
        self.rss_end_bytes = rss_bytes;
        self.rss_peak_bytes = @max(self.rss_peak_bytes, rss_bytes);
        self.sample_count += 1;
    }

    pub fn finish(self: MemoryAccumulator, threshold: ?u64) MemoryReport {
        const growth = @as(i128, self.rss_end_bytes) - @as(i128, self.rss_start_bytes);
        var report = MemoryReport{
            .rss_start_bytes = self.rss_start_bytes,
            .rss_end_bytes = self.rss_end_bytes,
            .rss_peak_bytes = self.rss_peak_bytes,
            .rss_growth_bytes = @intCast(growth),
            .fail_on_growth_bytes = threshold,
            .sample_count = self.sample_count,
            .status = .pass,
        };
        report.status = evaluateMemoryThreshold(report, threshold);
        return report;
    }
};

pub const CpuAccumulator = struct {
    sample_count: usize = 0,
    interval_count: usize = 0,
    total_percent: f64 = 0,
    peak_percent: f64 = 0,

    pub fn addSample(self: *CpuAccumulator) void {
        self.sample_count += 1;
    }

    pub fn addPercent(self: *CpuAccumulator, percent: f64) void {
        self.interval_count += 1;
        self.total_percent += percent;
        self.peak_percent = @max(self.peak_percent, percent);
    }

    pub fn finish(self: CpuAccumulator) CpuReport {
        if (self.interval_count == 0) {
            return .{
                .available = false,
                .sample_count = self.sample_count,
            };
        }
        return .{
            .available = true,
            .average_percent = self.total_percent / @as(f64, @floatFromInt(self.interval_count)),
            .peak_percent = self.peak_percent,
            .sample_count = self.sample_count,
        };
    }
};

pub fn evaluateMemoryThreshold(memory: MemoryReport, threshold: ?u64) InstrumentStatus {
    const limit = threshold orelse return .pass;
    if (memory.rss_growth_bytes <= 0) return .pass;
    return if (@as(u64, @intCast(memory.rss_growth_bytes)) > limit) .fail else .pass;
}

pub fn appendFailureReasons(
    allocator: std.mem.Allocator,
    memory: ?MemoryReport,
    process: ProcessReport,
) ![]const FailureReason {
    var reasons = std.array_list.Managed(FailureReason).init(allocator);
    if (memory) |mem| {
        if (mem.status == .fail) {
            try reasons.append(.{
                .kind = .memory_growth_exceeded,
                .message = "memory growth exceeded threshold",
            });
        }
    }
    if (process.exit_code) |code| {
        if (code != 0) {
            try reasons.append(.{
                .kind = .process_exit_failed,
                .message = "process exited with failure",
            });
        }
    }
    return reasons.toOwnedSlice();
}

pub const ProcessMetrics = struct {
    rss_bytes: ?u64 = null,
    cpu_total_100ns: ?u64 = null,
};

pub const ProcessSampler = struct {
    handle: std.process.Child.Id,

    pub fn init(child: std.process.Child) ProcessSampler {
        return .{ .handle = child.id.? };
    }

    pub fn sample(self: ProcessSampler) !ProcessMetrics {
        return switch (builtin.os.tag) {
            .windows => sampleWindows(self.handle),
            else => error.PlatformInstrumentationUnavailable,
        };
    }

    pub fn hasExited(self: ProcessSampler) !bool {
        return switch (builtin.os.tag) {
            .windows => windowsHasExited(self.handle),
            else => error.PlatformInstrumentationUnavailable,
        };
    }

    pub fn processId(self: ProcessSampler) ?u32 {
        return switch (builtin.os.tag) {
            .windows => windowsProcessId(self.handle),
            else => null,
        };
    }
};

fn sampleWindows(handle: std.process.Child.Id) !ProcessMetrics {
    const windows = std.os.windows;
    var counters: ProcessMemoryCountersEx2 = .{ .cb = @sizeOf(ProcessMemoryCountersEx2) };
    if (K32GetProcessMemoryInfo(handle, &counters, @sizeOf(ProcessMemoryCountersEx2)) == .FALSE) {
        return error.MemoryInstrumentationFailed;
    }
    if (counters.PrivateWorkingSetSize == 0) {
        return error.PrivateWorkingSetUnavailable;
    }

    var times: windows.KERNEL_USER_TIMES = undefined;
    var times_len: windows.ULONG = 0;
    const times_status = windows.ntdll.NtQueryInformationProcess(
        handle,
        .Times,
        &times,
        @sizeOf(windows.KERNEL_USER_TIMES),
        &times_len,
    );
    const cpu_total = if (times_status == .SUCCESS)
        @as(u64, @intCast(times.KernelTime + times.UserTime))
    else
        null;

    return .{
        .rss_bytes = counters.PrivateWorkingSetSize,
        .cpu_total_100ns = cpu_total,
    };
}

const ProcessMemoryCountersEx2 = extern struct {
    cb: std.os.windows.DWORD = 0,
    PageFaultCount: std.os.windows.DWORD = 0,
    PeakWorkingSetSize: std.os.windows.SIZE_T = 0,
    WorkingSetSize: std.os.windows.SIZE_T = 0,
    QuotaPeakPagedPoolUsage: std.os.windows.SIZE_T = 0,
    QuotaPagedPoolUsage: std.os.windows.SIZE_T = 0,
    QuotaPeakNonPagedPoolUsage: std.os.windows.SIZE_T = 0,
    QuotaNonPagedPoolUsage: std.os.windows.SIZE_T = 0,
    PagefileUsage: std.os.windows.SIZE_T = 0,
    PeakPagefileUsage: std.os.windows.SIZE_T = 0,
    PrivateUsage: std.os.windows.SIZE_T = 0,
    PrivateWorkingSetSize: std.os.windows.SIZE_T = 0,
    SharedCommitUsage: u64 = 0,
};

extern "kernel32" fn K32GetProcessMemoryInfo(
    Process: std.os.windows.HANDLE,
    ppsmemCounters: *ProcessMemoryCountersEx2,
    cb: std.os.windows.DWORD,
) callconv(.winapi) std.os.windows.BOOL;

extern "kernel32" fn GetProcessId(
    Process: std.os.windows.HANDLE,
) callconv(.winapi) std.os.windows.DWORD;

fn windowsProcessId(handle: std.process.Child.Id) ?u32 {
    const pid = GetProcessId(handle);
    if (pid == 0) return null;
    return pid;
}

fn windowsHasExited(handle: std.process.Child.Id) !bool {
    const windows = std.os.windows;
    var timeout: windows.LARGE_INTEGER = 0;
    const status = windows.ntdll.NtWaitForSingleObject(handle, .FALSE, &timeout);
    if (status == .SUCCESS) return true;
    if (status == .TIMEOUT) return false;
    return error.ProcessStatusUnavailable;
}

fn writeJsonString(writer: anytype, value: []const u8) !void {
    try writer.writeAll("\"");
    for (value) |byte| {
        switch (byte) {
            '"' => try writer.writeAll("\\\""),
            '\\' => try writer.writeAll("\\\\"),
            '\n' => try writer.writeAll("\\n"),
            '\r' => try writer.writeAll("\\r"),
            '\t' => try writer.writeAll("\\t"),
            else => try writer.print("{c}", .{byte}),
        }
    }
    try writer.writeAll("\"");
}

fn upperStatus(status: InstrumentStatus) []const u8 {
    return switch (status) {
        .pass => "PASS",
        .fail => "FAIL",
    };
}

fn writeBytes(writer: anytype, bytes: u64) !void {
    const mb = @as(f64, @floatFromInt(bytes)) / (1024.0 * 1024.0);
    if (mb >= 1.0) return writer.print("{d:.1} MB", .{mb});
    const kb = @as(f64, @floatFromInt(bytes)) / 1024.0;
    if (kb >= 1.0) return writer.print("{d:.1} KB", .{kb});
    return writer.print("{d} B", .{bytes});
}

fn writeSignedBytes(writer: anytype, bytes: i64) !void {
    if (bytes >= 0) {
        try writer.writeAll("+");
        return writeBytes(writer, @intCast(bytes));
    }
    try writer.writeAll("-");
    return writeBytes(writer, @intCast(-bytes));
}

test "threshold evaluation passes below and equal threshold" {
    const below = MemoryReport{ .rss_growth_bytes = 1024, .fail_on_growth_bytes = 2048 };
    try std.testing.expectEqual(InstrumentStatus.pass, evaluateMemoryThreshold(below, below.fail_on_growth_bytes));

    const equal = MemoryReport{ .rss_growth_bytes = 2048, .fail_on_growth_bytes = 2048 };
    try std.testing.expectEqual(InstrumentStatus.pass, evaluateMemoryThreshold(equal, equal.fail_on_growth_bytes));
}

test "threshold evaluation fails above threshold with reason" {
    const memory = MemoryReport{
        .rss_growth_bytes = 4096,
        .fail_on_growth_bytes = 2048,
        .status = evaluateMemoryThreshold(.{ .rss_growth_bytes = 4096 }, 2048),
    };
    try std.testing.expectEqual(InstrumentStatus.fail, memory.status);

    const reasons = try appendFailureReasons(std.testing.allocator, memory, .{ .end_reason = .exited, .exit_code = 0 });
    defer std.testing.allocator.free(reasons);
    try std.testing.expectEqual(@as(usize, 1), reasons.len);
    try std.testing.expectEqual(InstrumentFailureKind.memory_growth_exceeded, reasons[0].kind);
}

test "report serializes stable fields and unavailable cpu honestly" {
    const tracks = [_]InstrumentKind{ .memory, .cpu };
    const reasons = [_]FailureReason{.{
        .kind = .memory_growth_exceeded,
        .message = "memory growth exceeded threshold",
    }};
    const report = Report{
        .target = "../kira-graphics/examples/basic_3d_cube",
        .backend = .hybrid,
        .tracks = &tracks,
        .duration_seconds = 30.0,
        .sample_rate_hz = 10.0,
        .samples = 300,
        .process = .{ .end_reason = .duration_completed, .exit_code = null },
        .memory = .{
            .rss_start_bytes = 35,
            .rss_end_bytes = 50,
            .rss_peak_bytes = 55,
            .rss_growth_bytes = 15,
            .fail_on_growth_bytes = 10,
            .sample_count = 300,
            .status = .fail,
        },
        .cpu = .{ .available = false, .sample_count = 300 },
        .status = .fail,
        .failure_reasons = &reasons,
    };

    var buffer: [4096]u8 = undefined;
    var writer = std.Io.Writer.fixed(&buffer);
    try report.writeJson(&writer);
    const json = writer.buffered();

    try std.testing.expect(std.mem.indexOf(u8, json, "\"command\": \"kira instruments run\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"target\": \"../kira-graphics/examples/basic_3d_cube\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"backend\": \"hybrid\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"tracks\": [\"memory\", \"cpu\"]") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"pid\": null") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"metric\": \"private_working_set\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"status\": \"fail\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"kind\": \"memory_growth_exceeded\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"available\": false") != null);
    try std.testing.expect(std.mem.indexOf(u8, json, "\"average_percent\": null") != null);
}
