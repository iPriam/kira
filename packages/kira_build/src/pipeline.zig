const std = @import("std");
const builtin = @import("builtin");
const diag_messages = @import("kira_diagnostic_messages");
const source_pkg = @import("kira_source");
const diagnostics = @import("kira_diagnostics");
const lexer = @import("kira_lexer");
const parser = @import("kira_parser");
const semantics = @import("kira_semantics");
const syntax = @import("kira_syntax_model");
const ir = @import("kira_ir");
const bytecode = @import("kira_bytecode");
const build_def = @import("kira_build_definition");
const llvm_backend = @import("kira_llvm_backend");
const native = @import("kira_native_lib_definition");
const ffi_support = @import("ffi_support.zig");
const package_manager = @import("kira_package_manager");
const program_graph = @import("kira_program_graph");

var timings_enabled: bool = false;

pub fn setTimingsEnabled(enabled: bool) void {
    timings_enabled = enabled;
    program_graph.setTimingsEnabled(enabled);
}

fn nowNs() i128 {
    if (builtin.os.tag == .windows) {
        var counter: std.os.windows.LARGE_INTEGER = undefined;
        var frequency: std.os.windows.LARGE_INTEGER = undefined;
        _ = std.os.windows.ntdll.RtlQueryPerformanceCounter(&counter);
        _ = std.os.windows.ntdll.RtlQueryPerformanceFrequency(&frequency);
        return @divTrunc(@as(i128, counter) * 1_000_000_000, @as(i128, frequency));
    }
    return 0;
}

fn elapsedNs(start: i128) u64 {
    return @intCast(nowNs() - start);
}

pub fn timingPrint(comptime fmt: []const u8, args: anytype) void {
    if (timings_enabled) std.debug.print(fmt, args);
}

fn countSourceBytes(allocator: std.mem.Allocator, files: [][]u8) !usize {
    var total: usize = 0;
    for (files) |path| {
        const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(1024 * 1024));
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

fn compilerPhaseForStage(stage: FrontendStage) diag_messages.CompilerPhase {
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

pub const CacheStatus = enum {
    not_checked,
    hit,
    miss,
    stored,
};

pub const FrontendPipelineResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    ir_program: ?ir.Program,
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    failure_stage: ?FrontendStage = null,

    pub fn failed(self: FrontendPipelineResult) bool {
        return self.ir_program == null;
    }
};

pub const VmPipelineResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    ir_program: ?ir.Program,
    bytecode_module: ?bytecode.Module,
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    failure_stage: ?FrontendStage = null,

    pub fn failed(self: VmPipelineResult) bool {
        return self.bytecode_module == null;
    }
};

pub const ExecutablePipelineResult = struct {
    source: source_pkg.SourceFile,
    diagnostics: []const diagnostics.Diagnostic,
    ir_program: ?ir.Program,
    bytecode_module: ?bytecode.Module = null,
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    failure_stage: ?FrontendStage = null,

    pub fn failed(self: ExecutablePipelineResult) bool {
        return self.ir_program == null or diagnostics.hasErrors(self.diagnostics);
    }
};

fn diagnosticsOwnedOrFallback(
    allocator: std.mem.Allocator,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
    stage: FrontendStage,
) ![]const diagnostics.Diagnostic {
    if (diags.items.len == 0) {
        try diags.append(try diag_messages.CompilerBugMessages.stageFailedWithoutDiagnostic(
            allocator,
            compilerPhaseForStage(stage),
        ));
    }
    return diags.toOwnedSlice();
}

fn graphDiagnosticStage(diags: []const diagnostics.Diagnostic) FrontendStage {
    for (diags) |diag| {
        if (diag.code) |code| {
            if (std.mem.eql(u8, code, "KSEM032")) return .semantics;
        }
    }
    return .graph;
}

pub fn compileFileToIr(allocator: std.mem.Allocator, path: []const u8) !FrontendPipelineResult {
    return compileFileToIrForTarget(allocator, path, null);
}

pub fn compileFileToIrForTarget(
    allocator: std.mem.Allocator,
    path: []const u8,
    target_selector: ?native.TargetSelector,
) !FrontendPipelineResult {
    const total_start = nowNs();
    const parsed = try parseFileForTarget(allocator, path, target_selector);
    if (parsed.program == null) {
        timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
        return .{
            .source = parsed.source,
            .diagnostics = parsed.diagnostics,
            .ir_program = null,
            .native_libraries = parsed.native_libraries,
            .failure_stage = parsed.failure_stage,
        };
    }

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    for (parsed.diagnostics) |diag| try diags.append(diag);

    const module_map_start = nowNs();
    const module_map = try package_manager.loadModuleMapForSource(allocator, parsed.source.path);
    timingPrint("[kira:timing] loadModuleMapForSource path={s} owners={d} ns={d}\n", .{ parsed.source.path, module_map.owners.len, elapsedNs(module_map_start) });

    const graph_start = nowNs();
    const merged_program = program_graph.buildProgramGraph(allocator, parsed.source.path, parsed.program.?, module_map, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            const stage = graphDiagnosticStage(diags.items);
            timingPrint("[kira:timing] buildProgramGraph path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(graph_start) });
            timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diagnosticsOwnedOrFallback(allocator, &diags, stage),
                .ir_program = null,
                .native_libraries = parsed.native_libraries,
                .failure_stage = stage,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] buildProgramGraph path={s} imports={d} declarations={d} functions={d} ns={d}\n", .{ parsed.source.path, merged_program.imports.len, merged_program.decls.len, merged_program.functions.len, elapsedNs(graph_start) });

    const native_import_start = nowNs();
    const native_libraries = try ffi_support.prepareImportedNativeLibrariesForTarget(allocator, parsed.native_libraries, merged_program.imports, module_map, target_selector);
    timingPrint("[kira:timing] prepareImportedNativeLibraries path={s} imports={d} native_libraries={d} ns={d}\n", .{ parsed.source.path, merged_program.imports.len, native_libraries.len, elapsedNs(native_import_start) });

    const validate_start = nowNs();
    validateImports(allocator, &parsed.source, merged_program, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] validateImports path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(validate_start) });
            timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diagnosticsOwnedOrFallback(allocator, &diags, .semantics),
                .ir_program = null,
                .native_libraries = native_libraries,
                .failure_stage = .semantics,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] validateImports path={s} imports={d} ns={d}\n", .{ parsed.source.path, merged_program.imports.len, elapsedNs(validate_start) });

    const semantics_start = nowNs();
    const hir = semantics.analyzeWithImports(allocator, merged_program, .{}, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] semantics.analyzeWithImports path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(semantics_start) });
            timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diagnosticsOwnedOrFallback(allocator, &diags, .semantics),
                .ir_program = null,
                .native_libraries = native_libraries,
                .failure_stage = .semantics,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] semantics.analyzeWithImports path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(semantics_start) });

    const ir_start = nowNs();
    const ir_program = ir.lowerProgram(allocator, hir) catch |err| switch (err) {
        error.UnsupportedExecutableFeature, error.UnsupportedType => {
            try diags.append(diag_messages.BackendMessages.unsupportedExecutableFeature());
            timingPrint("[kira:timing] ir.lowerProgram path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(ir_start) });
            timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diags.toOwnedSlice(),
                .ir_program = null,
                .native_libraries = native_libraries,
                .failure_stage = .ir,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] ir.lowerProgram path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(ir_start) });
    timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
    return .{
        .source = parsed.source,
        .diagnostics = try diags.toOwnedSlice(),
        .ir_program = ir_program,
        .native_libraries = native_libraries,
    };
}

pub fn compileFileToBytecode(allocator: std.mem.Allocator, path: []const u8) !VmPipelineResult {
    const prepared = try compileFileForBackend(allocator, path, .vm, &.{});
    return .{
        .source = prepared.source,
        .diagnostics = prepared.diagnostics,
        .ir_program = prepared.ir_program,
        .bytecode_module = prepared.bytecode_module,
        .native_libraries = prepared.native_libraries,
        .failure_stage = prepared.failure_stage,
    };
}

pub fn compileFileForBackend(
    allocator: std.mem.Allocator,
    path: []const u8,
    target: build_def.ExecutionTarget,
    explicit_native_libraries: []const native.ResolvedNativeLibrary,
) !ExecutablePipelineResult {
    return compileFileForBackendWithSelector(allocator, path, target, null, explicit_native_libraries);
}

pub fn compileFileForBackendWithSelector(
    allocator: std.mem.Allocator,
    path: []const u8,
    target: build_def.ExecutionTarget,
    target_selector: ?native.TargetSelector,
    explicit_native_libraries: []const native.ResolvedNativeLibrary,
) !ExecutablePipelineResult {
    const total_start = nowNs();
    const frontend = try compileFileToIrForTarget(allocator, path, target_selector);
    if (frontend.ir_program == null or diagnostics.hasErrors(frontend.diagnostics)) {
        timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
        return .{
            .source = frontend.source,
            .diagnostics = frontend.diagnostics,
            .ir_program = null,
            .bytecode_module = null,
            .native_libraries = frontend.native_libraries,
            .failure_stage = frontend.failure_stage,
        };
    }

    const ir_program = frontend.ir_program.?;
    const merge_start = nowNs();
    const merged_native_libraries = try mergeNativeLibraries(allocator, explicit_native_libraries, frontend.native_libraries);
    timingPrint("[kira:timing] mergeNativeLibraries path={s} explicit={d} discovered={d} merged={d} ns={d}\n", .{ path, explicit_native_libraries.len, frontend.native_libraries.len, merged_native_libraries.len, elapsedNs(merge_start) });

    switch (target) {
        .vm => {
            const bytecode_start = nowNs();
            const module = bytecode.compileProgram(allocator, ir_program, .vm) catch |err| {
                const backend_diagnostics = try backendDiagnosticsForVm(allocator, frontend.source.path, err, merged_native_libraries);
                timingPrint("[kira:timing] bytecode.compileProgram path={s} backend=vm ns={d}\n", .{ path, elapsedNs(bytecode_start) });
                timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
                return .{
                    .source = frontend.source,
                    .diagnostics = backend_diagnostics,
                    .ir_program = ir_program,
                    .bytecode_module = null,
                    .native_libraries = merged_native_libraries,
                    .failure_stage = .backend_prepare,
                };
            };
            timingPrint("[kira:timing] bytecode.compileProgram path={s} backend=vm ns={d}\n", .{ path, elapsedNs(bytecode_start) });
            timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
            return .{
                .source = frontend.source,
                .diagnostics = frontend.diagnostics,
                .ir_program = ir_program,
                .bytecode_module = module,
                .native_libraries = merged_native_libraries,
                .failure_stage = frontend.failure_stage,
            };
        },
        .llvm_native => {
            const llvm_start = nowNs();
            llvm_backend.validate(allocator, .{
                .mode = .llvm_native,
                .program = &ir_program,
                .module_name = std.fs.path.stem(path),
                .emit = dummyNativeEmitOptions(),
                .target_selector = target_selector,
                .resolved_native_libraries = merged_native_libraries,
            }) catch |err| {
                const backend_diagnostics = try backendDiagnostics(allocator, frontend.source.path, err);
                timingPrint("[kira:timing] llvm_backend.validate path={s} backend=llvm_native ns={d}\n", .{ path, elapsedNs(llvm_start) });
                timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
                return .{
                    .source = frontend.source,
                    .diagnostics = backend_diagnostics,
                    .ir_program = ir_program,
                    .bytecode_module = null,
                    .native_libraries = merged_native_libraries,
                    .failure_stage = .backend_prepare,
                };
            };
            timingPrint("[kira:timing] llvm_backend.validate path={s} backend=llvm_native ns={d}\n", .{ path, elapsedNs(llvm_start) });
            timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
            return .{
                .source = frontend.source,
                .diagnostics = frontend.diagnostics,
                .ir_program = ir_program,
                .bytecode_module = null,
                .native_libraries = merged_native_libraries,
                .failure_stage = frontend.failure_stage,
            };
        },
        .hybrid => {
            const bytecode_start = nowNs();
            const module = bytecode.compileProgram(allocator, ir_program, .hybrid_runtime) catch |err| {
                const backend_diagnostics = try backendDiagnostics(allocator, frontend.source.path, err);
                timingPrint("[kira:timing] bytecode.compileProgram path={s} backend=hybrid_runtime ns={d}\n", .{ path, elapsedNs(bytecode_start) });
                timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
                return .{
                    .source = frontend.source,
                    .diagnostics = backend_diagnostics,
                    .ir_program = ir_program,
                    .bytecode_module = null,
                    .native_libraries = merged_native_libraries,
                    .failure_stage = .backend_prepare,
                };
            };
            timingPrint("[kira:timing] bytecode.compileProgram path={s} backend=hybrid_runtime ns={d}\n", .{ path, elapsedNs(bytecode_start) });
            const llvm_start = nowNs();
            llvm_backend.validate(allocator, .{
                .mode = .hybrid,
                .program = &ir_program,
                .module_name = std.fs.path.stem(path),
                .emit = dummyNativeEmitOptions(),
                .target_selector = target_selector,
                .resolved_native_libraries = merged_native_libraries,
            }) catch |err| {
                const backend_diagnostics = try backendDiagnostics(allocator, frontend.source.path, err);
                timingPrint("[kira:timing] llvm_backend.validate path={s} backend=hybrid ns={d}\n", .{ path, elapsedNs(llvm_start) });
                timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
                return .{
                    .source = frontend.source,
                    .diagnostics = backend_diagnostics,
                    .ir_program = ir_program,
                    .bytecode_module = null,
                    .native_libraries = merged_native_libraries,
                    .failure_stage = .backend_prepare,
                };
            };
            timingPrint("[kira:timing] llvm_backend.validate path={s} backend=hybrid ns={d}\n", .{ path, elapsedNs(llvm_start) });
            timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
            return .{
                .source = frontend.source,
                .diagnostics = frontend.diagnostics,
                .ir_program = ir_program,
                .bytecode_module = module,
                .native_libraries = merged_native_libraries,
                .failure_stage = frontend.failure_stage,
            };
        },
    }
}

pub fn checkFileForBackend(
    allocator: std.mem.Allocator,
    path: []const u8,
    target: build_def.ExecutionTarget,
) !CheckPipelineResult {
    return checkFileForBackendWithSelector(allocator, path, target, null);
}

pub fn checkFileForBackendWithSelector(
    allocator: std.mem.Allocator,
    path: []const u8,
    target: build_def.ExecutionTarget,
    target_selector: ?native.TargetSelector,
) !CheckPipelineResult {
    const total_start = nowNs();
    const prepared = try compileFileForBackendWithSelector(allocator, path, target, target_selector, &.{});
    const reached_ir = prepared.ir_program != null or prepared.failure_stage == .ir or prepared.failure_stage == .backend_prepare;
    const reached_bytecode = prepared.bytecode_module != null;
    const reached_llvm = (target == .llvm_native or target == .hybrid) and prepared.failure_stage == null and prepared.ir_program != null;
    timingPrint("[kira:timing] checkFileForBackend.total path={s} backend={s} reached_ir={any} reached_bytecode={any} reached_llvm={any} ns={d}\n", .{
        path,
        @tagName(target),
        reached_ir,
        reached_bytecode,
        reached_llvm,
        elapsedNs(total_start),
    });
    return .{
        .source = prepared.source,
        .diagnostics = prepared.diagnostics,
        .failure_stage = prepared.failure_stage,
    };
}

pub fn checkFileFrontend(allocator: std.mem.Allocator, path: []const u8) !CheckPipelineResult {
    const total_start = nowNs();
    const parsed = try parseFileWithoutNativePreparation(allocator, path);
    if (parsed.program == null) {
        timingPrint("[kira:timing] checkFileFrontend.total path={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ path, elapsedNs(total_start) });
        return .{
            .source = parsed.source,
            .diagnostics = parsed.diagnostics,
            .failure_stage = parsed.failure_stage,
        };
    }

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    for (parsed.diagnostics) |diag| try diags.append(diag);

    const module_map_start = nowNs();
    const module_map = try package_manager.loadModuleMapForSource(allocator, parsed.source.path);
    timingPrint("[kira:timing] loadModuleMapForSource path={s} owners={d} ns={d}\n", .{ parsed.source.path, module_map.owners.len, elapsedNs(module_map_start) });

    const graph_start = nowNs();
    const merged_program = program_graph.buildProgramGraph(allocator, parsed.source.path, parsed.program.?, module_map, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            const stage = graphDiagnosticStage(diags.items);
            timingPrint("[kira:timing] buildProgramGraph path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(graph_start) });
            timingPrint("[kira:timing] checkFileFrontend.total path={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diagnosticsOwnedOrFallback(allocator, &diags, stage),
                .failure_stage = stage,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] buildProgramGraph path={s} imports={d} declarations={d} functions={d} ns={d}\n", .{ parsed.source.path, merged_program.imports.len, merged_program.decls.len, merged_program.functions.len, elapsedNs(graph_start) });

    const validate_start = nowNs();
    validateImports(allocator, &parsed.source, merged_program, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] validateImports path={s} imports={d} ns={d}\n", .{ parsed.source.path, merged_program.imports.len, elapsedNs(validate_start) });
            timingPrint("[kira:timing] checkFileFrontend.total path={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diagnosticsOwnedOrFallback(allocator, &diags, .semantics),
                .failure_stage = .semantics,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] validateImports path={s} imports={d} ns={d}\n", .{ parsed.source.path, merged_program.imports.len, elapsedNs(validate_start) });

    const semantics_start = nowNs();
    _ = semantics.analyzeWithImports(allocator, merged_program, .{}, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => {
            timingPrint("[kira:timing] semantics.analyzeWithImports path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(semantics_start) });
            timingPrint("[kira:timing] checkFileFrontend.total path={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diagnosticsOwnedOrFallback(allocator, &diags, .semantics),
                .failure_stage = .semantics,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] semantics.analyzeWithImports path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(semantics_start) });
    timingPrint("[kira:timing] checkFileFrontend.total path={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ path, elapsedNs(total_start) });

    return .{
        .source = parsed.source,
        .diagnostics = try diags.toOwnedSlice(),
        .failure_stage = null,
    };
}

fn dummyNativeEmitOptions() @import("kira_backend_api").NativeEmitOptions {
    return .{
        .object_path = "__kira_check__.o",
        .executable_path = "__kira_check__",
        .shared_library_path = "__kira_check__.so",
    };
}

fn mergeNativeLibraries(
    allocator: std.mem.Allocator,
    explicit: []const native.ResolvedNativeLibrary,
    discovered: []const native.ResolvedNativeLibrary,
) ![]const native.ResolvedNativeLibrary {
    const merged = try allocator.alloc(native.ResolvedNativeLibrary, explicit.len + discovered.len);
    @memcpy(merged[0..explicit.len], explicit);
    @memcpy(merged[explicit.len..], discovered);
    return merged;
}

pub fn backendDiagnostic(allocator: std.mem.Allocator, source_path: []const u8, err: anyerror) !diagnostics.Diagnostic {
    return switch (err) {
        error.NativeFunctionInVmBuild => try diag_messages.BackendMessages.nativeCodeRequiresNativeBackend(allocator, source_path),
        error.LlvmToolchainUnavailable => diag_messages.ToolchainMessages.missingLlvmToolchain(),
        error.RuntimeEntrypointInNativeBuild => diag_messages.BackendMessages.runtimeEntrypointInNativeBuild(),
        error.RuntimeCallInNativeBuild => diag_messages.BackendMessages.runtimeCallInNativeBuild(),
        error.HybridBuildRequiresExplicitExecution => diag_messages.BackendMessages.hybridBuildRequiresExplicitExecution(),
        error.UnsupportedExecutableFeature, error.UnsupportedType => diag_messages.BackendMessages.unsupportedExecutableFeature(),
        else => try diag_messages.ToolchainMessages.invalidToolchainActivation(allocator, @errorName(err)),
    };
}

pub fn backendDiagnostics(allocator: std.mem.Allocator, source_path: []const u8, err: anyerror) ![]diagnostics.Diagnostic {
    const items = try allocator.alloc(diagnostics.Diagnostic, 1);
    items[0] = try backendDiagnostic(allocator, source_path, err);
    return items;
}

fn backendDiagnosticsForVm(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    err: anyerror,
    native_libraries: []const native.ResolvedNativeLibrary,
) ![]diagnostics.Diagnostic {
    if (err == error.NativeFunctionInVmBuild and native_libraries.len != 0) {
        const items = try allocator.alloc(diagnostics.Diagnostic, 1);
        items[0] = try diag_messages.BackendMessages.nativeFfiPackageRequiresNativeBackend(
            allocator,
            source_path,
            packageNameForNativeLibrary(native_libraries[0]),
        );
        return items;
    }
    return backendDiagnostics(allocator, source_path, err);
}

fn packageNameForNativeLibrary(library: native.ResolvedNativeLibrary) []const u8 {
    if (std.mem.indexOf(u8, library.name, "sokol") != null) return "kira-graphics";
    return library.name;
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

fn parseFileWithoutNativePreparation(allocator: std.mem.Allocator, path: []const u8) !ParsePipelineResult {
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
        const libraries = try ffi_support.prepareNativeLibrariesForTarget(allocator, path, program.imports, target_selector);
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

pub fn checkFile(allocator: std.mem.Allocator, path: []const u8) !CheckPipelineResult {
    return checkFileForBackend(allocator, path, .vm);
}

pub fn checkPackageRoot(allocator: std.mem.Allocator, source_root: []const u8) !CheckPipelineResult {
    const total_start = nowNs();
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

    const semantics_start = nowNs();
    _ = semantics.analyzeLibrary(allocator, merged_program, .{}, &diags) catch |err| switch (err) {
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

fn validateImports(
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

test "check and build stop points share imported graph diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "App/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/project.toml",
        .data =
        \\[project]
        \\name = "App"
        \\version = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "vm"
        \\build_target = "host"
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/app/main.kira",
        .data =
        \\import support as Support
        \\
        \\@Main
        \\function main() {
        \\    return;
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/app/support.kira",
        .data = "function helper( { return; }\n",
    });

    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app/main.kira", arena.allocator());
    const checked = try checkFileForBackend(arena.allocator(), source_path, .vm);
    const built = try compileFileForBackend(arena.allocator(), source_path, .vm, &.{});

    try std.testing.expectEqual(FrontendStage.graph, checked.failure_stage.?);
    try std.testing.expectEqual(FrontendStage.graph, built.failure_stage.?);
    try std.testing.expectEqualStrings(checked.diagnostics[0].code.?, built.diagnostics[0].code.?);
    try std.testing.expectEqualStrings(checked.diagnostics[0].title, built.diagnostics[0].title);
}

test "check reaches backend preparation for selected backend" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "App/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "App/app/main.kira",
        .data =
        \\@Main
        \\function main() {
        \\    nativeHelper();
        \\    return;
        \\}
        \\
        \\@Native
        \\function nativeHelper() {
        \\    return;
        \\}
        ,
    });

    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app/main.kira", arena.allocator());
    const result = try checkFileForBackend(arena.allocator(), source_path, .vm);

    try std.testing.expect(result.failed());
    try std.testing.expectEqual(FrontendStage.backend_prepare, result.failure_stage.?);
    try std.testing.expectEqualStrings("KBE001", result.diagnostics[0].code.?);
}

test "built-in Foundation resolves before installed package conflicts" {
    const package_manager_pkg = @import("kira_package_manager");

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Workspace/App/app");
    try tmp.dir.createDirPath(std.testing.io, "Workspace/ConflictFoundation");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/App/kira.toml",
        .data =
        \\[package]
        \\name = "App"
        \\version = "0.1.0"
        \\kind = "app"
        \\kira = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "vm"
        \\build_target = "host"
        \\
        \\[dependencies]
        \\Foundation = { path = "../ConflictFoundation" }
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/App/app/main.kira",
        .data =
        \\import Foundation
        \\
        \\@Main
        \\function main() {
        \\    Foundation.printLine("ok");
        \\    return;
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/ConflictFoundation/kira.toml",
        .data =
        \\[package]
        \\name = "Foundation"
        \\version = "9.9.9"
        \\kind = "library"
        \\kira = "0.1.0"
        \\module_root = "Foundation"
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/ConflictFoundation/Foundation.kira",
        .data = "function broken( { return; }\n",
    });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Workspace/App", arena.allocator());
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Workspace/App/app/main.kira", arena.allocator());

    var package_diags = std.array_list.Managed(diagnostics.Diagnostic).init(arena.allocator());
    _ = try package_manager_pkg.syncProject(arena.allocator(), app_root, "0.1.0", .{}, &package_diags);

    const result = try checkFile(arena.allocator(), source_path);
    try std.testing.expectEqual(@as(usize, 0), result.diagnostics.len);
}

test "path dependency rooted at repo root resolves module file from app directory" {
    const package_manager_pkg = @import("kira_package_manager");

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Workspace/KiraUI/app");
    try tmp.dir.createDirPath(std.testing.io, "Workspace/CardExample/app");

    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/KiraUI/kira.toml",
        .data =
        \\[package]
        \\name = "KiraUI"
        \\version = "0.1.0"
        \\kind = "library"
        \\kira = "0.1.0"
        \\module_root = "KiraUI"
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/KiraUI/app/kiraui.kira",
        .data =
        \\function hello() {
        \\    return;
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/CardExample/kira.toml",
        .data =
        \\[package]
        \\name = "CardExample"
        \\version = "0.1.0"
        \\kind = "app"
        \\kira = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "vm"
        \\build_target = "host"
        \\
        \\[dependencies]
        \\KiraUI = { path = "../KiraUI" }
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/CardExample/app/main.kira",
        .data =
        \\import KiraUI
        \\
        \\@Main
        \\function main() {
        \\    hello();
        \\    return;
        \\}
        ,
    });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Workspace/CardExample", arena.allocator());
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Workspace/CardExample/app/main.kira", arena.allocator());
    var package_diags = std.array_list.Managed(diagnostics.Diagnostic).init(arena.allocator());
    _ = try package_manager_pkg.syncProject(arena.allocator(), app_root, "0.1.0", .{}, &package_diags);

    const result = try checkFile(arena.allocator(), source_path);
    try std.testing.expectEqual(@as(usize, 0), result.diagnostics.len);
}

test "current library root import exposes declarations from every library file" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Workspace/UILibrary/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/UILibrary/kira.toml",
        .data =
        \\[package]
        \\name = "UILibrary"
        \\version = "0.1.0"
        \\kind = "library"
        \\kira = "0.1.0"
        \\module_root = "UI"
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/UILibrary/app/main.kira",
        .data =
        \\import UI
        \\
        \\@Main
        \\function main() {
        \\    header()
        \\    footer()
        \\    return;
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/UILibrary/app/UI.kira",
        .data =
        \\function header() {
        \\    return;
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/UILibrary/app/Footer.kira",
        .data =
        \\function footer() {
        \\    return;
        \\}
        ,
    });

    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Workspace/UILibrary/app/main.kira", arena.allocator());
    const result = try checkFile(arena.allocator(), source_path);
    try std.testing.expectEqual(@as(usize, 0), result.diagnostics.len);
}

test "compile frontend deduplicates mixed-separator paths while walking current package imports" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Workspace/callbacks/app");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/callbacks/project.toml",
        .data =
        \\[project]
        \\name = "callbacks"
        \\version = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "llvm"
        \\build_target = "host"
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/callbacks/app/main.kira",
        .data =
        \\import callbacks as cb
        \\
        \\@Main
        \\function main() {
        \\    cb.hello()
        \\    return
        \\}
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Workspace/callbacks/app/callbacks.kira",
        .data =
        \\function hello() {
        \\    return
        \\}
        ,
    });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Workspace/callbacks/app", arena.allocator());
    const mixed_source_path = try std.fmt.allocPrint(arena.allocator(), "{s}/main.kira", .{app_root});
    const result = try compileFileToIr(arena.allocator(), mixed_source_path);

    try std.testing.expectEqual(@as(usize, 0), result.diagnostics.len);
    try std.testing.expect(result.ir_program != null);
}
