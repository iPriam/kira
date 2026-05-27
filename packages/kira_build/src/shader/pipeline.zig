const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const package_manager = @import("kira_package_manager");
const syntax = @import("kira_ksl_syntax_model");
const parser = @import("kira_ksl_parser");
const semantics = @import("kira_ksl_semantics");
const shader_model = @import("kira_shader_model");
const shader_ir = @import("kira_shader_ir");
const glsl_backend = @import("kira_glsl_backend");
const wgsl_backend = @import("kira_wgsl_backend");
const hlsl_backend = @import("kira_hlsl_backend");
const msl_backend = @import("kira_msl_backend");
const spirv_backend = @import("kira_spirv_backend");
const json = @import("json.zig");

pub const ShaderTarget = shader_model.BackendTarget;

pub const ShaderFrontendStage = enum {
    lexer,
    parser,
    semantics,
    lowering,
};

pub const ShaderLexResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    tokens: ?[]const syntax.Token,
    failure_stage: ?ShaderFrontendStage = null,
};

pub const ShaderParseResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    module: ?syntax.ast.Module,
    imports: []const semantics.ImportedModule = &.{},
    failure_stage: ?ShaderFrontendStage = null,
};

pub const ShaderCheckResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    program: ?shader_ir.Program,
    failure_stage: ?ShaderFrontendStage = null,
};

pub const LoweredShaderArtifact = struct {
    shader_name: []const u8,
    target: ShaderTarget = .glsl_330,
    vertex_glsl: ?[]const u8 = null,
    fragment_glsl: ?[]const u8 = null,
    vertex_wgsl: ?[]const u8 = null,
    fragment_wgsl: ?[]const u8 = null,
    vertex_hlsl: ?[]const u8 = null,
    fragment_hlsl: ?[]const u8 = null,
    vertex_msl: ?[]const u8 = null,
    fragment_msl: ?[]const u8 = null,
    vertex_spirv: ?[]const u8 = null,
    fragment_spirv: ?[]const u8 = null,
    reflection_json: []const u8,
};

pub const ShaderBuildResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    program: ?shader_ir.Program,
    artifacts: []const LoweredShaderArtifact = &.{},
    failure_stage: ?ShaderFrontendStage = null,
};

pub fn lexFile(allocator: std.mem.Allocator, path: []const u8) !ShaderLexResult {
    const source = try source_pkg.SourceFile.fromPath(allocator, path);
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = parser.tokenize(allocator, &source, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            return .{
                .source = source,
                .diagnostics = try diags.toOwnedSlice(),
                .tokens = null,
                .failure_stage = .lexer,
            };
        },
        else => return err,
    };
    return .{
        .source = source,
        .diagnostics = try diags.toOwnedSlice(),
        .tokens = tokens,
    };
}

pub fn parseFile(allocator: std.mem.Allocator, path: []const u8) !ShaderParseResult {
    const lexed = try lexFile(allocator, path);
    if (lexed.tokens == null) {
        return .{
            .source = lexed.source,
            .diagnostics = lexed.diagnostics,
            .module = null,
            .failure_stage = lexed.failure_stage,
        };
    }

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    for (lexed.diagnostics) |diag| try diags.append(diag);
    const module = parser.parse(allocator, lexed.tokens.?, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            return .{
                .source = lexed.source,
                .diagnostics = try diags.toOwnedSlice(),
                .module = null,
                .failure_stage = .parser,
            };
        },
        else => return err,
    };

    const imports = loadImports(allocator, lexed.source.path, module, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            return .{
                .source = lexed.source,
                .diagnostics = try diags.toOwnedSlice(),
                .module = null,
                .failure_stage = .parser,
            };
        },
        else => return err,
    };
    return .{
        .source = lexed.source,
        .diagnostics = try diags.toOwnedSlice(),
        .module = module,
        .imports = imports,
    };
}

pub fn checkFile(allocator: std.mem.Allocator, path: []const u8) !ShaderCheckResult {
    const parsed = try parseFile(allocator, path);
    if (parsed.module == null) {
        return .{
            .source = parsed.source,
            .diagnostics = parsed.diagnostics,
            .program = null,
            .failure_stage = parsed.failure_stage,
        };
    }

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    for (parsed.diagnostics) |diag| try diags.append(diag);
    const program = semantics.analyze(allocator, parsed.module.?, parsed.imports, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            return .{
                .source = parsed.source,
                .diagnostics = try diags.toOwnedSlice(),
                .program = null,
                .failure_stage = .semantics,
            };
        },
        else => return err,
    };
    return .{
        .source = parsed.source,
        .diagnostics = try diags.toOwnedSlice(),
        .program = program,
    };
}

pub fn buildFile(allocator: std.mem.Allocator, path: []const u8) !ShaderBuildResult {
    return buildFileForTarget(allocator, path, .glsl_330);
}

pub fn buildFileForTarget(allocator: std.mem.Allocator, path: []const u8, target: ShaderTarget) !ShaderBuildResult {
    const checked = try checkFile(allocator, path);
    if (checked.program == null) {
        return .{
            .source = checked.source,
            .diagnostics = checked.diagnostics,
            .program = null,
            .failure_stage = checked.failure_stage,
        };
    }

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    for (checked.diagnostics) |diag| try diags.append(diag);
    var artifacts = std.array_list.Managed(LoweredShaderArtifact).init(allocator);
    for (checked.program.?.shaders) |shader_decl| {
        const reflection = try reflectionForTarget(allocator, shader_decl.reflection, target);
        switch (target) {
            .glsl_330 => {
                const lowered = glsl_backend.lowerShader(allocator, checked.program.?, shader_decl, &diags) catch |err| switch (err) {
                    error.DiagnosticsEmitted => {
                        return .{
                            .source = checked.source,
                            .diagnostics = try diags.toOwnedSlice(),
                            .program = checked.program,
                            .failure_stage = .lowering,
                        };
                    },
                    else => return err,
                };
                try artifacts.append(.{
                    .shader_name = shader_decl.name,
                    .target = target,
                    .vertex_glsl = lowered.vertex_source,
                    .fragment_glsl = lowered.fragment_source,
                    .reflection_json = try json.renderReflectionJson(allocator, reflection),
                });
            },
            .wgsl => {
                const lowered = wgsl_backend.lowerShader(allocator, checked.program.?, shader_decl, &diags) catch |err| switch (err) {
                    error.DiagnosticsEmitted => {
                        return .{
                            .source = checked.source,
                            .diagnostics = try diags.toOwnedSlice(),
                            .program = checked.program,
                            .failure_stage = .lowering,
                        };
                    },
                    else => return err,
                };
                try artifacts.append(.{
                    .shader_name = shader_decl.name,
                    .target = target,
                    .vertex_wgsl = lowered.vertex_source,
                    .fragment_wgsl = lowered.fragment_source,
                    .reflection_json = try json.renderReflectionJson(allocator, reflection),
                });
            },
            .hlsl => {
                const lowered = hlsl_backend.lowerShader(allocator, checked.program.?, shader_decl, &diags) catch |err| switch (err) {
                    error.DiagnosticsEmitted => {
                        return .{
                            .source = checked.source,
                            .diagnostics = try diags.toOwnedSlice(),
                            .program = checked.program,
                            .failure_stage = .lowering,
                        };
                    },
                    else => return err,
                };
                try artifacts.append(.{
                    .shader_name = shader_decl.name,
                    .target = target,
                    .vertex_hlsl = lowered.vertex_source,
                    .fragment_hlsl = lowered.fragment_source,
                    .reflection_json = try json.renderReflectionJson(allocator, reflection),
                });
            },
            .msl => {
                const lowered = msl_backend.lowerShader(allocator, checked.program.?, shader_decl, &diags) catch |err| switch (err) {
                    error.DiagnosticsEmitted => {
                        return .{
                            .source = checked.source,
                            .diagnostics = try diags.toOwnedSlice(),
                            .program = checked.program,
                            .failure_stage = .lowering,
                        };
                    },
                    else => return err,
                };
                try artifacts.append(.{
                    .shader_name = shader_decl.name,
                    .target = target,
                    .vertex_msl = lowered.vertex_source,
                    .fragment_msl = lowered.fragment_source,
                    .reflection_json = try json.renderReflectionJson(allocator, reflection),
                });
            },
            .spirv => {
                const lowered = spirv_backend.lowerShader(allocator, checked.program.?, shader_decl, &diags) catch |err| switch (err) {
                    error.DiagnosticsEmitted => {
                        return .{
                            .source = checked.source,
                            .diagnostics = try diags.toOwnedSlice(),
                            .program = checked.program,
                            .failure_stage = .lowering,
                        };
                    },
                    else => return err,
                };
                try artifacts.append(.{
                    .shader_name = shader_decl.name,
                    .target = target,
                    .vertex_spirv = lowered.vertex_source,
                    .fragment_spirv = lowered.fragment_source,
                    .reflection_json = try json.renderReflectionJson(allocator, reflection),
                });
            },
        }
    }

    return .{
        .source = checked.source,
        .diagnostics = try diags.toOwnedSlice(),
        .program = checked.program,
        .artifacts = try artifacts.toOwnedSlice(),
    };
}

fn reflectionForTarget(
    allocator: std.mem.Allocator,
    reflection: shader_model.Reflection,
    target: ShaderTarget,
) !shader_model.Reflection {
    if (target == .glsl_330) return reflection;
    const resources = try allocator.alloc(shader_model.ReflectedResource, reflection.resources.len);
    for (reflection.resources, 0..) |resource_decl, resource_index| {
        const bindings = try allocator.alloc(shader_model.BackendBinding, resource_decl.backend_bindings.len);
        for (resource_decl.backend_bindings, 0..) |binding, binding_index| {
            bindings[binding_index] = .{
                .target = target,
                .group_index = binding.group_index,
                .binding_index = binding.binding_index,
                .glsl_name = binding.glsl_name,
            };
        }
        resources[resource_index] = resource_decl;
        resources[resource_index].backend_bindings = bindings;
    }
    return .{
        .shader_name = reflection.shader_name,
        .shader_kind = reflection.shader_kind,
        .backend = target,
        .options = reflection.options,
        .stages = reflection.stages,
        .types = reflection.types,
        .resources = resources,
    };
}

fn loadImports(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    module: syntax.ast.Module,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) ![]const semantics.ImportedModule {
    var imports = std.array_list.Managed(semantics.ImportedModule).init(allocator);
    var visited = std.StringHashMap(void).init(allocator);
    for (module.imports) |import_decl| {
        const resolved = resolveImportPath(allocator, source_path, import_decl.module_name) catch |err| switch (err) {
            error.FileNotFound => {
                try diagnostics.appendOwned(allocator, out_diagnostics, .{
                    .severity = .@"error",
                    .code = "KSL062",
                    .title = "unresolved shader import",
                    .message = try std.fmt.allocPrint(allocator, "KSL could not resolve the imported module '{s}'.", .{try renderQualifiedName(allocator, import_decl.module_name, '.')}),
                    .labels = &.{diagnostics.primaryLabel(import_decl.span, "import does not resolve to a .ksl file")},
                    .help = "Create the imported `.ksl` module or fix the import path casing and module name.",
                });
                return error.DiagnosticsEmitted;
            },
            else => return err,
        };
        if (visited.contains(resolved.path)) continue;
        try visited.put(try allocator.dupe(u8, resolved.path), {});

        const imported_source = try source_pkg.SourceFile.fromPath(allocator, resolved.path);
        const tokens = parser.tokenize(allocator, &imported_source, out_diagnostics) catch return error.DiagnosticsEmitted;
        const imported_module = parser.parse(allocator, tokens, out_diagnostics) catch return error.DiagnosticsEmitted;
        try imports.append(.{
            .alias = import_decl.alias orelse moduleAliasFromName(import_decl.module_name),
            .module_name = resolved.module_name,
            .module = imported_module,
        });
    }
    return imports.toOwnedSlice();
}

const ResolvedImport = struct {
    module_name: []const u8,
    path: []const u8,
};

fn resolveImportPath(allocator: std.mem.Allocator, source_path: []const u8, module_name: syntax.ast.QualifiedName) !ResolvedImport {
    const rendered_name = try renderQualifiedName(allocator, module_name, '.');
    const relative_path = try renderQualifiedName(allocator, module_name, std.fs.path.sep);
    const file_relative = try std.fmt.allocPrint(allocator, "{s}.ksl", .{relative_path});
    const source_dir = std.fs.path.dirname(source_path) orelse ".";
    var cursor = try absolutizePath(allocator, source_dir);
    while (true) {
        const file_candidate = try std.fs.path.join(allocator, &.{ cursor, file_relative });
        if (fileExists(file_candidate)) {
            return .{
                .module_name = rendered_name,
                .path = file_candidate,
            };
        }
        const dir_candidate = try std.fs.path.join(allocator, &.{ cursor, relative_path, "main.ksl" });
        if (fileExists(dir_candidate)) {
            return .{
                .module_name = rendered_name,
                .path = dir_candidate,
            };
        }
        const parent = std.fs.path.dirname(cursor) orelse break;
        if (std.mem.eql(u8, parent, cursor)) break;
        cursor = try allocator.dupe(u8, parent);
    }
    return error.FileNotFound;
}

fn moduleAliasFromName(name: syntax.ast.QualifiedName) []const u8 {
    return name.segments[name.segments.len - 1].text;
}

fn renderQualifiedName(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName, separator: u8) ![]const u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    for (name.segments, 0..) |segment, index| {
        if (index != 0) try out.append(separator);
        try out.appendSlice(segment.text);
    }
    return out.toOwnedSlice();
}

fn absolutizePath(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(path)) return allocator.dupe(u8, path);
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, path, allocator);
}

fn fileExists(path: []const u8) bool {
    std.Io.Dir.cwd().access(std.Options.debug_io, path, .{}) catch return false;
    return true;
}

test "shader pipeline builds textured quad and emits reflection" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try buildFile(arena.allocator(), "tests/shaders/pass/graphics/textured_quad/main.ksl");
    try std.testing.expect(result.program != null);
    try std.testing.expectEqual(@as(usize, 1), result.artifacts.len);
    try std.testing.expect(result.artifacts[0].vertex_glsl != null);
    try std.testing.expect(result.artifacts[0].fragment_glsl != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].reflection_json, "\"backend\": \"glsl_330\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].reflection_json, "\"size\": 64") != null);
}

test "shader pipeline matches textured quad golden outputs" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const result = try buildFile(allocator, "tests/shaders/pass/graphics/textured_quad/main.ksl");
    try std.testing.expect(result.artifacts.len == 1);
    try expectFileText(allocator, "tests/shaders/pass/graphics/textured_quad/expected.vert.glsl", result.artifacts[0].vertex_glsl.?);
    try expectFileText(allocator, "tests/shaders/pass/graphics/textured_quad/expected.frag.glsl", result.artifacts[0].fragment_glsl.?);
    try expectFileText(allocator, "tests/shaders/pass/graphics/textured_quad/expected.reflection.json", result.artifacts[0].reflection_json);
}

test "shader pipeline matches basic triangle golden outputs" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const result = try buildFile(allocator, "tests/shaders/pass/graphics/basic_triangle/main.ksl");
    try std.testing.expect(result.artifacts.len == 1);
    try expectFileText(allocator, "tests/shaders/pass/graphics/basic_triangle/expected.vert.glsl", result.artifacts[0].vertex_glsl.?);
    try expectFileText(allocator, "tests/shaders/pass/graphics/basic_triangle/expected.frag.glsl", result.artifacts[0].fragment_glsl.?);
    try expectFileText(allocator, "tests/shaders/pass/graphics/basic_triangle/expected.reflection.json", result.artifacts[0].reflection_json);
}

test "shader pipeline emits WGSL graphics artifacts" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try buildFileForTarget(arena.allocator(), "tests/shaders/pass/graphics/basic_triangle/main.ksl", .wgsl);
    try std.testing.expect(result.artifacts.len == 1);
    try std.testing.expect(result.artifacts[0].vertex_wgsl != null);
    try std.testing.expect(result.artifacts[0].fragment_wgsl != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].vertex_wgsl.?, "@vertex") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].vertex_wgsl.?, "@builtin(position)") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].fragment_wgsl.?, "@fragment") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].reflection_json, "\"backend\": \"wgsl\"") != null);
}

test "shader pipeline emits HLSL MSL and SPIR-V graphics artifacts" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const hlsl = try buildFileForTarget(allocator, "tests/shaders/pass/graphics/basic_triangle/main.ksl", .hlsl);
    try std.testing.expect(hlsl.artifacts.len == 1);
    try std.testing.expect(hlsl.artifacts[0].vertex_hlsl != null);
    try std.testing.expect(hlsl.artifacts[0].fragment_hlsl != null);
    try std.testing.expect(std.mem.indexOf(u8, hlsl.artifacts[0].vertex_hlsl.?, "SV_Position") != null);
    try std.testing.expect(std.mem.indexOf(u8, hlsl.artifacts[0].fragment_hlsl.?, "SV_Target") != null);
    try std.testing.expect(std.mem.indexOf(u8, hlsl.artifacts[0].reflection_json, "\"backend\": \"hlsl\"") != null);

    const msl = try buildFileForTarget(allocator, "tests/shaders/pass/graphics/basic_triangle/main.ksl", .msl);
    try std.testing.expect(msl.artifacts.len == 1);
    try std.testing.expect(msl.artifacts[0].vertex_msl != null);
    try std.testing.expect(msl.artifacts[0].fragment_msl != null);
    try std.testing.expect(std.mem.indexOf(u8, msl.artifacts[0].vertex_msl.?, "vertex VertexOut") != null);
    try std.testing.expect(std.mem.indexOf(u8, msl.artifacts[0].fragment_msl.?, "[[color(0)]]") != null);
    try std.testing.expect(std.mem.indexOf(u8, msl.artifacts[0].reflection_json, "\"backend\": \"msl\"") != null);

    const spirv = try buildFileForTarget(allocator, "tests/shaders/pass/graphics/basic_triangle/main.ksl", .spirv);
    try std.testing.expect(spirv.artifacts.len == 1);
    try std.testing.expect(spirv.artifacts[0].vertex_spirv != null);
    try std.testing.expect(spirv.artifacts[0].fragment_spirv != null);
    try std.testing.expect(std.mem.indexOf(u8, spirv.artifacts[0].vertex_spirv.?, "OpEntryPoint Vertex") != null);
    try std.testing.expect(std.mem.indexOf(u8, spirv.artifacts[0].fragment_spirv.?, "OpExecutionMode %main OriginUpperLeft") != null);
    try std.testing.expect(std.mem.indexOf(u8, spirv.artifacts[0].reflection_json, "\"backend\": \"spirv\"") != null);
}

test "shader pipeline supports imports" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try buildFile(arena.allocator(), "examples/shaders/lit_surface.ksl");
    try std.testing.expect(result.program != null);
    try std.testing.expectEqual(@as(usize, 1), result.artifacts.len);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].fragment_glsl.?, "Lighting__lambert") != null);
}

test "shader parser rejects malformed resource declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try parseFile(arena.allocator(), "tests/shaders/fail/parser/malformed_resource/main.ksl");
    try std.testing.expect(result.module == null);
    try expectDiagnosticCode(result.diagnostics, "KSLP010");
}

test "shader parser reports unresolved imports" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try parseFile(arena.allocator(), "tests/shaders/fail/parser/missing_import/main.ksl");
    try std.testing.expect(result.module == null);
    try expectDiagnosticCode(result.diagnostics, "KSL062");
}

test "shader semantics rejects ambiguous integer literals" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try checkFile(arena.allocator(), "tests/shaders/fail/semantics/ambiguous_literal/main.ksl");
    try std.testing.expect(result.program == null);
    try expectDiagnosticCode(result.diagnostics, "KSL021");
}

test "shader semantics rejects stage io mismatches" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try checkFile(arena.allocator(), "tests/shaders/fail/semantics/mismatched_stage_io/main.ksl");
    try std.testing.expect(result.program == null);
    try expectDiagnosticCode(result.diagnostics, "KSL041");
}

test "shader semantics rejects illegal builtins" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try checkFile(arena.allocator(), "tests/shaders/fail/semantics/illegal_builtin/main.ksl");
    try std.testing.expect(result.program == null);
    try expectDiagnosticCode(result.diagnostics, "KSL051");
}

test "shader semantics rejects uniform writes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try checkFile(arena.allocator(), "tests/shaders/fail/semantics/uniform_write/main.ksl");
    try std.testing.expect(result.program == null);
    try expectDiagnosticCode(result.diagnostics, "KSL071");
}

test "shader binding assignment is deterministic and class ordered" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const temp_dir = std.testing.tmpDir(.{});
    defer temp_dir.cleanup();
    try temp_dir.dir.writeFile(std.testing.io, .{
        .sub_path = "main.ksl",
        .data =
        \\type CameraUniform { let view_projection: Float4x4 }
        \\type MaterialUniform { let tint: Float4 }
        \\type VertexIn { let position: Float2 }
        \\type VertexOut { @builtin(position) let clip_position: Float4 }
        \\shader Ordered {
        \\    group Material { uniform material: MaterialUniform }
        \\    group Frame { uniform camera: CameraUniform }
        \\    vertex {
        \\        input VertexIn
        \\        output VertexOut
        \\        function entry(input: VertexIn) -> VertexOut {
        \\            let out: VertexOut
        \\            out.clip_position = Float4(input.position, 0.0, 1.0)
        \\            return out
        \\        }
        \\    }
        \\}
        ,
    });
    const source_path = try temp_dir.dir.realPathFileAlloc(std.testing.io, "main.ksl", allocator);
    const result = try buildFile(allocator, source_path);
    try std.testing.expect(result.artifacts.len == 1);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].reflection_json, "\"group\": \"Frame\"") != null);
    try std.testing.expect(std.mem.indexOf(u8, result.artifacts[0].reflection_json, "\"group_index\": 0") != null);
}

test "shader lowering rejects compute on glsl 330 backend" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const result = try buildFile(arena.allocator(), "tests/shaders/fail/lowering/compute_glsl/main.ksl");
    try std.testing.expect(result.artifacts.len == 0);
    try expectDiagnosticCode(result.diagnostics, "KSL121");
}

fn expectDiagnosticCode(items: []const diagnostics.Diagnostic, code: []const u8) !void {
    for (items) |item| {
        if (item.code) |item_code| {
            if (std.mem.eql(u8, item_code, code)) return;
        }
    }
    return error.ExpectedDiagnosticNotFound;
}

fn expectFileText(allocator: std.mem.Allocator, path: []const u8, actual: []const u8) !void {
    const expected = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(1 << 20));
    try std.testing.expectEqualStrings(expected, actual);
}
