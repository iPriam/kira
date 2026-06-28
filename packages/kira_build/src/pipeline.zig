const std = @import("std");
const diag_messages = @import("kira_diagnostic_messages");
const source_pkg = @import("kira_source");
const diagnostics = @import("kira_diagnostics");
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
const synth_test_driver = @import("synth_test_driver.zig");
const frontend_pipeline = @import("pipeline_frontend.zig");
const timing = @import("pipeline_timing.zig");

pub const setTimingsEnabled = timing.setTimingsEnabled;
pub const timingsEnabled = timing.timingsEnabled;
const nowNs = timing.nowNs;
const elapsedNs = timing.elapsedNs;
pub const timingPrint = timing.timingPrint;

pub const FrontendStage = frontend_pipeline.FrontendStage;
pub const LexPipelineResult = frontend_pipeline.LexPipelineResult;
pub const ParsePipelineResult = frontend_pipeline.ParsePipelineResult;
pub const CheckPipelineResult = frontend_pipeline.CheckPipelineResult;
pub const CacheStatus = frontend_pipeline.CacheStatus;
pub const lexFile = frontend_pipeline.lexFile;
pub const parseFile = frontend_pipeline.parseFile;
pub const parseFileForTarget = frontend_pipeline.parseFileForTarget;
pub const checkPackageRoot = frontend_pipeline.checkPackageRoot;

const compilerPhaseForStage = frontend_pipeline.compilerPhaseForStage;
const diagnosticsOwnedOrFallback = frontend_pipeline.diagnosticsOwnedOrFallback;
const displayTargetSelector = frontend_pipeline.displayTargetSelector;
const graphDiagnosticStage = frontend_pipeline.graphDiagnosticStage;
const parseFileWithoutNativePreparation = frontend_pipeline.parseFileWithoutNativePreparation;
const validateImports = frontend_pipeline.validateImports;

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
    /// Set only when the executable-obligation verifier passed; the proof carried alongside
    /// the raw IR so downstream emission constructs a backend request from a `VerifiedProgram`
    /// rather than re-wrapping a raw program.
    verified_program: ?ir.VerifiedProgram = null,
    bytecode_module: ?bytecode.Module = null,
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    failure_stage: ?FrontendStage = null,

    pub fn failed(self: ExecutablePipelineResult) bool {
        return self.ir_program == null or diagnostics.hasErrors(self.diagnostics);
    }
};

pub fn compileFileToIr(allocator: std.mem.Allocator, path: []const u8) !FrontendPipelineResult {
    return compileFileToIrForTargetWithFfi(allocator, path, null, false);
}

pub fn compileFileToIrForTarget(
    allocator: std.mem.Allocator,
    path: []const u8,
    target_selector: ?native.TargetSelector,
) !FrontendPipelineResult {
    return compileFileToIrForTargetWithFfi(allocator, path, target_selector, false);
}

pub fn compileFileToIrForTargetWithFfi(
    allocator: std.mem.Allocator,
    path: []const u8,
    target_selector: ?native.TargetSelector,
    allow_runtime_direct_ffi: bool,
) !FrontendPipelineResult {
    return compileFileToIrForTargetWithOptions(allocator, path, target_selector, .{
        .allow_runtime_direct_ffi = allow_runtime_direct_ffi,
    });
}

pub const CompileOptions = struct {
    allow_runtime_direct_ffi: bool = false,
    require_main: bool = true,
    test_mode: bool = false,
    /// Synthesize a pure-Kira test driver entry (`__kira_test_main`) that runs
    /// every `Test` and prints PASS/FAIL/SKIP, so the suite executes as ordinary
    /// Kira on the selected backend instead of through a Zig comparison runner.
    synthesize_test_driver: bool = false,
};

pub fn compileFileToIrForTargetWithOptions(
    allocator: std.mem.Allocator,
    path: []const u8,
    target_selector: ?native.TargetSelector,
    options: CompileOptions,
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

    // Declared dependency packages must have their native artifacts and generated
    // `bindings/` sources ready before the program graph collects package files,
    // otherwise freshly generated bindings only become visible one run later.
    const native_import_start = nowNs();
    const native_libraries = ffi_support.prepareDeclaredNativeLibrariesForTarget(allocator, parsed.native_libraries, module_map, target_selector) catch |err| switch (err) {
        error.UnsupportedTarget => {
            const display_target = try displayTargetSelector(allocator, target_selector);
            try diags.append(try diag_messages.ToolchainMessages.unsupportedNativeLibraryTarget(allocator, display_target));
            timingPrint("[kira:timing] prepareDeclaredNativeLibraries path={s} native_libraries=0 ns={d}\n", .{ parsed.source.path, elapsedNs(native_import_start) });
            timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
            return .{
                .source = parsed.source,
                .diagnostics = try diags.toOwnedSlice(),
                .ir_program = null,
                .native_libraries = &.{},
                .failure_stage = .backend_prepare,
            };
        },
        else => return err,
    };
    timingPrint("[kira:timing] prepareDeclaredNativeLibraries path={s} native_libraries={d} ns={d}\n", .{ parsed.source.path, native_libraries.len, elapsedNs(native_import_start) });

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

    // In test mode, synthesize a pure-Kira driver entry that runs every Test and
    // prints PASS/FAIL/SKIP, so the suite executes as ordinary Kira on the chosen
    // backend (the runner invokes the driver instead of comparing in Zig).
    const program_for_analysis = if (options.synthesize_test_driver)
        try synth_test_driver.injectTestDriver(allocator, merged_program, &diags)
    else
        merged_program;

    const semantics_start = nowNs();
    const hir = semantics.analyzeWithImportsOptions(allocator, program_for_analysis, .{}, .{
        .allow_runtime_direct_ffi = options.allow_runtime_direct_ffi,
        .require_main = options.require_main,
    }, &diags) catch |err| switch (err) {
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
    const ir_program = ir.lowerProgramWithDiagnostics(allocator, hir, .{
        .include_tests = options.test_mode,
    }, &diags) catch |err| switch (err) {
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
    } orelse {
        timingPrint("[kira:timing] ir.lowerProgram path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(ir_start) });
        timingPrint("[kira:timing] compileFileToIr.total path={s} ns={d}\n", .{ path, elapsedNs(total_start) });
        return .{
            .source = parsed.source,
            .diagnostics = try diags.toOwnedSlice(),
            .ir_program = null,
            .native_libraries = native_libraries,
            .failure_stage = .ir,
        };
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
    return compileFileForBackendWithOptions(allocator, path, target, target_selector, explicit_native_libraries, .{
        .allow_runtime_direct_ffi = target == .vm,
    });
}

pub fn compileFileForBackendWithOptions(
    allocator: std.mem.Allocator,
    path: []const u8,
    target: build_def.ExecutionTarget,
    target_selector: ?native.TargetSelector,
    explicit_native_libraries: []const native.ResolvedNativeLibrary,
    options: CompileOptions,
) !ExecutablePipelineResult {
    const total_start = nowNs();
    const effective_selector = if (target == .wasm32_emscripten and target_selector == null)
        try llvm_backend.emscripten.selector(allocator)
    else
        target_selector;
    const frontend = try compileFileToIrForTargetWithOptions(allocator, path, effective_selector, options);
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

    // Phase gate: prove the lowered program satisfies its executable obligations before any
    // backend consumes it. A failure here is a precise compile diagnostic, not a later
    // runtime detonation. Native backends additionally require known aggregate layouts.
    const verify_start = nowNs();
    const verify_caps = ir.BackendCapabilities{ .requires_native_layout = target != .vm };
    const verify_result = try ir.verify(allocator, .{ .program = ir_program }, verify_caps);
    timingPrint("[kira:timing] verifyExecutableProgram path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(verify_start) });
    switch (verify_result) {
        .failure => |failure| {
            const diag = try diag_messages.BackendMessages.executableObligationUnmet(
                allocator,
                failure.kind.summary(),
                failure.function_name,
                failure.detail,
            );
            const items = try allocator.alloc(diagnostics.Diagnostic, 1);
            items[0] = diag;
            timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
            return .{
                .source = frontend.source,
                .diagnostics = items,
                .ir_program = ir_program,
                .bytecode_module = null,
                .native_libraries = frontend.native_libraries,
                .failure_stage = .backend_prepare,
            };
        },
        .verified => {},
    }
    const verified_program = verify_result.verified;

    const merge_start = nowNs();
    const merged_native_libraries = try mergeNativeLibraries(allocator, explicit_native_libraries, frontend.native_libraries);
    timingPrint("[kira:timing] mergeNativeLibraries path={s} explicit={d} discovered={d} merged={d} ns={d}\n", .{ path, explicit_native_libraries.len, frontend.native_libraries.len, merged_native_libraries.len, elapsedNs(merge_start) });

    switch (target) {
        .vm => {
            const bytecode_start = nowNs();
            const module = bytecode.compileProgram(allocator, verified_program, .vm) catch |err| {
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
                .verified_program = verified_program,
                .bytecode_module = module,
                .native_libraries = merged_native_libraries,
                .failure_stage = frontend.failure_stage,
            };
        },
        .llvm_native, .wasm32_emscripten => {
            const llvm_start = nowNs();
            llvm_backend.validate(allocator, .{
                .mode = .llvm_native,
                .program = &verified_program,
                .module_name = std.fs.path.stem(path),
                .emit = dummyNativeEmitOptions(),
                .target_selector = effective_selector,
                .resolved_native_libraries = merged_native_libraries,
            }) catch |err| {
                const backend_diagnostics = try backendDiagnostics(allocator, frontend.source.path, err);
                timingPrint("[kira:timing] llvm_backend.validate path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(llvm_start) });
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
            timingPrint("[kira:timing] llvm_backend.validate path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(llvm_start) });
            timingPrint("[kira:timing] compileFileForBackend.total path={s} backend={s} ns={d}\n", .{ path, @tagName(target), elapsedNs(total_start) });
            return .{
                .source = frontend.source,
                .diagnostics = frontend.diagnostics,
                .ir_program = ir_program,
                .verified_program = verified_program,
                .bytecode_module = null,
                .native_libraries = merged_native_libraries,
                .failure_stage = frontend.failure_stage,
            };
        },
        .hybrid => {
            const bytecode_start = nowNs();
            const module = bytecode.compileProgram(allocator, verified_program, .hybrid_runtime) catch |err| {
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
                .program = &verified_program,
                .module_name = std.fs.path.stem(path),
                .emit = dummyNativeEmitOptions(),
                .target_selector = effective_selector,
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
                .verified_program = verified_program,
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
    const reached_llvm = (target == .llvm_native or target == .wasm32_emscripten or target == .hybrid) and prepared.failure_stage == null and prepared.ir_program != null;
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
    const hir = semantics.analyzeWithImports(allocator, merged_program, .{}, &diags) catch |err| switch (err) {
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
    const mid_start = nowNs();
    const prepared = try ir.prepareProgram(allocator, hir, .{}, &diags);
    timingPrint("[kira:timing] ir.prepareProgram path={s} ns={d}\n", .{ parsed.source.path, elapsedNs(mid_start) });
    if (prepared == .failed) {
        timingPrint("[kira:timing] checkFileFrontend.total path={s} reached_ir=false reached_bytecode=false reached_llvm=false ns={d}\n", .{ path, elapsedNs(total_start) });
        return .{
            .source = parsed.source,
            .diagnostics = try diags.toOwnedSlice(),
            .failure_stage = .ir,
        };
    }
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

pub fn checkFile(allocator: std.mem.Allocator, path: []const u8) !CheckPipelineResult {
    return checkFileForBackend(allocator, path, .vm);
}
