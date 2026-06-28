const std = @import("std");
const api = @import("api.zig");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const bytecode = @import("kira_bytecode");
const diagnostics = @import("kira_diagnostics");
const kira_project = @import("kira_project");
const source_pkg = @import("kira_source");
const runtime_abi = @import("kira_runtime_abi");
const vm_runtime = @import("kira_vm_runtime");
const hybrid_runtime = @import("kira_hybrid_runtime");
const wrappers = @import("runtime_wrappers.zig");

pub const DeveloperFacade = struct {
    arena: std.heap.ArenaAllocator,
    report_buffer: [65537]u8 = [_]u8{0} ** 65537,
    error_buffer: [1025]u8 = [_]u8{0} ** 1025,

    pub fn create() !*DeveloperFacade {
        const developer = try std.heap.c_allocator.create(DeveloperFacade);
        developer.* = .{
            .arena = std.heap.ArenaAllocator.init(std.heap.c_allocator),
        };
        return developer;
    }

    pub fn destroy(self: *DeveloperFacade) void {
        self.arena.deinit();
        std.heap.c_allocator.destroy(self);
    }

    pub fn check(self: *DeveloperFacade, path: []const u8, backend: api.KiraDeveloperBackend) !bool {
        self.reset();
        build.setNativePreparationMode(.resolve_only);
        defer build.setNativePreparationMode(.full);

        const allocator = self.arena.allocator();
        const input = try resolveInput(allocator, path);
        const target_backend = selectedBackend(input, backend);
        var system = build.BuildSystem.init(allocator);
        const result = switch (input.target.target_kind) {
            .library => try system.checkPackageRoot(input.target.source_root.?),
            .executable, .example, .source_file => blk: {
                const source_path = input.target.source_path.?;
                // `kira check` is the public executable-validity contract: it runs the
                // executable-obligation verifier for the backend the program would build/run
                // with (explicit backend > project default > vm), not a frontend-only pass.
                // A program that passes `kira check --backend X` is guaranteed to clear the
                // executable phase gate for X, so build/run cannot later hit a lowering gap.
                const resolved_backend = target_backend orelse input.default_backend orelse .vm;
                break :blk try system.checkForBackend(source_path, resolved_backend);
            },
        };
        if (!diagnostics.hasErrors(result.diagnostics)) {
            try self.setReport("check passed\n");
            return true;
        }
        try self.setDiagnosticsReport(&result.source, result.diagnostics);
        return false;
    }

    pub fn buildPackage(self: *DeveloperFacade, path: []const u8, backend: api.KiraDeveloperBackend) !bool {
        self.reset();
        build.setNativePreparationMode(.artifacts_only);
        defer build.setNativePreparationMode(.full);

        const allocator = self.arena.allocator();
        const input = try resolveInput(allocator, path);
        const resolved_backend = selectedBackend(input, backend) orelse input.default_backend orelse .vm;
        var system = build.BuildSystem.init(allocator);
        if (input.target.target_kind == .library) {
            const result = try system.checkPackageRoot(input.target.source_root.?);
            if (diagnostics.hasErrors(result.diagnostics)) {
                try self.setDiagnosticsReport(&result.source, result.diagnostics);
                return false;
            }
            try self.setReportFmt("built library {s}\n", .{input.target.source_root.?});
            return true;
        }

        const source_path = input.target.source_path.?;
        const output_root = try outputRoot(allocator, input.target.root_path);
        try ensurePath(output_root);
        const output_path = try defaultOutputPath(
            allocator,
            output_root,
            input.target.project_name orelse std.fs.path.stem(source_path),
            resolved_backend,
        );
        const result = try system.build(.{
            .source_path = source_path,
            .output_path = output_path,
            .target = .{ .execution = resolved_backend },
        });
        if (result.failed()) {
            try self.setDiagnosticsReport(if (result.source) |*compiled_source| compiled_source else null, result.diagnostics);
            return false;
        }
        var output: std.Io.Writer.Allocating = .init(allocator);
        defer output.deinit();
        for (result.artifacts) |artifact| try output.writer.print("wrote {s}\n", .{artifact.path});
        try self.setReport(output.written());
        return true;
    }

    pub fn testPackage(self: *DeveloperFacade, path: []const u8, backend: api.KiraDeveloperBackend) !bool {
        self.reset();
        // Test functions execute through the VM runner. The VM can additionally
        // dispatch into @Native packages when the leaf is built for hybrid, so
        // `--backend hybrid` (and a hybrid project default) is supported and lets
        // FFI / native-bridge code be exercised as Foundation `Test` declarations.
        // llvm/wasm produce a standalone native artifact rather than VM-runnable
        // Test functions, so they remain unsupported here.
        if (backend == .wasm32_emscripten) {
            try self.setReport("error[KCLI020]: unsupported test backend\n  kira test executes Test functions through the build-time VM; wasm is not supported.\n");
            return false;
        }
        build.setNativePreparationMode(.artifacts_only);
        defer build.setNativePreparationMode(.full);

        const allocator = self.arena.allocator();
        const leaves = try discoverTestLeaves(allocator, path);
        var full: std.Io.Writer.Allocating = .init(allocator);
        defer full.deinit();
        var aggregate = TestReport{};
        for (leaves) |leaf| {
            if (leaves.len > 1) try full.writer.print("suite {s}\n", .{leaf});
            const leaf_report = try self.executeLeaf(leaf, backend, &full.writer);
            aggregate.add(leaf_report);
        }
        try full.writer.print("test result: {d} passed; {d} failed; {d} total\n", .{
            aggregate.passed,
            aggregate.failed,
            aggregate.total,
        });
        try self.setReport(full.written());
        return aggregate.failed == 0;
    }

    pub fn report(self: *DeveloperFacade) [*:0]const u8 {
        return @ptrCast(&self.report_buffer);
    }

    pub fn lastError(self: *DeveloperFacade) [*:0]const u8 {
        return @ptrCast(&self.error_buffer);
    }

    fn executeLeaf(self: *DeveloperFacade, input_path: []const u8, backend: api.KiraDeveloperBackend, writer: anytype) !TestReport {
        const allocator = self.arena.allocator();
        const input = try resolveInput(allocator, input_path);
        const source_path = input.target.source_path orelse return error.ProjectEntrypointNotFound;
        const expected_diagnostic = try discoverExpectedDiagnostic(allocator, input.target.root_path orelse std.fs.path.dirname(source_path) orelse ".");
        // Test functions run on the VM. When `--backend hybrid` is explicitly
        // requested the leaf is compiled for hybrid so its @Native packages
        // produce native libraries the VM dispatches into, letting FFI /
        // native-bridge code be exercised as `Test` declarations. Bare `kira test`
        // stays on the VM (fast) regardless of the project's default backend.
        // Tests EXECUTE on the build-time VM (comptime; backend-independent), so
        // the verdict is identical on every backend. A non-vm `--backend` is a
        // parity check: the program must additionally compile/codegen for that
        // backend (verified below), but the test outcome itself is the single
        // build-time result. @Native packages can't build for vm, so they execute
        // under the hybrid bridge instead.
        const test_backend: build_def.ExecutionTarget = if (backend == .hybrid) .hybrid else .vm;
        // Default: the pure-Kira test driver — synthesize a Kira entry that runs
        // every Test, compares in Kira (`==`), and reports PASS/FAIL, with trap
        // tests re-run in isolation and checked for the abort. No Zig comparison
        // override. KIRA_LEGACY_TEST=1 falls back to the historical Zig runner.
        const pure_test = std.c.getenv("KIRA_LEGACY_TEST") == null;
        // Under hybrid the driver must run through the hybrid runtime so its
        // @Native/FFI calls bridge — which needs the linked native library only
        // the artifact build produces. Take the dedicated hybrid path.
        if (pure_test and backend == .hybrid) {
            const output_root = try outputRoot(allocator, input.target.root_path);
            try ensurePath(output_root);
            return executeViaHybridDriver(self, source_path, output_root, writer);
        }
        const result = try build.compileFileForBackendWithOptions(allocator, source_path, test_backend, null, &.{}, .{
            .allow_runtime_direct_ffi = true,
            .require_main = false,
            .test_mode = true,
            .synthesize_test_driver = pure_test,
        });
        if (result.failed()) {
            // A native-FFI suite's @Native packages cannot run on the VM, so its
            // verdict build fails with KBE001. Such a suite is inherently hybrid-
            // shaped (VM runtime + native bridge) and cannot be a pure native
            // LLVM executable, so the pure-LLVM codegen gate does not apply --
            // hybrid IS its native form. When a native-capable backend was
            // requested, run the (backend-independent) verdict through the hybrid
            // driver instead of reporting a spurious failure.
            if (expected_diagnostic == null and pure_test and backend == .llvm and
                isNativeBackendRequiredError(result.diagnostics))
            {
                const output_root = try outputRoot(allocator, input.target.root_path);
                try ensurePath(output_root);
                return executeViaHybridDriver(self, source_path, output_root, writer);
            }
            if (expected_diagnostic) |expected| {
                const actual = firstErrorCode(result.diagnostics) orelse "";
                if (std.mem.eql(u8, expected, actual)) {
                    try writer.print("PASS {s} (diagnostic {s})\n", .{ input.target.displayPath(), expected });
                    return .{ .passed = 1, .total = 1 };
                }
                try writer.print("FAIL {s} (wrong diagnostic: expected {s}, got {s})\n", .{ input.target.displayPath(), expected, if (actual.len == 0) "<none>" else actual });
                return .{ .failed = 1, .total = 1 };
            }
            try writeDiagnostics(writer, &result.source, result.diagnostics);
            return .{ .failed = 1, .total = 1 };
        }
        if (expected_diagnostic) |expected| {
            try writer.print("FAIL {s} (expected diagnostic {s}, but program succeeded)\n", .{ input.target.displayPath(), expected });
            return .{ .failed = 1, .total = 1 };
        }
        // Full backend parity: `--backend llvm` additionally proves the program
        // clears the LLVM executable phase gate (codegens for native). The test
        // verdict itself is the backend-independent build-time VM run below.
        if (backend == .llvm) {
            if (try llvmTestParityFailure(allocator, source_path, pure_test, input.target.displayPath(), writer)) |parity_failure| return parity_failure;
        }
        if (pure_test) return executeViaDriver(allocator, result, writer);
        return executeCompiledTests(allocator, result, writer);
    }

    fn setDiagnosticsReport(self: *DeveloperFacade, source: ?*const source_pkg.SourceFile, items: []const diagnostics.Diagnostic) !void {
        var output: std.Io.Writer.Allocating = .init(self.arena.allocator());
        defer output.deinit();
        try writeDiagnostics(&output.writer, source, items);
        try self.setReport(output.written());
    }

    fn setReportFmt(self: *DeveloperFacade, comptime fmt: []const u8, args: anytype) !void {
        var output: std.Io.Writer.Allocating = .init(self.arena.allocator());
        defer output.deinit();
        try output.writer.print(fmt, args);
        try self.setReport(output.written());
    }

    fn setReport(self: *DeveloperFacade, message: []const u8) !void {
        const length = @min(message.len, self.report_buffer.len - 1);
        @memcpy(self.report_buffer[0..length], message[0..length]);
        self.report_buffer[length] = 0;
        if (length + 1 < self.report_buffer.len) @memset(self.report_buffer[length + 1 ..], 0);
    }

    fn setError(self: *DeveloperFacade, message: []const u8) void {
        const length = @min(message.len, self.error_buffer.len - 1);
        @memcpy(self.error_buffer[0..length], message[0..length]);
        self.error_buffer[length] = 0;
        if (length + 1 < self.error_buffer.len) @memset(self.error_buffer[length + 1 ..], 0);
    }

    fn reset(self: *DeveloperFacade) void {
        self.arena.deinit();
        self.arena = std.heap.ArenaAllocator.init(std.heap.c_allocator);
        @memset(&self.report_buffer, 0);
        @memset(&self.error_buffer, 0);
    }
};

const ResolvedInput = struct {
    target: kira_project.ResolvedTarget,
    default_backend: ?build_def.ExecutionTarget = null,
};

const TestReport = struct {
    passed: usize = 0,
    failed: usize = 0,
    total: usize = 0,

    fn add(self: *TestReport, other: TestReport) void {
        self.passed += other.passed;
        self.failed += other.failed;
        self.total += other.total;
    }
};

const TestExpectation = union(enum) {
    ok: runtime_abi.Value,
    expected_error: ExpectedKiraError,
};

const ExpectedKiraError = struct {
    kind: []const u8,
    message: []const u8,
};

pub export fn kira_developer_create() callconv(.c) ?*DeveloperFacade {
    return DeveloperFacade.create() catch null;
}

pub export fn kira_developer_destroy(developer: ?*DeveloperFacade) callconv(.c) void {
    if (developer) |value| value.destroy();
}

pub export fn kira_developer_check(developer: ?*DeveloperFacade, path: ?[*:0]const u8, backend: api.KiraDeveloperBackend) callconv(.c) api.KiraStatus {
    return runDeveloperCommand(developer, path, backend, DeveloperFacade.check);
}

pub export fn kira_developer_build(developer: ?*DeveloperFacade, path: ?[*:0]const u8, backend: api.KiraDeveloperBackend) callconv(.c) api.KiraStatus {
    return runDeveloperCommand(developer, path, backend, DeveloperFacade.buildPackage);
}

pub export fn kira_developer_test(developer: ?*DeveloperFacade, path: ?[*:0]const u8, backend: api.KiraDeveloperBackend) callconv(.c) api.KiraStatus {
    return runDeveloperCommand(developer, path, backend, DeveloperFacade.testPackage);
}

pub export fn kira_developer_report(developer: ?*DeveloperFacade) callconv(.c) ?[*:0]const u8 {
    if (developer == null) return null;
    return developer.?.report();
}

pub export fn kira_developer_last_error(developer: ?*DeveloperFacade) callconv(.c) ?[*:0]const u8 {
    if (developer == null) return null;
    return developer.?.lastError();
}

fn runDeveloperCommand(
    developer: ?*DeveloperFacade,
    path: ?[*:0]const u8,
    backend: api.KiraDeveloperBackend,
    comptime command: fn (*DeveloperFacade, []const u8, api.KiraDeveloperBackend) anyerror!bool,
) api.KiraStatus {
    if (developer == null or path == null) return .fail;
    const ok = command(developer.?, wrappers.cStringSlice(path.?), backend) catch |err| {
        developer.?.setError(@errorName(err));
        return .fail;
    };
    return if (ok) .ok else .fail;
}

fn resolveInput(allocator: std.mem.Allocator, path: []const u8) !ResolvedInput {
    const target = try kira_project.resolveTargetFromPath(allocator, path);
    return .{
        .target = target,
        .default_backend = if (target.project) |project| parseExecutionTarget(project.manifest.execution_mode) catch null else null,
    };
}

fn selectedBackend(input: ResolvedInput, backend: api.KiraDeveloperBackend) ?build_def.ExecutionTarget {
    return switch (backend) {
        .default => input.default_backend,
        .vm => .vm,
        .llvm => .llvm_native,
        .hybrid => .hybrid,
        .wasm32_emscripten => .wasm32_emscripten,
    };
}

fn parseExecutionTarget(text: []const u8) !build_def.ExecutionTarget {
    if (std.mem.eql(u8, text, "vm")) return .vm;
    if (std.mem.eql(u8, text, "llvm") or std.mem.eql(u8, text, "llvm_native")) return .llvm_native;
    if (std.mem.eql(u8, text, "wasm") or std.mem.eql(u8, text, "wasm32-emscripten")) return .wasm32_emscripten;
    if (std.mem.eql(u8, text, "hybrid")) return .hybrid;
    return error.InvalidProjectExecutionMode;
}

fn outputRoot(allocator: std.mem.Allocator, project_root: ?[]const u8) ![]u8 {
    if (project_root) |root| return std.fs.path.join(allocator, &.{ root, "generated" });
    return allocator.dupe(u8, "generated");
}

fn ensurePath(path: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, path);
}

fn defaultOutputPath(allocator: std.mem.Allocator, output_root: []const u8, stem: []const u8, backend: build_def.ExecutionTarget) ![]const u8 {
    return switch (backend) {
        .vm => std.fmt.allocPrint(allocator, "{s}/{s}.kbc", .{ output_root, stem }),
        .llvm_native => std.fmt.allocPrint(allocator, "{s}/{s}{s}", .{ output_root, stem, build.executableExtension() }),
        .wasm32_emscripten => std.fmt.allocPrint(allocator, "{s}/{s}.js", .{ output_root, stem }),
        .hybrid => std.fmt.allocPrint(allocator, "{s}/{s}.khm", .{ output_root, stem }),
    };
}

fn executeCompiledTests(allocator: std.mem.Allocator, result: build.ExecutablePipelineResult, writer: anytype) !TestReport {
    const module = result.bytecode_module orelse return error.MissingBytecodeArtifact;
    var vm = vm_runtime.Vm.init(std.heap.smp_allocator);
    defer vm.deinit();
    var ffi_dispatcher = vm_runtime.FfiDispatcher.init(std.heap.smp_allocator, &module);
    defer ffi_dispatcher.deinit();
    for (result.native_libraries) |library| try ffi_dispatcher.registerLibrary(library.name, library.artifact_path);

    var report = TestReport{};
    for (module.construct_implementations) |implementation| {
        if (!std.mem.eql(u8, implementation.construct_constraint.construct_name, "Test")) continue;
        report.total += 1;
        const test_name = try std.fmt.allocPrint(allocator, "{s}__test", .{implementation.type_name});
        const expect_name = try std.fmt.allocPrint(allocator, "{s}__expect", .{implementation.type_name});
        const test_function = findFunctionByName(module, test_name) orelse {
            report.failed += 1;
            try writer.print("FAIL {s} (missing test artifact)\n", .{implementation.type_name});
            continue;
        };
        const expect_function = findFunctionByName(module, expect_name) orelse {
            report.failed += 1;
            try writer.print("FAIL {s} (missing expect artifact)\n", .{implementation.type_name});
            continue;
        };
        const expected = vm.runFunctionById(&module, expect_function.id, &.{}, writer, .{
            .context = &ffi_dispatcher,
            .call_native = vm_runtime.FfiDispatcher.hook,
        }) catch |err| {
            report.failed += 1;
            try writer.print("FAIL {s} ({s})\n", .{ implementation.type_name, @errorName(err) });
            continue;
        };
        const expectation = decodeTestExpectation(&vm, &module, expect_function.return_type, expected) catch |err| {
            report.failed += 1;
            try writer.print("FAIL {s} (invalid expected Result: {s})\n", .{ implementation.type_name, @errorName(err) });
            continue;
        };
        try executeOneTest(&report, &vm, &module, test_function, expectation, &ffi_dispatcher, writer, implementation.type_name);
    }
    return report;
}

/// Execute the synthesized pure-Kira test driver: run `__kira_test_main` (which
/// runs every Test, compares in Kira, and prints PASS/FAIL/SKIP) and tally its
/// output. No Zig comparison override — the suite ran as ordinary Kira.
fn executeViaDriver(allocator: std.mem.Allocator, result: build.ExecutablePipelineResult, writer: anytype) !TestReport {
    const module = result.bytecode_module orelse return error.MissingBytecodeArtifact;
    const driver = findFunctionByName(module, "__kira_test_main") orelse return .{};

    var vm = vm_runtime.Vm.init(std.heap.smp_allocator);
    defer vm.deinit();
    var ffi_dispatcher = vm_runtime.FfiDispatcher.init(std.heap.smp_allocator, &module);
    defer ffi_dispatcher.deinit();
    for (result.native_libraries) |library| try ffi_dispatcher.registerLibrary(library.name, library.artifact_path);

    var captured: std.Io.Writer.Allocating = .init(allocator);
    defer captured.deinit();
    _ = vm.runFunctionById(&module, driver.id, &.{}, &captured.writer, .{
        .context = &ffi_dispatcher,
        .call_native = vm_runtime.FfiDispatcher.hook,
    }) catch |err| {
        try writer.print("FAIL <test-driver> ({s})\n", .{@errorName(err)});
        return .{ .failed = 1, .total = 1 };
    };

    return tallyDriverOutput(allocator, captured.written(), writer, VmTrapChecker{
        .vm = &vm,
        .module = &module,
        .ffi_dispatcher = &ffi_dispatcher,
    });
}

/// Parse the synthesized driver's PASS/FAIL/KTRAP lines into a TestReport.
/// PASS/FAIL lines are forwarded as-is. A `KTRAP <name>` line is a
/// trap-expectation test the driver could not run inline (a hard abort would
/// kill the whole driver): `trap_ctx.traps(allocator, name)` re-runs that test's
/// `test()` in isolation and reports whether it trapped — turning it into a real
/// PASS/FAIL. `trap_ctx` is backend-specific (VM or hybrid).
fn tallyDriverOutput(allocator: std.mem.Allocator, output: []const u8, writer: anytype, trap_ctx: anytype) !TestReport {
    var report = TestReport{};
    var lines = std.mem.tokenizeScalar(u8, output, '\n');
    while (lines.next()) |raw_line| {
        if (raw_line.len == 0) continue;
        // Driver markers are NUL-prefixed (see synth_test_driver.zig). Anything
        // else is ordinary output the tests themselves printed onto the shared
        // stdout stream: forward it for visibility, but never count it as a
        // result (a test that printed "PASS x" must not become a passed test).
        if (raw_line[0] != 0) {
            try writer.print("{s}\n", .{raw_line});
            continue;
        }
        const line = raw_line[1..];
        if (std.mem.startsWith(u8, line, "KTRAP ")) {
            const name = line["KTRAP ".len..];
            const outcome = try trap_ctx.traps(allocator, name);
            if (outcome.trapped and outcome.message_matched) {
                report.passed += 1;
                try writer.print("PASS {s}\n", .{name});
            } else if (!outcome.trapped) {
                report.failed += 1;
                try writer.print("FAIL {s} (expected a runtime trap, but the test produced a value)\n", .{name});
            } else {
                report.failed += 1;
                try writer.print("FAIL {s} (trap message mismatch: expected to contain \"{s}\", got \"{s}\")\n", .{ name, outcome.expected, outcome.actual });
            }
        } else {
            try writer.print("{s}\n", .{line});
            if (std.mem.startsWith(u8, line, "PASS ")) {
                report.passed += 1;
            } else if (std.mem.startsWith(u8, line, "FAIL ")) {
                report.failed += 1;
            }
        }
    }
    report.total = report.passed + report.failed;
    return report;
}

/// The expected runtime-failure message for a trap test: run its `__expect()`
/// (pure Kira) and decode the `Result.Error(TestFailure ...)` payload. Returns
/// "" when there is no specific message to match (or anything fails to decode),
/// in which case any trap is accepted -- matching the legacy substring check's
/// empty-message case. The result is duped so it survives the subsequent
/// `__test()` run (which clobbers the VM's error buffer).
fn expectedTrapMessage(
    allocator: std.mem.Allocator,
    vm: *vm_runtime.Vm,
    module: *const bytecode.Module,
    name: []const u8,
    run_options: vm_runtime.Hooks,
) []const u8 {
    const expect_name = std.fmt.allocPrint(allocator, "{s}__expect", .{name}) catch return "";
    const expect_fn = findFunctionByName(module.*, expect_name) orelse return "";
    var discard: std.Io.Writer.Allocating = .init(allocator);
    defer discard.deinit();
    const value = vm.runFunctionById(module, expect_fn.id, &.{}, &discard.writer, run_options) catch return "";
    const expectation = decodeTestExpectation(vm, module, expect_fn.return_type, value) catch return "";
    return switch (expectation) {
        .expected_error => |expected_error| allocator.dupe(u8, expected_error.message) catch "",
        .ok => "",
    };
}

/// Same as `expectedTrapMessage`, but evaluates `<name>__expect()` THROUGH THE
/// HYBRID BRIDGE so an expect block that calls @Native/FFI runs correctly (the
/// embedded VM with empty hooks would otherwise abort or mis-evaluate it). The
/// resulting value is still a pure-Kira `Result`, so it decodes against the
/// runtime's own VM + module exactly as the pure-VM path does.
fn expectedTrapMessageHybrid(
    allocator: std.mem.Allocator,
    runtime: *hybrid_runtime.HybridRuntime,
    name: []const u8,
) []const u8 {
    const expect_name = std.fmt.allocPrint(allocator, "{s}__expect", .{name}) catch return "";
    const expect_fn = findFunctionByName(runtime.module, expect_name) orelse return "";
    var discard: std.Io.Writer.Allocating = .init(allocator);
    defer discard.deinit();
    const value = runtime.runFunctionForValue(expect_fn.id, &discard.writer) catch return "";
    const expectation = decodeTestExpectation(&runtime.vm, &runtime.module, expect_fn.return_type, value) catch return "";
    return switch (expectation) {
        .expected_error => |expected_error| allocator.dupe(u8, expected_error.message) catch "",
        .ok => "",
    };
}

/// Outcome of re-running a trap-expectation test's `test()` in isolation.
const TrapResult = struct {
    /// The test raised a runtime trap (the abort the driver could not catch).
    trapped: bool,
    /// The trap's message contained the expected substring, or no specific
    /// message was named. Only meaningful when `trapped`.
    message_matched: bool,
    actual: []const u8 = "",
    expected: []const u8 = "",
};

/// Re-runs a trap-expectation test's `test()` on the build-time VM and reports
/// whether it raised the expected runtime failure: it must trap, and (when the
/// test named a specific failure message) the trap's message must contain it, so
/// a divide-by-zero cannot satisfy a test that expected a "recursion" trap.
const VmTrapChecker = struct {
    vm: *vm_runtime.Vm,
    module: *const bytecode.Module,
    ffi_dispatcher: *vm_runtime.FfiDispatcher,

    fn traps(self: VmTrapChecker, allocator: std.mem.Allocator, name: []const u8) !TrapResult {
        const run_options = vm_runtime.Hooks{
            .context = self.ffi_dispatcher,
            .call_native = vm_runtime.FfiDispatcher.hook,
        };
        const expected = expectedTrapMessage(allocator, self.vm, self.module, name, run_options);
        const fn_name = try std.fmt.allocPrint(allocator, "{s}__test", .{name});
        const func = findFunctionByName(self.module.*, fn_name) orelse return .{ .trapped = false, .message_matched = false };
        var discard: std.Io.Writer.Allocating = .init(allocator);
        defer discard.deinit();
        _ = self.vm.runFunctionById(self.module, func.id, &.{}, &discard.writer, run_options) catch {
            const actual = allocator.dupe(u8, self.vm.lastError() orelse "") catch "";
            return .{
                .trapped = true,
                .message_matched = expected.len == 0 or std.mem.indexOf(u8, actual, expected) != null,
                .actual = actual,
                .expected = expected,
            };
        };
        return .{ .trapped = false, .message_matched = false };
    }
};

/// Same, but re-runs the trap test through the hybrid runtime (so a trapping
/// @Native-bridged test is detected too). The expected-message check matches the
/// VM path: the trap must occur and, when a message was named, contain it. The
/// expected message is decoded by running `__expect()` THROUGH THE BRIDGE (so an
/// expect block that calls @Native/FFI evaluates correctly), and the actual trap
/// message comes from the runtime's VM error buffer.
const HybridTrapChecker = struct {
    runtime: *hybrid_runtime.HybridRuntime,

    fn traps(self: HybridTrapChecker, allocator: std.mem.Allocator, name: []const u8) !TrapResult {
        const expected = expectedTrapMessageHybrid(allocator, self.runtime, name);
        const fn_name = try std.fmt.allocPrint(allocator, "{s}__test", .{name});
        const fn_id = blk: {
            for (self.runtime.manifest.functions) |function| {
                if (std.mem.eql(u8, function.name, fn_name)) break :blk function.id;
            }
            return .{ .trapped = false, .message_matched = false };
        };
        var discard: std.Io.Writer.Allocating = .init(allocator);
        defer discard.deinit();
        self.runtime.runFunctionWithWriter(fn_id, &discard.writer) catch {
            const actual = allocator.dupe(u8, self.runtime.vm.lastError() orelse "") catch "";
            return .{
                .trapped = true,
                .message_matched = expected.len == 0 or std.mem.indexOf(u8, actual, expected) != null,
                .actual = actual,
                .expected = expected,
            };
        };
        return .{ .trapped = false, .message_matched = false };
    }
};

/// Run the pure-Kira test driver under the hybrid runtime so @Native/FFI calls
/// bridge: build the leaf for hybrid (with the driver + Test sections), load the
/// manifest, and invoke `__kira_test_main` through the bridge, capturing output.
fn executeViaHybridDriver(self: *DeveloperFacade, source_path: []const u8, output_root: []const u8, writer: anytype) !TestReport {
    const allocator = self.arena.allocator();
    const stem = std.fs.path.stem(source_path);
    const manifest_path = try std.fmt.allocPrint(allocator, "{s}/{s}.test.khm", .{ output_root, stem });
    var system = build.BuildSystem.init(allocator);
    const outcome = try system.build(.{
        .source_path = source_path,
        .output_path = manifest_path,
        .target = .{ .execution = .hybrid },
        .test_mode = true,
        .synthesize_test_driver = true,
    });
    if (outcome.failed()) {
        if (outcome.source) |source| {
            try writeDiagnostics(writer, &source, outcome.diagnostics);
        }
        return .{ .failed = 1, .total = 1 };
    }
    const manifest_artifact = blk: {
        for (outcome.artifacts) |artifact| {
            if (artifact.kind == .hybrid_manifest) break :blk artifact;
        }
        return error.MissingHybridManifestArtifact;
    };
    const manifest = try hybrid_runtime.loadHybridModule(allocator, manifest_artifact.path);
    var runtime = try hybrid_runtime.HybridRuntime.init(allocator, manifest);
    defer runtime.deinit();

    const driver_id = blk: {
        for (manifest.functions) |function| {
            if (std.mem.eql(u8, function.name, "__kira_test_main")) break :blk function.id;
        }
        return .{}; // no tests
    };

    var captured: std.Io.Writer.Allocating = .init(allocator);
    defer captured.deinit();
    runtime.runFunctionWithWriter(driver_id, &captured.writer) catch |err| {
        try writer.print("FAIL <test-driver> ({s})\n", .{@errorName(err)});
        return .{ .failed = 1, .total = 1 };
    };
    return tallyDriverOutput(allocator, captured.written(), writer, HybridTrapChecker{ .runtime = &runtime });
}

fn executeOneTest(
    report: *TestReport,
    vm: *vm_runtime.Vm,
    module: *const bytecode.Module,
    test_function: bytecode.Function,
    expectation: TestExpectation,
    ffi_dispatcher: *vm_runtime.FfiDispatcher,
    writer: anytype,
    type_name: []const u8,
) !void {
    switch (expectation) {
        .ok => |expected_value| {
            const actual = vm.runFunctionById(module, test_function.id, &.{}, writer, .{
                .context = ffi_dispatcher,
                .call_native = vm_runtime.FfiDispatcher.hook,
            }) catch |err| {
                report.failed += 1;
                try writer.print("FAIL {s} ({s})\n", .{ type_name, @errorName(err) });
                return;
            };
            if (valuesEqual(module, expected_value, actual, test_function.return_type)) {
                report.passed += 1;
                try writer.print("PASS {s}\n", .{type_name});
            } else {
                report.failed += 1;
                try writer.print("FAIL {s} (value mismatch)\n", .{type_name});
            }
        },
        .expected_error => |expected_error| {
            _ = vm.runFunctionById(module, test_function.id, &.{}, writer, .{
                .context = ffi_dispatcher,
                .call_native = vm_runtime.FfiDispatcher.hook,
            }) catch |err| {
                if (err == error.RuntimeFailure) {
                    const actual_message = vm.lastError() orelse "";
                    if (expected_error.message.len == 0 or std.mem.indexOf(u8, actual_message, expected_error.message) != null) {
                        report.passed += 1;
                        try writer.print("PASS {s}\n", .{type_name});
                    } else {
                        report.failed += 1;
                        try writer.print("FAIL {s} (runtime error mismatch)\n", .{type_name});
                    }
                    return;
                }
                report.failed += 1;
                try writer.print("FAIL {s} ({s})\n", .{ type_name, @errorName(err) });
                return;
            };
            report.failed += 1;
            try writer.print("FAIL {s} (expected {s} error)\n", .{ type_name, expected_error.kind });
        },
    }
}

fn decodeTestExpectation(vm: *vm_runtime.Vm, module: *const bytecode.Module, result_ty: bytecode.TypeRef, value: runtime_abi.Value) !TestExpectation {
    const result_name = result_ty.name orelse return error.ExpectedResultTypeMissing;
    const slots = enumSlots(value) orelse return error.ExpectedResultValueMissing;
    const tag = enumTag(slots) orelse return error.ExpectedResultTagMissing;
    const variant = enumVariantName(module, result_name, tag) orelse return error.ExpectedResultVariantMissing;
    if (std.mem.eql(u8, variant, "Ok")) return .{ .ok = slots[1] };
    if (std.mem.eql(u8, variant, "Error")) return .{ .expected_error = try decodeExpectedKiraError(vm, module, slots[1]) };
    return error.ExpectedResultVariantMissing;
}

fn decodeExpectedKiraError(vm: *vm_runtime.Vm, module: *const bytecode.Module, value: runtime_abi.Value) !ExpectedKiraError {
    const failure_name = if (value == .raw_ptr and value.raw_ptr != 0) vm.managedStructTypeName(value.raw_ptr) orelse "TestFailure" else "TestFailure";
    const slots = enumSlots(value) orelse return error.ExpectedFailureValueMissing;
    const tag = enumTag(slots) orelse return error.ExpectedFailureTagMissing;
    return .{
        .kind = enumVariantName(module, failure_name, tag) orelse return error.ExpectedFailureVariantMissing,
        .message = if (slots[1] == .string) slots[1].string else "",
    };
}

fn enumSlots(value: runtime_abi.Value) ?[*]align(1) const runtime_abi.Value {
    if (value != .raw_ptr or value.raw_ptr == 0) return null;
    return @ptrFromInt(value.raw_ptr);
}

fn enumTag(slots: [*]align(1) const runtime_abi.Value) ?u32 {
    if (slots[0] != .integer or slots[0].integer < 0) return null;
    return @intCast(slots[0].integer);
}

fn enumVariantName(module: *const bytecode.Module, enum_name: []const u8, discriminant: u32) ?[]const u8 {
    for (module.enums) |enum_decl| {
        if (!std.mem.eql(u8, enum_decl.name, enum_name)) continue;
        for (enum_decl.variants) |variant| if (variant.discriminant == discriminant) return variant.name;
    }
    return null;
}

fn valuesEqual(module: *const bytecode.Module, expected: runtime_abi.Value, actual: runtime_abi.Value, ty: bytecode.TypeRef) bool {
    if (ty.kind == .enum_instance) return enumValuesEqual(module, expected, actual, ty.name orelse return false);
    if (std.meta.activeTag(expected) != std.meta.activeTag(actual)) return false;
    return switch (expected) {
        .void => true,
        .integer => |value| value == actual.integer,
        .float => |value| value == actual.float,
        .string => |value| std.mem.eql(u8, value, actual.string),
        .boolean => |value| value == actual.boolean,
        .raw_ptr => |value| value == actual.raw_ptr,
    };
}

fn enumValuesEqual(module: *const bytecode.Module, expected: runtime_abi.Value, actual: runtime_abi.Value, enum_name: []const u8) bool {
    const expected_slots = enumSlots(expected) orelse return false;
    const actual_slots = enumSlots(actual) orelse return false;
    const expected_tag = enumTag(expected_slots) orelse return false;
    const actual_tag = enumTag(actual_slots) orelse return false;
    if (expected_tag != actual_tag) return false;
    const payload_ty = enumVariantPayloadType(module, enum_name, expected_tag) orelse return true;
    return valuesEqual(module, expected_slots[1], actual_slots[1], payload_ty);
}

fn enumVariantPayloadType(module: *const bytecode.Module, enum_name: []const u8, discriminant: u32) ?bytecode.TypeRef {
    for (module.enums) |enum_decl| {
        if (!std.mem.eql(u8, enum_decl.name, enum_name)) continue;
        for (enum_decl.variants) |variant| if (variant.discriminant == discriminant) return variant.payload_ty;
    }
    return null;
}

fn findFunctionByName(module: bytecode.Module, name: []const u8) ?bytecode.Function {
    for (module.functions) |function| if (std.mem.eql(u8, function.name, name)) return function;
    return null;
}

fn discoverTestLeaves(allocator: std.mem.Allocator, input_path: []const u8) ![]const []const u8 {
    if (!directoryExists(input_path) or isKiraAppPackage(input_path)) return allocator.dupe([]const u8, &.{input_path});
    var leaves = std.array_list.Managed([]const u8).init(allocator);
    try collectTestLeaves(allocator, input_path, &leaves);
    if (leaves.items.len == 0) return allocator.dupe([]const u8, &.{input_path});
    return leaves.toOwnedSlice();
}

fn collectTestLeaves(allocator: std.mem.Allocator, dir_path: []const u8, leaves: *std.array_list.Managed([]const u8)) !void {
    if (isKiraAppPackage(dir_path)) {
        try leaves.append(try allocator.dupe(u8, dir_path));
        return;
    }
    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, dir_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .directory) continue;
        try collectTestLeaves(allocator, try std.fs.path.join(allocator, &.{ dir_path, entry.name }), leaves);
    }
}

fn isKiraAppPackage(path: []const u8) bool {
    if (!hasManifest(path)) return false;
    const app_path = std.fs.path.join(std.heap.page_allocator, &.{ path, "app" }) catch return false;
    defer std.heap.page_allocator.free(app_path);
    return directoryExists(app_path);
}

fn hasManifest(path: []const u8) bool {
    for ([_][]const u8{ "kira.toml", "project.toml", "Kira.toml" }) |name| {
        const manifest_path = std.fs.path.join(std.heap.page_allocator, &.{ path, name }) catch return false;
        defer std.heap.page_allocator.free(manifest_path);
        if (fileExists(manifest_path)) return true;
    }
    return false;
}

fn discoverExpectedDiagnostic(allocator: std.mem.Allocator, root_path: []const u8) !?[]const u8 {
    const app_path = try std.fs.path.join(allocator, &.{ root_path, "app" });
    if (!directoryExists(app_path)) return null;
    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, app_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file or !std.mem.endsWith(u8, entry.name, ".kira")) continue;
        const file_path = try std.fs.path.join(allocator, &.{ app_path, entry.name });
        const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, file_path, allocator, .limited(2 * 1024 * 1024));
        if (extractKiraErrorCode(allocator, text)) |code| return code;
    }
    return null;
}

fn extractKiraErrorCode(allocator: std.mem.Allocator, text: []const u8) ?[]const u8 {
    const start_marker = std.mem.indexOf(u8, text, "KiraError.") orelse return null;
    var end = start_marker + "KiraError.".len;
    const start = end;
    while (end < text.len and ((text[end] >= 'A' and text[end] <= 'Z') or (text[end] >= '0' and text[end] <= '9'))) : (end += 1) {}
    if (end == start) return null;
    return allocator.dupe(u8, text[start..end]) catch null;
}

fn firstErrorCode(items: []const diagnostics.Diagnostic) ?[]const u8 {
    for (items) |item| if (item.severity == .@"error") return item.code;
    return null;
}

/// True when the build failed because a package needs a native-capable backend
/// (its @Native/FFI packages cannot run on the VM) — diagnostic code KBE001.
fn isNativeBackendRequiredError(items: []const diagnostics.Diagnostic) bool {
    const code = firstErrorCode(items) orelse return false;
    return std.mem.eql(u8, code, "KBE001");
}

/// `--backend llvm` parity gate: prove the program codegens for native LLVM
/// using test-mode lowering (require_main = false + the synthesized driver), so
/// suites whose entrypoint is only `Test` declarations are validated through the
/// driver rather than the executable path. Returns a failing report (after
/// writing the diagnostic) when codegen fails, or null on success.
fn llvmTestParityFailure(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    pure_test: bool,
    display_path: []const u8,
    writer: anytype,
) !?TestReport {
    const llvm_result = try build.compileFileForBackendWithOptions(allocator, source_path, .llvm_native, null, &.{}, .{
        .allow_runtime_direct_ffi = true,
        .require_main = false,
        .test_mode = true,
        .synthesize_test_driver = pure_test,
    });
    if (llvm_result.failed()) {
        try writer.print("FAIL {s} (llvm backend parity: program does not codegen for llvm)\n", .{display_path});
        try writeDiagnostics(writer, &llvm_result.source, llvm_result.diagnostics);
        return TestReport{ .failed = 1, .total = 1 };
    }
    return null;
}

// Render `kira check`/`build`/`test` diagnostics. With the compiled source available, route through
// the shared diagnostics renderer so every error reports its `--> path:line:column` location and
// source snippet; without a source (no entrypoint resolved) fall back to a code/title/help summary.
fn writeDiagnostics(writer: anytype, source: ?*const source_pkg.SourceFile, items: []const diagnostics.Diagnostic) !void {
    if (source) |compiled_source| {
        try diagnostics.renderer.renderAll(writer, compiled_source, items);
        return;
    }
    for (items) |item| {
        const severity = switch (item.severity) {
            .@"error" => "error",
            .warning => "warning",
            .note => "note",
        };
        if (item.code) |code| try writer.print("{s}[{s}]: {s}\n", .{ severity, code, item.title }) else try writer.print("{s}: {s}\n", .{ severity, item.title });
        try writer.print("  {s}\n", .{item.message});
        if (item.help) |help| try writer.print("  help: {s}\n", .{help});
    }
}

fn fileExists(path: []const u8) bool {
    var file = if (std.fs.path.isAbsolute(path)) std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false else std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn directoryExists(path: []const u8) bool {
    var dir = if (std.fs.path.isAbsolute(path)) std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false else std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

test "developer facade checks and tests through C API boundary" {
    const developer = (kira_developer_create() orelse return error.CreateDeveloperFailed);
    defer kira_developer_destroy(developer);
    try std.testing.expectEqual(api.KiraStatus.ok, kira_developer_check(developer, "examples/hello", .vm));
    try std.testing.expect(std.mem.indexOf(u8, std.mem.span(kira_developer_report(developer).?), "check passed") != null);
}
