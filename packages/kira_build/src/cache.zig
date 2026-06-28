const std = @import("std");
const builtin = @import("builtin");
const build_def = @import("kira_build_definition");
const build_options = @import("kira_build_build_options");
const hybrid = @import("kira_hybrid_definition");
const kira_toolchain = @import("kira_toolchain");

pub const Cache = struct {
    allocator: std.mem.Allocator,
    root_path: []const u8,

    pub fn initForSource(allocator: std.mem.Allocator, source_path: []const u8) !Cache {
        const root = try projectRootForSource(allocator, source_path);
        defer allocator.free(root);
        const cache_root = try std.fs.path.join(allocator, &.{ root, ".kira-build" });
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, cache_root);
        return .{ .allocator = allocator, .root_path = cache_root };
    }

    pub fn entryForBuild(self: Cache, source_path: []const u8, target: build_def.ExecutionTarget) !Entry {
        return self.entryForBuildWithNativeDeps(source_path, target, "");
    }

    /// Like `entryForBuild`, but mixes `native_deps_fingerprint` into the cache
    /// key. Callers building an executable that links native static libraries
    /// pass a fingerprint of those libraries' artifacts so that editing native
    /// sources/headers (which relinks the binary) invalidates the cached
    /// executable instead of restoring a stale one.
    pub fn entryForBuildWithNativeDeps(self: Cache, source_path: []const u8, target: build_def.ExecutionTarget, native_deps_fingerprint: []const u8) !Entry {
        const key = try fingerprintBuild(self.allocator, source_path, @tagName(target), native_deps_fingerprint);
        defer self.allocator.free(key);
        const backend_dir = backendName(target);
        const root = try std.fs.path.join(self.allocator, &.{ self.root_path, "cache", backend_dir, key });
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, root);
        return Entry.init(self.allocator, root, target);
    }

    pub fn entryForFrontendCheck(self: Cache, source_path: []const u8) !Entry {
        const key = try fingerprintBuild(self.allocator, source_path, "frontend-check", "");
        defer self.allocator.free(key);
        const root = try std.fs.path.join(self.allocator, &.{ self.root_path, "cache", "frontend-check", key });
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, root);
        return Entry.init(self.allocator, root, .vm);
    }

    pub fn entryForPackageCheck(self: Cache, representative_source_path: []const u8) !Entry {
        const key = try fingerprintBuild(self.allocator, representative_source_path, "package-check", "");
        defer self.allocator.free(key);
        const root = try std.fs.path.join(self.allocator, &.{ self.root_path, "cache", "package-check", key });
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, root);
        return Entry.init(self.allocator, root, .vm);
    }
};

pub const Entry = struct {
    allocator: std.mem.Allocator,
    root_path: []const u8,
    target: build_def.ExecutionTarget,

    const complete_marker_name = "complete.ok";

    fn init(allocator: std.mem.Allocator, root_path: []const u8, target: build_def.ExecutionTarget) Entry {
        return .{
            .allocator = allocator,
            .root_path = root_path,
            .target = target,
        };
    }

    pub fn outputPath(self: Entry) ![]const u8 {
        return switch (self.target) {
            .vm => std.fs.path.join(self.allocator, &.{ self.root_path, "main.kbc" }),
            .llvm_native => std.fs.path.join(self.allocator, &.{ self.root_path, try executableName(self.allocator, "main") }),
            .wasm32_emscripten => std.fs.path.join(self.allocator, &.{ self.root_path, "main.js" }),
            .hybrid => std.fs.path.join(self.allocator, &.{ self.root_path, "main.khm" }),
        };
    }

    pub fn hasArtifacts(self: Entry) bool {
        if (!fileExistsJoin(self.root_path, complete_marker_name)) return false;
        return switch (self.target) {
            .vm => fileExistsNonEmptyJoin(self.root_path, "main.kbc"),
            .llvm_native => fileExistsNonEmptyJoin(self.root_path, objectName()) and fileExistsNonEmptyJoin(self.root_path, executableNamePage("main")),
            .wasm32_emscripten => fileExistsNonEmptyJoin(self.root_path, objectName()) and fileExistsNonEmptyJoin(self.root_path, "main.js") and fileExistsNonEmptyJoin(self.root_path, "main.wasm"),
            .hybrid => fileExistsNonEmptyJoin(self.root_path, "main.kbc") and
                fileExistsNonEmptyJoin(self.root_path, objectName()) and
                fileExistsNonEmptyJoin(self.root_path, sharedLibraryName()) and
                fileExistsNonEmptyJoin(self.root_path, "main.khm"),
        };
    }

    pub fn hasCheckSuccess(self: Entry) bool {
        return fileExistsJoin(self.root_path, "check.ok");
    }

    pub fn storeCheckSuccess(self: Entry) !void {
        try publishTextFileAtomic(try self.join("check.ok"), "ok\n");
    }

    pub fn hasFrontendCheckSuccess(self: Entry) bool {
        return fileExistsJoin(self.root_path, "frontend_check.ok");
    }

    pub fn storeFrontendCheckSuccess(self: Entry) !void {
        try publishTextFileAtomic(try self.join("frontend_check.ok"), "ok\n");
    }

    pub fn hasPackageCheckSuccess(self: Entry) bool {
        return fileExistsJoin(self.root_path, "package_check.ok");
    }

    pub fn storePackageCheckSuccess(self: Entry) !void {
        try publishTextFileAtomic(try self.join("package_check.ok"), "ok\n");
    }

    pub fn restoreTo(self: Entry, output_path: []const u8) ![]build_def.Artifact {
        return switch (self.target) {
            .vm => self.restoreVm(output_path),
            .llvm_native => self.restoreLlvm(output_path),
            .wasm32_emscripten => self.restoreWasm(output_path),
            .hybrid => self.restoreHybrid(output_path),
        };
    }

    pub fn storeFrom(self: Entry, output_path: []const u8) !void {
        if (self.hasArtifacts()) return;

        var stage = try StageDir.init(self.allocator, self.root_path);
        defer stage.cleanup();

        switch (self.target) {
            .vm => {
                try copyFile(output_path, try stage.join("main.kbc"));
                try publishStagedFileAtomic(try stage.join("main.kbc"), try self.join("main.kbc"));
            },
            .llvm_native => {
                const object_stage = try stage.join(objectName());
                const executable_stage = try stage.join(executableNamePage("main"));
                try copyFile(try defaultObjectPath(self.allocator, output_path), object_stage);
                try copyFile(output_path, executable_stage);
                try publishStagedFileAtomic(object_stage, try self.join(objectName()));
                try publishStagedFileAtomic(executable_stage, try self.join(executableNamePage("main")));
            },
            .wasm32_emscripten => {
                const object_stage = try stage.join(objectName());
                const js_stage = try stage.join("main.js");
                const wasm_stage = try stage.join("main.wasm");
                try copyFile(try defaultObjectPath(self.allocator, output_path), object_stage);
                try copyFile(output_path, js_stage);
                try copyFile(try replaceExtension(self.allocator, output_path, ".wasm"), wasm_stage);
                try publishStagedFileAtomic(object_stage, try self.join(objectName()));
                try publishStagedFileAtomic(js_stage, try self.join("main.js"));
                try publishStagedFileAtomic(wasm_stage, try self.join("main.wasm"));
            },
            .hybrid => {
                const bytecode_stage = try stage.join("main.kbc");
                const object_stage = try stage.join(objectName());
                const library_stage = try stage.join(sharedLibraryName());
                const manifest_stage = try stage.join("main.khm");

                try copyFile(try replaceExtension(self.allocator, output_path, ".kbc"), bytecode_stage);
                try copyFile(try replaceExtension(self.allocator, output_path, objectExtension()), object_stage);
                try copyFile(try replaceExtension(self.allocator, output_path, sharedLibraryExtension()), library_stage);

                var manifest = try hybrid.HybridModuleManifest.readFromFile(self.allocator, output_path);
                manifest.bytecode_path = try self.join("main.kbc");
                manifest.native_library_path = try self.join(sharedLibraryName());
                try manifest.writeToFile(manifest_stage);

                try publishStagedFileAtomic(bytecode_stage, try self.join("main.kbc"));
                try publishStagedFileAtomic(object_stage, try self.join(objectName()));
                try publishStagedFileAtomic(library_stage, try self.join(sharedLibraryName()));
                try publishStagedFileAtomic(manifest_stage, try self.join("main.khm"));
            },
        }

        try publishTextFileAtomic(try self.join(complete_marker_name), "ok\n");
    }

    fn restoreVm(self: Entry, output_path: []const u8) ![]build_def.Artifact {
        try copyFile(try self.join("main.kbc"), output_path);
        const artifacts = try self.allocator.alloc(build_def.Artifact, 1);
        artifacts[0] = .{ .kind = .bytecode, .path = try self.allocator.dupe(u8, output_path) };
        return artifacts;
    }

    fn restoreLlvm(self: Entry, output_path: []const u8) ![]build_def.Artifact {
        const object_path = try defaultObjectPath(self.allocator, output_path);
        try copyFile(try self.join(objectName()), object_path);
        try copyFile(try self.join(executableNamePage("main")), output_path);
        try makeExecutable(output_path);

        const artifacts = try self.allocator.alloc(build_def.Artifact, 2);
        artifacts[0] = .{ .kind = .native_object, .path = object_path };
        artifacts[1] = .{ .kind = .executable, .path = try self.allocator.dupe(u8, output_path) };
        return artifacts;
    }

    fn restoreWasm(self: Entry, output_path: []const u8) ![]build_def.Artifact {
        const object_path = try defaultObjectPath(self.allocator, output_path);
        const wasm_path = try replaceExtension(self.allocator, output_path, ".wasm");
        try copyFile(try self.join(objectName()), object_path);
        try copyFile(try self.join("main.js"), output_path);
        try copyFile(try self.join("main.wasm"), wasm_path);

        const artifacts = try self.allocator.alloc(build_def.Artifact, 3);
        artifacts[0] = .{ .kind = .native_object, .path = object_path };
        artifacts[1] = .{ .kind = .executable, .path = try self.allocator.dupe(u8, output_path) };
        artifacts[2] = .{ .kind = .executable, .path = wasm_path };
        return artifacts;
    }

    fn restoreHybrid(self: Entry, output_path: []const u8) ![]build_def.Artifact {
        const bytecode_path = try replaceExtension(self.allocator, output_path, ".kbc");
        const object_path = try replaceExtension(self.allocator, output_path, objectExtension());
        const library_path = try replaceExtension(self.allocator, output_path, sharedLibraryExtension());

        try copyFile(try self.join("main.kbc"), bytecode_path);
        try copyFile(try self.join(objectName()), object_path);
        try copyFile(try self.join(sharedLibraryName()), library_path);

        var manifest = try hybrid.HybridModuleManifest.readFromFile(self.allocator, try self.join("main.khm"));
        manifest.bytecode_path = bytecode_path;
        manifest.native_library_path = library_path;
        try manifest.writeToFile(output_path);

        const artifacts = try self.allocator.alloc(build_def.Artifact, 4);
        artifacts[0] = .{ .kind = .bytecode, .path = bytecode_path };
        artifacts[1] = .{ .kind = .hybrid_manifest, .path = try self.allocator.dupe(u8, output_path) };
        artifacts[2] = .{ .kind = .native_object, .path = object_path };
        artifacts[3] = .{ .kind = .native_library, .path = library_path };
        return artifacts;
    }

    fn join(self: Entry, name: []const u8) ![]const u8 {
        return std.fs.path.join(self.allocator, &.{ self.root_path, name });
    }
};

const StageDir = struct {
    allocator: std.mem.Allocator,
    path: []const u8,

    fn init(allocator: std.mem.Allocator, root_path: []const u8) !StageDir {
        var bytes: [8]u8 = undefined;
        std.Options.debug_io.random(&bytes);
        const suffix = std.mem.readInt(u64, &bytes, .little);
        const path = try std.fmt.allocPrint(allocator, "{s}{c}staging{c}{x}", .{ root_path, std.fs.path.sep, std.fs.path.sep, suffix });
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, path);
        return .{ .allocator = allocator, .path = path };
    }

    fn cleanup(self: *StageDir) void {
        deleteTreeAbsolute(self.path) catch {};
        self.allocator.free(self.path);
    }

    fn join(self: StageDir, name: []const u8) ![]const u8 {
        return std.fs.path.join(self.allocator, &.{ self.path, name });
    }
};

fn fingerprintBuild(allocator: std.mem.Allocator, source_path: []const u8, target_name: []const u8, native_deps_fingerprint: []const u8) ![]const u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update("kira-build-cache-v2\n");
    hasher.update(target_name);
    hasher.update("\n");
    // Native static libraries linked into the executable. Including their
    // fingerprint here means a change to native sources/headers (which produces
    // a new .a) invalidates the cached executable so it is relinked rather than
    // restored stale.
    hasher.update("native-deps=");
    hasher.update(native_deps_fingerprint);
    hasher.update("\n");
    const canonical_source = try absolutize(allocator, source_path);
    defer allocator.free(canonical_source);
    hasher.update("source=");
    hasher.update(canonical_source);
    hasher.update("\n");
    hasher.update(@tagName(builtin.cpu.arch));
    hasher.update("-");
    hasher.update(@tagName(builtin.os.tag));
    hasher.update("-");
    hasher.update(@tagName(builtin.abi));
    hasher.update("\n");
    try hashCompilerIdentity(allocator, &hasher);
    if (kira_toolchain.envVarOwned(allocator, "KIRA_LLVM_HOME")) |value| {
        defer allocator.free(value);
        hasher.update("KIRA_LLVM_HOME=");
        hasher.update(value);
        hasher.update("\n");
    } else |_| {}
    // Native codegen path marker. The LLVM C-API backend (with drop on) is the sole native
    // codegen path now that the text-IR writer is retired; bumping this string invalidates
    // caches produced by the old text-writer default.
    hasher.update("native-backend=capi-drop-v1\n");
    // Owned-value drop elaboration (KIRA_CAPI_DROP) changes the emitted code, so a
    // toggle must not reuse a binary built with the other setting.
    if (kira_toolchain.envVarOwned(allocator, "KIRA_CAPI_DROP")) |value| {
        defer allocator.free(value);
        hasher.update("KIRA_CAPI_DROP=");
        hasher.update(value);
        hasher.update("\n");
    } else |_| {}
    try hashRepoSupportInputs(allocator, &hasher);

    const files = try inputFiles(allocator, source_path);
    defer allocator.free(files);
    for (files) |path| {
        hasher.update(path);
        hasher.update("\n");
        const contents = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(64 * 1024 * 1024));
        defer allocator.free(contents);
        hasher.update(contents);
        hasher.update("\n");
    }

    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    return hexDigest(allocator, &digest);
}

fn hashRepoSupportInputs(allocator: std.mem.Allocator, hasher: anytype) !void {
    const helper_source = try std.fs.path.join(allocator, &.{ build_options.repo_root, "packages", "kira_native_bridge", "src", "runtime_helpers.c" });
    defer allocator.free(helper_source);
    try hashFileIfPresent(allocator, hasher, "native_bridge_runtime_helpers", helper_source);
}

fn hashFileIfPresent(allocator: std.mem.Allocator, hasher: anytype, label: []const u8, path: []const u8) !void {
    const canonical = absolutize(allocator, path) catch return;
    defer allocator.free(canonical);
    const contents = std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, canonical, allocator, .limited(64 * 1024 * 1024)) catch return;
    defer allocator.free(contents);
    hasher.update(label);
    hasher.update("=");
    hasher.update(canonical);
    hasher.update("\n");
    hasher.update(contents);
    hasher.update("\n");
}

fn hashCompilerIdentity(allocator: std.mem.Allocator, hasher: anytype) !void {
    const exe_path = std.process.executablePathAlloc(std.Options.debug_io, allocator) catch return;
    defer allocator.free(exe_path);
    const stat = if (std.fs.path.isAbsolute(exe_path))
        std.Io.Dir.cwd().statFile(std.Options.debug_io, exe_path, .{}) catch return
    else
        std.Io.Dir.cwd().statFile(std.Options.debug_io, exe_path, .{}) catch return;

    hasher.update("compiler=");
    hasher.update(exe_path);
    hasher.update("\n");
    var buffer: [128]u8 = undefined;
    const text = try std.fmt.bufPrint(&buffer, "size={d};mtime={d}\n", .{ stat.size, stat.mtime });
    hasher.update(text);
}

fn inputFiles(allocator: std.mem.Allocator, source_path: []const u8) ![]const []const u8 {
    var files = std.array_list.Managed([]const u8).init(allocator);
    const root = try projectRootForSource(allocator, source_path);
    defer allocator.free(root);
    var visited_roots = std.StringHashMap(void).init(allocator);
    try collectRootInputs(allocator, root, &visited_roots, &files);
    if (@import("kira_package_manager").loadModuleMapForSource(allocator, source_path)) |module_map| {
        for (module_map.owners) |owner| {
            const package_root = std.fs.path.dirname(owner.source_root) orelse owner.source_root;
            try collectRootInputs(allocator, package_root, &visited_roots, &files);
        }
    } else |_| {}
    if (files.items.len == 0) {
        try files.append(try absolutize(allocator, source_path));
    }
    sortStrings(files.items);
    return files.toOwnedSlice();
}

fn collectRootInputs(
    allocator: std.mem.Allocator,
    root: []const u8,
    visited_roots: *std.StringHashMap(void),
    files: *std.array_list.Managed([]const u8),
) !void {
    const canonical = try absolutize(allocator, root);
    if (visited_roots.contains(canonical)) {
        allocator.free(canonical);
        return;
    }
    try visited_roots.put(canonical, {});
    try collectInputs(allocator, canonical, "", files);
}

fn collectInputs(
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
            if (skipDirectory(entry.name)) continue;
            const child_rel = if (relative.len == 0) try allocator.dupe(u8, entry.name) else try std.fs.path.join(allocator, &.{ relative, entry.name });
            try collectInputs(allocator, root, child_rel, files);
            continue;
        }
        if (entry.kind != .file or !isInputFile(entry.name)) continue;
        const rel_path = if (relative.len == 0) try allocator.dupe(u8, entry.name) else try std.fs.path.join(allocator, &.{ relative, entry.name });
        const full_path = try std.fs.path.join(allocator, &.{ root, rel_path });
        try files.append(try absolutize(allocator, full_path));
    }
}

fn projectRootForSource(allocator: std.mem.Allocator, source_path: []const u8) ![]const u8 {
    const absolute = try absolutize(allocator, source_path);
    defer allocator.free(absolute);
    var current = try allocator.dupe(u8, std.fs.path.dirname(absolute) orelse ".");
    errdefer allocator.free(current);

    while (true) {
        if (hasProjectManifest(current) and sourceBelongsToProject(allocator, current, absolute)) return current;
        const parent = std.fs.path.dirname(current) orelse break;
        if (std.mem.eql(u8, parent, current)) break;
        const parent_copy = try allocator.dupe(u8, parent);
        allocator.free(current);
        current = parent_copy;
    }

    allocator.free(current);
    return allocator.dupe(u8, std.fs.path.dirname(absolute) orelse ".");
}

fn hasProjectManifest(path: []const u8) bool {
    return fileExistsAt(path, "kira.toml") or fileExistsAt(path, "project.toml") or fileExistsAt(path, "Kira.toml");
}

fn sourceBelongsToProject(allocator: std.mem.Allocator, root: []const u8, source_path: []const u8) bool {
    const app_root = std.fs.path.join(allocator, &.{ root, "app" }) catch return false;
    defer allocator.free(app_root);
    if (pathStartsWith(source_path, app_root)) return true;

    const bindings_root = std.fs.path.join(allocator, &.{ root, "bindings" }) catch return false;
    defer allocator.free(bindings_root);
    return pathStartsWith(source_path, bindings_root);
}

fn pathStartsWith(path: []const u8, prefix: []const u8) bool {
    if (!std.mem.startsWith(u8, path, prefix)) return false;
    if (path.len == prefix.len) return true;
    const next = path[prefix.len];
    return next == std.fs.path.sep or next == '/' or next == '\\';
}

fn skipDirectory(name: []const u8) bool {
    return std.mem.eql(u8, name, ".git") or
        std.mem.eql(u8, name, ".zig-cache") or
        std.mem.eql(u8, name, ".kira-build") or
        std.mem.eql(u8, name, ".kira") or
        std.mem.eql(u8, name, "zig-out") or
        std.mem.eql(u8, name, "generated");
}

fn isInputFile(name: []const u8) bool {
    const ext = std.fs.path.extension(name);
    return std.mem.eql(u8, ext, ".kira") or
        std.mem.eql(u8, ext, ".toml") or
        std.mem.eql(u8, ext, ".ksl") or
        std.mem.eql(u8, ext, ".c") or
        std.mem.eql(u8, ext, ".h") or
        std.mem.eql(u8, ext, ".m") or
        std.mem.eql(u8, ext, ".mm");
}

fn sortStrings(items: [][]const u8) void {
    std.mem.sort([]const u8, items, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.lessThan(u8, lhs, rhs);
        }
    }.lessThan);
}

fn copyFile(source_path: []const u8, destination_path: []const u8) !void {
    const data = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, source_path, std.heap.page_allocator, .limited(256 * 1024 * 1024));
    defer std.heap.page_allocator.free(data);
    try ensureParentDir(destination_path);
    const file = if (std.fs.path.isAbsolute(destination_path))
        try std.Io.Dir.createFileAbsolute(std.Options.debug_io, destination_path, .{ .truncate = true })
    else
        try std.Io.Dir.cwd().createFile(std.Options.debug_io, destination_path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn makeExecutable(path: []const u8) !void {
    if (!std.Io.File.Permissions.has_executable_bit) return;

    const permissions: std.Io.File.Permissions = @enumFromInt(0o755);
    if (std.fs.path.isAbsolute(path)) {
        const parent_path = std.fs.path.dirname(path) orelse return error.InvalidCachePath;
        const base_name = std.fs.path.basename(path);
        var parent_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, parent_path, .{});
        defer parent_dir.close(std.Options.debug_io);
        try parent_dir.setFilePermissions(std.Options.debug_io, base_name, permissions, .{});
        return;
    }

    try std.Io.Dir.cwd().setFilePermissions(std.Options.debug_io, path, permissions, .{});
}

fn publishStagedFileAtomic(source_path: []const u8, destination_path: []const u8) !void {
    const data = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, source_path, std.heap.page_allocator, .limited(256 * 1024 * 1024));
    defer std.heap.page_allocator.free(data);
    try publishFileAtomic(destination_path, data);
}

fn publishTextFileAtomic(path: []const u8, data: []const u8) !void {
    try publishFileAtomic(path, data);
}

fn publishFileAtomic(path: []const u8, data: []const u8) !void {
    try ensureParentDir(path);
    const parent_path = std.fs.path.dirname(path) orelse return error.InvalidCachePath;
    const base_name = std.fs.path.basename(path);
    var parent_dir = if (std.fs.path.isAbsolute(parent_path))
        try std.Io.Dir.openDirAbsolute(std.Options.debug_io, parent_path, .{})
    else
        try std.Io.Dir.cwd().openDir(std.Options.debug_io, parent_path, .{});
    defer parent_dir.close(std.Options.debug_io);

    var atomic_file = try parent_dir.createFileAtomic(std.Options.debug_io, base_name, .{
        .replace = false,
        .make_path = false,
    });
    defer atomic_file.deinit(std.Options.debug_io);
    try atomic_file.file.writeStreamingAll(std.Options.debug_io, data);
    atomic_file.link(std.Options.debug_io) catch |err| switch (err) {
        error.PathAlreadyExists => {},
        else => return err,
    };
}

fn ensureParentDir(path: []const u8) !void {
    const dir = std.fs.path.dirname(path) orelse return;
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, dir);
}

fn deleteTreeAbsolute(path: []const u8) !void {
    const parent_path = std.fs.path.dirname(path) orelse return;
    const base_name = std.fs.path.basename(path);
    var parent_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, parent_path, .{});
    defer parent_dir.close(std.Options.debug_io);
    try parent_dir.deleteTree(std.Options.debug_io, base_name);
}

fn fileExistsAt(root: []const u8, name: []const u8) bool {
    const path = std.fs.path.join(std.heap.page_allocator, &.{ root, name }) catch return false;
    defer std.heap.page_allocator.free(path);
    return fileExists(path);
}

fn fileExistsJoin(root: []const u8, name: []const u8) bool {
    const path = std.fs.path.join(std.heap.page_allocator, &.{ root, name }) catch return false;
    defer std.heap.page_allocator.free(path);
    return fileExists(path);
}

fn fileExistsNonEmptyJoin(root: []const u8, name: []const u8) bool {
    const path = std.fs.path.join(std.heap.page_allocator, &.{ root, name }) catch return false;
    defer std.heap.page_allocator.free(path);
    return fileExistsNonEmpty(path);
}

fn fileExists(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn fileExistsNonEmpty(path: []const u8) bool {
    var file = if (std.fs.path.isAbsolute(path))
        std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false
    else
        std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    defer file.close(std.Options.debug_io);
    const stat = file.stat(std.Options.debug_io) catch return false;
    return stat.size != 0;
}

fn absolutize(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(path)) return allocator.dupe(u8, path);
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, path, allocator);
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

fn backendName(target: build_def.ExecutionTarget) []const u8 {
    return switch (target) {
        .vm => "vm",
        .llvm_native => "llvm",
        .wasm32_emscripten => "wasm32-emscripten",
        .hybrid => "hybrid",
    };
}

fn objectName() []const u8 {
    return if (builtin.os.tag == .windows) "main.obj" else "main.o";
}

fn objectExtension() []const u8 {
    return if (builtin.os.tag == .windows) ".obj" else ".o";
}

fn executableNamePage(comptime base: []const u8) []const u8 {
    return if (builtin.os.tag == .windows) base ++ ".exe" else base;
}

fn executableName(allocator: std.mem.Allocator, base: []const u8) ![]const u8 {
    return if (builtin.os.tag == .windows) std.fmt.allocPrint(allocator, "{s}.exe", .{base}) else allocator.dupe(u8, base);
}

fn sharedLibraryName() []const u8 {
    return switch (builtin.os.tag) {
        .windows => "main.dll",
        .macos => "main.dylib",
        else => "main.so",
    };
}

fn sharedLibraryExtension() []const u8 {
    return switch (builtin.os.tag) {
        .windows => ".dll",
        .macos => ".dylib",
        else => ".so",
    };
}

fn defaultObjectPath(allocator: std.mem.Allocator, executable_path: []const u8) ![]const u8 {
    const ext = if (builtin.os.tag == .windows) ".exe" else "";
    if (ext.len > 0 and std.mem.endsWith(u8, executable_path, ext)) {
        return std.fmt.allocPrint(allocator, "{s}{s}", .{ executable_path[0 .. executable_path.len - ext.len], objectExtension() });
    }
    return std.fmt.allocPrint(allocator, "{s}{s}", .{ executable_path, objectExtension() });
}

fn replaceExtension(allocator: std.mem.Allocator, path: []const u8, extension: []const u8) ![]const u8 {
    const ext = std.fs.path.extension(path);
    if (ext.len == 0) return std.fmt.allocPrint(allocator, "{s}{s}", .{ path, extension });
    return std.fmt.allocPrint(allocator, "{s}{s}", .{ path[0 .. path.len - ext.len], extension });
}
