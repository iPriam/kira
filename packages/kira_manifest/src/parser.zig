const std = @import("std");
const native = @import("kira_native_lib_definition");
const dependency = @import("dependency.zig");
const LockFile = @import("lockfile.zig").LockFile;
const ProjectManifest = @import("project_manifest.zig").ProjectManifest;
const PackageKind = @import("project_manifest.zig").PackageKind;
const PackageManifest = @import("package_manifest.zig").PackageManifest;
const NativeLibManifest = @import("native_lib_manifest.zig").NativeLibManifest;
const platform_config = @import("platform_config.zig");

pub fn parseProjectManifest(allocator: std.mem.Allocator, text: []const u8) !ProjectManifest {
    var name: []const u8 = "";
    var version: []const u8 = "0.1.0";
    var kind: PackageKind = .app;
    var kira_version: []const u8 = "0.1.0";
    var module_root: ?[]const u8 = null;
    var native_libraries = std.array_list.Managed([]const u8).init(allocator);
    var execution_mode: []const u8 = "vm";
    var build_target: []const u8 = "host";
    var registry_url: ?[]const u8 = null;
    var registry_token_env: ?[]const u8 = null;
    var packages = std.array_list.Managed([]const u8).init(allocator);
    var dependencies = std.array_list.Managed(dependency.DependencySpec).init(allocator);
    var execution_default_backend: platform_config.ExecutionBackend = .vm;
    var execution_default_source: platform_config.BackendSelectionSource = .platform_default;
    var execution_hybrid_selection: platform_config.HybridSelectionMode = .annotation_driven;
    var execution_libraries = std.array_list.Managed(platform_config.LibraryExecutionPolicy).init(allocator);
    var execution_web = platform_config.WebExecutionPolicy{};
    var section: []const u8 = "";
    var pending_array: ?enum {
        packages,
        native_libraries,
    } = null;

    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |raw_line| {
        const line = trimComment(raw_line);
        if (line.len == 0) continue;
        if (pending_array) |pending_kind| {
            const finished = switch (pending_kind) {
                .packages => try appendStringArrayContinuation(allocator, line, &packages),
                .native_libraries => try appendStringArrayContinuation(allocator, line, &native_libraries),
            };
            if (finished) pending_array = null;
            continue;
        }
        if (isSectionHeader(line)) {
            section = line[1 .. line.len - 1];
            try platform_config.validateProfileSection(section);
            continue;
        }

        const kv = try splitKeyValue(line);
        if (std.mem.eql(u8, kv.key, "packages")) {
            if (!try appendStringArrayValue(allocator, kv.value, &packages)) pending_array = .packages;
            continue;
        }
        if (std.mem.eql(u8, kv.key, "native_libraries")) {
            if (!try appendStringArrayValue(allocator, kv.value, &native_libraries)) pending_array = .native_libraries;
            continue;
        }

        if (std.mem.eql(u8, section, "project") or std.mem.eql(u8, section, "package")) {
            if (std.mem.eql(u8, kv.key, "name")) name = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "version")) version = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "kind")) kind = try parsePackageKind(kv.value);
            if (std.mem.eql(u8, kv.key, "kira")) kira_version = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "module_root")) module_root = try parseOwnedString(allocator, kv.value);
            continue;
        }

        if (std.mem.eql(u8, section, "defaults")) {
            if (std.mem.eql(u8, kv.key, "execution_mode")) execution_mode = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "build_target")) build_target = try parseOwnedString(allocator, kv.value);
            continue;
        }

        if (std.mem.eql(u8, section, "execution")) {
            if (std.mem.eql(u8, kv.key, "default_backend")) {
                const parsed_backend = platform_config.ExecutionBackend.parse(try parseBorrowedString(kv.value)) orelse return error.InvalidManifest;
                execution_default_backend = parsed_backend;
                execution_default_source = .app_manifest;
                execution_mode = try allocator.dupe(u8, parsed_backend.label());
            }
            continue;
        }

        if (std.mem.eql(u8, section, "execution.hybrid")) {
            if (std.mem.eql(u8, kv.key, "selection")) {
                execution_hybrid_selection = platform_config.HybridSelectionMode.parse(try parseBorrowedString(kv.value)) orelse return error.InvalidManifest;
            }
            continue;
        }

        if (std.mem.eql(u8, section, "execution.web")) {
            if (std.mem.eql(u8, kv.key, "backend")) {
                execution_web.backend = platform_config.ExecutionBackend.parse(try parseBorrowedString(kv.value)) orelse return error.InvalidManifest;
            }
            if (std.mem.eql(u8, kv.key, "graphics_bridge")) {
                execution_web.graphics_bridge = platform_config.WebGraphicsBridge.parse(try parseBorrowedString(kv.value)) orelse return error.InvalidManifest;
            }
            continue;
        }

        if (parseExecutionLibrarySection(section)) |library_name| {
            const policy = try ensureExecutionLibraryPolicy(allocator, &execution_libraries, library_name);
            if (std.mem.eql(u8, kv.key, "backend")) {
                policy.backend = platform_config.ExecutionBackend.parse(try parseBorrowedString(kv.value)) orelse return error.InvalidManifest;
            }
            if (std.mem.eql(u8, kv.key, "native_required")) {
                policy.native_required = try parseBool(kv.value);
            }
            if (std.mem.eql(u8, kv.key, "ffi_allowed")) {
                policy.ffi_allowed = try parseBool(kv.value);
            }
            if (std.mem.eql(u8, kv.key, "hybrid_selection")) {
                policy.hybrid_selection = platform_config.HybridSelectionMode.parse(try parseBorrowedString(kv.value)) orelse return error.InvalidManifest;
            }
            continue;
        }

        if (std.mem.eql(u8, section, "registry")) {
            if (std.mem.eql(u8, kv.key, "url")) registry_url = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "token_env")) registry_token_env = try parseOwnedString(allocator, kv.value);
            continue;
        }

        if (std.mem.eql(u8, section, "dependencies")) {
            try appendDependency(allocator, &dependencies, kv.key, kv.value);
            continue;
        }
    }

    return .{
        .name = name,
        .version = version,
        .kind = kind,
        .kira_version = kira_version,
        .module_root = module_root,
        .native_libraries = try native_libraries.toOwnedSlice(),
        .dependencies = try dependencies.toOwnedSlice(),
        .packages = try packages.toOwnedSlice(),
        .execution_mode = execution_mode,
        .execution_policy = .{
            .default_backend = execution_default_backend,
            .default_source = execution_default_source,
            .hybrid_selection = execution_hybrid_selection,
            .libraries = try execution_libraries.toOwnedSlice(),
            .web = execution_web,
        },
        .build_target = build_target,
        .registry_url = registry_url,
        .registry_token_env = registry_token_env,
    };
}

fn parseExecutionLibrarySection(section: []const u8) ?[]const u8 {
    const prefix = "execution.libraries.";
    if (!std.mem.startsWith(u8, section, prefix)) return null;
    const raw = section[prefix.len..];
    if (raw.len < 2 or raw[0] != '"' or raw[raw.len - 1] != '"') return null;
    return raw[1 .. raw.len - 1];
}

fn ensureExecutionLibraryPolicy(
    allocator: std.mem.Allocator,
    list: *std.array_list.Managed(platform_config.LibraryExecutionPolicy),
    package: []const u8,
) !*platform_config.LibraryExecutionPolicy {
    for (list.items) |*item| {
        if (std.mem.eql(u8, item.package, package)) return item;
    }
    try list.append(.{
        .package = try allocator.dupe(u8, package),
        .backend = .hybrid,
        .source = .app_manifest,
    });
    return &list.items[list.items.len - 1];
}

pub fn parsePackageManifest(allocator: std.mem.Allocator, text: []const u8) !PackageManifest {
    const project = try parseProjectManifest(allocator, text);
    return .{
        .name = project.name,
        .version = project.version,
        .kind = if (project.kind == .app) .library else project.kind,
        .kira_version = project.kira_version,
        .module_root = project.module_root,
        .dependencies = project.dependencies,
    };
}

pub fn parseLockFile(allocator: std.mem.Allocator, text: []const u8) !LockFile {
    var schema_version: u32 = 1;
    var root = LockFile.Root{};
    var packages = std.array_list.Managed(LockFile.LockedPackage).init(allocator);
    var root_dependencies = std.array_list.Managed(LockFile.RootDependency).init(allocator);

    const Context = enum {
        top,
        root,
        root_dependency,
        package,
    };
    var context: Context = .top;

    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |raw_line| {
        const line = trimComment(raw_line);
        if (line.len == 0) continue;

        if (std.mem.eql(u8, line, "[root]")) {
            context = .root;
            continue;
        }
        if (std.mem.eql(u8, line, "[[root_dependency]]")) {
            context = .root_dependency;
            try root_dependencies.append(.{
                .name = "",
                .source = .{ .registry = .{ .version = "" } },
            });
            continue;
        }
        if (std.mem.eql(u8, line, "[[package]]")) {
            context = .package;
            try packages.append(.{
                .name = "",
                .module_root = "",
                .source = .{ .path = .{ .path = "" } },
            });
            continue;
        }

        const kv = try splitKeyValue(line);
        switch (context) {
            .top => {
                if (std.mem.eql(u8, kv.key, "version")) {
                    schema_version = try std.fmt.parseInt(u32, kv.value, 10);
                }
            },
            .root => {
                if (std.mem.eql(u8, kv.key, "name")) root.name = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "version")) root.version = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "kind")) root.kind = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "kira")) root.kira_version = try parseOwnedString(allocator, kv.value);
            },
            .root_dependency => {
                if (root_dependencies.items.len == 0) return error.InvalidManifest;
                const item = &root_dependencies.items[root_dependencies.items.len - 1];
                try applyRootDependencyField(allocator, item, kv);
            },
            .package => {
                if (packages.items.len == 0) return error.InvalidManifest;
                const item = &packages.items[packages.items.len - 1];
                try applyLockedPackageField(allocator, item, kv);
            },
        }
    }

    root.dependencies = try root_dependencies.toOwnedSlice();
    return .{
        .schema_version = schema_version,
        .root = root,
        .packages = try packages.toOwnedSlice(),
    };
}

pub fn writeProjectManifest(writer: anytype, manifest: ProjectManifest) !void {
    try writer.writeAll("[package]\n");
    try writer.print("name = \"{s}\"\n", .{manifest.name});
    try writer.print("version = \"{s}\"\n", .{manifest.version});
    try writer.print("kind = \"{s}\"\n", .{@tagName(manifest.kind)});
    try writer.print("kira = \"{s}\"\n", .{manifest.kira_version});
    if (manifest.module_root) |module_root| {
        try writer.print("module_root = \"{s}\"\n", .{module_root});
    }
    if (manifest.native_libraries.len > 0) {
        try writer.writeAll("native_libraries = [");
        for (manifest.native_libraries, 0..) |path, index| {
            if (index != 0) try writer.writeAll(", ");
            try writer.print("\"{s}\"", .{path});
        }
        try writer.writeAll("]\n");
    }

    try writer.writeAll("\n[defaults]\n");
    try writer.print("execution_mode = \"{s}\"\n", .{manifest.execution_mode});
    try writer.print("build_target = \"{s}\"\n", .{manifest.build_target});

    if (manifest.registry_url != null or manifest.registry_token_env != null) {
        try writer.writeAll("\n[registry]\n");
        if (manifest.registry_url) |url| try writer.print("url = \"{s}\"\n", .{url});
        if (manifest.registry_token_env) |token_env| try writer.print("token_env = \"{s}\"\n", .{token_env});
    }

    if (manifest.dependencies.len > 0) {
        const sorted = try cloneAndSortDependencies(std.heap.page_allocator, manifest.dependencies);
        defer std.heap.page_allocator.free(sorted);

        try writer.writeAll("\n[dependencies]\n");
        for (sorted) |item| {
            try writer.print("{s} = ", .{item.name});
            switch (item.source) {
                .registry => |registry| try writer.print("\"{s}\"\n", .{registry.version}),
                .path => |path| try writer.print("{{ path = \"{s}\" }}\n", .{path.path}),
                .git => |git| {
                    try writer.print("{{ git = \"{s}\"", .{git.url});
                    if (git.rev) |rev| try writer.print(", rev = \"{s}\"", .{rev});
                    if (git.tag) |tag| try writer.print(", tag = \"{s}\"", .{tag});
                    try writer.writeAll(" }\n");
                },
            }
        }
    }
}

pub fn writeLockFile(writer: anytype, lockfile: LockFile) !void {
    try writer.print("version = {d}\n", .{lockfile.schema_version});
    try writer.writeAll("\n[root]\n");
    try writer.print("name = \"{s}\"\n", .{lockfile.root.name});
    try writer.print("version = \"{s}\"\n", .{lockfile.root.version});
    try writer.print("kind = \"{s}\"\n", .{lockfile.root.kind});
    try writer.print("kira = \"{s}\"\n", .{lockfile.root.kira_version});

    const root_dependencies = try cloneAndSortRootDependencies(std.heap.page_allocator, lockfile.root.dependencies);
    defer std.heap.page_allocator.free(root_dependencies);
    for (root_dependencies) |item| {
        try writer.writeAll("\n[[root_dependency]]\n");
        try writer.print("name = \"{s}\"\n", .{item.name});
        switch (item.source) {
            .registry => |registry| {
                try writer.writeAll("source = \"registry\"\n");
                try writer.print("version = \"{s}\"\n", .{registry.version});
            },
            .path => |path| {
                try writer.writeAll("source = \"path\"\n");
                try writer.print("path = \"{s}\"\n", .{path.path});
            },
            .git => |git| {
                try writer.writeAll("source = \"git\"\n");
                try writer.print("git = \"{s}\"\n", .{git.url});
                if (git.rev) |rev| try writer.print("rev = \"{s}\"\n", .{rev});
                if (git.tag) |tag| try writer.print("tag = \"{s}\"\n", .{tag});
            },
        }
    }

    const packages = try cloneAndSortLockedPackages(std.heap.page_allocator, lockfile.packages);
    defer std.heap.page_allocator.free(packages);
    for (packages) |item| {
        try writer.writeAll("\n[[package]]\n");
        try writer.print("name = \"{s}\"\n", .{item.name});
        if (item.version.len > 0) try writer.print("version = \"{s}\"\n", .{item.version});
        try writer.print("kind = \"{s}\"\n", .{item.kind});
        try writer.print("kira = \"{s}\"\n", .{item.kira_version});
        try writer.print("module_root = \"{s}\"\n", .{item.module_root});
        switch (item.source) {
            .registry => |registry| {
                try writer.writeAll("source = \"registry\"\n");
                try writer.print("registry_url = \"{s}\"\n", .{registry.registry_url});
                try writer.print("archive_path = \"{s}\"\n", .{registry.archive_path});
                try writer.print("checksum = \"{s}\"\n", .{registry.checksum});
            },
            .path => |path| {
                try writer.writeAll("source = \"path\"\n");
                try writer.print("path = \"{s}\"\n", .{path.path});
            },
            .git => |git| {
                try writer.writeAll("source = \"git\"\n");
                try writer.print("git = \"{s}\"\n", .{git.url});
                try writer.print("commit = \"{s}\"\n", .{git.commit});
                if (git.requested_rev) |rev| try writer.print("requested_rev = \"{s}\"\n", .{rev});
                if (git.requested_tag) |tag| try writer.print("requested_tag = \"{s}\"\n", .{tag});
            },
        }

        if (item.dependencies.len > 0) {
            const deps = try cloneAndSortStrings(std.heap.page_allocator, item.dependencies);
            defer std.heap.page_allocator.free(deps);
            try writer.writeAll("dependencies = [");
            for (deps, 0..) |dep_name, index| {
                if (index != 0) try writer.writeAll(", ");
                try writer.print("\"{s}\"", .{dep_name});
            }
            try writer.writeAll("]\n");
        }
    }
}

pub fn parseNativeLibManifest(allocator: std.mem.Allocator, text: []const u8) !NativeLibManifest {
    var section: []const u8 = "";
    var target_name: ?[]const u8 = null;

    var library_name: []const u8 = "";
    var link_mode: native.LinkMode = .static;
    var abi: native.LibraryAbi = .c;
    var headers = native.HeaderSpec{};
    var autobinding_module_name: ?[]const u8 = null;
    var autobinding_output_path: ?[]const u8 = null;
    var autobinding_headers: []const []const u8 = &.{};
    var autobinding_bindings = native.AutobindingBindings{};
    var build = native.BuildRecipe{};
    var targets = std.array_list.Managed(native.TargetSpec).init(allocator);

    var lines = std.mem.splitScalar(u8, text, '\n');
    while (lines.next()) |raw_line| {
        const line = trimComment(raw_line);
        if (line.len == 0) continue;
        if (line[0] == '[' and line[line.len - 1] == ']') {
            section = line[1 .. line.len - 1];
            if (std.mem.startsWith(u8, section, "target.")) {
                target_name = section["target.".len..];
                const selector = try native.TargetSelector.parse(allocator, target_name.?);
                try targets.append(.{ .selector = selector });
            } else {
                target_name = null;
            }
            continue;
        }

        if (std.mem.eql(u8, section, "library")) {
            if (assignString(line, "name")) |value| library_name = try allocator.dupe(u8, value);
            if (assignString(line, "link_mode")) |value| link_mode = parseLinkMode(value);
            if (assignString(line, "abi")) |value| abi = parseAbi(value);
        } else if (std.mem.eql(u8, section, "headers")) {
            if (assignString(line, "entrypoint")) |value| headers.entrypoint = try allocator.dupe(u8, value);
            if (std.mem.startsWith(u8, line, "include_dirs")) headers.include_dirs = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "defines")) headers.defines = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "frameworks")) headers.frameworks = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "system_libs")) headers.system_libs = try parseStringArray(allocator, (try splitKeyValue(line)).value);
        } else if (std.mem.eql(u8, section, "autobinding")) {
            if (assignString(line, "module")) |value| autobinding_module_name = try allocator.dupe(u8, value);
            if (assignString(line, "output")) |value| autobinding_output_path = try allocator.dupe(u8, value);
            if (std.mem.startsWith(u8, line, "headers")) autobinding_headers = try parseStringArray(allocator, (try splitKeyValue(line)).value);
        } else if (std.mem.eql(u8, section, "bindings")) {
            if (assignString(line, "mode")) |value| {
                autobinding_bindings.mode = if (std.mem.eql(u8, value, "all_public")) .all_public else .listed;
            }
            if (assignString(line, "profile")) |value| autobinding_bindings.profile = parseAutobindingProfile(value);
            if (std.mem.startsWith(u8, line, "functions")) autobinding_bindings.functions = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "structs")) autobinding_bindings.structs = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "callbacks")) autobinding_bindings.callbacks = try parseStringArray(allocator, (try splitKeyValue(line)).value);
        } else if (std.mem.eql(u8, section, "build")) {
            if (std.mem.startsWith(u8, line, "sources")) build.sources = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "include_dirs")) build.include_dirs = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "defines")) build.defines = try parseStringArray(allocator, (try splitKeyValue(line)).value);
        } else if (target_name != null and targets.items.len > 0) {
            var current = &targets.items[targets.items.len - 1];
            if (assignString(line, "static_lib")) |value| current.static_lib = try allocator.dupe(u8, value);
            if (assignString(line, "dynamic_lib")) |value| current.dynamic_lib = try allocator.dupe(u8, value);
            if (std.mem.startsWith(u8, line, "frameworks")) current.link.frameworks = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "system_libs")) current.link.system_libs = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "include_dirs")) current.link.include_dirs = try parseStringArray(allocator, (try splitKeyValue(line)).value);
            if (std.mem.startsWith(u8, line, "defines")) current.link.defines = try parseStringArray(allocator, (try splitKeyValue(line)).value);
        }
    }

    return .{
        .library = .{
            .name = library_name,
            .link_mode = link_mode,
            .abi = abi,
            .headers = headers,
            .autobinding = if (autobinding_module_name != null and autobinding_output_path != null) .{
                .module_name = autobinding_module_name.?,
                .output_path = autobinding_output_path.?,
                .headers = autobinding_headers,
                .bindings = autobinding_bindings,
            } else null,
            .build = build,
            .targets = try targets.toOwnedSlice(),
        },
    };
}

fn applyRootDependencyField(
    allocator: std.mem.Allocator,
    item: *LockFile.RootDependency,
    kv: KeyValue,
) !void {
    if (std.mem.eql(u8, kv.key, "name")) {
        item.name = try parseOwnedString(allocator, kv.value);
        return;
    }

    if (std.mem.eql(u8, kv.key, "source")) {
        const source_kind = try parseOwnedString(allocator, kv.value);
        if (std.mem.eql(u8, source_kind, "registry")) {
            item.source = .{ .registry = .{ .version = "" } };
        } else if (std.mem.eql(u8, source_kind, "path")) {
            item.source = .{ .path = .{ .path = "" } };
        } else if (std.mem.eql(u8, source_kind, "git")) {
            item.source = .{ .git = .{ .url = "" } };
        } else {
            return error.InvalidManifest;
        }
        return;
    }

    switch (item.source) {
        .registry => |*registry| {
            if (std.mem.eql(u8, kv.key, "version")) registry.version = try parseOwnedString(allocator, kv.value);
        },
        .path => |*path| {
            if (std.mem.eql(u8, kv.key, "path")) path.path = try parseOwnedString(allocator, kv.value);
        },
        .git => |*git| {
            if (std.mem.eql(u8, kv.key, "git")) git.url = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "rev")) git.rev = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "tag")) git.tag = try parseOwnedString(allocator, kv.value);
        },
    }
}

fn applyLockedPackageField(
    allocator: std.mem.Allocator,
    item: *LockFile.LockedPackage,
    kv: KeyValue,
) !void {
    if (std.mem.eql(u8, kv.key, "name")) item.name = try parseOwnedString(allocator, kv.value);
    if (std.mem.eql(u8, kv.key, "version")) item.version = try parseOwnedString(allocator, kv.value);
    if (std.mem.eql(u8, kv.key, "kind")) item.kind = try parseOwnedString(allocator, kv.value);
    if (std.mem.eql(u8, kv.key, "kira")) item.kira_version = try parseOwnedString(allocator, kv.value);
    if (std.mem.eql(u8, kv.key, "module_root")) item.module_root = try parseOwnedString(allocator, kv.value);
    if (std.mem.eql(u8, kv.key, "dependencies")) item.dependencies = try parseStringArray(allocator, kv.value);

    if (std.mem.eql(u8, kv.key, "source")) {
        const source_kind = try parseOwnedString(allocator, kv.value);
        if (std.mem.eql(u8, source_kind, "registry")) {
            item.source = .{ .registry = .{
                .registry_url = "",
                .archive_path = "",
                .checksum = "",
            } };
        } else if (std.mem.eql(u8, source_kind, "path")) {
            item.source = .{ .path = .{ .path = "" } };
        } else if (std.mem.eql(u8, source_kind, "git")) {
            item.source = .{ .git = .{
                .url = "",
                .commit = "",
            } };
        } else {
            return error.InvalidManifest;
        }
        return;
    }

    switch (item.source) {
        .registry => |*registry| {
            if (std.mem.eql(u8, kv.key, "registry_url")) registry.registry_url = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "archive_path")) registry.archive_path = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "checksum")) registry.checksum = try parseOwnedString(allocator, kv.value);
        },
        .path => |*path| {
            if (std.mem.eql(u8, kv.key, "path")) path.path = try parseOwnedString(allocator, kv.value);
        },
        .git => |*git| {
            if (std.mem.eql(u8, kv.key, "git")) git.url = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "commit")) git.commit = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "requested_rev")) git.requested_rev = try parseOwnedString(allocator, kv.value);
            if (std.mem.eql(u8, kv.key, "requested_tag")) git.requested_tag = try parseOwnedString(allocator, kv.value);
        },
    }
}

fn appendDependency(
    allocator: std.mem.Allocator,
    list: *std.array_list.Managed(dependency.DependencySpec),
    name: []const u8,
    value: []const u8,
) !void {
    for (list.items) |item| {
        if (std.mem.eql(u8, item.name, name)) return error.InvalidManifest;
    }
    try list.append(try parseDependencySpec(allocator, name, value));
}

fn parseDependencySpec(allocator: std.mem.Allocator, name: []const u8, value: []const u8) !dependency.DependencySpec {
    if (value.len == 0) return error.InvalidManifest;
    if (value[0] == '"') {
        const version = try parseOwnedString(allocator, value);
        try validateExactVersion(version);
        return .{
            .name = try allocator.dupe(u8, name),
            .source = .{ .registry = .{ .version = version } },
        };
    }

    const fields = try parseInlineTable(allocator, value);
    defer allocator.free(fields);

    var version: ?[]const u8 = null;
    var path: ?[]const u8 = null;
    var git: ?[]const u8 = null;
    var rev: ?[]const u8 = null;
    var tag: ?[]const u8 = null;
    var branch_seen = false;

    for (fields) |field| {
        if (std.mem.eql(u8, field.key, "version")) version = try allocator.dupe(u8, field.value);
        if (std.mem.eql(u8, field.key, "path")) path = try allocator.dupe(u8, field.value);
        if (std.mem.eql(u8, field.key, "git")) git = try allocator.dupe(u8, field.value);
        if (std.mem.eql(u8, field.key, "rev")) rev = try allocator.dupe(u8, field.value);
        if (std.mem.eql(u8, field.key, "tag")) tag = try allocator.dupe(u8, field.value);
        if (std.mem.eql(u8, field.key, "branch")) branch_seen = true;
    }

    if (branch_seen) return error.UnsupportedVersionRange;
    if (path != null) {
        if (version != null or git != null) return error.InvalidManifest;
        return .{
            .name = try allocator.dupe(u8, name),
            .source = .{ .path = .{ .path = path.? } },
        };
    }
    if (git != null) {
        if (version != null) return error.InvalidManifest;
        if (rev == null and tag == null) return error.InvalidManifest;
        return .{
            .name = try allocator.dupe(u8, name),
            .source = .{ .git = .{
                .url = git.?,
                .rev = rev,
                .tag = tag,
            } },
        };
    }
    if (version != null) {
        try validateExactVersion(version.?);
        return .{
            .name = try allocator.dupe(u8, name),
            .source = .{ .registry = .{ .version = version.? } },
        };
    }
    return error.InvalidManifest;
}

fn validateExactVersion(version: []const u8) !void {
    if (version.len == 0) return error.InvalidManifest;
    if (std.mem.indexOfAny(u8, version, "^~*<>, ")) |_| return error.UnsupportedVersionRange;
}

fn parsePackageKind(value: []const u8) !PackageKind {
    const text = try parseBorrowedString(value);
    if (std.mem.eql(u8, text, "app")) return .app;
    if (std.mem.eql(u8, text, "library")) return .library;
    return error.InvalidManifest;
}

fn parseBool(value: []const u8) !bool {
    if (std.mem.eql(u8, value, "true")) return true;
    if (std.mem.eql(u8, value, "false")) return false;
    return error.InvalidManifest;
}

fn trimComment(line: []const u8) []const u8 {
    var in_string = false;
    var index: usize = 0;
    while (index < line.len) : (index += 1) {
        const ch = line[index];
        if (ch == '"') in_string = !in_string;
        if (ch == '#' and !in_string) break;
    }
    return std.mem.trim(u8, line[0..index], " \t\r");
}

fn isSectionHeader(line: []const u8) bool {
    return line.len >= 3 and line[0] == '[' and line[line.len - 1] == ']';
}

const KeyValue = struct {
    key: []const u8,
    value: []const u8,
};

fn splitKeyValue(line: []const u8) !KeyValue {
    const equal_index = std.mem.indexOfScalar(u8, line, '=') orelse return error.InvalidManifest;
    return .{
        .key = std.mem.trim(u8, line[0..equal_index], " \t"),
        .value = std.mem.trim(u8, line[equal_index + 1 ..], " \t"),
    };
}

fn assignString(line: []const u8, key: []const u8) ?[]const u8 {
    const kv = splitKeyValue(line) catch return null;
    if (!std.mem.eql(u8, kv.key, key)) return null;
    return parseBorrowedString(kv.value) catch null;
}

fn parseOwnedString(allocator: std.mem.Allocator, value: []const u8) ![]const u8 {
    return allocator.dupe(u8, try parseBorrowedString(value));
}

fn parseBorrowedString(value: []const u8) ![]const u8 {
    if (value.len < 2 or value[0] != '"' or value[value.len - 1] != '"') return error.InvalidManifest;
    return value[1 .. value.len - 1];
}

fn parseStringArray(allocator: std.mem.Allocator, value: []const u8) ![]const []const u8 {
    if (value.len < 2 or value[0] != '[' or value[value.len - 1] != ']') return error.InvalidManifest;
    const body = value[1 .. value.len - 1];
    var items = std.array_list.Managed([]const u8).init(allocator);
    var start: usize = 0;
    var in_string = false;
    for (body, 0..) |ch, index| {
        if (ch == '"') in_string = !in_string;
        if (ch == ',' and !in_string) {
            const part = std.mem.trim(u8, body[start..index], " \t");
            if (part.len > 0) try items.append(try parseOwnedString(allocator, part));
            start = index + 1;
        }
    }
    const trailing = std.mem.trim(u8, body[start..], " \t");
    if (trailing.len > 0) try items.append(try parseOwnedString(allocator, trailing));
    return items.toOwnedSlice();
}

fn appendStringArrayValue(
    allocator: std.mem.Allocator,
    value: []const u8,
    list: *std.array_list.Managed([]const u8),
) !bool {
    if (value.len < 1 or value[0] != '[') return error.InvalidManifest;
    if (value.len >= 2 and value[value.len - 1] == ']') {
        const parsed = try parseStringArray(allocator, value);
        try list.appendSlice(parsed);
        return true;
    }

    try appendStringArrayFragment(allocator, value[1..], list);
    return false;
}

fn appendStringArrayContinuation(
    allocator: std.mem.Allocator,
    line: []const u8,
    list: *std.array_list.Managed([]const u8),
) !bool {
    if (std.mem.eql(u8, line, "]")) return true;
    if (line[line.len - 1] == ']') {
        try appendStringArrayFragment(allocator, line[0 .. line.len - 1], list);
        return true;
    }

    try appendStringArrayFragment(allocator, line, list);
    return false;
}

fn appendStringArrayFragment(
    allocator: std.mem.Allocator,
    fragment: []const u8,
    list: *std.array_list.Managed([]const u8),
) !void {
    const trimmed = std.mem.trim(u8, fragment, " \t,");
    if (trimmed.len == 0) return;
    try list.append(try parseOwnedString(allocator, trimmed));
}

fn parseInlineTable(allocator: std.mem.Allocator, value: []const u8) ![]const KeyValue {
    if (value.len < 2 or value[0] != '{' or value[value.len - 1] != '}') return error.InvalidManifest;
    const body = value[1 .. value.len - 1];
    var fields = std.array_list.Managed(KeyValue).init(allocator);
    var start: usize = 0;
    var in_string = false;
    for (body, 0..) |ch, index| {
        if (ch == '"') in_string = !in_string;
        if (ch == ',' and !in_string) {
            const part = std.mem.trim(u8, body[start..index], " \t");
            if (part.len > 0) try fields.append(try parseInlineField(allocator, part));
            start = index + 1;
        }
    }
    const trailing = std.mem.trim(u8, body[start..], " \t");
    if (trailing.len > 0) try fields.append(try parseInlineField(allocator, trailing));
    return fields.toOwnedSlice();
}

fn parseInlineField(allocator: std.mem.Allocator, part: []const u8) !KeyValue {
    const kv = try splitKeyValue(part);
    return .{
        .key = try allocator.dupe(u8, kv.key),
        .value = try parseOwnedString(allocator, kv.value),
    };
}

fn cloneAndSortDependencies(allocator: std.mem.Allocator, items: []const dependency.DependencySpec) ![]dependency.DependencySpec {
    const cloned = try allocator.alloc(dependency.DependencySpec, items.len);
    for (items, 0..) |item, index| cloned[index] = item;
    std.mem.sort(dependency.DependencySpec, cloned, {}, lessDependencySpec);
    return cloned;
}

fn lessDependencySpec(_: void, lhs: dependency.DependencySpec, rhs: dependency.DependencySpec) bool {
    return std.mem.order(u8, lhs.name, rhs.name) == .lt;
}

fn cloneAndSortRootDependencies(allocator: std.mem.Allocator, items: []const LockFile.RootDependency) ![]LockFile.RootDependency {
    const cloned = try allocator.alloc(LockFile.RootDependency, items.len);
    for (items, 0..) |item, index| cloned[index] = item;
    std.mem.sort(LockFile.RootDependency, cloned, {}, lessRootDependency);
    return cloned;
}

fn lessRootDependency(_: void, lhs: LockFile.RootDependency, rhs: LockFile.RootDependency) bool {
    return std.mem.order(u8, lhs.name, rhs.name) == .lt;
}

fn cloneAndSortLockedPackages(allocator: std.mem.Allocator, items: []const LockFile.LockedPackage) ![]LockFile.LockedPackage {
    const cloned = try allocator.alloc(LockFile.LockedPackage, items.len);
    for (items, 0..) |item, index| cloned[index] = item;
    std.mem.sort(LockFile.LockedPackage, cloned, {}, lessLockedPackage);
    return cloned;
}

fn lessLockedPackage(_: void, lhs: LockFile.LockedPackage, rhs: LockFile.LockedPackage) bool {
    const name_order = std.mem.order(u8, lhs.name, rhs.name);
    if (name_order != .eq) return name_order == .lt;
    return std.mem.order(u8, lhs.module_root, rhs.module_root) == .lt;
}

fn cloneAndSortStrings(allocator: std.mem.Allocator, items: []const []const u8) ![][]const u8 {
    const cloned = try allocator.alloc([]const u8, items.len);
    for (items, 0..) |item, index| cloned[index] = item;
    std.mem.sort([]const u8, cloned, {}, lessString);
    return cloned;
}

fn lessString(_: void, lhs: []const u8, rhs: []const u8) bool {
    return std.mem.order(u8, lhs, rhs) == .lt;
}

fn parseLinkMode(value: []const u8) native.LinkMode {
    if (std.mem.eql(u8, value, "dynamic")) return .dynamic;
    return .static;
}

fn parseAbi(value: []const u8) native.LibraryAbi {
    _ = value;
    return .c;
}

fn parseAutobindingProfile(value: []const u8) native.AutobindingProfile {
    if (std.mem.eql(u8, value, "vulkan")) return .vulkan;
    if (std.mem.eql(u8, value, "directx12")) return .directx12;
    return .generic;
}

test "parses project manifest dependencies" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const manifest = try parseProjectManifest(arena.allocator(),
        \\[package]
        \\name = "DemoApp"
        \\version = "0.1.0"
        \\kind = "app"
        \\kira = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "vm"
        \\build_target = "host"
        \\
        \\[dependencies]
        \\FrostUI = "0.1.0"
        \\LocalDemo = { path = "../LocalDemo" }
        \\GameKit = { git = "https://example.com/GameKit.git", rev = "abc123" }
    );

    try std.testing.expectEqualStrings("DemoApp", manifest.name);
    try std.testing.expectEqual(@as(usize, 3), manifest.dependencies.len);
    try std.testing.expect(manifest.dependencies[0].source == .registry);
    try std.testing.expect(manifest.dependencies[1].source == .path);
    try std.testing.expect(manifest.dependencies[2].source == .git);
}

test "parses project manifest native libraries" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const manifest = try parseProjectManifest(arena.allocator(),
        \\[project]
        \\name = "DemoApp"
        \\version = "0.1.0"
        \\native_libraries = ["NativeLibs/callbacks.toml", "NativeLibs/sokol.toml"]
    );

    try std.testing.expectEqual(@as(usize, 2), manifest.native_libraries.len);
    try std.testing.expectEqualStrings("NativeLibs/callbacks.toml", manifest.native_libraries[0]);
    try std.testing.expectEqualStrings("NativeLibs/sokol.toml", manifest.native_libraries[1]);
}

test "parses multiline root package array" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const manifest = try parseProjectManifest(arena.allocator(),
        \\[project]
        \\name = "Repo"
        \\version = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "vm"
        \\build_target = "host"
        \\
        \\packages = [
        \\  "packages/a",
        \\  "packages/b",
        \\]
    );

    try std.testing.expectEqual(@as(usize, 2), manifest.packages.len);
    try std.testing.expectEqualStrings("packages/a", manifest.packages[0]);
    try std.testing.expectEqualStrings("packages/b", manifest.packages[1]);
}

test "rejects unsupported registry version ranges" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    try std.testing.expectError(error.UnsupportedVersionRange, parseProjectManifest(arena.allocator(),
        \\[package]
        \\name = "DemoApp"
        \\
        \\[dependencies]
        \\FrostUI = "^0.1.0"
    ));
}

test "lockfile round trip stays parseable" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const lockfile: LockFile = .{
        .schema_version = 1,
        .root = .{
            .name = "DemoApp",
            .version = "0.1.0",
            .dependencies = &.{
                .{
                    .name = "FrostUI",
                    .source = .{ .registry = .{ .version = "0.1.0" } },
                },
            },
        },
        .packages = &.{
            .{
                .name = "FrostUI",
                .version = "0.1.0",
                .module_root = "FrostUI",
                .source = .{ .registry = .{
                    .registry_url = "https://registry.example.test",
                    .archive_path = "packages/frostui/0.1.0.tar",
                    .checksum = "abc",
                } },
                .dependencies = &.{"KiraStd"},
            },
        },
    };

    var output: std.Io.Writer.Allocating = .init(allocator);
    defer output.deinit();
    try writeLockFile(&output.writer, lockfile);

    const reparsed = try parseLockFile(allocator, output.written());
    try std.testing.expectEqual(@as(u32, 1), reparsed.schema_version);
    try std.testing.expectEqualStrings("DemoApp", reparsed.root.name);
    try std.testing.expectEqual(@as(usize, 1), reparsed.packages.len);
    try std.testing.expectEqualStrings("FrostUI", reparsed.packages[0].name);
}

test "parses native library manifest" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const manifest = try parseNativeLibManifest(arena.allocator(),
        \\[library]
        \\name = "sokol_gfx"
        \\link_mode = "static"
        \\abi = "c"
        \\
        \\[headers]
        \\entrypoint = "vendor/sokol/sokol_gfx.h"
        \\include_dirs = ["vendor/sokol"]
        \\defines = ["SOKOL_DUMMY_BACKEND"]
        \\
        \\[autobinding]
        \\module = "sokol_gfx"
        \\output = "sokol_gfx.kira"
        \\headers = ["vendor/sokol/sokol_gfx.h"]
        \\
        \\[bindings]
        \\profile = "vulkan"
        \\functions = ["sg_setup"]
        \\structs = ["sg_desc"]
        \\
        \\[build]
        \\sources = ["vendor/sokol/sokol_gfx_impl.c"]
        \\defines = ["SOKOL_IMPL", "SOKOL_DUMMY_BACKEND"]
        \\
        \\[target.x86_64-linux-gnu]
        \\static_lib = "generated/native/sokol_gfx/x86_64-linux-gnu/libsokol_gfx.a"
        \\frameworks = ["X11"]
    );

    try std.testing.expectEqualStrings("sokol_gfx", manifest.library.name);
    try std.testing.expectEqualStrings("vendor/sokol/sokol_gfx.h", manifest.library.headers.entrypoint.?);
    try std.testing.expectEqualStrings("sokol_gfx", manifest.library.autobinding.?.module_name);
    try std.testing.expectEqualStrings("vendor/sokol/sokol_gfx_impl.c", manifest.library.build.sources[0]);
    try std.testing.expectEqual(native.AutobindingProfile.vulkan, manifest.library.autobinding.?.bindings.profile);
    try std.testing.expectEqualStrings("sg_setup", manifest.library.autobinding.?.bindings.functions[0]);
    try std.testing.expectEqual(@as(usize, 1), manifest.library.targets.len);
    try std.testing.expectEqualStrings("generated/native/sokol_gfx/x86_64-linux-gnu/libsokol_gfx.a", manifest.library.targets[0].static_lib.?);
}
