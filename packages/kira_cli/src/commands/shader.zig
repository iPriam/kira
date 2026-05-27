const std = @import("std");
const build = @import("kira_build");
const diagnostics = @import("kira_diagnostics");
const shader_model = @import("kira_shader_model");
const syntax = @import("kira_ksl_syntax_model");
const support = @import("../support.zig");

pub fn execute(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    if (args.len < 1) return error.InvalidArguments;
    const subcommand = args[0];
    if (std.mem.eql(u8, subcommand, "check")) return executeCheck(allocator, args[1..], stdout, stderr);
    if (std.mem.eql(u8, subcommand, "ast")) return executeAst(allocator, args[1..], stdout, stderr);
    if (std.mem.eql(u8, subcommand, "build")) return executeBuild(allocator, args[1..], stdout, stderr);
    return error.InvalidArguments;
}

fn executeCheck(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    if (args.len != 1) return error.InvalidArguments;
    const path = args[0];
    try support.logFrontendStarted(stderr, "shader-check", path);
    const result = try build.checkShaderFile(allocator, path);
    if (result.program == null or diagnostics.hasErrors(result.diagnostics)) {
        try support.logFrontendFailed(stderr, null, path, result.diagnostics.len);
        try support.renderDiagnostics(stderr, &result.source, result.diagnostics);
        return error.CommandFailed;
    }
    try stdout.writeAll("shader check passed\n");
}

fn executeAst(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    if (args.len != 1) return error.InvalidArguments;
    const path = args[0];
    try support.logFrontendStarted(stderr, "shader-ast", path);
    const result = try build.parseShaderFile(allocator, path);
    if (result.module == null or diagnostics.hasErrors(result.diagnostics)) {
        try support.logFrontendFailed(stderr, null, path, result.diagnostics.len);
        try support.renderDiagnostics(stderr, &result.source, result.diagnostics);
        return error.CommandFailed;
    }
    try syntax.ast.dumpModule(stdout, result.module.?);
}

fn executeBuild(allocator: std.mem.Allocator, args: []const []const u8, stdout: anytype, stderr: anytype) !void {
    if (args.len > 5) return error.InvalidArguments;
    const parsed = try parseBuildArgs(args);
    const resolved = try resolveBuildInputs(allocator, parsed, stderr);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, resolved.output_dir);
    var artifact_sets_written: usize = 0;

    for (resolved.paths) |path| {
        try support.logFrontendStarted(stderr, "shader-build", path);
        const result = try build.buildShaderFileForTarget(allocator, path, resolved.target);
        if (result.program == null or diagnostics.hasErrors(result.diagnostics)) {
            try support.logFrontendFailed(stderr, null, path, result.diagnostics.len);
            try support.renderDiagnostics(stderr, &result.source, result.diagnostics);
            return error.CommandFailed;
        }

        for (result.artifacts) |artifact| {
            if (artifact.vertex_glsl) |vertex_glsl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.vert.glsl", .{artifact.shader_name}), vertex_glsl);
            }
            if (artifact.fragment_glsl) |fragment_glsl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.frag.glsl", .{artifact.shader_name}), fragment_glsl);
            }
            if (artifact.vertex_wgsl) |vertex_wgsl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.vert.wgsl", .{artifact.shader_name}), vertex_wgsl);
            }
            if (artifact.fragment_wgsl) |fragment_wgsl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.frag.wgsl", .{artifact.shader_name}), fragment_wgsl);
            }
            if (artifact.vertex_hlsl) |vertex_hlsl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.vert.hlsl", .{artifact.shader_name}), vertex_hlsl);
            }
            if (artifact.fragment_hlsl) |fragment_hlsl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.frag.hlsl", .{artifact.shader_name}), fragment_hlsl);
            }
            if (artifact.vertex_msl) |vertex_msl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.vert.metal", .{artifact.shader_name}), vertex_msl);
            }
            if (artifact.fragment_msl) |fragment_msl| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.frag.metal", .{artifact.shader_name}), fragment_msl);
            }
            if (artifact.vertex_spirv) |vertex_spirv| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.vert.spvasm", .{artifact.shader_name}), vertex_spirv);
            }
            if (artifact.fragment_spirv) |fragment_spirv| {
                try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.frag.spvasm", .{artifact.shader_name}), fragment_spirv);
            }
            try writeTextFile(allocator, resolved.output_dir, try std.fmt.allocPrint(allocator, "{s}.reflection.json", .{artifact.shader_name}), artifact.reflection_json);
            artifact_sets_written += 1;
        }
    }

    try stdout.print("shader build wrote {d} artifact set(s) from {d} shader file(s) to {s}\n", .{ artifact_sets_written, resolved.paths.len, resolved.output_dir });
}

const BuildArgs = struct {
    path: ?[]const u8 = null,
    output_dir: ?[]const u8 = null,
    target: shader_model.BackendTarget = .glsl_330,
};

const ResolvedBuildInputs = struct {
    paths: []const []const u8,
    output_dir: []const u8,
    target: shader_model.BackendTarget,
};

fn parseBuildArgs(args: []const []const u8) !BuildArgs {
    var output_dir: ?[]const u8 = null;
    var target: shader_model.BackendTarget = .glsl_330;
    var path: ?[]const u8 = null;
    var index: usize = 0;
    while (index < args.len) : (index += 1) {
        const arg = args[index];
        if (std.mem.eql(u8, arg, "--out-dir")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            output_dir = args[index];
            continue;
        }
        if (std.mem.eql(u8, arg, "--target")) {
            index += 1;
            if (index >= args.len) return error.InvalidArguments;
            target = shader_model.BackendTarget.parse(args[index]) orelse return error.InvalidArguments;
            continue;
        }
        if (path != null) return error.InvalidArguments;
        path = arg;
    }
    return .{ .path = path, .output_dir = output_dir, .target = target };
}

fn defaultOutputDir(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    const dir = std.fs.path.dirname(path) orelse ".";
    return std.fs.path.join(allocator, &.{ dir, "generated", "shaders" });
}

fn resolveBuildInputs(allocator: std.mem.Allocator, parsed: BuildArgs, stderr: anytype) !ResolvedBuildInputs {
    if (parsed.path) |path| {
        if (directoryExists(path)) {
            const discovered = try discoverShaderFilesInDir(allocator, path, false, stderr);
            const output_dir = parsed.output_dir orelse try defaultOutputDirForDirectory(allocator, path);
            return .{ .paths = discovered, .output_dir = output_dir, .target = parsed.target };
        }
        const output_dir = parsed.output_dir orelse try defaultOutputDir(allocator, path);
        const single = try allocator.alloc([]const u8, 1);
        single[0] = path;
        return .{ .paths = single, .output_dir = output_dir, .target = parsed.target };
    }

    if (!directoryExists("Shaders")) {
        try stderr.writeAll("shader build without an explicit path expects a Shaders/ directory in the current project root\n");
        return error.CommandFailed;
    }

    return .{
        .paths = try discoverShaderFilesInDir(allocator, "Shaders", true, stderr),
        .output_dir = parsed.output_dir orelse try allocator.dupe(u8, "generated/Shaders"),
        .target = parsed.target,
    };
}

fn defaultOutputDirForDirectory(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    const base = std.fs.path.basename(path);
    if (std.mem.eql(u8, base, "Shaders")) {
        const parent = std.fs.path.dirname(path) orelse ".";
        return std.fs.path.join(allocator, &.{ parent, "generated", "Shaders" });
    }
    return std.fs.path.join(allocator, &.{ path, "generated", "shaders" });
}

fn discoverShaderFilesInDir(allocator: std.mem.Allocator, dir_path: []const u8, enforce_pascal: bool, stderr: anytype) ![]const []const u8 {
    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, dir_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);

    var paths = std.array_list.Managed([]const u8).init(allocator);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file) continue;
        if (!std.mem.endsWith(u8, entry.name, ".ksl")) continue;

        const stem = entry.name[0 .. entry.name.len - 4];
        if (enforce_pascal and !isPascalCase(stem)) {
            try stderr.print("shader build expected PascalCase shader entry files in Shaders/, but found {s}\n", .{entry.name});
            return error.CommandFailed;
        }

        try paths.append(try std.fs.path.join(allocator, &.{ dir_path, entry.name }));
    }

    if (paths.items.len == 0) {
        try stderr.print("shader build found no .ksl entry shaders in {s}\n", .{dir_path});
        return error.CommandFailed;
    }

    std.mem.sort([]const u8, paths.items, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.lessThan(u8, lhs, rhs);
        }
    }.lessThan);
    return try paths.toOwnedSlice();
}

fn isPascalCase(name: []const u8) bool {
    if (name.len == 0) return false;
    if (!(name[0] >= 'A' and name[0] <= 'Z')) return false;
    for (name) |char| {
        if (char == '_' or char == '-') return false;
    }
    return true;
}

fn directoryExists(path: []const u8) bool {
    var dir = std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    defer dir.close(std.Options.debug_io);
    return true;
}

fn writeTextFile(allocator: std.mem.Allocator, output_dir: []const u8, file_name: []const u8, text: []const u8) !void {
    defer allocator.free(file_name);
    const path = try std.fs.path.join(allocator, &.{ output_dir, file_name });
    defer allocator.free(path);
    const file = try std.Io.Dir.cwd().createFile(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, text);
}

test "shader check command succeeds for a valid shader" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var stdout_buffer: [256]u8 = undefined;
    var stderr_buffer: [512]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    try execute(arena.allocator(), &.{ "check", "examples/shaders/textured_quad.ksl" }, &stdout, &stderr);
    try std.testing.expect(std.mem.indexOf(u8, stdout.buffered(), "shader check passed") != null);
}

test "shader ast command prints shader declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var stdout_buffer: [2048]u8 = undefined;
    var stderr_buffer: [512]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    try execute(arena.allocator(), &.{ "ast", "examples/shaders/textured_quad.ksl" }, &stdout, &stderr);
    try std.testing.expect(std.mem.indexOf(u8, stdout.buffered(), "shader TexturedQuad") != null);
}

test "shader build command writes artifacts" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const out_dir = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);

    var stdout_buffer: [256]u8 = undefined;
    var stderr_buffer: [512]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    try execute(allocator, &.{ "build", "examples/shaders/textured_quad.ksl", "--out-dir", out_dir }, &stdout, &stderr);

    try std.testing.expect(fileExists(out_dir, "TexturedQuad.vert.glsl"));
    try std.testing.expect(fileExists(out_dir, "TexturedQuad.frag.glsl"));
    try std.testing.expect(fileExists(out_dir, "TexturedQuad.reflection.json"));
}

test "shader build command writes WGSL artifacts when requested" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const out_dir = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);

    var stdout_buffer: [256]u8 = undefined;
    var stderr_buffer: [1024]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    try execute(allocator, &.{ "build", "tests/shaders/pass/graphics/basic_triangle/main.ksl", "--target", "wgsl", "--out-dir", out_dir }, &stdout, &stderr);

    try std.testing.expect(fileExists(out_dir, "BasicTriangle.vert.wgsl"));
    try std.testing.expect(fileExists(out_dir, "BasicTriangle.frag.wgsl"));
    try std.testing.expect(fileExists(out_dir, "BasicTriangle.reflection.json"));
}

test "shader build command writes cross-target artifacts when requested" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    inline for (.{
        .{ .target = "hlsl", .vertex = "BasicTriangle.vert.hlsl", .fragment = "BasicTriangle.frag.hlsl" },
        .{ .target = "msl", .vertex = "BasicTriangle.vert.metal", .fragment = "BasicTriangle.frag.metal" },
        .{ .target = "spirv", .vertex = "BasicTriangle.vert.spvasm", .fragment = "BasicTriangle.frag.spvasm" },
    }) |case| {
        var tmp = std.testing.tmpDir(.{});
        defer tmp.cleanup();
        const out_dir = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);

        var stdout_buffer: [256]u8 = undefined;
        var stderr_buffer: [1024]u8 = undefined;
        var stdout = std.Io.Writer.fixed(&stdout_buffer);
        var stderr = std.Io.Writer.fixed(&stderr_buffer);

        try execute(allocator, &.{ "build", "tests/shaders/pass/graphics/basic_triangle/main.ksl", "--target", case.target, "--out-dir", out_dir }, &stdout, &stderr);

        try std.testing.expect(fileExists(out_dir, case.vertex));
        try std.testing.expect(fileExists(out_dir, case.fragment));
        try std.testing.expect(fileExists(out_dir, "BasicTriangle.reflection.json"));
    }
}

test "shader build discovers PascalCase entry shaders in Shaders directory" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.createDirPath(std.testing.io, "DemoApp/Shaders");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/Shaders/BasicTriangle.ksl",
        .data =
        \\type VertexIn { let position: Float2 }
        \\type VertexOut { @builtin(position) let clip_position: Float4 }
        \\type FragmentOut { let color: Float4 }
        \\shader BasicTriangle {
        \\    vertex {
        \\        input VertexIn
        \\        output VertexOut
        \\        function entry(input: VertexIn) -> VertexOut {
        \\            let out: VertexOut
        \\            out.clip_position = Float4(input.position, 0.0, 1.0)
        \\            return out
        \\        }
        \\    }
        \\    fragment {
        \\        input VertexOut
        \\        output FragmentOut
        \\        function entry(input: VertexOut) -> FragmentOut {
        \\            let out: FragmentOut
        \\            out.color = Float4(1.0, 0.25, 0.25, 1.0)
        \\            return out
        \\        }
        \\    }
        \\}
        ,
    });

    var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
    defer {
        std.process.setCurrentDir(std.testing.io, original_cwd) catch {};
        original_cwd.close(std.Options.debug_io);
    }
    var app_dir = try tmp.dir.openDir(std.testing.io, "DemoApp", .{});
    defer app_dir.close(std.Options.debug_io);
    try std.process.setCurrentDir(std.testing.io, app_dir);

    var stdout_buffer: [256]u8 = undefined;
    var stderr_buffer: [512]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    try execute(arena.allocator(), &.{"build"}, &stdout, &stderr);

    try std.testing.expect(fileExists("generated/Shaders", "BasicTriangle.vert.glsl"));
    try std.testing.expect(fileExists("generated/Shaders", "BasicTriangle.frag.glsl"));
    try std.testing.expect(fileExists("generated/Shaders", "BasicTriangle.reflection.json"));
}

test "shader build rejects non-PascalCase shader entry files in Shaders directory" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    try tmp.dir.createDirPath(std.testing.io, "DemoApp/Shaders");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "DemoApp/Shaders/basic_triangle.ksl",
        .data = "shader Broken {}",
    });

    var original_cwd = try std.Io.Dir.cwd().openDir(std.Options.debug_io, ".", .{});
    defer {
        std.process.setCurrentDir(std.testing.io, original_cwd) catch {};
        original_cwd.close(std.Options.debug_io);
    }
    var app_dir = try tmp.dir.openDir(std.testing.io, "DemoApp", .{});
    defer app_dir.close(std.Options.debug_io);
    try std.process.setCurrentDir(std.testing.io, app_dir);

    var stdout_buffer: [128]u8 = undefined;
    var stderr_buffer: [512]u8 = undefined;
    var stdout = std.Io.Writer.fixed(&stdout_buffer);
    var stderr = std.Io.Writer.fixed(&stderr_buffer);

    try std.testing.expectError(error.CommandFailed, execute(arena.allocator(), &.{"build"}, &stdout, &stderr));
    try std.testing.expect(std.mem.indexOf(u8, stderr.buffered(), "PascalCase") != null);
}

fn fileExists(dir_path: []const u8, file_name: []const u8) bool {
    const full_path = std.fs.path.join(std.testing.allocator, &.{ dir_path, file_name }) catch return false;
    defer std.testing.allocator.free(full_path);
    std.Io.Dir.cwd().access(std.Options.debug_io, full_path, .{}) catch return false;
    return true;
}
