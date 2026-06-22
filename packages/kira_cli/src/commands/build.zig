const std = @import("std");
const builtin = @import("builtin");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const manifest = @import("kira_manifest");
const kira_main = @import("kira_main");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
    _ = parsed.offline;
    _ = parsed.locked;
    // Honor `--timings` (and KIRA_TIMINGS) so `kira build` emits the same per-stage
    // breakdown `kira run` does; previously the flag was parsed but ignored.
    const previous_timings = build.timingsEnabled();
    build.setTimingsEnabled(parsed.timings or timingsEnvEnabled());
    defer build.setTimingsEnabled(previous_timings);
    const path = try allocator.dupeZ(u8, parsed.input_path);
    defer allocator.free(path);
    const developer = kira_main.kira_developer_create() orelse return error.OutOfMemory;
    defer kira_main.kira_developer_destroy(developer);
    const status = kira_main.kira_developer_build(developer, path.ptr, backendArg(selectedBackend(parsed)));
    const report = std.mem.span(kira_main.kira_developer_report(developer) orelse "");
    if (status == .ok) {
        try stdout.writeAll(report);
        return;
    }
    if (report.len != 0) try stderr.writeAll(report) else try stderr.writeAll(std.mem.span(kira_main.kira_developer_last_error(developer) orelse ""));
    return error.CommandFailed;
}

const ParsedArgs = struct {
    backend: ?build_def.ExecutionTarget = null,
    profile: ?manifest.BuildProfile = null,
    offline: bool = false,
    locked: bool = false,
    timings: bool = false,
    input_path: []const u8,
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    var backend: ?build_def.ExecutionTarget = null;
    var profile: ?manifest.BuildProfile = null;
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
        if (std.mem.eql(u8, arg, "--target")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            if (!std.mem.eql(u8, args[index], "wasm32-emscripten")) return error.InvalidArguments;
            backend = .wasm32_emscripten;
            continue;
        }
        if (std.mem.eql(u8, arg, "--profile")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            profile = manifest.BuildProfile.parse(args[index]) orelse return error.InvalidArguments;
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
        .profile = profile,
        .offline = offline,
        .locked = locked,
        .timings = timings,
        .input_path = input_path orelse support.defaultCommandInputPath(),
    };
}

fn profileBackend(profile: ?manifest.BuildProfile) ?build_def.ExecutionTarget {
    return switch (profile orelse return null) {
        .debug => .vm,
        .profiler, .release => .llvm_native,
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
    if (std.mem.eql(u8, arg, "wasm32-emscripten") or std.mem.eql(u8, arg, "wasm")) return .wasm32_emscripten;
    if (std.mem.eql(u8, arg, "hybrid")) return .hybrid;
    return null;
}

fn selectedBackend(parsed: ParsedArgs) ?build_def.ExecutionTarget {
    if (parsed.backend) |backend| return backend;
    return profileBackend(parsed.profile);
}

fn backendArg(backend: ?build_def.ExecutionTarget) kira_main.KiraDeveloperBackend {
    return switch (backend orelse return .default) {
        .vm => .vm,
        .llvm_native => .llvm,
        .wasm32_emscripten => .wasm32_emscripten,
        .hybrid => .hybrid,
    };
}
