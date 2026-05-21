const std = @import("std");
const manifest = @import("kira_manifest");
const kira_project = @import("kira_project");

pub const ResolvedLiveTarget = struct {
    target_root: []const u8,
    target_manifest_path: []const u8,
    target_package_name: []const u8,
    target_kind: manifest.PackageKind,
    validation_app_root: []const u8,
    validation_manifest_path: []const u8,
    validation_entrypoint_path: []const u8,
    output_root: []const u8,
    runner_display_name: []const u8,
};

pub fn resolveLiveTarget(allocator: std.mem.Allocator, input_path: []const u8) !ResolvedLiveTarget {
    const resolved = try kira_project.loadPackageRootFromPath(allocator, input_path);
    const output_root = try std.fs.path.join(allocator, &.{ resolved.root_path, ".kira-build", "live" });

    if (resolved.project.manifest.kind == .app) {
        const entrypoint = resolved.entrypoint_path orelse return error.ProjectEntrypointNotFound;
        return .{
            .target_root = resolved.root_path,
            .target_manifest_path = resolved.manifest_path,
            .target_package_name = resolved.project.manifest.name,
            .target_kind = resolved.project.manifest.kind,
            .validation_app_root = resolved.root_path,
            .validation_manifest_path = resolved.manifest_path,
            .validation_entrypoint_path = entrypoint,
            .output_root = output_root,
            .runner_display_name = try defaultRunnerName(allocator, resolved.project.manifest.name),
        };
    }

    const validation = try discoverValidationApp(allocator, resolved.root_path, resolved.project.manifest.name);
    return .{
        .target_root = resolved.root_path,
        .target_manifest_path = resolved.manifest_path,
        .target_package_name = resolved.project.manifest.name,
        .target_kind = resolved.project.manifest.kind,
        .validation_app_root = validation.root_path,
        .validation_manifest_path = validation.manifest_path,
        .validation_entrypoint_path = validation.entrypoint_path,
        .output_root = output_root,
        .runner_display_name = try defaultRunnerName(allocator, resolved.project.manifest.name),
    };
}

const ValidationApp = struct {
    root_path: []const u8,
    manifest_path: []const u8,
    entrypoint_path: []const u8,
};

fn discoverValidationApp(allocator: std.mem.Allocator, package_root: []const u8, package_name: []const u8) !ValidationApp {
    const preferred = try std.fs.path.join(allocator, &.{ package_root, "Examples", "basic-foundation-app" });
    if (matchesValidationApp(allocator, preferred, package_root, package_name)) |value| return value;

    const examples_root = try std.fs.path.join(allocator, &.{ package_root, "Examples" });
    if (!directoryExists(examples_root)) return error.LiveValidationAppNotFound;

    var names = std.array_list.Managed([]const u8).init(allocator);
    var dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, examples_root, .{ .iterate = true });
    defer dir.close(std.Options.debug_io);
    var iterator = dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .directory) continue;
        try names.append(try allocator.dupe(u8, entry.name));
    }
    std.mem.sort([]const u8, names.items, {}, struct {
        fn lessThan(_: void, lhs: []const u8, rhs: []const u8) bool {
            return std.mem.lessThan(u8, lhs, rhs);
        }
    }.lessThan);

    for (names.items) |name| {
        const candidate = try std.fs.path.join(allocator, &.{ examples_root, name });
        if (matchesValidationApp(allocator, candidate, package_root, package_name)) |value| return value;
    }
    return error.LiveValidationAppNotFound;
}

fn matchesValidationApp(
    allocator: std.mem.Allocator,
    candidate_root: []const u8,
    package_root: []const u8,
    package_name: []const u8,
) ?ValidationApp {
    const loaded = kira_project.loadProjectFromPath(allocator, candidate_root) catch return null;
    if (loaded.project.manifest.kind != .app) return null;
    for (loaded.project.manifest.dependencies) |dependency| {
        if (!std.mem.eql(u8, dependency.name, package_name)) continue;
        if (dependency.source != .path) continue;
        const resolved_path = resolveDependencyPath(allocator, loaded.root_path, dependency.source.path.path) catch return null;
        defer allocator.free(resolved_path);
        if (std.mem.eql(u8, resolved_path, package_root)) {
            return .{
                .root_path = loaded.root_path,
                .manifest_path = loaded.manifest_path,
                .entrypoint_path = loaded.entrypoint_path,
            };
        }
    }
    return null;
}

fn resolveDependencyPath(allocator: std.mem.Allocator, parent_root: []const u8, relative_path: []const u8) ![]const u8 {
    if (std.fs.path.isAbsolute(relative_path)) return allocator.dupe(u8, relative_path);
    const joined = try std.fs.path.join(allocator, &.{ parent_root, relative_path });
    defer allocator.free(joined);
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, joined, allocator);
}

fn directoryExists(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}

fn defaultRunnerName(allocator: std.mem.Allocator, package_name: []const u8) ![]const u8 {
    const trimmed = if (std.mem.startsWith(u8, package_name, "Kira")) package_name["Kira".len..] else package_name;
    return std.fmt.allocPrint(allocator, "{s}LiveRunner", .{trimmed});
}

test "target resolution discovers ui-foundation validation app" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const resolved = try resolveLiveTarget(arena.allocator(), "../ui-foundation");
    try std.testing.expectEqual(manifest.PackageKind.library, resolved.target_kind);
    try std.testing.expect(std.mem.endsWith(u8, resolved.validation_app_root, "Examples/basic-foundation-app"));
    try std.testing.expect(std.mem.endsWith(u8, resolved.validation_entrypoint_path, "Examples/basic-foundation-app/app/main.kira"));
}
