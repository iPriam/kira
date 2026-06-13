const std = @import("std");
const build_def = @import("kira_build_definition");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const bytecode = @import("kira_bytecode");
const hybrid = @import("kira_hybrid_definition");
const runtime_abi = @import("kira_runtime_abi");
const llvm_backend = @import("kira_llvm_backend");
const native = @import("kira_native_lib_definition");
const pipeline = @import("pipeline.zig");
const ffi_support = @import("ffi_support.zig");
const builtin = @import("builtin");
const cache = @import("cache.zig");

pub const BuildFailureKind = enum {
    frontend,
    build,
    toolchain,
};

pub const BuildArtifactOutcome = struct {
    source: ?source_pkg.SourceFile = null,
    diagnostics: []const diagnostics.Diagnostic = &.{},
    artifacts: []const build_def.Artifact = &.{},
    /// Native libraries resolved for the build target. The VM runner uses these
    /// to map FFI library names to loadable paths for LibFFI dispatch.
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    failure_kind: ?BuildFailureKind = null,
    failure_stage: ?pipeline.FrontendStage = null,
    cache_status: pipeline.CacheStatus = .not_checked,
    cache_restore_ns: u64 = 0,
    cache_store_ns: u64 = 0,

    pub fn failed(self: BuildArtifactOutcome) bool {
        return diagnostics.hasErrors(self.diagnostics);
    }
};

pub const BuildSystem = struct {
    allocator: std.mem.Allocator,
    use_cache: bool = true,

    pub fn init(allocator: std.mem.Allocator) BuildSystem {
        return .{ .allocator = allocator };
    }

    pub fn check(self: BuildSystem, path: []const u8) ![]const diagnostics.Diagnostic {
        const result = try pipeline.checkFile(self.allocator, path);
        return result.diagnostics;
    }

    pub fn checkFrontend(self: BuildSystem, path: []const u8) !pipeline.CheckPipelineResult {
        if (self.use_cache) {
            const maybe_cache = cache.Cache.initForSource(self.allocator, path) catch null;
            if (maybe_cache) |build_cache| {
                const maybe_entry = build_cache.entryForFrontendCheck(path) catch null;
                if (maybe_entry) |entry| {
                    if (entry.hasFrontendCheckSuccess()) {
                        pipeline.timingPrint("[kira:timing] check.frontend_cache_hit path={s}\n", .{path});
                        return .{
                            .source = try source_pkg.SourceFile.fromPath(self.allocator, path),
                            .diagnostics = &.{},
                            .failure_stage = null,
                            .cache_status = .hit,
                        };
                    }

                    var result = try pipeline.checkFileFrontend(self.allocator, path);
                    result.cache_status = .miss;
                    if (!result.failed()) {
                        const store_start = nowTimestamp();
                        entry.storeFrontendCheckSuccess() catch {};
                        result.cache_status = .stored;
                        result.cache_store_ns = elapsedNs(store_start);
                        pipeline.timingPrint("[kira:timing] check.frontend_cache_store path={s} ns={d}\n", .{ path, result.cache_store_ns });
                    }
                    return result;
                }
            }
        }
        return pipeline.checkFileFrontend(self.allocator, path);
    }

    pub fn checkForBackend(self: BuildSystem, path: []const u8, target: build_def.ExecutionTarget) !pipeline.CheckPipelineResult {
        if (self.use_cache) {
            const maybe_cache = cache.Cache.initForSource(self.allocator, path) catch null;
            if (maybe_cache) |build_cache| {
                const maybe_entry = build_cache.entryForBuild(path, target) catch null;
                if (maybe_entry) |entry| {
                    if (entry.hasCheckSuccess()) {
                        pipeline.timingPrint("[kira:timing] check.cache_hit path={s} backend={s}\n", .{ path, @tagName(target) });
                        return .{
                            .source = try source_pkg.SourceFile.fromPath(self.allocator, path),
                            .diagnostics = &.{},
                            .failure_stage = null,
                            .cache_status = .hit,
                        };
                    }

                    var result = try pipeline.checkFileForBackend(self.allocator, path, target);
                    result.cache_status = .miss;
                    if (!result.failed()) {
                        const store_start = nowTimestamp();
                        entry.storeCheckSuccess() catch {};
                        result.cache_status = .stored;
                        result.cache_store_ns = elapsedNs(store_start);
                        pipeline.timingPrint("[kira:timing] check.cache_store path={s} backend={s} ns={d}\n", .{ path, @tagName(target), result.cache_store_ns });
                    }
                    return result;
                }
            }
        }
        return pipeline.checkFileForBackend(self.allocator, path, target);
    }

    pub fn checkForBuildTarget(self: BuildSystem, path: []const u8, target: build_def.BuildTarget) !pipeline.CheckPipelineResult {
        return pipeline.checkFileForBackendWithSelector(self.allocator, path, target.execution, target.selector);
    }

    pub fn checkPackageRoot(self: BuildSystem, source_root: []const u8) !pipeline.CheckPipelineResult {
        if (self.use_cache) {
            const module_files = @import("kira_program_graph").collectPackageModuleFiles(self.allocator, source_root) catch &.{};
            if (module_files.len != 0) {
                const representative = module_files[0];
                const maybe_cache = cache.Cache.initForSource(self.allocator, representative) catch null;
                if (maybe_cache) |build_cache| {
                    const maybe_entry = build_cache.entryForPackageCheck(representative) catch null;
                    if (maybe_entry) |entry| {
                        if (entry.hasPackageCheckSuccess()) {
                            pipeline.timingPrint("[kira:timing] check.package_cache_hit source_root={s}\n", .{source_root});
                            return .{
                                .source = try source_pkg.SourceFile.fromPath(self.allocator, representative),
                                .diagnostics = &.{},
                                .failure_stage = null,
                                .cache_status = .hit,
                            };
                        }

                        var result = try pipeline.checkPackageRoot(self.allocator, source_root);
                        result.cache_status = .miss;
                        if (!result.failed()) {
                            const store_start = nowTimestamp();
                            entry.storePackageCheckSuccess() catch {};
                            result.cache_status = .stored;
                            result.cache_store_ns = elapsedNs(store_start);
                            pipeline.timingPrint("[kira:timing] check.package_cache_store source_root={s} ns={d}\n", .{ source_root, result.cache_store_ns });
                        }
                        return result;
                    }
                }
            }
        }
        return pipeline.checkPackageRoot(self.allocator, source_root);
    }

    pub fn compileVm(self: BuildSystem, path: []const u8) !pipeline.VmPipelineResult {
        return pipeline.compileFileToBytecode(self.allocator, path);
    }

    pub fn compileFrontend(self: BuildSystem, path: []const u8) !pipeline.FrontendPipelineResult {
        return pipeline.compileFileToIr(self.allocator, path);
    }

    pub fn compileForBackend(self: BuildSystem, request: build_def.BuildRequest) !pipeline.ExecutablePipelineResult {
        return pipeline.compileFileForBackendWithSelector(
            self.allocator,
            request.source_path,
            request.target.execution,
            request.target.selector,
            request.native_libraries,
        );
    }

    pub fn build(self: BuildSystem, request: build_def.BuildRequest) !BuildArtifactOutcome {
        const total_start = nowTimestamp();
        if (self.use_cache) {
            const maybe_cache = cache.Cache.initForSource(self.allocator, request.source_path) catch null;
            if (maybe_cache) |build_cache| {
                const maybe_entry = build_cache.entryForBuild(request.source_path, request.target.execution) catch null;
                if (maybe_entry) |entry| {
                    if (entry.hasArtifacts()) {
                        const restore_start = nowTimestamp();
                        const artifacts = try entry.restoreTo(request.output_path);
                        const restore_ns = elapsedNs(restore_start);
                        pipeline.timingPrint("[kira:timing] build.cache_hit path={s} backend={s} restore_ns={d} total_ns={d}\n", .{
                            request.source_path,
                            @tagName(request.target.execution),
                            restore_ns,
                            elapsedNs(total_start),
                        });
                        // A cache hit skips the frontend, so re-resolve the
                        // project's native libraries (artifacts already exist)
                        // to keep the VM's LibFFI dispatcher able to load them.
                        const restored_native_libraries = self.resolveCachedNativeLibraries(request);
                        return .{
                            .artifacts = artifacts,
                            .native_libraries = restored_native_libraries,
                            .cache_status = .hit,
                            .cache_restore_ns = restore_ns,
                        };
                    }

                    var uncached = try self.buildUncached(request);
                    uncached.cache_status = .miss;
                    if (!uncached.failed()) {
                        const store_start = nowTimestamp();
                        entry.storeFrom(request.output_path) catch {};
                        uncached.cache_status = .stored;
                        uncached.cache_store_ns = elapsedNs(store_start);
                        pipeline.timingPrint("[kira:timing] build.cache_store path={s} backend={s} store_ns={d} total_ns={d}\n", .{
                            request.source_path,
                            @tagName(request.target.execution),
                            uncached.cache_store_ns,
                            elapsedNs(total_start),
                        });
                    }
                    return uncached;
                }
            }
        }
        const result = try self.buildUncached(request);
        pipeline.timingPrint("[kira:timing] build.total path={s} backend={s} cached=false ns={d}\n", .{ request.source_path, @tagName(request.target.execution), elapsedNs(total_start) });
        return result;
    }

    /// Re-resolves the project's declared native libraries for a cache-restored
    /// VM build. Only the VM backend dispatches FFI through these paths, so other
    /// backends skip the work. Best-effort: failures yield no libraries.
    fn resolveCachedNativeLibraries(self: BuildSystem, request: build_def.BuildRequest) []const native.ResolvedNativeLibrary {
        if (request.target.execution != .vm) return &.{};
        return ffi_support.prepareNativeLibrariesForTarget(self.allocator, request.source_path, &.{}, request.target.selector) catch &.{};
    }

    fn buildUncached(self: BuildSystem, request: build_def.BuildRequest) !BuildArtifactOutcome {
        return switch (request.target.execution) {
            .vm => self.buildBytecodeArtifact(request),
            .llvm_native => self.buildNativeArtifact(request),
            .wasm32_emscripten => self.buildWasmEmscriptenArtifact(request),
            .hybrid => self.buildHybridArtifact(request),
        };
    }

    pub fn buildBytecodeArtifact(self: BuildSystem, request: build_def.BuildRequest) !BuildArtifactOutcome {
        const compiled = try self.compileForBackend(request);
        if (compiled.bytecode_module == null) {
            return .{
                .source = compiled.source,
                .diagnostics = compiled.diagnostics,
                .failure_kind = if (compiled.failure_stage == .backend_prepare) .build else .frontend,
                .failure_stage = compiled.failure_stage,
            };
        }

        try compiled.bytecode_module.?.writeToFile(request.output_path);

        const artifacts = try self.allocator.alloc(build_def.Artifact, 1);
        artifacts[0] = .{
            .kind = .bytecode,
            .path = request.output_path,
        };
        return .{ .artifacts = artifacts, .native_libraries = compiled.native_libraries };
    }

    pub fn buildNativeArtifact(self: BuildSystem, request: build_def.BuildRequest) !BuildArtifactOutcome {
        return self.buildLlvmExecutableArtifact(request, .llvm_native);
    }

    pub fn buildWasmEmscriptenArtifact(self: BuildSystem, request: build_def.BuildRequest) !BuildArtifactOutcome {
        var wasm_request = request;
        wasm_request.target.selector = try llvm_backend.emscripten.selector(self.allocator);
        llvm_backend.emscripten.validateAvailable(self.allocator) catch |err| {
            const source = try @import("kira_source").SourceFile.fromPath(self.allocator, request.source_path);
            const backend_diagnostics = try pipeline.backendDiagnostics(self.allocator, source.path, err);
            return .{
                .source = source,
                .diagnostics = backend_diagnostics,
                .failure_kind = .toolchain,
                .failure_stage = .backend_prepare,
            };
        };
        return self.buildLlvmExecutableArtifact(wasm_request, .wasm32_emscripten);
    }

    fn buildLlvmExecutableArtifact(self: BuildSystem, request: build_def.BuildRequest, mode: build_def.ExecutionTarget) !BuildArtifactOutcome {
        const total_start = nowTimestamp();
        const compiled = try self.compileForBackend(request);
        if (compiled.failed()) {
            return .{
                .source = compiled.source,
                .diagnostics = compiled.diagnostics,
                .failure_kind = if (compiled.failure_stage == .backend_prepare) .build else .frontend,
                .failure_stage = compiled.failure_stage,
            };
        }

        const ir_program = compiled.ir_program.?;
        const object_path = try defaultObjectPath(self.allocator, request.output_path);
        const emit_start = nowTimestamp();
        const backend_result = llvm_backend.compile(self.allocator, .{
            .mode = .llvm_native,
            .program = &ir_program,
            .module_name = std.fs.path.stem(request.source_path),
            .emit = .{
                .object_path = object_path,
                .executable_path = request.output_path,
            },
            .target_selector = request.target.selector,
            .resolved_native_libraries = compiled.native_libraries,
        }) catch |err| {
            const backend_diagnostics = try pipeline.backendDiagnostics(self.allocator, compiled.source.path, err);
            return .{
                .source = compiled.source,
                .diagnostics = backend_diagnostics,
                .failure_kind = .toolchain,
                .failure_stage = .backend_prepare,
            };
        };
        pipeline.timingPrint("[kira:timing] llvm_backend.compile path={s} backend={s} ns={d}\n", .{ request.source_path, @tagName(mode), elapsedNs(emit_start) });

        const extra_wasm_artifacts: usize = if (mode == .wasm32_emscripten) 1 else 0;
        const artifacts = try self.allocator.alloc(build_def.Artifact, backend_result.artifacts.len + extra_wasm_artifacts);
        for (backend_result.artifacts, 0..) |artifact, index| {
            artifacts[index] = .{
                .kind = switch (artifact.kind) {
                    .bytecode => .bytecode,
                    .native_object => .native_object,
                    .native_library => .native_library,
                    .executable => .executable,
                    .hybrid_bundle => return error.NotImplemented,
                },
                .path = artifact.path,
            };
        }
        if (mode == .wasm32_emscripten) {
            artifacts[backend_result.artifacts.len] = .{
                .kind = .executable,
                .path = try replaceExtension(self.allocator, request.output_path, ".wasm"),
            };
        }
        pipeline.timingPrint("[kira:timing] buildLlvmExecutableArtifact.total path={s} backend={s} ns={d}\n", .{ request.source_path, @tagName(mode), elapsedNs(total_start) });
        return .{ .artifacts = artifacts };
    }

    pub fn buildHybridArtifact(self: BuildSystem, request: build_def.BuildRequest) !BuildArtifactOutcome {
        const total_start = nowTimestamp();
        const compiled = try self.compileForBackend(request);
        if (compiled.failed()) {
            return .{
                .source = compiled.source,
                .diagnostics = compiled.diagnostics,
                .failure_kind = if (compiled.failure_stage == .backend_prepare) .build else .frontend,
                .failure_stage = compiled.failure_stage,
            };
        }

        const ir_program = compiled.ir_program.?;
        const bytecode_path = try replaceExtension(self.allocator, request.output_path, ".kbc");
        const object_path = try replaceExtension(self.allocator, request.output_path, objectExtension());
        const library_path = try replaceExtension(self.allocator, request.output_path, sharedLibraryExtension());

        const bytecode_module = compiled.bytecode_module orelse return .{
            .source = compiled.source,
            .diagnostics = compiled.diagnostics,
            .failure_kind = .build,
            .failure_stage = compiled.failure_stage,
        };
        const bytecode_write_start = nowTimestamp();
        try bytecode_module.writeToFile(bytecode_path);
        pipeline.timingPrint("[kira:timing] bytecode.writeToFile path={s} ns={d}\n", .{ bytecode_path, elapsedNs(bytecode_write_start) });

        const emit_start = nowTimestamp();
        const backend_result = llvm_backend.compile(self.allocator, .{
            .mode = .hybrid,
            .program = &ir_program,
            .module_name = std.fs.path.stem(request.source_path),
            .emit = .{
                .object_path = object_path,
                .shared_library_path = library_path,
            },
            .target_selector = request.target.selector,
            .resolved_native_libraries = compiled.native_libraries,
        }) catch |err| {
            const backend_diagnostics = try pipeline.backendDiagnostics(self.allocator, compiled.source.path, err);
            return .{
                .source = compiled.source,
                .diagnostics = backend_diagnostics,
                .failure_kind = .toolchain,
                .failure_stage = .backend_prepare,
            };
        };
        pipeline.timingPrint("[kira:timing] llvm_backend.compile path={s} backend=hybrid ns={d}\n", .{ request.source_path, elapsedNs(emit_start) });

        const manifest_start = nowTimestamp();
        const manifest = buildHybridManifest(self.allocator, ir_program, std.fs.path.stem(request.source_path), bytecode_path, library_path) catch |err| {
            const backend_diagnostics = try pipeline.backendDiagnostics(self.allocator, compiled.source.path, err);
            return .{
                .source = compiled.source,
                .diagnostics = backend_diagnostics,
                .failure_kind = .build,
                .failure_stage = .backend_prepare,
            };
        };
        try manifest.writeToFile(request.output_path);
        pipeline.timingPrint("[kira:timing] hybrid_manifest.write path={s} ns={d}\n", .{ request.output_path, elapsedNs(manifest_start) });

        const artifacts = try self.allocator.alloc(build_def.Artifact, backend_result.artifacts.len + 2);
        artifacts[0] = .{ .kind = .bytecode, .path = bytecode_path };
        artifacts[1] = .{ .kind = .hybrid_manifest, .path = request.output_path };
        for (backend_result.artifacts, 0..) |artifact, index| {
            artifacts[index + 2] = .{
                .kind = switch (artifact.kind) {
                    .bytecode => .bytecode,
                    .native_object => .native_object,
                    .native_library => .native_library,
                    .executable => .executable,
                    .hybrid_bundle => return error.NotImplemented,
                },
                .path = artifact.path,
            };
        }
        pipeline.timingPrint("[kira:timing] buildHybridArtifact.total path={s} ns={d}\n", .{ request.source_path, elapsedNs(total_start) });
        return .{ .artifacts = artifacts };
    }

    pub fn readBytecode(self: BuildSystem, path: []const u8) !bytecode.Module {
        _ = self;
        return bytecode.Module.readFromFile(std.heap.page_allocator, path);
    }
};

fn defaultObjectPath(allocator: std.mem.Allocator, executable_path: []const u8) ![]const u8 {
    const ext = executableExtension();
    if (ext.len > 0 and std.mem.endsWith(u8, executable_path, ext)) {
        const stem = executable_path[0 .. executable_path.len - ext.len];
        return std.fmt.allocPrint(allocator, "{s}{s}", .{ stem, objectExtension() });
    }
    return std.fmt.allocPrint(allocator, "{s}{s}", .{ executable_path, objectExtension() });
}

fn replaceExtension(allocator: std.mem.Allocator, path: []const u8, extension: []const u8) ![]const u8 {
    const ext = std.fs.path.extension(path);
    if (ext.len == 0) return std.fmt.allocPrint(allocator, "{s}{s}", .{ path, extension });
    return std.fmt.allocPrint(allocator, "{s}{s}", .{ path[0 .. path.len - ext.len], extension });
}

fn objectExtension() []const u8 {
    return if (builtin.os.tag == .windows) ".obj" else ".o";
}

pub fn executableExtension() []const u8 {
    return if (builtin.os.tag == .windows) ".exe" else "";
}

pub fn sharedLibraryExtension() []const u8 {
    return switch (builtin.os.tag) {
        .windows => ".dll",
        .macos => ".dylib",
        else => ".so",
    };
}

fn buildHybridManifest(
    allocator: std.mem.Allocator,
    program: @import("kira_ir").Program,
    module_name: []const u8,
    bytecode_path: []const u8,
    library_path: []const u8,
) !hybrid.HybridModuleManifest {
    var functions = std.array_list.Managed(hybrid.FunctionManifest).init(allocator);
    for (program.functions) |function_decl| {
        const resolved_execution = resolveHybridExecution(function_decl.execution);
        try functions.append(.{
            .id = function_decl.id,
            .name = function_decl.name,
            .execution = resolved_execution,
            .param_types = try lowerHybridTypeRefs(allocator, function_decl.param_types),
            .param_ownership = try lowerHybridOwnershipModes(allocator, function_decl.param_ownership),
            .return_type = lowerHybridTypeRef(function_decl.return_type),
            .return_ownership = lowerHybridOwnershipMode(function_decl.return_ownership),
            .exported_name = if (resolved_execution == .native and !function_decl.is_extern)
                try std.fmt.allocPrint(allocator, "kira_native_fn_{d}", .{function_decl.id})
            else
                null,
        });
    }

    const entry_function = program.functions[program.entry_index];
    return .{
        .module_name = try allocator.dupe(u8, module_name),
        .bytecode_path = try allocator.dupe(u8, bytecode_path),
        .native_library_path = try allocator.dupe(u8, library_path),
        .entry_function_id = entry_function.id,
        .entry_execution = resolveHybridExecution(entry_function.execution),
        .functions = try functions.toOwnedSlice(),
    };
}

fn resolveHybridExecution(execution: runtime_abi.FunctionExecution) runtime_abi.FunctionExecution {
    return switch (execution) {
        .inherited => .runtime,
        else => execution,
    };
}

fn lowerHybridTypeRefs(allocator: std.mem.Allocator, types: []const @import("kira_ir").ValueType) ![]hybrid.TypeRef {
    const lowered = try allocator.alloc(hybrid.TypeRef, types.len);
    for (types, 0..) |value_type, index| lowered[index] = lowerHybridTypeRef(value_type);
    return lowered;
}

fn lowerHybridTypeRef(value_type: @import("kira_ir").ValueType) hybrid.TypeRef {
    return .{
        .kind = switch (value_type.kind) {
            .void => .void,
            .integer => .integer,
            .float => .float,
            .string => .string,
            .boolean => .boolean,
            .construct_any => .construct_any,
            .array => .array,
            .raw_ptr => .raw_ptr,
            .ffi_struct => .ffi_struct,
            .enum_instance => .enum_instance,
        },
        .name = value_type.name,
        .construct_constraint = if (value_type.construct_constraint) |constraint| .{ .construct_name = constraint.construct_name } else null,
    };
}

fn lowerHybridOwnershipModes(allocator: std.mem.Allocator, values: []const @import("kira_ir").OwnershipMode) ![]hybrid.OwnershipMode {
    const lowered = try allocator.alloc(hybrid.OwnershipMode, values.len);
    for (values, 0..) |value, index| lowered[index] = lowerHybridOwnershipMode(value);
    return lowered;
}

fn lowerHybridOwnershipMode(value: @import("kira_ir").OwnershipMode) hybrid.OwnershipMode {
    return switch (value) {
        .owned => .owned,
        .borrow_read => .borrow_read,
        .borrow_mut => .borrow_mut,
        .move => .move,
        .copy => .copy,
    };
}

fn nowTimestamp() std.Io.Clock.Timestamp {
    return std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
}

fn elapsedNs(start: std.Io.Clock.Timestamp) u64 {
    const duration_ns = start.durationTo(std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake)).raw.toNanoseconds();
    return @intCast(@max(duration_ns, 0));
}
