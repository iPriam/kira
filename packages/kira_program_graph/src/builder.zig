const std = @import("std");
const builtin = @import("builtin");
const source_pkg = @import("kira_source");
const diagnostics = @import("kira_diagnostics");
const lexer = @import("kira_lexer");
const parser = @import("kira_parser");
const syntax = @import("kira_syntax_model");
const package_manager = @import("kira_package_manager");
const imports = @import("imports.zig");
const paths = @import("paths.zig");

var timings_enabled: bool = false;

pub fn setTimingsEnabled(enabled: bool) void {
    timings_enabled = enabled;
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

fn timingPrint(comptime fmt: []const u8, args: anytype) void {
    if (timings_enabled) std.debug.print(fmt, args);
}

const GraphTimingStats = struct {
    allocator: std.mem.Allocator,
    parse_calls: usize = 0,
    parse_bytes: usize = 0,
    collect_package_calls: usize = 0,
    package_files: usize = 0,
    graph_parse_ns: u64 = 0,
    collect_package_ns: u64 = 0,
    package_parse_counts: std.StringHashMap(usize),

    fn init(allocator: std.mem.Allocator) GraphTimingStats {
        return .{
            .allocator = allocator,
            .package_parse_counts = std.StringHashMap(usize).init(allocator),
        };
    }

    fn addPackageParse(self: *GraphTimingStats, package_name: []const u8) !void {
        const existing = self.package_parse_counts.get(package_name) orelse 0;
        if (existing == 0) {
            try self.package_parse_counts.put(try self.allocator.dupe(u8, package_name), 1);
        } else {
            try self.package_parse_counts.put(package_name, existing + 1);
        }
    }
};

pub const ProgramGraph = struct {
    program: syntax.ast.Program,
};

pub fn buildProgramGraph(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    root_program: syntax.ast.Program,
    module_map: package_manager.ModuleMap,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    var stats = GraphTimingStats.init(allocator);
    var visited = std.StringHashMap(void).init(allocator);
    var import_list = std.array_list.Managed(syntax.ast.ImportDecl).init(allocator);
    var import_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    var decls = std.array_list.Managed(syntax.ast.Decl).init(allocator);
    var decl_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    var functions = std.array_list.Managed(syntax.ast.FunctionDecl).init(allocator);
    var function_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);

    try appendProgramGraph(allocator, &stats, &visited, &import_list, &import_origins, &decls, &decl_origins, &functions, &function_origins, source_path, root_program, module_map, diags, true, null);
    printGraphStats("buildProgramGraph", &stats, import_list.items.len, decls.items.len, functions.items.len);

    return .{
        .imports = try import_list.toOwnedSlice(),
        .decls = try decls.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .import_origins = try import_origins.toOwnedSlice(),
        .decl_origins = try decl_origins.toOwnedSlice(),
        .function_origins = try function_origins.toOwnedSlice(),
    };
}

pub fn buildProgramGraphFromFiles(
    allocator: std.mem.Allocator,
    source_paths: [][]u8,
    module_map: package_manager.ModuleMap,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    var stats = GraphTimingStats.init(allocator);
    var visited = std.StringHashMap(void).init(allocator);
    var import_list = std.array_list.Managed(syntax.ast.ImportDecl).init(allocator);
    var import_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    var decls = std.array_list.Managed(syntax.ast.Decl).init(allocator);
    var decl_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    var functions = std.array_list.Managed(syntax.ast.FunctionDecl).init(allocator);
    var function_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);

    for (source_paths) |source_path| {
        const program = try parseModuleProgramTimed(allocator, &stats, source_path, diags, null);
        try appendProgramGraph(allocator, &stats, &visited, &import_list, &import_origins, &decls, &decl_origins, &functions, &function_origins, source_path, program, module_map, diags, true, null);
    }
    printGraphStats("buildProgramGraphFromFiles", &stats, import_list.items.len, decls.items.len, functions.items.len);

    return .{
        .imports = try import_list.toOwnedSlice(),
        .decls = try decls.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .import_origins = try import_origins.toOwnedSlice(),
        .decl_origins = try decl_origins.toOwnedSlice(),
        .function_origins = try function_origins.toOwnedSlice(),
    };
}

fn appendProgramGraph(
    allocator: std.mem.Allocator,
    stats: *GraphTimingStats,
    visited: *std.StringHashMap(void),
    import_list: *std.array_list.Managed(syntax.ast.ImportDecl),
    import_origins: *std.array_list.Managed(syntax.ast.DeclOrigin),
    decls: *std.array_list.Managed(syntax.ast.Decl),
    decl_origins: *std.array_list.Managed(syntax.ast.DeclOrigin),
    functions: *std.array_list.Managed(syntax.ast.FunctionDecl),
    function_origins: *std.array_list.Managed(syntax.ast.DeclOrigin),
    source_path: []const u8,
    program: syntax.ast.Program,
    module_map: package_manager.ModuleMap,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
    expose_imports: bool,
    origin_package: ?[]const u8,
) !void {
    try validateSourcePathAllowed(allocator, source_path, module_map, diags);

    const visited_key = try paths.canonicalizeExistingPath(allocator, source_path);
    defer allocator.free(visited_key);

    if (visited.contains(visited_key)) return;
    try visited.put(try allocator.dupe(u8, visited_key), {});

    const origin: syntax.ast.DeclOrigin = .{
        .package_name = if (origin_package) |package_name| try allocator.dupe(u8, package_name) else null,
        .source_path = try allocator.dupe(u8, visited_key),
    };

    // Always record imports together with the package they originate from.
    // Root imports (package_name == null) are importer-visible; dependency
    // imports stay scoped to their owning package so that a dependency's own
    // qualified references (e.g. `KiraUIFoundation.Text` inside KiraUI) resolve
    // without leaking those names into the importer's public namespace.
    // `expose_imports` is preserved for callers but importer-visibility is now
    // governed by package scoping in the semantics layer.
    _ = expose_imports;
    for (program.imports) |import_decl| {
        try import_list.append(import_decl);
        try import_origins.append(origin);
    }
    for (program.decls) |decl| {
        try decls.append(decl);
        try decl_origins.append(origin);
    }
    for (program.functions) |function_decl| {
        try functions.append(function_decl);
        try function_origins.append(origin);
    }

    for (program.imports) |import_decl| {
        if (imports.packageRootOwnerForImport(module_map, import_decl.module_name)) |owner| {
            const collect_start = nowNs();
            const module_files = try collectPackageModuleFiles(allocator, owner.source_root);
            const collect_ns = elapsedNs(collect_start);
            stats.collect_package_calls += 1;
            stats.collect_package_ns += collect_ns;
            stats.package_files += module_files.len;
            timingPrint("[kira:timing] graph.collectPackageModuleFiles package={s} files={d} ns={d}\n", .{ owner.package_name, module_files.len, collect_ns });
            defer freeModuleFiles(allocator, module_files);
            if (module_files.len == 0) {
                const resolved = try imports.resolveImportPath(allocator, source_path, import_decl.module_name, module_map);
                defer freeResolution(allocator, resolved);
                try appendUnresolvedImportDiagnostic(allocator, diags, import_decl, resolved);
                return error.DiagnosticsEmitted;
            }
            for (module_files) |module_path| {
                try stats.addPackageParse(owner.package_name);
                const imported_program = try parseModuleProgramTimed(allocator, stats, module_path, diags, owner.package_name);
                try appendProgramGraph(allocator, stats, visited, import_list, import_origins, decls, decl_origins, functions, function_origins, module_path, imported_program, module_map, diags, false, owner.package_name);
            }
            continue;
        }

        const resolved = try imports.resolveImportPath(allocator, source_path, import_decl.module_name, module_map);
        defer freeResolution(allocator, resolved);

        const module_path = imports.firstExistingCandidate(resolved.candidates) orelse {
            try appendUnresolvedImportDiagnostic(allocator, diags, import_decl, resolved);
            return error.DiagnosticsEmitted;
        };
        const imported_program = try parseModuleProgramTimed(allocator, stats, module_path, diags, null);
        try appendProgramGraph(allocator, stats, visited, import_list, import_origins, decls, decl_origins, functions, function_origins, module_path, imported_program, module_map, diags, false, origin_package);
    }
}

fn appendUnresolvedImportDiagnostic(
    allocator: std.mem.Allocator,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
    import_decl: syntax.ast.ImportDecl,
    resolved: imports.ImportResolution,
) !void {
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
        .notes = try imports.resolvedCandidateNotes(allocator, resolved.candidates),
        .help = "Create the imported module under an allowed `app/` source root or remove the import.",
    });
}

fn parseModuleProgramTimed(
    allocator: std.mem.Allocator,
    stats: *GraphTimingStats,
    module_path: []const u8,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    package_name: ?[]const u8,
) !syntax.ast.Program {
    const start = nowNs();
    const source = try source_pkg.SourceFile.fromPath(allocator, module_path);
    stats.parse_calls += 1;
    stats.parse_bytes += source.text.len;
    var module_diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = lexer.tokenize(allocator, &source, &module_diags) catch {
        for (module_diags.items) |diag| try out_diagnostics.append(diag);
        return error.DiagnosticsEmitted;
    };
    const program = parser.parse(allocator, tokens, &module_diags) catch {
        for (module_diags.items) |diag| try out_diagnostics.append(diag);
        return error.DiagnosticsEmitted;
    };
    const ns = elapsedNs(start);
    stats.graph_parse_ns += ns;
    if (package_name) |name| {
        timingPrint("[kira:timing] graph.parseModuleProgram package={s} path={s} bytes={d} ns={d}\n", .{ name, module_path, source.text.len, ns });
    } else {
        timingPrint("[kira:timing] graph.parseModuleProgram path={s} bytes={d} ns={d}\n", .{ module_path, source.text.len, ns });
    }
    return program;
}

fn printGraphStats(label: []const u8, stats: *GraphTimingStats, imports_len: usize, decls_len: usize, functions_len: usize) void {
    if (!timings_enabled) return;
    std.debug.print("[kira:timing] {s}.summary parse_calls={d} parse_bytes={d} imports={d} declarations={d} functions={d} collect_package_calls={d} imported_package_files={d} parse_ns={d} collect_package_ns={d}\n", .{
        label,
        stats.parse_calls,
        stats.parse_bytes,
        imports_len,
        decls_len,
        functions_len,
        stats.collect_package_calls,
        stats.package_files,
        stats.graph_parse_ns,
        stats.collect_package_ns,
    });
    var iterator = stats.package_parse_counts.iterator();
    while (iterator.next()) |entry| {
        std.debug.print("[kira:timing] {s}.package_parse_count package={s} count={d}\n", .{ label, entry.key_ptr.*, entry.value_ptr.* });
    }
}

pub fn parseModuleProgram(
    allocator: std.mem.Allocator,
    module_path: []const u8,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    const source = try source_pkg.SourceFile.fromPath(allocator, module_path);
    var module_diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = lexer.tokenize(allocator, &source, &module_diags) catch {
        for (module_diags.items) |diag| try out_diagnostics.append(diag);
        return error.DiagnosticsEmitted;
    };
    return parser.parse(allocator, tokens, &module_diags) catch {
        for (module_diags.items) |diag| try out_diagnostics.append(diag);
        return error.DiagnosticsEmitted;
    };
}

pub fn collectPackageModuleFiles(allocator: std.mem.Allocator, source_root: []const u8) ![][]u8 {
    const canonical_root = try paths.canonicalizeSourceRoot(allocator, source_root);
    defer allocator.free(canonical_root);

    var files = std.array_list.Managed([]u8).init(allocator);
    if (paths.dirExists(canonical_root)) try appendPackageModuleFiles(allocator, &files, canonical_root);

    const bindings_root = try paths.bindingsRootForSourceRoot(allocator, canonical_root);
    defer allocator.free(bindings_root);
    if (paths.dirExists(bindings_root)) try appendPackageModuleFiles(allocator, &files, bindings_root);

    sortPaths(files.items);
    return files.toOwnedSlice();
}

fn appendPackageModuleFiles(
    allocator: std.mem.Allocator,
    files: *std.array_list.Managed([]u8),
    current_path: []const u8,
) !void {
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, current_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);

    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        const child_path = try std.fs.path.join(allocator, &.{ current_path, entry.name });
        defer allocator.free(child_path);
        switch (entry.kind) {
            .directory => try appendPackageModuleFiles(allocator, files, child_path),
            .file => {
                if (!std.mem.eql(u8, std.fs.path.extension(entry.name), ".kira")) continue;
                try files.append(try paths.canonicalizeExistingPath(allocator, child_path));
            },
            else => {},
        }
    }
}

fn validateSourcePathAllowed(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    module_map: package_manager.ModuleMap,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !void {
    if (try imports.sourcePathIsAllowed(allocator, source_path, module_map)) return;

    try diagnostics.appendOwned(allocator, diags, .{
        .severity = .@"error",
        .code = "KGRAPH001",
        .title = "source file outside canonical source root",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira source file `{s}` is outside the allowed `app/` and `bindings/` source roots for this package graph.",
            .{source_path},
        ),
        .notes = try allowedRootNotes(allocator, module_map),
        .help = "Move the Kira source file under the package `app/` directory (or `bindings/` for generated FFI bindings), or import a declared dependency through its `app/` source root.",
    });
    return error.DiagnosticsEmitted;
}

fn allowedRootNotes(allocator: std.mem.Allocator, module_map: package_manager.ModuleMap) ![]const []const u8 {
    const notes = try allocator.alloc([]const u8, module_map.owners.len);
    for (module_map.owners, 0..) |owner, index| {
        notes[index] = try std.fmt.allocPrint(
            allocator,
            "allowed source root for `{s}` is {s}",
            .{ owner.package_name, owner.source_root },
        );
    }
    return notes;
}

fn freeResolution(allocator: std.mem.Allocator, resolution: imports.ImportResolution) void {
    allocator.free(resolution.display_name);
    for (resolution.candidates) |candidate| allocator.free(candidate);
    allocator.free(resolution.candidates);
}

fn freeModuleFiles(allocator: std.mem.Allocator, module_files: [][]u8) void {
    for (module_files) |module_file| allocator.free(module_file);
    allocator.free(module_files);
}

fn sortPaths(items: [][]u8) void {
    var index: usize = 1;
    while (index < items.len) : (index += 1) {
        var cursor = index;
        while (cursor > 0 and std.mem.order(u8, items[cursor - 1], items[cursor]) == .gt) : (cursor -= 1) {
            const tmp = items[cursor - 1];
            items[cursor - 1] = items[cursor];
            items[cursor] = tmp;
        }
    }
}

test "collectPackageModuleFiles ignores package-root Kira files" {
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Package/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/root.kira", .data = "function rootOnly() { return; }\n" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/app/main.kira", .data = "function appOnly() { return; }\n" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app", std.testing.allocator);
    defer std.testing.allocator.free(app_root);
    const files = try collectPackageModuleFiles(std.testing.allocator, app_root);
    defer freeModuleFiles(std.testing.allocator, files);

    try std.testing.expectEqual(@as(usize, 1), files.len);
    try std.testing.expectEqualStrings("main.kira", std.fs.path.basename(files[0]));
}

test "canonical visited identity deduplicates alternate path spellings" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Package/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/app/main.kira", .data = "function onlyOnce() { return; }\n" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app", allocator);
    const canonical = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app/main.kira", allocator);
    const alternate = try std.fmt.allocPrint(allocator, "{s}/main.kira", .{app_root});
    const source_paths = try allocator.alloc([]u8, 2);
    source_paths[0] = canonical;
    source_paths[1] = alternate;

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try buildProgramGraphFromFiles(allocator, source_paths, .{ .owners = &.{} }, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 1), program.functions.len);
}

test "dependency imports do not become importer-visible imports" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "App/app");
    try tmp.dir.createDirPath(std.testing.io, "Dep/app");
    try tmp.dir.createDirPath(std.testing.io, "Foundation/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "App/app/main.kira", .data = "import Dep\nfunction appEntry() { return; }\n" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Dep/app/Dep.kira", .data = "import Foundation\nfunction depEntry() { return; }\n" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Foundation/app/Foundation.kira", .data = "function foundationEntry() { return; }\n" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app", allocator);
    const dep_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Dep/app", allocator);
    const foundation_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Foundation/app", allocator);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app/main.kira", allocator);
    const owners = [_]package_manager.ModuleMap.ModuleOwner{
        .{ .module_root = "App", .package_name = "App", .source_root = app_root },
        .{ .module_root = "Dep", .package_name = "Dep", .source_root = dep_root },
        .{ .module_root = "Foundation", .package_name = "Foundation", .source_root = foundation_root },
    };

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const root_program = try parseModuleProgram(allocator, source_path, &diags);
    const program = try buildProgramGraph(allocator, source_path, root_program, .{ .owners = owners[0..] }, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), program.imports.len);
    try std.testing.expectEqualStrings("Dep", program.imports[0].module_name.segments[0].text);
    try std.testing.expectEqualStrings("Foundation", program.imports[1].module_name.segments[0].text);
    try std.testing.expect(program.import_origins[0].package_name == null);
    try std.testing.expectEqualStrings("Dep", program.import_origins[1].package_name.?);
    try std.testing.expectEqual(@as(usize, 3), program.functions.len);
    try std.testing.expectEqual(@as(usize, 3), program.function_origins.len);
    try std.testing.expect(program.function_origins[0].package_name == null);
    try std.testing.expectEqualStrings("Dep", program.function_origins[1].package_name.?);
    try std.testing.expectEqualStrings("Foundation", program.function_origins[2].package_name.?);
}

test "graph rejects an entry source outside declared app roots" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Package/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/main.kira", .data = "" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app", allocator);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/main.kira", allocator);
    const owners = [_]package_manager.ModuleMap.ModuleOwner{.{
        .module_root = "Package",
        .package_name = "Package",
        .source_root = app_root,
    }};

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const empty_program: syntax.ast.Program = .{ .imports = &.{}, .decls = &.{}, .functions = &.{} };
    try std.testing.expectError(
        error.DiagnosticsEmitted,
        buildProgramGraph(allocator, source_path, empty_program, .{ .owners = owners[0..] }, &diags),
    );
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("KGRAPH001", diags.items[0].code.?);
}
