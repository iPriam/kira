const std = @import("std");
const builtin = @import("builtin");
const program_graph = @import("kira_program_graph");

var timings_enabled: bool = false;

pub fn setTimingsEnabled(enabled: bool) void {
    timings_enabled = enabled;
    program_graph.setTimingsEnabled(enabled);
}

pub fn timingsEnabled() bool {
    return timings_enabled;
}

pub fn nowNs() i128 {
    if (builtin.os.tag == .windows) {
        var counter: std.os.windows.LARGE_INTEGER = undefined;
        var frequency: std.os.windows.LARGE_INTEGER = undefined;
        _ = std.os.windows.ntdll.RtlQueryPerformanceCounter(&counter);
        _ = std.os.windows.ntdll.RtlQueryPerformanceFrequency(&frequency);
        return @divTrunc(@as(i128, counter) * 1_000_000_000, @as(i128, frequency));
    }
    return 0;
}

pub fn elapsedNs(start: i128) u64 {
    return @intCast(nowNs() - start);
}

pub fn timingPrint(comptime fmt: []const u8, args: anytype) void {
    if (timings_enabled) std.debug.print(fmt, args);
}
