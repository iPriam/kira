const std = @import("std");
const api = @import("api.zig");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const bytecode = @import("kira_bytecode");
const diagnostics = @import("kira_diagnostics");
const kira_project = @import("kira_project");
const runtime_abi = @import("kira_runtime_abi");
const vm_runtime = @import("kira_vm_runtime");
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
        try self.setDiagnosticsReport(result.diagnostics);
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
                try self.setDiagnosticsReport(result.diagnostics);
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
            try self.setDiagnosticsReport(result.diagnostics);
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
        if (backend != .default and backend != .vm) {
            try self.setReport("error[KCLI020]: unsupported test backend\n  kira test currently executes generated Test artifacts through the VM runner.\n");
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
            const leaf_report = try self.executeLeaf(leaf, &full.writer);
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

    fn executeLeaf(self: *DeveloperFacade, input_path: []const u8, writer: anytype) !TestReport {
        const allocator = self.arena.allocator();
        const input = try resolveInput(allocator, input_path);
        const source_path = input.target.source_path orelse return error.ProjectEntrypointNotFound;
        const expected_diagnostic = try discoverExpectedDiagnostic(allocator, input.target.root_path orelse std.fs.path.dirname(source_path) orelse ".");
        const result = try build.compileFileForBackendWithOptions(allocator, source_path, .vm, null, &.{}, .{
            .allow_runtime_direct_ffi = true,
            .require_main = false,
            .test_mode = true,
        });
        if (result.failed()) {
            if (expected_diagnostic) |expected| {
                const actual = firstErrorCode(result.diagnostics) orelse "";
                if (std.mem.eql(u8, expected, actual)) {
                    try writer.print("PASS {s} (diagnostic {s})\n", .{ input.target.displayPath(), expected });
                    return .{ .passed = 1, .total = 1 };
                }
                try writer.print("FAIL {s} (wrong diagnostic: expected {s}, got {s})\n", .{ input.target.displayPath(), expected, if (actual.len == 0) "<none>" else actual });
                return .{ .failed = 1, .total = 1 };
            }
            try writeDiagnostics(writer, result.diagnostics);
            return .{ .failed = 1, .total = 1 };
        }
        if (expected_diagnostic) |expected| {
            try writer.print("FAIL {s} (expected diagnostic {s}, but program succeeded)\n", .{ input.target.displayPath(), expected });
            return .{ .failed = 1, .total = 1 };
        }
        return executeCompiledTests(allocator, result, writer);
    }

    fn setDiagnosticsReport(self: *DeveloperFacade, items: []const diagnostics.Diagnostic) !void {
        var output: std.Io.Writer.Allocating = .init(self.arena.allocator());
        defer output.deinit();
        try writeDiagnostics(&output.writer, items);
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
        const test_name = try std.fmt.allocPrint(allocator, "{s}.test", .{implementation.type_name});
        const expect_name = try std.fmt.allocPrint(allocator, "{s}.expect", .{implementation.type_name});
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

fn writeDiagnostics(writer: anytype, items: []const diagnostics.Diagnostic) !void {
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
