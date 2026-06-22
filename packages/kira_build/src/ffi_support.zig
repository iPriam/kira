const std = @import("std");
const builtin = @import("builtin");
const manifest = @import("kira_manifest");
const native = @import("kira_native_lib_definition");
const syntax = @import("kira_syntax_model");
const package_manager = @import("kira_package_manager");
const program_graph = @import("kira_program_graph");
const llvm_backend = @import("kira_llvm_backend");
const resolver = @import("native_lib_resolver.zig");
const autobind = @import("ffi_autobind.zig");
const autobind_cache = @import("ffi_autobind_cache.zig");

fn nowTimestamp() std.Io.Clock.Timestamp {
    return std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake);
}

fn elapsedNs(start: std.Io.Clock.Timestamp) u64 {
    const duration_ns = start.durationTo(std.Io.Clock.Timestamp.now(std.Options.debug_io, .awake)).raw.toNanoseconds();
    return @intCast(@max(duration_ns, 0));
}

fn timingsEnvEnabled() bool {
    if (!builtin.link_libc) return false;
    const raw = std.c.getenv("KIRA_TIMINGS") orelse return false;
    const value = std.mem.span(raw);
    return value.len != 0 and !std.mem.eql(u8, value, "0") and !std.mem.eql(u8, value, "false");
}

fn timingPrint(comptime fmt: []const u8, args: anytype) void {
    if (timingsEnvEnabled()) std.debug.print(fmt, args);
}

pub const NativePreparationMode = enum {
    full,
    artifacts_only,
    resolve_only,
};

pub const NativeWarningKind = enum {
    artifact_out_of_date,
    bindings_out_of_date,
};

pub const NativeWarning = struct {
    kind: NativeWarningKind,
    library_name: []const u8,
    manifest_path: ?[]const u8 = null,
    artifact_path: ?[]const u8 = null,
    bindings_path: ?[]const u8 = null,
};

var native_preparation_mode: NativePreparationMode = .full;

pub fn setNativePreparationMode(mode: NativePreparationMode) void {
    native_preparation_mode = mode;
    autobind.setBindingMode(if (mode == .full) .ensure else .skip);
}

pub fn prepareNativeLibraries(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    imports: []const syntax.ast.ImportDecl,
) ![]const native.ResolvedNativeLibrary {
    return prepareNativeLibrariesForTarget(allocator, source_path, imports, null);
}

pub fn prepareNativeLibrariesForTarget(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    imports: []const syntax.ast.ImportDecl,
    explicit_selector: ?native.TargetSelector,
) ![]const native.ResolvedNativeLibrary {
    const selector = try resolvedTargetSelector(allocator, explicit_selector);
    const manifest_paths = try loadProjectNativeManifestPaths(allocator, source_path);
    _ = imports;

    var libraries = std.array_list.Managed(native.ResolvedNativeLibrary).init(allocator);
    for (manifest_paths) |manifest_path| {
        var library = try resolver.resolveNativeManifestFile(allocator, manifest_path, selector);
        try applyPreparationPolicy(allocator, &library);
        try libraries.append(library);
    }
    return libraries.toOwnedSlice();
}

pub fn prepareImportedNativeLibraries(
    allocator: std.mem.Allocator,
    existing: []const native.ResolvedNativeLibrary,
    imports: []const syntax.ast.ImportDecl,
    module_map: package_manager.ModuleMap,
) ![]const native.ResolvedNativeLibrary {
    return prepareImportedNativeLibrariesForTarget(allocator, existing, imports, module_map, null);
}

/// Prepare native libraries (artifacts + generated bindings) for every package the
/// module map declares, not just packages named by parsed imports. This must run
/// before the program graph is built so freshly generated `bindings/` sources are
/// part of the same compilation instead of appearing one run later.
pub fn prepareDeclaredNativeLibrariesForTarget(
    allocator: std.mem.Allocator,
    existing: []const native.ResolvedNativeLibrary,
    module_map: package_manager.ModuleMap,
    explicit_selector: ?native.TargetSelector,
) ![]const native.ResolvedNativeLibrary {
    const selector = try resolvedTargetSelector(allocator, explicit_selector);
    var libraries = std.array_list.Managed(native.ResolvedNativeLibrary).init(allocator);
    var seen = std.StringHashMap(void).init(allocator);
    var visited_packages = std.StringHashMap(void).init(allocator);

    for (existing) |library| {
        try libraries.append(library);
        try seen.put(try artifactIdentity(allocator, library), {});
    }

    for (module_map.owners) |owner| {
        const package_root = std.fs.path.dirname(owner.source_root) orelse continue;
        try appendNativeLibrariesFromPackageRootRecursive(allocator, selector, package_root, module_map, &visited_packages, &seen, &libraries);
    }

    return libraries.toOwnedSlice();
}

pub fn collectDeclaredNativeWarningsForSource(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    explicit_selector: ?native.TargetSelector,
) ![]const NativeWarning {
    const libraries = try resolveDeclaredNativeLibrariesForSource(allocator, source_path, explicit_selector);
    return collectWarningsForLibraries(allocator, libraries);
}

pub fn collectDeclaredNativeWarningsForSourceRoot(
    allocator: std.mem.Allocator,
    source_root: []const u8,
    explicit_selector: ?native.TargetSelector,
) ![]const NativeWarning {
    const module_files = try program_graph.collectPackageModuleFiles(allocator, source_root);
    if (module_files.len == 0) return &.{};
    return collectDeclaredNativeWarningsForSource(allocator, module_files[0], explicit_selector);
}

pub fn ensureDeclaredNativeBindingsForSource(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    explicit_selector: ?native.TargetSelector,
) ![]const native.ResolvedNativeLibrary {
    const libraries = try resolveDeclaredNativeLibrariesForSource(allocator, source_path, explicit_selector);
    for (libraries) |library| {
        try autobind.ensureGeneratedBindings(allocator, library);
    }
    return libraries;
}

pub fn ensureDeclaredNativeBindingsForSourceRoot(
    allocator: std.mem.Allocator,
    source_root: []const u8,
    explicit_selector: ?native.TargetSelector,
) ![]const native.ResolvedNativeLibrary {
    const module_files = try program_graph.collectPackageModuleFiles(allocator, source_root);
    if (module_files.len == 0) return &.{};
    return ensureDeclaredNativeBindingsForSource(allocator, module_files[0], explicit_selector);
}

pub fn prepareImportedNativeLibrariesForTarget(
    allocator: std.mem.Allocator,
    existing: []const native.ResolvedNativeLibrary,
    imports: []const syntax.ast.ImportDecl,
    module_map: package_manager.ModuleMap,
    explicit_selector: ?native.TargetSelector,
) ![]const native.ResolvedNativeLibrary {
    const selector = try resolvedTargetSelector(allocator, explicit_selector);
    var libraries = std.array_list.Managed(native.ResolvedNativeLibrary).init(allocator);
    var seen = std.StringHashMap(void).init(allocator);
    var visited_packages = std.StringHashMap(void).init(allocator);

    for (existing) |library| {
        try libraries.append(library);
        try seen.put(try artifactIdentity(allocator, library), {});
    }

    for (imports) |import_decl| {
        const owner = program_graph.packageRootOwnerForImport(module_map, import_decl.module_name) orelse continue;
        const package_root = std.fs.path.dirname(owner.source_root) orelse continue;
        try appendNativeLibrariesFromPackageRootRecursive(allocator, selector, package_root, module_map, &visited_packages, &seen, &libraries);
    }

    return libraries.toOwnedSlice();
}

fn appendNativeLibrariesFromPackageRootRecursive(
    allocator: std.mem.Allocator,
    selector: native.TargetSelector,
    package_root: []const u8,
    module_map: package_manager.ModuleMap,
    visited_packages: *std.StringHashMap(void),
    seen: *std.StringHashMap(void),
    libraries: *std.array_list.Managed(native.ResolvedNativeLibrary),
) !void {
    const package_key = try packageIdentity(allocator, package_root);
    if (visited_packages.contains(package_key)) return;
    try visited_packages.put(package_key, {});

    try appendNativeLibrariesFromPackageRoot(allocator, selector, package_root, seen, libraries);

    const project_manifest = try loadProjectManifestFromRoot(allocator, package_root);
    for (project_manifest.dependencies) |dependency| {
        const owner = findModuleOwner(module_map, dependency.name) orelse continue;
        const dependency_root = std.fs.path.dirname(owner.source_root) orelse continue;
        try appendNativeLibrariesFromPackageRootRecursive(allocator, selector, dependency_root, module_map, visited_packages, seen, libraries);
    }
}

fn findModuleOwner(module_map: package_manager.ModuleMap, package_name: []const u8) ?package_manager.ModuleMap.ModuleOwner {
    for (module_map.owners) |owner| {
        if (std.mem.eql(u8, owner.package_name, package_name)) return owner;
    }
    return null;
}

fn appendNativeLibrariesFromPackageRoot(
    allocator: std.mem.Allocator,
    selector: native.TargetSelector,
    package_root: []const u8,
    seen: *std.StringHashMap(void),
    libraries: *std.array_list.Managed(native.ResolvedNativeLibrary),
) !void {
    const manifest_paths = try loadNativeManifestPathsFromProjectRoot(allocator, package_root);
    for (manifest_paths) |manifest_path| {
        var library = try resolver.resolveNativeManifestFile(allocator, manifest_path, selector);
        const identity = try artifactIdentity(allocator, library);
        if (seen.contains(identity)) continue;
        try applyPreparationPolicy(allocator, &library);
        try seen.put(identity, {});
        try libraries.append(library);
    }
}

fn resolveDeclaredNativeLibrariesForSource(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    explicit_selector: ?native.TargetSelector,
) ![]const native.ResolvedNativeLibrary {
    const selector = try resolvedTargetSelector(allocator, explicit_selector);
    var libraries = std.array_list.Managed(native.ResolvedNativeLibrary).init(allocator);
    var seen = std.StringHashMap(void).init(allocator);
    var visited_packages = std.StringHashMap(void).init(allocator);

    const manifest_paths = try loadProjectNativeManifestPaths(allocator, source_path);
    try appendResolvedNativeLibrariesFromManifestPaths(allocator, selector, manifest_paths, &seen, &libraries);

    if (package_manager.loadModuleMapForSource(allocator, source_path)) |module_map| {
        for (module_map.owners) |owner| {
            const package_root = std.fs.path.dirname(owner.source_root) orelse continue;
            try appendResolvedNativeLibrariesFromPackageRootRecursive(allocator, selector, package_root, module_map, &visited_packages, &seen, &libraries);
        }
    } else |_| {}

    return libraries.toOwnedSlice();
}

fn appendResolvedNativeLibrariesFromManifestPaths(
    allocator: std.mem.Allocator,
    selector: native.TargetSelector,
    manifest_paths: []const []const u8,
    seen: *std.StringHashMap(void),
    libraries: *std.array_list.Managed(native.ResolvedNativeLibrary),
) !void {
    for (manifest_paths) |manifest_path| {
        const library = try resolver.resolveNativeManifestFile(allocator, manifest_path, selector);
        const identity = try artifactIdentity(allocator, library);
        if (seen.contains(identity)) continue;
        try seen.put(identity, {});
        try libraries.append(library);
    }
}

fn appendResolvedNativeLibrariesFromPackageRootRecursive(
    allocator: std.mem.Allocator,
    selector: native.TargetSelector,
    package_root: []const u8,
    module_map: package_manager.ModuleMap,
    visited_packages: *std.StringHashMap(void),
    seen: *std.StringHashMap(void),
    libraries: *std.array_list.Managed(native.ResolvedNativeLibrary),
) !void {
    const package_key = try packageIdentity(allocator, package_root);
    if (visited_packages.contains(package_key)) return;
    try visited_packages.put(package_key, {});

    const manifest_paths = try loadNativeManifestPathsFromProjectRoot(allocator, package_root);
    try appendResolvedNativeLibrariesFromManifestPaths(allocator, selector, manifest_paths, seen, libraries);

    const project_manifest = try loadProjectManifestFromRoot(allocator, package_root);
    for (project_manifest.dependencies) |dependency| {
        const owner = findModuleOwner(module_map, dependency.name) orelse continue;
        const dependency_root = std.fs.path.dirname(owner.source_root) orelse continue;
        try appendResolvedNativeLibrariesFromPackageRootRecursive(allocator, selector, dependency_root, module_map, visited_packages, seen, libraries);
    }
}

fn collectWarningsForLibraries(
    allocator: std.mem.Allocator,
    libraries: []const native.ResolvedNativeLibrary,
) ![]const NativeWarning {
    var warnings = std.array_list.Managed(NativeWarning).init(allocator);
    for (libraries) |library| {
        if (library.build.sources.len != 0) {
            const fingerprint = try nativeArtifactFingerprint(allocator, library);
            defer allocator.free(fingerprint);
            const fingerprint_path = try nativeArtifactFingerprintPath(allocator, library);
            defer allocator.free(fingerprint_path);
            if (!try nativeArtifactIsFresh(allocator, library.artifact_path, fingerprint_path, fingerprint)) {
                try warnings.append(.{
                    .kind = .artifact_out_of_date,
                    .library_name = library.name,
                    .manifest_path = library.manifest_path,
                    .artifact_path = library.artifact_path,
                });
            }
        }
        if (library.autobinding) |autobinding| {
            const cache_key = try autobind_cache.cacheKey(allocator, library, autobinding);
            defer allocator.free(cache_key);
            if (!try autobind_cache.bindingsAreCurrent(allocator, autobinding.output_path, cache_key)) {
                try warnings.append(.{
                    .kind = .bindings_out_of_date,
                    .library_name = library.name,
                    .manifest_path = library.manifest_path,
                    .bindings_path = autobinding.output_path,
                });
            }
        }
    }
    return warnings.toOwnedSlice();
}

fn applyPreparationPolicy(allocator: std.mem.Allocator, library: *native.ResolvedNativeLibrary) !void {
    switch (native_preparation_mode) {
        .resolve_only => {},
        .artifacts_only => {
            const artifact_start = nowTimestamp();
            try ensureNativeArtifact(allocator, library);
            timingPrint("[kira:timing] native.ensureArtifact library={s} path={s} ns={d}\n", .{ library.name, library.artifact_path, elapsedNs(artifact_start) });
        },
        .full => {
            const artifact_start = nowTimestamp();
            try ensureNativeArtifact(allocator, library);
            timingPrint("[kira:timing] native.ensureArtifact library={s} path={s} ns={d}\n", .{ library.name, library.artifact_path, elapsedNs(artifact_start) });
            const autobind_start = nowTimestamp();
            try autobind.ensureGeneratedBindings(allocator, library.*);
            timingPrint("[kira:timing] native.ensureGeneratedBindings library={s} ns={d}\n", .{ library.name, elapsedNs(autobind_start) });
        },
    }
}

fn artifactIdentity(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) ![]const u8 {
    if (library.artifact_path.len == 0) {
        return std.fmt.allocPrint(allocator, "runtime-dynamic:{s}:{s}", .{ library.manifest_path orelse "", library.name });
    }
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, library.artifact_path, allocator) catch allocator.dupe(u8, library.artifact_path);
}

fn packageIdentity(allocator: std.mem.Allocator, package_root: []const u8) ![]const u8 {
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, package_root, allocator) catch allocator.dupe(u8, package_root);
}

fn loadProjectNativeManifestPaths(allocator: std.mem.Allocator, source_path: []const u8) ![]const []const u8 {
    const project_manifest_path = try discoverProjectManifestPath(allocator, source_path) orelse return &.{};
    const project_root = std.fs.path.dirname(project_manifest_path) orelse ".";
    return loadNativeManifestPathsFromProjectRoot(allocator, project_root);
}

fn loadNativeManifestPathsFromProjectRoot(allocator: std.mem.Allocator, project_root: []const u8) ![]const []const u8 {
    const project_manifest = try loadProjectManifestFromRoot(allocator, project_root);

    var manifests = std.array_list.Managed([]const u8).init(allocator);
    for (project_manifest.native_libraries) |value| {
        const project_manifest_path = try findManifestInDirectory(allocator, project_root) orelse return &.{};
        try manifests.append(try absolutizeFromManifest(allocator, project_manifest_path, value));
    }
    return manifests.toOwnedSlice();
}

fn loadProjectManifestFromRoot(allocator: std.mem.Allocator, project_root: []const u8) !manifest.ProjectManifest {
    const project_manifest_path = try findManifestInDirectory(allocator, project_root) orelse return error.ProjectManifestNotFound;
    const manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, project_manifest_path, allocator, .limited(1024 * 1024));
    return manifest.parseProjectManifest(allocator, manifest_text);
}

fn ensureNativeArtifact(allocator: std.mem.Allocator, library: *native.ResolvedNativeLibrary) !void {
    if (library.build.sources.len == 0) return;
    const maybe_dir = std.fs.path.dirname(library.artifact_path) orelse ".";
    try makePath(maybe_dir);

    const fingerprint = try nativeArtifactFingerprint(allocator, library.*);
    defer allocator.free(fingerprint);
    const fingerprint_path = try nativeArtifactFingerprintPath(allocator, library.*);
    defer allocator.free(fingerprint_path);

    if (try nativeArtifactIsFresh(allocator, library.artifact_path, fingerprint_path, fingerprint)) {
        return;
    }
    switch (library.link_mode) {
        .static => try compileStaticLibraryViaClang(allocator, library),
        .dynamic => try compileSharedLibraryViaClang(allocator, library),
    }
    try writeFile(fingerprint_path, fingerprint);
}

fn nativeArtifactFingerprintPath(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) ![]const u8 {
    const metadata_root = try nativeMetadataRoot(allocator, library);
    defer allocator.free(metadata_root);
    const digest = try nativeMetadataDigest(allocator, library);
    defer allocator.free(digest);
    const file_name = try std.fmt.allocPrint(allocator, "{s}.fingerprint", .{digest});
    defer allocator.free(file_name);
    return std.fs.path.join(allocator, &.{ metadata_root, file_name });
}

fn nativeArtifactIsFresh(allocator: std.mem.Allocator, artifact_path: []const u8, fingerprint_path: []const u8, expected_fingerprint: []const u8) !bool {
    std.Io.Dir.cwd().access(std.Options.debug_io, artifact_path, .{}) catch return false;
    const existing = std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, fingerprint_path, allocator, .limited(1024)) catch return false;
    defer allocator.free(existing);
    return std.mem.eql(u8, existing, expected_fingerprint);
}

fn nativeArtifactFingerprint(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) ![]const u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update("kira-native-artifact-v1\n");
    hashString(&hasher, "name", library.name);
    hashString(&hasher, "link_mode", @tagName(library.link_mode));
    hashString(&hasher, "abi", @tagName(library.abi));
    hashString(&hasher, "arch", library.target.architecture);
    hashString(&hasher, "os", library.target.operating_system);
    hashString(&hasher, "target_abi", library.target.abi);
    if (library.manifest_path) |path| try hashFile(allocator, &hasher, path);
    try hashFiles(allocator, &hasher, library.build.sources);
    try hashFiles(allocator, &hasher, library.headers.include_dirs);
    try hashFiles(allocator, &hasher, library.build.include_dirs);
    if (library.headers.entrypoint) |path| try hashFile(allocator, &hasher, path);
    if (library.autobinding) |autobinding| try hashFiles(allocator, &hasher, autobinding.headers);
    hashStrings(&hasher, "header_define", library.headers.defines);
    hashStrings(&hasher, "build_define", library.build.defines);
    hashStrings(&hasher, "header_framework", library.headers.frameworks);
    hashStrings(&hasher, "link_framework", library.link.frameworks);
    hashStrings(&hasher, "header_system_lib", library.headers.system_libs);
    hashStrings(&hasher, "link_system_lib", library.link.system_libs);

    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    return hexDigest(allocator, &digest);
}

fn nativeMetadataRoot(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) ![]const u8 {
    const base_path = if (library.manifest_path) |manifest_path|
        std.fs.path.dirname(manifest_path) orelse manifest_path
    else
        std.fs.path.dirname(library.artifact_path) orelse ".";
    const project_root = try discoverProjectRootFromPath(allocator, base_path);
    defer allocator.free(project_root);
    return std.fs.path.join(allocator, &.{ project_root, ".kira-build", "native" });
}

fn nativeMetadataDigest(allocator: std.mem.Allocator, library: native.ResolvedNativeLibrary) ![]const u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    if (library.manifest_path) |manifest_path| {
        const canonical_manifest = try absolutize(allocator, manifest_path);
        defer allocator.free(canonical_manifest);
        hasher.update(canonical_manifest);
    }
    hasher.update(library.name);
    hasher.update("\n");
    hasher.update(library.artifact_path);
    hasher.update("\n");
    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    const hex = try hexDigest(allocator, &digest);
    return hex[0 .. hex.len - 1];
}

fn hashFiles(allocator: std.mem.Allocator, hasher: anytype, paths: []const []const u8) !void {
    var files = std.array_list.Managed([]const u8).init(allocator);
    for (paths) |path| {
        try collectNativeInputFiles(allocator, path, &files);
    }
    sortStrings(files.items);
    for (files.items) |path| {
        try hashFile(allocator, hasher, path);
    }
}

fn collectNativeInputFiles(allocator: std.mem.Allocator, path: []const u8, files: *std.array_list.Managed([]const u8)) !void {
    const stat = if (std.fs.path.isAbsolute(path))
        try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{})
    else
        try std.Io.Dir.cwd().statFile(std.Options.debug_io, path, .{});

    if (stat.kind == .file) {
        if (isNativeBuildInput(path)) try files.append(try absolutize(allocator, path));
        return;
    }
    if (stat.kind != .directory) return;
    const absolute = try absolutize(allocator, path);
    try collectNativeInputFilesInDir(allocator, absolute, "", files);
}

fn collectNativeInputFilesInDir(
    allocator: std.mem.Allocator,
    root: []const u8,
    relative: []const u8,
    files: *std.array_list.Managed([]const u8),
) !void {
    const dir_path = if (relative.len == 0) root else try std.fs.path.join(allocator, &.{ root, relative });
    var dir = try std.Io.Dir.cwd().openDir(std.Options.debug_io, dir_path, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);

    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind == .directory) {
            if (std.mem.eql(u8, entry.name, ".git") or std.mem.eql(u8, entry.name, ".kira-build") or std.mem.eql(u8, entry.name, "generated")) continue;
            const child_rel = if (relative.len == 0) try allocator.dupe(u8, entry.name) else try std.fs.path.join(allocator, &.{ relative, entry.name });
            try collectNativeInputFilesInDir(allocator, root, child_rel, files);
            continue;
        }
        if (entry.kind != .file or !isNativeBuildInput(entry.name)) continue;
        const rel_path = if (relative.len == 0) try allocator.dupe(u8, entry.name) else try std.fs.path.join(allocator, &.{ relative, entry.name });
        try files.append(try std.fs.path.join(allocator, &.{ root, rel_path }));
    }
}

fn isNativeBuildInput(path: []const u8) bool {
    const ext = std.fs.path.extension(path);
    return std.mem.eql(u8, ext, ".c") or
        std.mem.eql(u8, ext, ".h") or
        std.mem.eql(u8, ext, ".hpp") or
        std.mem.eql(u8, ext, ".hh") or
        std.mem.eql(u8, ext, ".inc") or
        std.mem.eql(u8, ext, ".m") or
        std.mem.eql(u8, ext, ".mm");
}

fn hashFile(allocator: std.mem.Allocator, hasher: anytype, path: []const u8) !void {
    hashString(hasher, "file", path);
    const contents = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(256 * 1024 * 1024));
    defer allocator.free(contents);
    hasher.update(contents);
    hasher.update("\n");
}

fn hashString(hasher: anytype, name: []const u8, value: []const u8) void {
    hasher.update(name);
    hasher.update("=");
    hasher.update(value);
    hasher.update("\n");
}

fn hashStrings(hasher: anytype, name: []const u8, values: []const []const u8) void {
    for (values) |value| hashString(hasher, name, value);
}

fn writeFile(path: []const u8, data: []const u8) !void {
    try makePath(std.fs.path.dirname(path) orelse ".");
    const file = if (std.fs.path.isAbsolute(path))
        try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true })
    else
        try std.Io.Dir.cwd().createFile(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn sortStrings(items: [][]const u8) void {
    std.mem.sort([]const u8, items, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.lessThan(u8, lhs, rhs);
        }
    }.lessThan);
}

fn hexDigest(allocator: std.mem.Allocator, digest: []const u8) ![]const u8 {
    const alphabet = "0123456789abcdef";
    const out = try allocator.alloc(u8, digest.len * 2);
    for (digest, 0..) |byte, index| {
        out[index * 2] = alphabet[byte >> 4];
        out[index * 2 + 1] = alphabet[byte & 0x0f];
    }
    return out;
}

fn compileStaticLibraryViaClang(allocator: std.mem.Allocator, library: *native.ResolvedNativeLibrary) !void {
    const llvm_toolchain = try llvm_backend.LlvmToolchain.discover(allocator);
    const clang_path = (try llvm_backend.clangDriver.appleClangPathForSelector(allocator, library.target)) orelse try llvm_toolchain.clangPath(allocator);
    defer allocator.free(clang_path);
    const llvm_ar_path = try llvm_toolchain.llvmArPath(allocator);
    defer allocator.free(llvm_ar_path);
    const target_triple = try targetTriple(allocator, library.target);
    defer allocator.free(target_triple);

    var object_paths = std.array_list.Managed([]const u8).init(allocator);
    defer {
        for (object_paths.items) |path| {
            std.Io.Dir.cwd().deleteFile(std.Options.debug_io, path) catch {};
        }
    }

    const build_suffix = randomBuildSuffix();
    const staged_artifact_path = try stagedArtifactPath(allocator, library.artifact_path, build_suffix);
    defer std.Io.Dir.cwd().deleteFile(std.Options.debug_io, staged_artifact_path) catch {};

    try compileSourcesToObjects(allocator, clang_path, target_triple, library.*, build_suffix, &object_paths);

    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.appendSlice(&.{ llvm_ar_path, "rcs", staged_artifact_path });
    try argv.appendSlice(object_paths.items);
    try runCommand(allocator, argv.items);
    try publishStagedArtifact(staged_artifact_path, library.artifact_path);
}

/// Compiles the library sources and links them into a shared library
/// (`.dll`/`.so`/`.dylib`) the VM can `dlopen` for direct LibFFI dispatch.
/// Exported symbols rely on `__declspec(dllexport)` (Windows) or default
/// visibility (POSIX) in the sources.
fn compileSharedLibraryViaClang(allocator: std.mem.Allocator, library: *native.ResolvedNativeLibrary) !void {
    const llvm_toolchain = try llvm_backend.LlvmToolchain.discover(allocator);
    const clang_path = (try llvm_backend.clangDriver.appleClangPathForSelector(allocator, library.target)) orelse try llvm_toolchain.clangPath(allocator);
    defer allocator.free(clang_path);
    const target_triple = try targetTriple(allocator, library.target);
    defer allocator.free(target_triple);

    var object_paths = std.array_list.Managed([]const u8).init(allocator);
    defer {
        for (object_paths.items) |path| {
            std.Io.Dir.cwd().deleteFile(std.Options.debug_io, path) catch {};
        }
    }

    const build_suffix = randomBuildSuffix();
    const staged_artifact_path = try stagedArtifactPath(allocator, library.artifact_path, build_suffix);
    defer std.Io.Dir.cwd().deleteFile(std.Options.debug_io, staged_artifact_path) catch {};

    try compileSourcesToObjects(allocator, clang_path, target_triple, library.*, build_suffix, &object_paths);

    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.append(clang_path);
    try llvm_backend.clangDriver.appendClangDriverArgs(allocator, &argv, library.target);
    try argv.append("-shared");
    try argv.appendSlice(&.{ "-o", staged_artifact_path });
    try argv.appendSlice(object_paths.items);
    if (isAppleOperatingSystem(library.target.operating_system)) {
        for (library.headers.frameworks) |framework| try argv.appendSlice(&.{ "-framework", framework });
        for (library.link.frameworks) |framework| try argv.appendSlice(&.{ "-framework", framework });
    }
    for (library.headers.system_libs) |system_lib| try argv.append(try std.fmt.allocPrint(allocator, "-l{s}", .{system_lib}));
    for (library.link.system_libs) |system_lib| try argv.append(try std.fmt.allocPrint(allocator, "-l{s}", .{system_lib}));
    try runCommand(allocator, argv.items);
    try publishStagedArtifact(staged_artifact_path, library.artifact_path);
}

fn randomBuildSuffix() u64 {
    var bytes: [8]u8 = undefined;
    std.Options.debug_io.random(&bytes);
    return std.mem.readInt(u64, &bytes, .little);
}

/// Compiles each declared source into an object file, appending the produced
/// object paths (caller owns cleanup). Shared by the static and dynamic builds.
fn compileSourcesToObjects(
    allocator: std.mem.Allocator,
    clang_path: []const u8,
    target_triple: []const u8,
    library: native.ResolvedNativeLibrary,
    build_suffix: u64,
    object_paths: *std.array_list.Managed([]const u8),
) !void {
    for (library.build.sources, 0..) |source_path, index| {
        const object_path = try sourceObjectPath(allocator, library.artifact_path, index, build_suffix);
        try object_paths.append(object_path);

        var argv = std.array_list.Managed([]const u8).init(allocator);
        try appendClangCompileCommand(&argv, clang_path, target_triple, library, source_path, object_path);
        for (library.headers.include_dirs) |include_dir| {
            try argv.append(try std.fmt.allocPrint(allocator, "-I{s}", .{include_dir}));
        }
        for (library.build.include_dirs) |include_dir| {
            try argv.append(try std.fmt.allocPrint(allocator, "-I{s}", .{include_dir}));
        }
        for (library.link.include_dirs) |include_dir| {
            try argv.append(try std.fmt.allocPrint(allocator, "-I{s}", .{include_dir}));
        }
        for (library.headers.defines) |define| {
            try argv.append(try std.fmt.allocPrint(allocator, "-D{s}", .{define}));
        }
        for (library.build.defines) |define| {
            try argv.append(try std.fmt.allocPrint(allocator, "-D{s}", .{define}));
        }
        for (library.link.defines) |define| {
            try argv.append(try std.fmt.allocPrint(allocator, "-D{s}", .{define}));
        }
        try runCommand(allocator, argv.items);
    }
}

fn appendClangCompileCommand(
    argv: *std.array_list.Managed([]const u8),
    clang_path: []const u8,
    _: []const u8,
    library: native.ResolvedNativeLibrary,
    source_path: []const u8,
    object_path: []const u8,
) !void {
    try argv.appendSlice(&.{ clang_path, "-c", "-O3" });
    try llvm_backend.clangDriver.appendClangDriverArgs(argv.allocator, argv, library.target);
    if (shouldCompileAsObjectiveC(library.target, library, source_path)) {
        try argv.appendSlice(&.{ "-x", "objective-c" });
    }
    try argv.appendSlice(&.{ source_path, "-o", object_path });
}

fn shouldCompileAsObjectiveC(selector: native.TargetSelector, library: native.ResolvedNativeLibrary, source_path: []const u8) bool {
    if (!isAppleOperatingSystem(selector.operating_system)) return false;
    if (library.link.frameworks.len == 0 and library.headers.frameworks.len == 0) return false;

    const extension = std.fs.path.extension(source_path);
    if (std.mem.eql(u8, extension, ".m") or std.mem.eql(u8, extension, ".mm")) return false;
    return std.mem.eql(u8, extension, ".c");
}

fn isAppleOperatingSystem(operating_system: []const u8) bool {
    return std.mem.eql(u8, operating_system, "macos") or
        std.mem.eql(u8, operating_system, "ios") or
        std.mem.eql(u8, operating_system, "tvos") or
        std.mem.eql(u8, operating_system, "xros");
}

fn sourceObjectPath(allocator: std.mem.Allocator, artifact_path: []const u8, index: usize, build_suffix: u64) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}.src-{x}-{d}.o", .{ artifact_path, build_suffix, index });
}

fn stagedArtifactPath(allocator: std.mem.Allocator, artifact_path: []const u8, build_suffix: u64) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}.stage-{x}", .{ artifact_path, build_suffix });
}

fn publishStagedArtifact(staged_path: []const u8, artifact_path: []const u8) !void {
    if (std.fs.path.isAbsolute(staged_path) and std.fs.path.isAbsolute(artifact_path)) {
        try std.Io.Dir.renameAbsolute(staged_path, artifact_path, std.Options.debug_io);
        return;
    }

    try std.Io.Dir.cwd().rename(staged_path, std.Io.Dir.cwd(), artifact_path, std.Options.debug_io);
}

fn runCommand(allocator: std.mem.Allocator, argv: []const []const u8) !void {
    const llvm_toolchain = try llvm_backend.LlvmToolchain.discover(allocator);
    var environ_map = try llvm_toolchain.processEnvironMap(allocator);
    defer environ_map.deinit();
    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = argv,
        .expand_arg0 = .expand,
        .environ_map = &environ_map,
        .stdout_limit = .limited(1024 * 1024),
        .stderr_limit = .limited(1024 * 1024),
    });
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);

    if (result.term == .exited and result.term.exited == 0) return;
    if (result.stdout.len != 0) std.debug.print("{s}", .{result.stdout});
    if (result.stderr.len != 0) std.debug.print("{s}", .{result.stderr});
    return error.NativeLibraryBuildFailed;
}

fn inheritedProcessEnviron() std.process.Environ {
    return switch (builtin.os.tag) {
        .windows => .{ .block = .global },
        .wasi, .emscripten, .freestanding, .other => .empty,
        else => .{ .block = .{ .slice = currentPosixEnvironBlock() } },
    };
}

fn currentPosixEnvironBlock() [:null]const ?[*:0]const u8 {
    if (!builtin.link_libc) return &.{};

    const environ = std.c.environ;
    var len: usize = 0;
    while (environ[len] != null) : (len += 1) {}
    return environ[0..len :null];
}

fn targetTriple(allocator: std.mem.Allocator, selector: native.TargetSelector) ![]const u8 {
    return std.fmt.allocPrint(
        allocator,
        "{s}-{s}-{s}",
        .{ selector.architecture, selector.operating_system, selector.abi },
    );
}

fn hostTargetSelector(allocator: std.mem.Allocator) !native.TargetSelector {
    return native.TargetSelector.parse(allocator, switch (builtin.os.tag) {
        .linux => switch (builtin.cpu.arch) {
            .x86_64 => "x86_64-linux-gnu",
            else => return error.UnsupportedTarget,
        },
        .macos => switch (builtin.cpu.arch) {
            .aarch64 => "aarch64-macos-none",
            else => return error.UnsupportedTarget,
        },
        .windows => switch (builtin.cpu.arch) {
            .x86_64 => if (builtin.abi == .gnu) "x86_64-windows-gnu" else "x86_64-windows-msvc",
            else => return error.UnsupportedTarget,
        },
        else => return error.UnsupportedTarget,
    });
}

fn resolvedTargetSelector(allocator: std.mem.Allocator, explicit_selector: ?native.TargetSelector) !native.TargetSelector {
    if (explicit_selector) |selector| return selector;
    return hostTargetSelector(allocator);
}

fn absolutize(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(path)) return allocator.dupe(u8, path);
    const cwd = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, ".", allocator);
    defer allocator.free(cwd);
    return std.fs.path.join(allocator, &.{ cwd, path });
}

fn absolutizeFromManifest(allocator: std.mem.Allocator, manifest_path: []const u8, value: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(value)) return allocator.dupe(u8, value);
    const base_dir = std.fs.path.dirname(manifest_path) orelse ".";
    const joined = try std.fs.path.join(allocator, &.{ base_dir, value });
    if (std.fs.path.isAbsolute(joined)) return joined;
    const cwd = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, ".", allocator);
    defer allocator.free(cwd);
    return std.fs.path.join(allocator, &.{ cwd, joined });
}

fn discoverProjectManifestPath(allocator: std.mem.Allocator, source_path: []const u8) !?[]const u8 {
    const source_dir = std.fs.path.dirname(source_path) orelse ".";
    var cursor = try absolutize(allocator, source_dir);
    errdefer allocator.free(cursor);

    while (true) {
        if (try findManifestInDirectory(allocator, cursor)) |manifest_path| {
            allocator.free(cursor);
            return manifest_path;
        }

        const parent = std.fs.path.dirname(cursor) orelse break;
        if (std.mem.eql(u8, parent, cursor)) break;
        const copy = try allocator.dupe(u8, parent);
        allocator.free(cursor);
        cursor = copy;
    }

    allocator.free(cursor);
    return null;
}

fn discoverProjectRootFromPath(allocator: std.mem.Allocator, start_path: []const u8) ![]const u8 {
    var cursor = try absolutize(allocator, start_path);
    errdefer allocator.free(cursor);
    const fallback = try allocator.dupe(u8, cursor);
    errdefer allocator.free(fallback);

    while (true) {
        if (try findManifestInDirectory(allocator, cursor)) |_| {
            allocator.free(fallback);
            return cursor;
        }

        const parent = std.fs.path.dirname(cursor) orelse break;
        if (std.mem.eql(u8, parent, cursor)) break;
        const copy = try allocator.dupe(u8, parent);
        allocator.free(cursor);
        cursor = copy;
    }

    allocator.free(cursor);
    return fallback;
}

fn findManifestInDirectory(allocator: std.mem.Allocator, directory: []const u8) !?[]const u8 {
    const names = [_][]const u8{ "kira.toml", "project.toml" };
    for (names) |name| {
        const candidate = try std.fs.path.join(allocator, &.{ directory, name });
        if (fileExists(candidate)) return candidate;
        allocator.free(candidate);
    }
    return null;
}

fn fileExists(path: []const u8) bool {
    var file = if (std.fs.path.isAbsolute(path))
        std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false
    else
        std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn makePath(path: []const u8) !void {
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, path);
}

fn processTempRoot() ?[]const u8 {
    if (!builtin.link_libc) return null;
    if (builtin.os.tag == .windows) {
        if (std.c.getenv("TEMP")) |raw| return std.mem.span(raw);
        if (std.c.getenv("TMP")) |raw| return std.mem.span(raw);
    } else if (std.c.getenv("TMPDIR")) |raw| {
        return std.mem.span(raw);
    }
    return null;
}

test "macOS framework-backed C source compiles as Objective-C" {
    const macos: native.TargetSelector = .{ .architecture = "aarch64", .operating_system = "macos", .abi = "none" };
    const linux: native.TargetSelector = .{ .architecture = "x86_64", .operating_system = "linux", .abi = "gnu" };
    const library: native.ResolvedNativeLibrary = .{
        .name = "sokol",
        .link_mode = .static,
        .abi = .c,
        .artifact_path = "/tmp/libsokol.a",
        .target = undefined,
        .headers = .{},
        .link = .{ .frameworks = &.{"AppKit"} },
    };

    try std.testing.expect(shouldCompileAsObjectiveC(macos, library, "/tmp/sokol_impl.c"));
    try std.testing.expect(!shouldCompileAsObjectiveC(macos, library, "/tmp/sokol_impl.m"));
    try std.testing.expect(!shouldCompileAsObjectiveC(linux, library, "/tmp/sokol_impl.c"));
}

test "native artifact freshness tracks C and header content changes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Native");
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Native/fresh.h",
        .data =
        \\#ifndef KIRA_NATIVE_FRESH_H
        \\#define KIRA_NATIVE_FRESH_H
        \\#define KIRA_NATIVE_STRESS_VALUE 41
        \\int kira_native_stress(void);
        \\#endif
        \\
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Native/fresh.c",
        .data =
        \\#include "fresh.h"
        \\int kira_native_stress(void) { return KIRA_NATIVE_STRESS_VALUE; }
        \\
        ,
    });
    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Native/fresh.toml",
        .data =
        \\[native]
        \\name = "fresh"
        \\
        ,
    });

    const root = try tmp.dir.realPathFileAlloc(std.testing.io, "Native", allocator);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Native/fresh.c", allocator);
    const header_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Native/fresh.h", allocator);
    const manifest_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Native/fresh.toml", allocator);
    var library: native.ResolvedNativeLibrary = .{
        .manifest_path = manifest_path,
        .name = "fresh",
        .link_mode = .static,
        .abi = .c,
        .artifact_path = try std.fs.path.join(allocator, &.{ root, "libfresh.a" }),
        .target = try hostTargetSelector(allocator),
        .headers = .{
            .entrypoint = header_path,
            .include_dirs = &.{root},
        },
        .build = .{
            .sources = &.{source_path},
            .include_dirs = &.{root},
        },
        .link = .{},
    };
    const artifact_path = library.artifact_path;
    const fingerprint_path = try nativeArtifactFingerprintPath(allocator, library);

    try ensureNativeArtifact(allocator, &library);
    const fingerprint1 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, fingerprint_path, allocator, .limited(1024));
    const artifact1 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, artifact_path, allocator, .limited(1024 * 1024));
    try std.testing.expect(try nativeArtifactIsFresh(allocator, artifact_path, fingerprint_path, fingerprint1));

    try ensureNativeArtifact(allocator, &library);
    const fingerprint2 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, fingerprint_path, allocator, .limited(1024));
    const artifact2 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, artifact_path, allocator, .limited(1024 * 1024));
    try std.testing.expectEqualStrings(fingerprint1, fingerprint2);
    try std.testing.expectEqualSlices(u8, artifact1, artifact2);

    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Native/fresh.c",
        .data =
        \\#include "fresh.h"
        \\int kira_native_stress(void) { return KIRA_NATIVE_STRESS_VALUE + 1; }
        \\
        ,
    });
    const source_fingerprint = try nativeArtifactFingerprint(allocator, library);
    try std.testing.expect(!std.mem.eql(u8, fingerprint1, source_fingerprint));
    try std.testing.expect(!try nativeArtifactIsFresh(allocator, artifact_path, fingerprint_path, source_fingerprint));
    try ensureNativeArtifact(allocator, &library);
    const fingerprint3 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, fingerprint_path, allocator, .limited(1024));
    const artifact3 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, artifact_path, allocator, .limited(1024 * 1024));
    try std.testing.expectEqualStrings(source_fingerprint, fingerprint3);
    try std.testing.expect(!std.mem.eql(u8, artifact1, artifact3));

    try tmp.dir.writeFile(std.testing.io, .{
        .sub_path = "Native/fresh.h",
        .data =
        \\#ifndef KIRA_NATIVE_FRESH_H
        \\#define KIRA_NATIVE_FRESH_H
        \\#define KIRA_NATIVE_STRESS_VALUE 55
        \\int kira_native_stress(void);
        \\#endif
        \\
        ,
    });
    const header_fingerprint = try nativeArtifactFingerprint(allocator, library);
    try std.testing.expect(!std.mem.eql(u8, fingerprint3, header_fingerprint));
    try std.testing.expect(!try nativeArtifactIsFresh(allocator, artifact_path, fingerprint_path, header_fingerprint));
    try ensureNativeArtifact(allocator, &library);
    const fingerprint4 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, fingerprint_path, allocator, .limited(1024));
    const artifact4 = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, artifact_path, allocator, .limited(1024 * 1024));
    try std.testing.expectEqualStrings(header_fingerprint, fingerprint4);
    try std.testing.expect(!std.mem.eql(u8, artifact3, artifact4));
    try std.testing.expect(try nativeArtifactIsFresh(allocator, artifact_path, fingerprint_path, fingerprint4));
}

test "native metadata stays inside the library root without a project manifest" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const allocator = arena.allocator();

    const scratch_parent = processTempRoot() orelse return error.SkipZigTest;
    const unique = try std.fmt.allocPrint(allocator, "{x}", .{nowTimestamp().raw.toNanoseconds()});
    const scratch_root = try std.fs.path.join(allocator, &.{ scratch_parent, "kirac-ffi-support-no-manifest", unique });
    try makePath(scratch_root);

    var parent_dir = try std.Io.Dir.openDirAbsolute(std.testing.io, scratch_parent, .{});
    defer parent_dir.close(std.testing.io);
    defer parent_dir.deleteTree(std.testing.io, std.fs.path.basename(scratch_root)) catch {};

    var scratch_dir = try std.Io.Dir.openDirAbsolute(std.testing.io, scratch_root, .{});
    defer scratch_dir.close(std.testing.io);
    try scratch_dir.createDirPath(std.testing.io, "Native");
    try scratch_dir.writeFile(std.testing.io, .{
        .sub_path = "Native/example.c",
        .data = "int kira_native_example(void) { return 0; }\n",
    });

    const root = try std.fs.path.join(allocator, &.{ scratch_root, "Native" });
    const source_path = try std.fs.path.join(allocator, &.{ scratch_root, "Native", "example.c" });
    const library: native.ResolvedNativeLibrary = .{
        .manifest_path = null,
        .name = "example",
        .link_mode = .static,
        .abi = .c,
        .artifact_path = try std.fs.path.join(allocator, &.{ root, "libexample.a" }),
        .target = try hostTargetSelector(allocator),
        .headers = .{},
        .build = .{
            .sources = &.{source_path},
        },
        .link = .{},
    };

    const metadata_root = try nativeMetadataRoot(allocator, library);
    const expected = try std.fs.path.join(allocator, &.{ root, ".kira-build", "native" });
    try std.testing.expectEqualStrings(expected, metadata_root);
}
