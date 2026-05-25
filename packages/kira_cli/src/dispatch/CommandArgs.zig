const std = @import("std");
const build_def = @import("kira_build_definition");
const Parsed = @import("../command/ParsedCommand.zig");
const ParsedCommand = Parsed.ParsedCommand;

pub fn toArgs(allocator: std.mem.Allocator, command: ParsedCommand) ![]const []const u8 {
    var list = std.array_list.Managed([]const u8).init(allocator);
    switch (command) {
        .check => |options| try appendProjectOptions(allocator, &list, options),
        .build => |options| try appendProjectOptions(allocator, &list, options),
        .run => |options| try appendRunOptions(allocator, &list, options),
        .live => |options| try appendLiveOptions(allocator, &list, options),
        .new => |options| {
            if (options.kind == .library) try list.append("--lib");
            try list.append(options.name);
            try list.append(options.destination);
        },
        .fetch_llvm => |options| switch (options.mode) {
            .download_and_install => {},
            .ci_metadata_json => try list.appendSlice(&.{ "--ci-metadata", "--json" }),
            .install_archive => try list.appendSlice(&.{ "--archive", options.archive_path.? }),
        },
        .sync => |options| {
            if (options.offline) try list.append("--offline");
            if (options.locked) try list.append("--locked");
            if (options.input_path) |path| try list.append(path);
        },
        .add => |options| {
            if (options.git_url) |url| try list.appendSlice(&.{ "--git", url });
            if (options.rev) |rev| try list.appendSlice(&.{ "--rev", rev });
            if (options.tag) |tag| try list.appendSlice(&.{ "--tag", tag });
            try list.append(options.package_name);
        },
        .remove => |options| try list.append(options.package_name),
        .update, .tokens, .ast => |options| {
            if (options.input_path) |path| try list.append(path);
        },
        .package => |options| {
            try list.append(switch (options.mode) {
                .pack => "pack",
                .inspect => "inspect",
            });
            if (options.input_path) |path| try list.append(path);
        },
        .shader => |options| {
            try list.append(switch (options.mode) {
                .check => "check",
                .ast => "ast",
                .build => "build",
            });
            if (options.input_path) |path| try list.append(path);
            if (options.out_dir) |out_dir| try list.appendSlice(&.{ "--out-dir", out_dir });
        },
        .instruments => |options| {
            try list.appendSlice(&.{ "run", options.input_path, "--backend", instrumentBackendLabel(options.backend) });
            for (options.tracks) |track| try list.appendSlice(&.{ "--track", instrumentTrackLabel(track) });
            try list.append("--duration");
            try options.duration.appendArgs(allocator, &list);
            try list.appendSlice(&.{ "--sample-rate", options.sample_rate });
            if (options.fail_on_growth) |value| try list.appendSlice(&.{ "--fail-on-growth", value });
            if (options.json_out) |path| try list.appendSlice(&.{ "--json-out", path });
        },
        .instrument_artifact => |options| {
            try list.appendSlice(&.{ "--backend", instrumentBackendLabel(options.backend), "--artifact", options.artifact_path });
            if (options.cwd) |cwd| try list.appendSlice(&.{ "--cwd", cwd });
        },
        .run_hybrid_artifact => |options| {
            try list.appendSlice(&.{ "--manifest", options.manifest_path });
            if (options.cwd) |cwd| try list.appendSlice(&.{ "--cwd", cwd });
        },
        .live_runner => |options| {
            try list.append(options.manifest_path);
        },
        .help, .version => {},
    }
    return list.toOwnedSlice();
}

fn appendProjectOptions(allocator: std.mem.Allocator, list: *std.array_list.Managed([]const u8), options: Parsed.ProjectOptions) !void {
    _ = allocator;
    if (options.backend) |backend| try list.appendSlice(&.{ "--backend", backendLabel(backend) });
    if (options.offline) try list.append("--offline");
    if (options.locked) try list.append("--locked");
    if (options.timings) try list.append("--timings");
    try list.append(options.input_path);
}

fn appendRunOptions(allocator: std.mem.Allocator, list: *std.array_list.Managed([]const u8), options: Parsed.RunOptions) !void {
    if (options.backend) |backend| try list.appendSlice(&.{ "--backend", backendLabel(backend) });
    if (options.offline) try list.append("--offline");
    if (options.locked) try list.append("--locked");
    if (options.trace_execution) try list.append("--trace-execution");
    if (options.timings) try list.append("--timings");
    if (options.quit_after) |duration| {
        try list.append("--quit-after");
        try duration.appendArgs(allocator, list);
    }
    try list.append(options.input_path);
}

fn appendLiveOptions(allocator: std.mem.Allocator, list: *std.array_list.Managed([]const u8), options: Parsed.LiveOptions) !void {
    switch (options.mode) {
        .runners_list => try list.appendSlice(&.{ "runners", "list", options.input_path }),
        .runners_build => try list.appendSlice(&.{ "runners", "build", options.input_path }),
        .runners_clean => try list.appendSlice(&.{ "runners", "clean", options.input_path }),
        .run => {
            try list.appendSlice(&.{ options.runner.legacyLabel(), options.input_path });
            if (options.quit_after) |duration| {
                try list.append("--quit-after");
                try duration.appendArgs(allocator, list);
            } else if (options.run_for) |duration| {
                try list.append("--run-for");
                try duration.appendArgs(allocator, list);
            }
            if (options.kill_after) try list.append("--kill-after");
            if (options.headless) try list.append("--headless");
            if (!std.mem.eql(u8, options.device, "auto") or options.runner == .ios_device) try list.appendSlice(&.{ "--device", options.device });
        },
    }
}

fn backendLabel(backend: build_def.ExecutionTarget) []const u8 {
    return switch (backend) {
        .vm => "vm",
        .llvm_native => "llvm",
        .hybrid => "hybrid",
    };
}

fn instrumentBackendLabel(backend: Parsed.InstrumentBackend) []const u8 {
    return switch (backend) {
        .runtime => "runtime",
        .llvm => "llvm",
        .hybrid => "hybrid",
    };
}

fn instrumentTrackLabel(track: Parsed.InstrumentTrack) []const u8 {
    return switch (track) {
        .memory => "memory",
        .cpu => "cpu",
    };
}
