const std = @import("std");
const diag_messages = @import("kira_diagnostic_messages");
const diagnostics = @import("kira_diagnostics");
const manifest = @import("kira_manifest");
const app_generation = @import("kira_app_generation");
const CommandKind = @import("../command/CommandKind.zig");
const Kind = CommandKind.CommandKind;
const Parsed = @import("../command/ParsedCommand.zig");
const ParsedCommand = Parsed.ParsedCommand;
const Duration = @import("../command/Duration.zig");
const values = @import("ValueParsing.zig");
const zig_cli_adapter = @import("ZigCliAdapter.zig");

pub const ParseFailure = struct {
    diagnostic: diagnostics.Diagnostic,
    command_for_help: ?Kind = null,
};

pub const ParseResult = union(enum) {
    command: ParsedCommand,
    failure: ParseFailure,
};

pub fn parse(allocator: std.mem.Allocator, args: []const []const u8) !ParseResult {
    _ = zig_cli_adapter.dependencyAvailable();
    if (args.len < 2) return .{ .command = .{ .help = .{} } };

    const raw_command = args[1];
    if (isHelpFlag(raw_command)) return .{ .command = .{ .help = .{} } };
    if (std.mem.eql(u8, raw_command, "help")) return parseHelp(allocator, args[2..]);
    if (std.mem.eql(u8, raw_command, "--version") or std.mem.eql(u8, raw_command, "version")) return .{ .command = .{ .version = .{} } };

    const kind = CommandKind.parse(raw_command) orelse {
        return .{ .failure = .{ .diagnostic = diag_messages.CliMessages.unknownCommand(raw_command) } };
    };
    if (hasCommandHelp(args[2..])) return .{ .command = .{ .help = .{ .command = kind } } };

    const command = parseCommandByKind(allocator, kind, args[2..]) catch |err| return switch (err) {
        error.InvalidBackend => .{ .failure = .{ .diagnostic = try diag_messages.CliMessages.invalidBackendFlag(allocator, last_value_for_error orelse "") } },
        error.InvalidDuration => .{ .failure = .{ .diagnostic = try diag_messages.CliMessages.invalidDurationFlag(allocator, last_flag_for_error orelse "--duration", last_value_for_error orelse "") } },
        error.InvalidLivePlatform => .{ .failure = .{ .diagnostic = try diag_messages.CliMessages.invalidLivePlatform(allocator, last_value_for_error orelse "") } },
        error.MissingValue => .{ .failure = .{ .diagnostic = try diag_messages.CliMessages.missingFlagValue(last_flag_for_error orelse "option", last_expected_for_error orelse "a value") } },
        error.InvalidArguments => .{ .failure = .{ .diagnostic = try diag_messages.CliMessages.invalidFlagValue(allocator, last_flag_for_error orelse "arguments", last_value_for_error orelse "", last_expected_for_error orelse "valid command syntax"), .command_for_help = kind } },
        else => return err,
    };
    return .{ .command = command };
}

fn parseCommandByKind(allocator: std.mem.Allocator, kind: Kind, args: []const []const u8) !ParsedCommand {
    return switch (kind) {
        .check => .{ .check = try parseProjectCommand(allocator, args) },
        .build => .{ .build = try parseProjectCommand(allocator, args) },
        .run => .{ .run = try parseRun(allocator, args) },
        .live => .{ .live = try parseLive(allocator, args) },
        .export_cmd => .{ .export_cmd = try parseExport(allocator, args) },
        .new => .{ .new = try parseNew(allocator, args) },
        .fetch_llvm => .{ .fetch_llvm = try parseFetchLlvm(allocator, args) },
        .sync => .{ .sync = try parseSync(allocator, args) },
        .add => .{ .add = try parseAdd(allocator, args) },
        .remove => .{ .remove = try parseRemove(allocator, args) },
        .update => .{ .update = try parseOptionalInput(allocator, args, .update) },
        .package => .{ .package = try parsePackage(allocator, args) },
        .shader => .{ .shader = try parseShader(allocator, args) },
        .tokens => .{ .tokens = try parseOptionalInput(allocator, args, .tokens) },
        .ast => .{ .ast = try parseOptionalInput(allocator, args, .ast) },
        .instruments => .{ .instruments = try parseInstruments(allocator, args) },
        .instrument_artifact => .{ .instrument_artifact = try parseInstrumentArtifact(allocator, args) },
        .run_hybrid_artifact => .{ .run_hybrid_artifact = try parseRunHybridArtifact(allocator, args) },
        .live_runner => .{ .live_runner = try parseLiveRunnerCommand(args) },
        .help, .version => unreachable,
    };
}

threadlocal var last_flag_for_error: ?[]const u8 = null;
threadlocal var last_value_for_error: ?[]const u8 = null;
threadlocal var last_expected_for_error: ?[]const u8 = null;

fn failMissing(flag: []const u8, expected: []const u8) error{MissingValue} {
    last_flag_for_error = flag;
    last_expected_for_error = expected;
    return error.MissingValue;
}

fn failInvalid(flag: []const u8, value: []const u8, expected: []const u8) error{InvalidArguments} {
    last_flag_for_error = flag;
    last_value_for_error = value;
    last_expected_for_error = expected;
    return error.InvalidArguments;
}

fn failDuration(flag: []const u8, value: []const u8) error{InvalidDuration} {
    last_flag_for_error = flag;
    last_value_for_error = value;
    return error.InvalidDuration;
}

fn failLivePlatform(value: []const u8) error{InvalidLivePlatform} {
    last_value_for_error = value;
    return error.InvalidLivePlatform;
}

fn isHelpFlag(arg: []const u8) bool {
    return std.mem.eql(u8, arg, "--help") or std.mem.eql(u8, arg, "-h");
}

fn hasCommandHelp(args: []const []const u8) bool {
    return args.len == 1 and isHelpFlag(args[0]);
}

fn parseHelp(allocator: std.mem.Allocator, args: []const []const u8) !ParseResult {
    if (args.len == 0) return .{ .command = .{ .help = .{} } };
    if (args.len != 1) return .{ .failure = .{ .diagnostic = try diag_messages.CliMessages.invalidFlagValue(allocator, "help", args[1], "zero or one command name") } };
    const kind = CommandKind.parse(args[0]) orelse {
        return .{ .failure = .{ .diagnostic = diag_messages.CliMessages.unknownCommand(args[0]) } };
    };
    return .{ .command = .{ .help = .{ .command = kind } } };
}

fn parseProjectCommand(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.ProjectOptions {
    _ = allocator;
    var parsed = Parsed.ProjectOptions{};
    var input_path: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return failMissing("--backend", "vm, llvm, or hybrid");
            parsed.backend = values.parseBackend(args[index]) orelse {
                last_value_for_error = args[index];
                return error.InvalidBackend;
            };
            continue;
        }
        if (std.mem.eql(u8, arg, "--profile")) {
            index += 1;
            if (index >= args.len) return failMissing("--profile", "debug, profiler, or release");
            parsed.profile = manifest.BuildProfile.parse(args[index]) orelse return failInvalid("--profile", args[index], "debug, profiler, or release");
            continue;
        }
        if (std.mem.eql(u8, arg, "--offline")) {
            parsed.offline = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--locked")) {
            parsed.locked = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--timings")) {
            parsed.timings = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--print-backend-policy")) {
            parsed.print_backend_policy = true;
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported flag");
        if (input_path != null) return failInvalid("target", arg, "a single target path");
        input_path = arg;
    }
    parsed.input_path = input_path orelse ".";
    return parsed;
}

fn parseRun(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.RunOptions {
    _ = allocator;
    var parsed = Parsed.RunOptions{};
    var input_path: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (parsed.runner == null and input_path == null and !isPathLike(arg)) {
            if (manifest.RunnerId.parse(arg)) |runner| {
                if (runner == .web) {
                    parsed.runner = runner;
                    continue;
                }
            }
        }
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return failMissing("--backend", "vm, llvm, or hybrid");
            parsed.backend = values.parseBackend(args[index]) orelse {
                last_value_for_error = args[index];
                return error.InvalidBackend;
            };
            continue;
        }
        if (std.mem.eql(u8, arg, "--quit-after") or std.mem.eql(u8, arg, "-quit-after")) {
            index += 1;
            if (index >= args.len) return failMissing(arg, "a duration");
            parsed.quit_after = Duration.parse(args[index]) orelse return failDuration(arg, args[index]);
            continue;
        }
        if (std.mem.eql(u8, arg, "--surface")) {
            index += 1;
            if (index >= args.len) return failMissing("--surface", "dom, webgpu, or hybrid");
            parsed.surface = manifest.WebSurface.parse(args[index]) orelse return failInvalid("--surface", args[index], "dom, webgpu, or hybrid");
            continue;
        }
        if (std.mem.eql(u8, arg, "--offline")) {
            parsed.offline = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--locked")) {
            parsed.locked = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--trace-execution")) {
            parsed.trace_execution = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--timings")) {
            parsed.timings = true;
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported run flag");
        if (input_path != null) return failInvalid("target", arg, "a single target path");
        input_path = arg;
    }
    parsed.input_path = input_path orelse ".";
    return parsed;
}

fn parseLive(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.LiveOptions {
    _ = allocator;
    if (args.len == 0) return .{ .runner = .desktop, .input_path = "." };
    if (std.mem.eql(u8, args[0], "runners")) {
        if (args.len != 3) return failInvalid("live runners", "", "list, build, or clean plus a target");
        const mode: Parsed.LiveMode = if (std.mem.eql(u8, args[1], "list")) .runners_list else if (std.mem.eql(u8, args[1], "build")) .runners_build else if (std.mem.eql(u8, args[1], "clean")) .runners_clean else return failInvalid("live runners", args[1], "list, build, or clean");
        return .{ .mode = mode, .input_path = args[2] };
    }

    var runner: Parsed.LiveRunnerKind = .desktop;
    var device: []const u8 = "auto";
    var index: usize = 0;
    if (!isPathLike(args[0])) {
        if (parseLiveRunner(args[0])) |explicit_runner| {
            runner = explicit_runner;
            device = liveDeviceSelectorForRunnerAlias(args[0]);
            index = 1;
        } else if (args.len >= 2 and !std.mem.startsWith(u8, args[1], "-")) {
            return failLivePlatform(args[0]);
        }
    }
    var input_path: ?[]const u8 = null;
    var parsed = Parsed.LiveOptions{ .runner = runner, .input_path = "", .device = device };
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--quit-after") or std.mem.eql(u8, arg, "-quit-after")) {
            index += 1;
            if (index >= args.len) return failMissing(arg, "a duration");
            parsed.quit_after = Duration.parse(args[index]) orelse return failDuration(arg, args[index]);
            continue;
        }
        if (std.mem.eql(u8, arg, "--run-for")) {
            index += 1;
            if (index >= args.len) return failMissing("--run-for", "a duration");
            parsed.run_for = Duration.parse(args[index]) orelse return failDuration("--run-for", args[index]);
            continue;
        }
        if (std.mem.eql(u8, arg, "--kill-after")) {
            parsed.kill_after = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--headless")) {
            parsed.headless = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--profile")) {
            index += 1;
            if (index >= args.len) return failMissing("--profile", "debug, profiler, or release");
            parsed.profile = manifest.BuildProfile.parse(args[index]) orelse return failInvalid("--profile", args[index], "debug, profiler, or release");
            continue;
        }
        if (std.mem.eql(u8, arg, "--surface")) {
            index += 1;
            if (index >= args.len) return failMissing("--surface", "dom, webgpu, or hybrid");
            parsed.surface = manifest.WebSurface.parse(args[index]) orelse return failInvalid("--surface", args[index], "dom, webgpu, or hybrid");
            continue;
        }
        if (std.mem.eql(u8, arg, "--host")) {
            index += 1;
            if (index >= args.len) return failMissing("--host", "a bind host");
            parsed.host = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--port")) {
            index += 1;
            if (index >= args.len) return failMissing("--port", "a TCP port");
            parsed.port = std.fmt.parseInt(u16, args[index], 10) catch return failInvalid("--port", args[index], "a TCP port from 1 to 65535");
            continue;
        }
        if (std.mem.eql(u8, arg, "--server-url")) {
            index += 1;
            if (index >= args.len) return failMissing("--server-url", "a live server URL");
            parsed.server_url = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--device")) {
            index += 1;
            if (index >= args.len) return failMissing("--device", "a device selector");
            parsed.device = args[index];
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported live flag");
        if (input_path != null) return failInvalid("target", arg, "a single target path");
        input_path = arg;
    }
    parsed.input_path = input_path orelse ".";
    return parsed;
}

fn parseLiveRunner(value: []const u8) ?Parsed.LiveRunnerKind {
    if (std.mem.eql(u8, value, "desktop")) return .desktop;
    if (std.mem.eql(u8, value, "macos")) return .macos;
    if (std.mem.eql(u8, value, "ios") or std.mem.eql(u8, value, "ios-simulator") or std.mem.eql(u8, value, "ios-device") or std.mem.eql(u8, value, "simulator") or std.mem.eql(u8, value, "device")) return .ios;
    if (std.mem.eql(u8, value, "tvos")) return .tvos;
    if (std.mem.eql(u8, value, "visionos")) return .visionos;
    if (std.mem.eql(u8, value, "windows")) return .windows;
    if (std.mem.eql(u8, value, "android")) return .android;
    if (std.mem.eql(u8, value, "web")) return .web;
    if (std.mem.eql(u8, value, "linux")) return .linux;
    return null;
}

fn liveDeviceSelectorForRunnerAlias(value: []const u8) []const u8 {
    if (std.mem.eql(u8, value, "ios-simulator") or std.mem.eql(u8, value, "simulator")) return "simulator";
    if (std.mem.eql(u8, value, "ios-device") or std.mem.eql(u8, value, "device")) return "device";
    return "auto";
}

fn parseExport(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.ExportOptions {
    _ = allocator;
    var family: ?manifest.ExportFamily = null;
    var input_path: ?[]const u8 = null;
    var parsed = Parsed.ExportOptions{ .family = .web, .input_path = "." };
    var index: usize = 0;
    if (args.len > 0 and !isPathLike(args[0])) {
        if (manifest.ExportFamily.parse(args[0])) |explicit_family| {
            family = explicit_family;
            index = 1;
        }
    }
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--profile")) {
            index += 1;
            if (index >= args.len) return failMissing("--profile", "debug, profiler, or release");
            parsed.profile = manifest.BuildProfile.parse(args[index]) orelse return failInvalid("--profile", args[index], "debug, profiler, or release");
            continue;
        }
        if (std.mem.eql(u8, arg, "--surface")) {
            index += 1;
            if (index >= args.len) return failMissing("--surface", "dom, webgpu, or hybrid");
            parsed.surface = manifest.WebSurface.parse(args[index]) orelse return failInvalid("--surface", args[index], "dom, webgpu, or hybrid");
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported export flag");
        if (input_path != null) return failInvalid("target", arg, "a single target path");
        if (family == null and !isPathLike(arg) and manifest.ExportFamily.parse(arg) == null) return failInvalid("export", arg, "a platform export family");
        input_path = arg;
    }
    parsed.family = family orelse return failInvalid("export", "", "an export family such as apple, ios, web, linux, windows, or android");
    parsed.input_path = input_path orelse ".";
    return parsed;
}

fn isPathLike(value: []const u8) bool {
    return std.mem.startsWith(u8, value, ".") or
        std.mem.startsWith(u8, value, "/") or
        std.mem.indexOfScalar(u8, value, std.fs.path.sep) != null or
        std.mem.indexOfScalar(u8, value, '/') != null;
}

fn parseNew(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.NewOptions {
    _ = allocator;
    var kind: app_generation.TemplateKind = .app;
    var name: ?[]const u8 = null;
    var destination: ?[]const u8 = null;
    for (args) |arg| {
        if (std.mem.eql(u8, arg, "--lib")) {
            kind = .library;
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "the --lib flag or positional arguments");
        if (name == null) {
            name = arg;
        } else if (destination == null) {
            destination = arg;
        } else return failInvalid("new", arg, "name and destination");
    }
    return .{ .kind = kind, .name = name orelse return failMissing("new", "a package name"), .destination = destination orelse return failMissing("new", "a destination") };
}

fn parseFetchLlvm(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.FetchLlvmOptions {
    _ = allocator;
    var ci_metadata = false;
    var json = false;
    var archive_path: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--ci-metadata")) {
            ci_metadata = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--json")) {
            json = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--archive")) {
            index += 1;
            if (index >= args.len) return failMissing("--archive", "an archive path");
            if (archive_path != null) return failInvalid("--archive", args[index], "one archive path");
            archive_path = args[index];
            continue;
        }
        return failInvalid(arg, "", "a supported fetch-llvm flag");
    }
    if (archive_path != null and (ci_metadata or json)) return failInvalid("fetch-llvm", "", "--archive without --json or --ci-metadata");
    if (json and !ci_metadata) return failInvalid("--json", "", "--ci-metadata --json");
    if (ci_metadata and !json) return failInvalid("--ci-metadata", "", "--ci-metadata --json");
    if (archive_path) |path| return .{ .mode = .install_archive, .archive_path = path };
    if (ci_metadata) return .{ .mode = .ci_metadata_json };
    return .{};
}

fn parseSync(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.SyncOptions {
    _ = allocator;
    var parsed = Parsed.SyncOptions{};
    for (args) |arg| {
        if (std.mem.eql(u8, arg, "--offline")) {
            parsed.offline = true;
            continue;
        }
        if (std.mem.eql(u8, arg, "--locked")) {
            parsed.locked = true;
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported sync flag");
        if (parsed.input_path != null) return failInvalid("target", arg, "a single target path");
        parsed.input_path = arg;
    }
    return parsed;
}

fn parseAdd(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.AddOptions {
    _ = allocator;
    var parsed = Parsed.AddOptions{ .package_name = "" };
    var package_name: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--git")) {
            index += 1;
            if (index >= args.len) return failMissing("--git", "a Git URL");
            parsed.git_url = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--rev")) {
            index += 1;
            if (index >= args.len) return failMissing("--rev", "a commit");
            parsed.rev = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--tag")) {
            index += 1;
            if (index >= args.len) return failMissing("--tag", "a tag");
            parsed.tag = args[index];
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported add flag");
        if (package_name != null) return failInvalid("package", arg, "one package name");
        package_name = arg;
    }
    parsed.package_name = package_name orelse return failMissing("add", "a package name");
    if (parsed.git_url != null and parsed.rev == null and parsed.tag == null) return failInvalid("--git", "", "--rev or --tag");
    return parsed;
}

fn parseRemove(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.SinglePackageOptions {
    _ = allocator;
    if (args.len != 1) return failInvalid("remove", "", "one package name");
    return .{ .package_name = args[0] };
}

fn parseOptionalInput(allocator: std.mem.Allocator, args: []const []const u8, kind: Kind) !Parsed.UpdateOptions {
    _ = allocator;
    if (args.len > 1) return failInvalid(kind.label(), args[1], "zero or one target path");
    return .{ .input_path = if (args.len == 0) null else args[0] };
}

fn parsePackage(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.PackageOptions {
    _ = allocator;
    if (args.len == 0) return failMissing("package", "pack or inspect");
    const mode: Parsed.PackageMode = if (std.mem.eql(u8, args[0], "pack")) .pack else if (std.mem.eql(u8, args[0], "inspect")) .inspect else return failInvalid("package", args[0], "pack or inspect");
    if (args.len > 2) return failInvalid("package", args[2], "at most one target path");
    if (mode == .inspect and args.len != 2) return failMissing("package inspect", "an archive or project path");
    return .{ .mode = mode, .input_path = if (args.len == 2) args[1] else null };
}

fn parseShader(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.ShaderOptions {
    _ = allocator;
    if (args.len == 0) return failMissing("shader", "check, ast, or build");
    const mode: Parsed.ShaderMode = if (std.mem.eql(u8, args[0], "check")) .check else if (std.mem.eql(u8, args[0], "ast")) .ast else if (std.mem.eql(u8, args[0], "build")) .build else return failInvalid("shader", args[0], "check, ast, or build");
    var parsed = Parsed.ShaderOptions{ .mode = mode };
    var index: usize = 1;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--out-dir")) {
            index += 1;
            if (index >= args.len) return failMissing("--out-dir", "an output directory");
            parsed.out_dir = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--target")) {
            index += 1;
            if (index >= args.len) return failMissing("--target", "glsl_330, wgsl, hlsl, msl, or spirv");
            parsed.target = args[index];
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported shader flag");
        if (parsed.input_path != null) return failInvalid("shader target", arg, "one shader path or Shaders directory");
        parsed.input_path = arg;
    }
    if ((mode == .check or mode == .ast) and parsed.input_path == null) return failMissing("shader", "a .ksl file");
    return parsed;
}

fn parseInstruments(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.InstrumentsOptions {
    if (args.len < 2 or !std.mem.eql(u8, args[0], "run")) return failInvalid("instruments", "", "the run subcommand");
    var input_path: ?[]const u8 = null;
    var backend: Parsed.InstrumentBackend = .runtime;
    var tracks = std.array_list.Managed(Parsed.InstrumentTrack).init(allocator);
    var duration: ?Duration = null;
    var sample_rate: ?[]const u8 = null;
    var fail_on_growth: ?[]const u8 = null;
    var json_out: ?[]const u8 = null;
    var index: usize = 1;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return failMissing("--backend", "runtime, llvm, or hybrid");
            backend = values.parseInstrumentBackend(args[index]) orelse return failInvalid("--backend", args[index], "runtime, llvm, or hybrid");
            continue;
        }
        if (std.mem.eql(u8, arg, "--track")) {
            index += 1;
            if (index >= args.len) return failMissing("--track", "memory or cpu");
            try tracks.append(values.parseTrack(args[index]) orelse return failInvalid("--track", args[index], "memory or cpu"));
            continue;
        }
        if (std.mem.eql(u8, arg, "--duration")) {
            index += 1;
            if (index >= args.len) return failMissing("--duration", "a duration");
            duration = Duration.parse(args[index]) orelse return failDuration("--duration", args[index]);
            continue;
        }
        if (std.mem.eql(u8, arg, "--sample-rate")) {
            index += 1;
            if (index >= args.len) return failMissing("--sample-rate", "a rate such as 10hz");
            sample_rate = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--fail-on-growth")) {
            index += 1;
            if (index >= args.len) return failMissing("--fail-on-growth", "a byte threshold");
            fail_on_growth = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--json-out")) {
            index += 1;
            if (index >= args.len) return failMissing("--json-out", "an output path");
            json_out = args[index];
            continue;
        }
        if (std.mem.startsWith(u8, arg, "-")) return failInvalid(arg, "", "a supported instruments flag");
        if (input_path != null) return failInvalid("target", arg, "one target path");
        input_path = arg;
    }
    if (tracks.items.len == 0) return failMissing("--track", "at least one track");
    return .{
        .input_path = input_path orelse return failMissing("instruments run", "a target path"),
        .backend = backend,
        .tracks = try tracks.toOwnedSlice(),
        .duration = duration orelse return failMissing("--duration", "a duration"),
        .sample_rate = sample_rate orelse return failMissing("--sample-rate", "a rate"),
        .fail_on_growth = fail_on_growth,
        .json_out = json_out,
    };
}

fn parseInstrumentArtifact(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.InstrumentArtifactOptions {
    _ = allocator;
    var backend: ?Parsed.InstrumentBackend = null;
    var artifact: ?[]const u8 = null;
    var cwd: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--backend")) {
            index += 1;
            if (index >= args.len) return failMissing("--backend", "runtime, llvm, or hybrid");
            backend = values.parseInstrumentBackend(args[index]) orelse return failInvalid("--backend", args[index], "runtime, llvm, or hybrid");
            continue;
        }
        if (std.mem.eql(u8, arg, "--artifact")) {
            index += 1;
            if (index >= args.len) return failMissing("--artifact", "an artifact path");
            artifact = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--cwd")) {
            index += 1;
            if (index >= args.len) return failMissing("--cwd", "a directory");
            cwd = args[index];
            continue;
        }
        return failInvalid(arg, "", "a supported __instrument-artifact flag");
    }
    return .{
        .backend = backend orelse return failMissing("--backend", "runtime, llvm, or hybrid"),
        .artifact_path = artifact orelse return failMissing("--artifact", "an artifact path"),
        .cwd = cwd,
    };
}

fn parseRunHybridArtifact(allocator: std.mem.Allocator, args: []const []const u8) !Parsed.RunHybridArtifactOptions {
    _ = allocator;
    var manifest_path: ?[]const u8 = null;
    var cwd: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--manifest")) {
            index += 1;
            if (index >= args.len) return failMissing("--manifest", "a hybrid manifest path");
            manifest_path = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--cwd")) {
            index += 1;
            if (index >= args.len) return failMissing("--cwd", "a directory");
            cwd = args[index];
            continue;
        }
        return failInvalid(arg, "", "a supported __run-hybrid-artifact flag");
    }
    return .{
        .manifest_path = manifest_path orelse return failMissing("--manifest", "a hybrid manifest path"),
        .cwd = cwd,
    };
}

fn parseLiveRunnerCommand(args: []const []const u8) !Parsed.LiveRunnerOptions {
    if (args.len != 1) return failMissing("__live-runner", "a runner manifest path");
    return .{ .manifest_path = args[0] };
}

test "parse run quit after" {
    const result = try parse(std.testing.allocator, &.{ "kira", "run", "examples/hello", "--quit-after", "5s" });
    try std.testing.expectEqual(@as(u64, 5 * std.time.ns_per_s), result.command.run.quit_after.?.nanoseconds);
}

test "parse live shorthand quit after" {
    const result = try parse(std.testing.allocator, &.{ "kira", "live", "examples/hello", "-quit-after", "5000ms" });
    try std.testing.expectEqual(@as(u64, 5000 * std.time.ns_per_ms), result.command.live.quit_after.?.nanoseconds);
    try std.testing.expectEqualStrings("examples/hello", result.command.live.input_path);
}

test "parse live desktop explicit target and legacy duration flags" {
    const result = try parse(std.testing.allocator, &.{ "kira", "live", "desktop", ".", "--run-for", "5s", "--kill-after" });
    try std.testing.expectEqual(.desktop, result.command.live.runner);
    try std.testing.expectEqualStrings(".", result.command.live.input_path);
    try std.testing.expectEqual(@as(u64, 5 * std.time.ns_per_s), result.command.live.run_for.?.nanoseconds);
    try std.testing.expect(result.command.live.kill_after);
}

test "parse live headless and ios simulator platform" {
    const result = try parse(std.testing.allocator, &.{ "kira", "live", "ios-simulator", "examples/hello", "--headless", "--quit-after", "1s" });
    try std.testing.expectEqual(.ios, result.command.live.runner);
    try std.testing.expectEqualStrings("simulator", result.command.live.device);
    try std.testing.expect(result.command.live.headless);
    try std.testing.expectEqual(@as(u64, std.time.ns_per_s), result.command.live.quit_after.?.nanoseconds);
}

test "parse live defaults target and disambiguates path-like runner names" {
    const inferred = try parse(std.testing.allocator, &.{ "kira", "live", "web", "--surface", "dom" });
    try std.testing.expectEqual(.web, inferred.command.live.runner);
    try std.testing.expectEqualStrings(".", inferred.command.live.input_path);
    try std.testing.expectEqual(.dom, inferred.command.live.surface);

    const path_like = try parse(std.testing.allocator, &.{ "kira", "live", "./ios", "--quit-after", "1s" });
    try std.testing.expectEqual(.desktop, path_like.command.live.runner);
    try std.testing.expectEqualStrings("./ios", path_like.command.live.input_path);
}

test "parse export platform target inference" {
    const result = try parse(std.testing.allocator, &.{ "kira", "export", "web", "--profile", "release", "--surface", "dom" });
    try std.testing.expectEqual(.export_cmd, result.command.kind());
    try std.testing.expectEqual(.web, result.command.export_cmd.family);
    try std.testing.expectEqualStrings(".", result.command.export_cmd.input_path);
    try std.testing.expectEqual(.release, result.command.export_cmd.profile);
}

test "parse live rejects unknown platform before treating it as target" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const result = try parse(arena.allocator(), &.{ "kira", "live", "not-a-platform", "." });
    try std.testing.expectEqual(.failure, std.meta.activeTag(result));
    try std.testing.expectEqualStrings("KCL041", result.failure.diagnostic.code.?);
}
