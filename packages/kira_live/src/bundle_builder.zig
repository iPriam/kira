const std = @import("std");
const build = @import("kira_build");
const build_def = @import("kira_build_definition");
const diagnostics = @import("kira_diagnostics");
const hybrid = @import("kira_hybrid_definition");
const llvm_backend = @import("kira_llvm_backend");
const manifest = @import("kira_manifest");
const native = @import("kira_native_lib_definition");
const package_manager = @import("kira_package_manager");
const ResolvedLiveTarget = @import("target.zig").ResolvedLiveTarget;
const model = @import("model.zig");

pub const BundleBuildArtifacts = struct {
    graph: model.BundleGraph,
    main_native_object_path: []const u8,
    main_native_library_path: []const u8,
    main_native_libraries: []const native.ResolvedNativeLibrary,
    native_contract_hash: []const u8,
};

pub fn buildBundles(
    allocator: std.mem.Allocator,
    target: ResolvedLiveTarget,
    selector: ?native.TargetSelector,
    embed_native_in_runner: bool,
) !BundleBuildArtifacts {
    const bundles_root = try std.fs.path.join(allocator, &.{ target.output_root, "bundles" });
    const native_root = try std.fs.path.join(allocator, &.{ target.output_root, "native" });
    const shims_root = try std.fs.path.join(allocator, &.{ target.output_root, "cache", "shims" });
    const server_graph_dir = try std.fs.path.join(allocator, &.{ target.output_root, "server", "graph" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, bundles_root);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, native_root);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, shims_root);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, server_graph_dir);

    const app_manifest_text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, target.validation_manifest_path, allocator, .limited(2 * 1024 * 1024));
    const app_manifest = try manifest.parseProjectManifest(allocator, app_manifest_text);

    var package_diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const sync_result = package_manager.syncProject(allocator, target.validation_app_root, "0.1.0", .{}, &package_diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => return error.LiveBundleBuildFailed,
        else => return err,
    };

    var bundle_results = std.array_list.Managed(BundleResult).init(allocator);
    const app_bundle_id = try bundleIdForPackage(allocator, app_manifest.name);
    const app_result = try buildProjectBundle(
        allocator,
        .{
            .bundle_id = app_bundle_id,
            .package_name = app_manifest.name,
            .package_root = target.validation_app_root,
            .version = app_manifest.version,
            .kind = "app",
            .module_root = app_manifest.module_root orelse app_manifest.name,
            .entrypoint_path = target.validation_entrypoint_path,
            .bundles_root = bundles_root,
            .target_validation_root = target.validation_app_root,
            .selector = selector,
            .emit_native_object = true,
            .emit_shared_library = !embed_native_in_runner,
            .native_root = native_root,
        },
    );
    try bundle_results.append(app_result);

    for (sync_result.graph.packages) |package| {
        const package_root = std.fs.path.dirname(package.source_root) orelse continue;
        try bundle_results.append(try buildDependencyBundle(
            allocator,
            package_root,
            package.name,
            package.version,
            package.kind,
            package.module_root,
            bundles_root,
            shims_root,
            target.validation_app_root,
            selector,
        ));
    }

    std.mem.sort(BundleResult, bundle_results.items, {}, struct {
        fn lessThan(_: void, lhs: BundleResult, rhs: BundleResult) bool {
            return std.mem.lessThan(u8, lhs.spec.id, rhs.spec.id);
        }
    }.lessThan);

    const bundles = try allocator.alloc(model.BundleSpec, bundle_results.items.len);
    for (bundle_results.items, 0..) |result, index| bundles[index] = result.spec;

    const graph = model.BundleGraph{
        .target_path = target.target_root,
        .target_package = target.target_package_name,
        .validation_app_path = target.validation_app_root,
        .main_bundle_id = app_bundle_id,
        .bundles = bundles,
    };

    const graph_path = try std.fs.path.join(allocator, &.{ server_graph_dir, "BundleGraph.toml" });
    try writeTomlFile(graph_path, graph);
    const graph_json_path = try std.fs.path.join(allocator, &.{ server_graph_dir, "graph.json" });
    try writeFile(graph_json_path, try graphJson(allocator, graph));

    return .{
        .graph = graph,
        .main_native_object_path = app_result.native_object_path orelse return error.LiveBundleBuildFailed,
        .main_native_library_path = app_result.native_library_path orelse "__kira_live_self__",
        .main_native_libraries = app_result.native_libraries,
        .native_contract_hash = try hashFilesAndStrings(allocator, &.{app_result.hybrid_path}, app_result.native_libraries),
    };
}

const BuildProjectBundleArgs = struct {
    bundle_id: []const u8,
    package_name: []const u8,
    package_root: []const u8,
    version: []const u8,
    kind: []const u8,
    module_root: []const u8,
    entrypoint_path: []const u8,
    bundles_root: []const u8,
    target_validation_root: []const u8,
    selector: ?native.TargetSelector,
    emit_native_object: bool,
    emit_shared_library: bool,
    native_root: []const u8,
};

const BundleResult = struct {
    spec: model.BundleSpec,
    hybrid_path: []const u8,
    native_object_path: ?[]const u8 = null,
    native_library_path: ?[]const u8 = null,
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
};

fn buildProjectBundle(allocator: std.mem.Allocator, args: BuildProjectBundleArgs) !BundleResult {
    const bundle_dir = try std.fs.path.join(allocator, &.{ args.bundles_root, try std.fmt.allocPrint(allocator, "{s}.klbundle", .{args.bundle_id}) });
    const modules_dir = try std.fs.path.join(allocator, &.{ bundle_dir, "modules" });
    const assets_dir = try std.fs.path.join(allocator, &.{ bundle_dir, "assets" });
    const resources_dir = try std.fs.path.join(allocator, &.{ bundle_dir, "resources" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, modules_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, assets_dir);
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, resources_dir);

    const compiled = try build.compileFileForBackendWithSelector(allocator, args.entrypoint_path, .hybrid, args.selector, &.{});
    if (compiled.failed()) {
        return error.LiveBundleBuildFailed;
    }

    const bytecode_rel_path = "modules/app.main.kirbc";
    const hybrid_rel_path = "modules/app.main.khm";
    const bytecode_path = try std.fs.path.join(allocator, &.{ bundle_dir, bytecode_rel_path });
    const hybrid_path = try std.fs.path.join(allocator, &.{ bundle_dir, hybrid_rel_path });
    try compiled.bytecode_module.?.writeToFile(bytecode_path);

    var native_object_path: ?[]const u8 = null;
    var native_library_path: ?[]const u8 = null;
    if (args.emit_native_object) {
        const target_dir = try nativeTargetDirectory(allocator, args.selector);
        const object_path = try std.fs.path.join(allocator, &.{ args.native_root, "objects", target_dir, try std.fmt.allocPrint(allocator, "{s}.o", .{args.bundle_id}) });
        const library_path = if (args.emit_shared_library)
            try std.fs.path.join(allocator, &.{ args.native_root, "libs", target_dir, try std.fmt.allocPrint(allocator, "{s}{s}", .{ args.bundle_id, sharedLibraryExtension() }) })
        else
            null;
        try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, std.fs.path.dirname(object_path) orelse ".");
        if (library_path) |path| try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, std.fs.path.dirname(path) orelse ".");
        _ = llvm_backend.compile(allocator, .{
            .mode = .hybrid,
            .program = &compiled.verified_program.?,
            .module_name = std.fs.path.stem(args.entrypoint_path),
            .emit = .{
                .object_path = object_path,
                .shared_library_path = library_path,
            },
            .target_selector = args.selector,
            .resolved_native_libraries = compiled.native_libraries,
        }) catch {
            return error.LiveBundleBuildFailed;
        };
        native_object_path = object_path;
        native_library_path = library_path;
    }

    const hybrid_manifest = try buildHybridManifest(
        allocator,
        compiled.ir_program.?,
        std.fs.path.stem(args.entrypoint_path),
        bytecode_rel_path,
        native_library_path orelse "__kira_live_self__",
    );
    try hybrid_manifest.writeToFile(hybrid_path);

    const bundle_manifest_path = try std.fs.path.join(allocator, &.{ bundle_dir, "KiraBundle.toml" });
    const content_hash = try hashPlainFiles(allocator, &.{ bytecode_path, hybrid_path });
    try writeTomlFile(bundle_manifest_path, model.BundleManifest{
        .id = args.bundle_id,
        .package_name = args.package_name,
        .version = args.version,
        .kind = args.kind,
        .module_root = args.module_root,
        .bytecode_rel_path = bytecode_rel_path,
        .hybrid_rel_path = hybrid_rel_path,
        .executable = std.mem.eql(u8, args.kind, "app"),
    });
    try writeFile(try std.fs.path.join(allocator, &.{ bundle_dir, "metadata.json" }), try bundleMetadataJson(allocator, args, content_hash));
    try writeFile(try std.fs.path.join(allocator, &.{ bundle_dir, "diagnostics.json" }), "{\"status\":\"ok\",\"diagnostics\":[]}\n");
    try writeFile(try std.fs.path.join(allocator, &.{ bundle_dir, "graph.json" }), try bundleLocalGraphJson(allocator, args.bundle_id, bytecode_rel_path, hybrid_rel_path, content_hash));

    return .{
        .spec = .{
            .id = args.bundle_id,
            .package_name = args.package_name,
            .package_root = args.package_root,
            .version = args.version,
            .kind = args.kind,
            .module_root = args.module_root,
            .manifest_rel_path = try std.fmt.allocPrint(allocator, "bundles/{s}.klbundle/KiraBundle.toml", .{args.bundle_id}),
            .bytecode_rel_path = try std.fmt.allocPrint(allocator, "bundles/{s}.klbundle/{s}", .{ args.bundle_id, bytecode_rel_path }),
            .hybrid_rel_path = try std.fmt.allocPrint(allocator, "bundles/{s}.klbundle/{s}", .{ args.bundle_id, hybrid_rel_path }),
            .executable = std.mem.eql(u8, args.kind, "app"),
            .validation_root = args.target_validation_root,
        },
        .hybrid_path = hybrid_path,
        .native_object_path = native_object_path,
        .native_library_path = native_library_path,
        .native_libraries = compiled.native_libraries,
    };
}

fn graphJson(allocator: std.mem.Allocator, graph: model.BundleGraph) ![]const u8 {
    var buffer: std.Io.Writer.Allocating = .init(allocator);
    errdefer buffer.deinit();
    const writer = &buffer.writer;
    try writer.print("{{\"target\":\"{s}\",\"package\":\"{s}\",\"validation_app\":\"{s}\",\"main_bundle\":\"{s}\",\"bundles\":[", .{ graph.target_path, graph.target_package, graph.validation_app_path, graph.main_bundle_id });
    for (graph.bundles, 0..) |bundle, index| {
        if (index != 0) try writer.writeAll(",");
        try writer.print("{{\"id\":\"{s}\",\"manifest\":\"{s}\",\"kind\":\"{s}\"}}", .{ bundle.id, bundle.manifest_rel_path, bundle.kind });
    }
    try writer.writeAll("]}\n");
    return buffer.toOwnedSlice();
}

fn bundleMetadataJson(allocator: std.mem.Allocator, args: BuildProjectBundleArgs, content_hash: []const u8) ![]const u8 {
    return std.fmt.allocPrint(
        allocator,
        "{{\"format\":\"klbundle\",\"bundle_version\":1,\"id\":\"{s}\",\"target_identity\":\"{s}\",\"profile\":\"debug\",\"backend\":\"hybrid\",\"entrypoint\":\"{s}\",\"hash\":\"{s}\",\"platform\":null,\"surface\":null}}\n",
        .{ args.bundle_id, args.package_name, args.entrypoint_path, std.mem.trim(u8, content_hash, " \t\r\n") },
    );
}

fn bundleLocalGraphJson(allocator: std.mem.Allocator, bundle_id: []const u8, bytecode_path: []const u8, hybrid_path: []const u8, content_hash: []const u8) ![]const u8 {
    return std.fmt.allocPrint(
        allocator,
        "{{\"bundle\":\"{s}\",\"modules\":[{{\"bytecode\":\"{s}\",\"hybrid\":\"{s}\"}}],\"assets\":[],\"resources\":[],\"hash\":\"{s}\"}}\n",
        .{ bundle_id, bytecode_path, hybrid_path, std.mem.trim(u8, content_hash, " \t\r\n") },
    );
}

fn buildDependencyBundle(
    allocator: std.mem.Allocator,
    package_root: []const u8,
    package_name: []const u8,
    version: []const u8,
    kind: []const u8,
    module_root: []const u8,
    bundles_root: []const u8,
    shims_root: []const u8,
    validation_root: []const u8,
    selector: ?native.TargetSelector,
) !BundleResult {
    const bundle_id = try bundleIdForPackage(allocator, package_name);
    const shim_root = try std.fs.path.join(allocator, &.{ shims_root, bundle_id });
    const shim_app_root = try std.fs.path.join(allocator, &.{ shim_root, "app" });
    try std.Io.Dir.cwd().createDirPath(std.Options.debug_io, shim_app_root);

    const shim_manifest_path = try std.fs.path.join(allocator, &.{ shim_root, "kira.toml" });
    const shim_manifest = try std.fmt.allocPrint(
        allocator,
        \\[package]
        \\name = "live-{s}"
        \\version = "0.1.0"
        \\kind = "app"
        \\kira = "0.1.0"
        \\
        \\[defaults]
        \\execution_mode = "hybrid"
        \\build_target = "host"
        \\
        \\[dependencies]
        \\{s} = {{ path = "{s}" }}
        \\
    ,
        .{ bundle_id, package_name, package_root },
    );
    try writeFile(shim_manifest_path, shim_manifest);

    const shim_entrypoint_path = try std.fs.path.join(allocator, &.{ shim_app_root, "main.kira" });
    const shim_source = try std.fmt.allocPrint(
        allocator,
        \\import {s}
        \\
        \\@Main
        \\function main() {{
        \\    return
        \\}}
    ,
        .{module_root},
    );
    try writeFile(shim_entrypoint_path, shim_source);

    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    _ = package_manager.syncProject(allocator, shim_root, "0.1.0", .{}, &diags) catch |err| switch (err) {
        error.DiagnosticsEmitted => return error.LiveBundleBuildFailed,
        else => return err,
    };

    return buildProjectBundle(allocator, .{
        .bundle_id = bundle_id,
        .package_name = package_name,
        .package_root = package_root,
        .version = version,
        .kind = kind,
        .module_root = module_root,
        .entrypoint_path = shim_entrypoint_path,
        .bundles_root = bundles_root,
        .target_validation_root = validation_root,
        .selector = selector,
        .emit_native_object = false,
        .emit_shared_library = false,
        .native_root = "",
    });
}

fn buildHybridManifest(
    allocator: std.mem.Allocator,
    program: @import("kira_ir").Program,
    module_name: []const u8,
    bytecode_path: []const u8,
    native_library_path: []const u8,
) !hybrid.HybridModuleManifest {
    var functions = std.array_list.Managed(hybrid.FunctionManifest).init(allocator);
    for (program.functions) |function_decl| {
        const resolved_execution = switch (function_decl.execution) {
            .inherited => .runtime,
            else => function_decl.execution,
        };
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
        .native_library_path = try allocator.dupe(u8, native_library_path),
        .entry_function_id = entry_function.id,
        .entry_execution = switch (entry_function.execution) {
            .inherited => .runtime,
            else => entry_function.execution,
        },
        .functions = try functions.toOwnedSlice(),
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

fn writeTomlFile(path: []const u8, value: anytype) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    var buffer: [4096]u8 = undefined;
    var writer = file.writer(std.Options.debug_io, &buffer);
    try value.writeToml(&writer.interface);
    try writer.interface.flush();
}

fn writeFile(path: []const u8, data: []const u8) !void {
    const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
    defer file.close(std.Options.debug_io);
    try file.writeStreamingAll(std.Options.debug_io, data);
}

fn bundleIdForPackage(allocator: std.mem.Allocator, package_name: []const u8) ![]const u8 {
    const raw = if (std.mem.startsWith(u8, package_name, "Kira") and package_name.len > 4) package_name[4..] else package_name;
    var builder = std.array_list.Managed(u8).init(allocator);
    try builder.appendSlice("com.kira.");
    for (raw, 0..) |ch, index| {
        if (ch == '-' or ch == '.' or ch == ' ') {
            try builder.append('_');
            continue;
        }
        if (std.ascii.isUpper(ch)) {
            if (index != 0) try builder.append('_');
            try builder.append(std.ascii.toLower(ch));
            continue;
        }
        try builder.append(std.ascii.toLower(ch));
    }
    return builder.toOwnedSlice();
}

fn hashFilesAndStrings(
    allocator: std.mem.Allocator,
    files: []const []const u8,
    native_libraries: []const native.ResolvedNativeLibrary,
) ![]const u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update("kira-live-native-contract-v1\n");
    for (files) |path| {
        const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(16 * 1024 * 1024));
        defer allocator.free(bytes);
        hasher.update(bytes);
    }
    for (native_libraries) |library| {
        hasher.update(library.name);
        hasher.update("\n");
        hasher.update(library.artifact_path);
        hasher.update("\n");
    }
    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    const alphabet = "0123456789abcdef";
    const out = try allocator.alloc(u8, digest.len * 2);
    for (digest, 0..) |byte, index| {
        out[index * 2] = alphabet[byte >> 4];
        out[index * 2 + 1] = alphabet[byte & 0x0f];
    }
    return out;
}

fn hashPlainFiles(allocator: std.mem.Allocator, files: []const []const u8) ![]const u8 {
    var hasher = std.crypto.hash.sha2.Sha256.init(.{});
    hasher.update("kira-klbundle-v1\n");
    for (files) |path| {
        const bytes = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(16 * 1024 * 1024));
        defer allocator.free(bytes);
        hasher.update(path);
        hasher.update("\n");
        hasher.update(bytes);
        hasher.update("\n");
    }
    var digest: [32]u8 = undefined;
    hasher.final(&digest);
    const alphabet = "0123456789abcdef";
    const out = try allocator.alloc(u8, digest.len * 2);
    for (digest, 0..) |byte, index| {
        out[index * 2] = alphabet[byte >> 4];
        out[index * 2 + 1] = alphabet[byte & 0x0f];
    }
    return out;
}

fn sharedLibraryExtension() []const u8 {
    return switch (@import("builtin").os.tag) {
        .windows => ".dll",
        .macos => ".dylib",
        else => ".so",
    };
}

fn nativeTargetDirectory(allocator: std.mem.Allocator, selector: ?native.TargetSelector) ![]const u8 {
    if (selector) |value| {
        return std.fmt.allocPrint(allocator, "{s}-{s}-{s}", .{ value.architecture, value.operating_system, value.abi });
    }
    return allocator.dupe(u8, "host");
}
