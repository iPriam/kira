const std = @import("std");
const builtin = @import("builtin");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const package_manager = @import("kira_package_manager");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    const parsed = try parseArgs(args);
    if (parsed.mode != .autobind) return error.InvalidArguments;

    build.setTimingsEnabled(parsed.timings or timingsEnvEnabled());
    build.setNativePreparationMode(.full);
    defer build.setNativePreparationMode(.full);

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

    const target_selector = try backendTargetSelector(allocator, parsed.backend);
    const libraries = switch (input.target.target_kind) {
        .library => blk: {
            const source_root = input.target.source_root orelse return error.ProjectEntrypointNotFound;
            break :blk try build.ensureDeclaredNativeBindingsForSourceRoot(allocator, source_root, target_selector);
        },
        .executable, .example, .source_file => blk: {
            const source_path = input.target.source_path orelse return error.ProjectEntrypointNotFound;
            break :blk try build.ensureDeclaredNativeBindingsForSource(allocator, source_path, target_selector);
        },
    };

    var generated: usize = 0;
    for (libraries) |library| {
        if (library.autobinding) |autobinding| {
            generated += 1;
            try stdout.print("autobind wrote {s}\n", .{autobinding.output_path});
        }
    }
    try stdout.print("ffi autobind completed for {d} native binding target(s)\n", .{generated});
}

const ParsedArgs = struct {
    mode: Mode = .autobind,
    backend: ?build_def.ExecutionTarget = null,
    offline: bool = false,
    locked: bool = false,
    timings: bool = false,
    input_path: []const u8,

    const Mode = enum { autobind };
};

fn parseArgs(args: []const []const u8) !ParsedArgs {
    if (args.len == 0 or !std.mem.eql(u8, args[0], "autobind")) return error.InvalidArguments;
    var backend: ?build_def.ExecutionTarget = null;
    var offline = false;
    var locked = false;
    var timings = false;
    var input_path: ?[]const u8 = null;

    var index: usize = 1;
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

fn parseBackend(arg: []const u8) ?build_def.ExecutionTarget {
    if (std.mem.eql(u8, arg, "vm")) return .vm;
    if (std.mem.eql(u8, arg, "llvm")) return .llvm_native;
    if (std.mem.eql(u8, arg, "wasm") or std.mem.eql(u8, arg, "wasm32-emscripten")) return .wasm32_emscripten;
    if (std.mem.eql(u8, arg, "hybrid")) return .hybrid;
    return null;
}

fn backendTargetSelector(allocator: std.mem.Allocator, backend: ?build_def.ExecutionTarget) !?build.NativeTargetSelector {
    const selected = backend orelse return null;
    const selector = try build.NativeTargetSelector.parse(allocator, switch (selected) {
        .vm, .llvm_native, .hybrid => switch (builtin.os.tag) {
            .linux => switch (builtin.cpu.arch) {
                .x86_64 => "x86_64-linux-gnu",
                else => return error.UnsupportedTarget,
            },
            .macos => switch (builtin.cpu.arch) {
                .aarch64 => "aarch64-macos-none",
                else => return error.UnsupportedTarget,
            },
            .windows => switch (builtin.cpu.arch) {
                .x86_64 => if (builtin.abi == .gnu) "x86_64-windows-gnu" else "x86_64-windows-msvc",
                else => return error.UnsupportedTarget,
            },
            else => return error.UnsupportedTarget,
        },
        .wasm32_emscripten => "wasm32-emscripten-none",
    });
    return selector;
}

test "parseArgs recognizes backend override for autobind" {
    const parsed = try parseArgs(&.{ "autobind", "--backend", "hybrid", "examples/hello" });
    try std.testing.expectEqual(build_def.ExecutionTarget.hybrid, parsed.backend.?);
    try std.testing.expectEqualStrings("examples/hello", parsed.input_path);
}
