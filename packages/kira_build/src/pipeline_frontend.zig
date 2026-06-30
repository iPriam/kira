const std = @import("std");
const diag_messages = @import("kira_diagnostic_messages");
const source_pkg = @import("kira_source");
const diagnostics = @import("kira_diagnostics");
const lexer = @import("kira_lexer");
const parser = @import("kira_parser");
const semantics = @import("kira_semantics");
const syntax = @import("kira_syntax_model");
const native = @import("kira_native_lib_definition");
const ffi_support = @import("ffi_support.zig");
const package_manager = @import("kira_package_manager");
const program_graph = @import("kira_program_graph");
const timing = @import("pipeline_timing.zig");
const macro_expand = @import("macro_expand.zig");

const nowNs = timing.nowNs;
const elapsedNs = timing.elapsedNs;
const timingPrint = timing.timingPrint;

fn countSourceBytes(allocator: std.mem.Allocator, files: [][]u8) !usize {
    var total: usize = 0;
    for (files) |path| {
        const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(source_pkg.max_source_file_bytes));
        defer allocator.free(bytes);
        total += bytes.len;
    }
    return total;
}

fn countImportedPackageFiles(allocator: std.mem.Allocator, module_map: package_manager.ModuleMap) !usize {
    var total: usize = 0;
    for (module_map.owners) |owner| {
        const module_files = try program_graph.collectPackageModuleFiles(allocator, owner.source_root);
        defer {
            for (module_files) |module_file| allocator.free(module_file);
            allocator.free(module_files);
        }
        total += module_files.len;
    }
    return total;
}

pub const FrontendStage = enum {
    lexer,
    parser,
    graph,
    semantics,
    ir,
    backend_prepare,
};

pub fn compilerPhaseForStage(stage: FrontendStage) diag_messages.CompilerPhase {
    return switch (stage) {
        .lexer => .parser,
        .parser => .parser,
        .graph => .graph,
        .semantics => .semantics,
        .ir => .lowering,
        .backend_prepare => .backend_prepare,
    };
}

pub const LexPipelineResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    tokens: ?[]const syntax.Token,
    failure_stage: ?FrontendStage = null,

    pub fn failed(self: LexPipelineResult) bool {
        return self.tokens == null;
    }
};

pub const ParsePipelineResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    program: ?syntax.ast.Program,
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    failure_stage: ?FrontendStage = null,

    pub fn failed(self: ParsePipelineResult) bool {
        return self.program == null;
    }
};

pub const CacheStatus = enum {
    not_checked,
    hit,
    miss,
    stored,
};

pub const CheckPipelineResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    failure_stage: ?FrontendStage = null,
    cache_status: CacheStatus = .not_checked,
    cache_restore_ns: u64 = 0,
    cache_store_ns: u64 = 0,

    pub fn failed(self: CheckPipelineResult) bool {
        return diagnostics.hasErrors(self.diagnostics);
    }
};

pub fn diagnosticsOwnedOrFallback(
    allocator: std.mem.Allocator,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
    stage: FrontendStage,
) ![]diagnostics.Diagnostic {
    if (diags.items.len == 0) {
        try diags.append(try diag_messages.CompilerBugMessages.stageFailedWithoutDiagnostic(
            allocator,
            compilerPhaseForStage(stage),
        ));
    }
    return diags.toOwnedSlice();
}

pub fn graphDiagnosticStage(diags: []const diagnostics.Diagnostic) FrontendStage {
    for (diags) |diag| {
        if (diag.code) |code| {
            if (std.mem.eql(u8, code, "KSEM032")) return .semantics;
        }
    }
    return .graph;
}

pub fn displayTargetSelector(allocator: std.mem.Allocator, selector: ?native.TargetSelector) ![]const u8 {
    const value = selector orelse return allocator.dupe(u8, "host");
    return std.fmt.allocPrint(allocator, "{s}-{s}-{s}", .{ value.architecture, value.operating_system, value.abi });
}

pub fn lexFile(allocator: std.mem.Allocator, path: []const u8) !LexPipelineResult {
    const source_start = nowNs();
    const source = try source_pkg.SourceFile.fromPath(allocator, path);
    timingPrint("[kira:timing] SourceFile.fromPath path={s} bytes={d} ns={d}\n", .{ path, source.text.len, elapsedNs(source_start) });
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const lex_start = nowNs();
    const tokens = lexer.tokenize(allocator, &source, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] lex/tokenize path={s} ns={d}\n", .{ path, elapsedNs(lex_start) });
            return .{
                .source = source,
                .diagnostics = try diags.toOwnedSlice(),
                .tokens = null,
                .failure_stage = .lexer,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] lex/tokenize path={s} tokens={d} ns={d}\n", .{ path, tokens.len, elapsedNs(lex_start) });

    return .{
        .source = source,
        .diagnostics = try diags.toOwnedSlice(),
        .tokens = tokens,
    };
}

pub fn parseFile(allocator: std.mem.Allocator, path: []const u8) !ParsePipelineResult {
    return parseFileForTarget(allocator, path, null);
}

pub fn parseFileForTarget(
    allocator: std.mem.Allocator,
    path: []const u8,
    target_selector: ?native.TargetSelector,
) !ParsePipelineResult {
    return parseFileWithNativePreparation(allocator, path, true, target_selector);
}

pub fn parseFileWithoutNativePreparation(allocator: std.mem.Allocator, path: []const u8) !ParsePipelineResult {
    return parseFileWithNativePreparation(allocator, path, false, null);
}

fn parseFileWithNativePreparation(
    allocator: std.mem.Allocator,
    path: []const u8,
    prepare_native: bool,
    target_selector: ?native.TargetSelector,
) !ParsePipelineResult {
    const lexed = try lexFile(allocator, path);
    if (lexed.tokens == null) {
        return .{
            .source = lexed.source,
            .diagnostics = lexed.diagnostics,
            .program = null,
            .failure_stage = lexed.failure_stage,
        };
    }

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    for (lexed.diagnostics) |diag| try diags.append(diag);

    const parse_start = nowNs();
    const program = parser.parse(allocator, lexed.tokens.?, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] parse path={s} ns={d}\n", .{ path, elapsedNs(parse_start) });
            return .{
                .source = lexed.source,
                .diagnostics = try diags.toOwnedSlice(),
                .program = null,
                .failure_stage = .parser,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] parse path={s} imports={d} declarations={d} functions={d} ns={d}\n", .{ path, program.imports.len, program.decls.len, program.functions.len, elapsedNs(parse_start) });

    const native_libraries = if (prepare_native) blk: {
        const native_start = nowNs();
        const libraries = ffi_support.prepareNativeLibrariesForTarget(allocator, path, program.imports, target_selector) catch |err| switch (err) {
            error.UnsupportedTarget => {
                const display_target = try displayTargetSelector(allocator, target_selector);
                try diags.append(try diag_messages.ToolchainMessages.unsupportedNativeLibraryTarget(allocator, display_target));
                timingPrint("[kira:timing] prepareNativeLibraries path={s} native_libraries=0 ns={d}\n", .{ path, elapsedNs(native_start) });
                return .{
                    .source = lexed.source,
                    .diagnostics = try diags.toOwnedSlice(),
                    .program = null,
                    .native_libraries = &.{},
                    .failure_stage = .backend_prepare,
                };
            },
            else => return err,
        };
        timingPrint("[kira:timing] prepareNativeLibraries path={s} native_libraries={d} ns={d}\n", .{ path, libraries.len, elapsedNs(native_start) });
        break :blk libraries;
    } else blk: {
        timingPrint("[kira:timing] prepareNativeLibraries path={s} skipped=true ns=0\n", .{path});
        break :blk &.{};
    };

    return .{
        .source = lexed.source,
        .diagnostics = try diags.toOwnedSlice(),
        .program = program,
        .native_libraries = native_libraries,
    };
}

pub fn checkPackageRoot(allocator: std.mem.Allocator, source_root: []const u8) !CheckPipelineResult {
    const total_start = nowNs();

    const native_prepare_start = nowNs();
    const own_libraries = ffi_support.prepareNativeLibrariesForTarget(allocator, source_root, &.{}, null) catch |err| switch (err) {
        error.UnsupportedTarget => &.{},
        else => return err,
    };
    timingPrint("[kira:timing] prepareNativeLibraries source_root={s} native_libraries={d} ns={d}\n", .{ source_root, own_libraries.len, elapsedNs(native_prepare_start) });

    const collect_start = nowNs();
    const module_files = try program_graph.collectPackageModuleFiles(allocator, source_root);
    const collect_ns = elapsedNs(collect_start);
    const source_bytes = try countSourceBytes(allocator, module_files);
    timingPrint("[kira:timing] collectPackageModuleFiles source_root={s} source_files={d} source_bytes={d} ns={d}\n", .{ source_root, module_files.len, source_bytes, collect_ns });
    if (module_files.len == 0) {
        const source = try source_pkg.SourceFile.initOwned(allocator, source_root, "");
        const diags = try allocator.alloc(diagnostics.Diagnostic, 1);
        diags[0] = try diag_messages.PackageMessages.noBuildableTarget(allocator, source_root);
        timingPrint("[kira:timing] checkPackageRoot.total source_root={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ source_root, elapsedNs(total_start) });
        return .{
            .source = source,
            .diagnostics = diags,
            .failure_stage = .parser,
        };
    }

    const source_start = nowNs();
    const source = try source_pkg.SourceFile.fromPath(allocator, module_files[0]);
    timingPrint("[kira:timing] SourceFile.fromPath package_root_primary={s} bytes={d} ns={d}\n", .{ module_files[0], source.text.len, elapsedNs(source_start) });
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const module_map_start = nowNs();
    const module_map = try package_manager.loadModuleMapForSource(allocator, module_files[0]);
    const imported_package_files = try countImportedPackageFiles(allocator, module_map);
    timingPrint("[kira:timing] loadModuleMapForSource package_root={s} owners={d} imported_package_files={d} ns={d}\n", .{ module_files[0], module_map.owners.len, imported_package_files, elapsedNs(module_map_start) });

    const declared_prepare_start = nowNs();
    const declared_libraries = ffi_support.prepareDeclaredNativeLibrariesForTarget(allocator, own_libraries, module_map, null) catch |err| switch (err) {
        error.UnsupportedTarget => own_libraries,
        else => return err,
    };
    timingPrint("[kira:timing] prepareDeclaredNativeLibraries package_root={s} native_libraries={d} ns={d}\n", .{ module_files[0], declared_libraries.len, elapsedNs(declared_prepare_start) });

    const graph_start = nowNs();
    const merged_program = program_graph.buildProgramGraphFromFiles(allocator, module_files, module_map, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            const stage = graphDiagnosticStage(diags.items);
            timingPrint("[kira:timing] buildProgramGraphFromFiles source_root={s} ns={d}\n", .{ source_root, elapsedNs(graph_start) });
            timingPrint("[kira:timing] checkPackageRoot.total source_root={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ source_root, elapsedNs(total_start) });
            return .{
                .source = source,
                .diagnostics = try diags.toOwnedSlice(),
                .failure_stage = stage,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] buildProgramGraphFromFiles source_root={s} imports={d} declarations={d} functions={d} ns={d}\n", .{ source_root, merged_program.imports.len, merged_program.decls.len, merged_program.functions.len, elapsedNs(graph_start) });

    const expanded_program = macro_expand.expandAndCheck(allocator, merged_program, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => return .{
            .source = source,
            .diagnostics = try diags.toOwnedSlice(),
            .failure_stage = .semantics,
        },
        else => return err,
    };

    const semantics_start = nowNs();
    _ = semantics.analyzeLibrary(allocator, expanded_program, .{}, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] semantics.analyzeLibrary source_root={s} ns={d}\n", .{ source_root, elapsedNs(semantics_start) });
            timingPrint("[kira:timing] checkPackageRoot.total source_root={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ source_root, elapsedNs(total_start) });
            return .{
                .source = source,
                .diagnostics = try diags.toOwnedSlice(),
                .failure_stage = .semantics,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] semantics.analyzeLibrary source_root={s} ns={d}\n", .{ source_root, elapsedNs(semantics_start) });
    timingPrint("[kira:timing] checkPackageRoot.total source_root={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ source_root, elapsedNs(total_start) });

    return .{
        .source = source,
        .diagnostics = try diags.toOwnedSlice(),
        .failure_stage = null,
    };
}

pub fn validateImports(
    allocator: std.mem.Allocator,
    source: *const source_pkg.SourceFile,
    program: syntax.ast.Program,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !void {
    const module_map = try package_manager.loadModuleMapForSource(allocator, source.path);
    for (program.imports) |import_decl| {
        if (program_graph.packageRootOwnerForImport(module_map, import_decl.module_name)) |owner| {
            const module_files = try program_graph.collectPackageModuleFiles(allocator, owner.source_root);
            defer {
                for (module_files) |module_file| allocator.free(module_file);
                allocator.free(module_files);
            }
            if (module_files.len != 0) continue;
        }

        const resolved = try program_graph.resolveImportPath(allocator, source.path, import_decl.module_name, module_map);
        defer allocator.free(resolved.display_name);
        defer {
            for (resolved.candidates) |candidate| allocator.free(candidate);
            allocator.free(resolved.candidates);
        }

        if (resolved.exists) continue;

        try diagnostics.appendOwned(allocator, diags, .{
            .severity = .@"error",
            .code = "KSEM032",
            .title = "unresolved import",
            .message = try std.fmt.allocPrint(
                allocator,
                "Kira could not find a module for import '{s}'.",
                .{resolved.display_name},
            ),
            .labels = &.{
                diagnostics.primaryLabel(import_decl.span, "import does not resolve to a module file"),
            },
            .notes = try program_graph.resolvedCandidateNotes(allocator, resolved.candidates),
            .help = "Create the imported module under an allowed `app/` source root or remove the import.",
        });
        return error.DiagnosticsEmitted;
    }
}
