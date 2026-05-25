const std = @import("std");
const manifest = @import("kira_manifest");
const Project = @import("project.zig").Project;
const ResolvedProject = @import("project.zig").ResolvedProject;
const ResolvedPackageRoot = @import("project.zig").ResolvedPackageRoot;
const ResolvedTarget = @import("project.zig").ResolvedTarget;
const TargetKind = @import("project.zig").TargetKind;

pub const preferred_manifest_file_name = "kira.toml";
pub const legacy_manifest_file_name = "project.toml";
pub const repo_manifest_file_name = "Kira.toml";
pub const manifest_file_name = preferred_manifest_file_name;
pub const entrypoint_rel_path = "app/main.kira";

pub const manifest_file_names = [_][]const u8{
    preferred_manifest_file_name,
    legacy_manifest_file_name,
    repo_manifest_file_name,
};

pub fn loadProjectFromFile(allocator: std.mem.Allocator, path: []const u8) !Project {
    const text = try std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(1024 * 1024));
    return .{
        .manifest = try manifest.parseProjectManifest(allocator, text),
    };
}

pub fn loadProjectFromPath(allocator: std.mem.Allocator, path: []const u8) !ResolvedProject {
    const root_path = try resolveRootPath(allocator, path);
    const manifest_path = try discoverManifestPath(allocator, root_path) orelse return error.ProjectManifestNotFound;

    const entrypoint_path = try std.fs.path.join(allocator, &.{ root_path, "app", "main.kira" });
    if (!fileExists(entrypoint_path)) return error.ProjectEntrypointNotFound;

    return .{
        .root_path = root_path,
        .manifest_path = manifest_path,
        .entrypoint_path = entrypoint_path,
        .project = try loadProjectFromFile(allocator, manifest_path),
    };
}

pub fn loadPackageRootFromPath(allocator: std.mem.Allocator, path: []const u8) !ResolvedPackageRoot {
    const root_path = try resolveRootPath(allocator, path);
    const manifest_path = try discoverManifestPath(allocator, root_path) orelse return error.ProjectManifestNotFound;
    const entrypoint_path = try std.fs.path.join(allocator, &.{ root_path, "app", "main.kira" });
    const module_source_root = try moduleSourceRoot(allocator, root_path);

    return .{
        .root_path = root_path,
        .manifest_path = manifest_path,
        .entrypoint_path = if (fileExists(entrypoint_path)) entrypoint_path else null,
        .module_source_root = module_source_root,
        .project = try loadProjectFromFile(allocator, manifest_path),
    };
}

pub fn resolveTargetFromPath(allocator: std.mem.Allocator, path: []const u8) !ResolvedTarget {
    const base = std.fs.path.basename(path);
    if (std.mem.eql(u8, base, preferred_manifest_file_name) or
        std.mem.eql(u8, base, legacy_manifest_file_name) or
        std.mem.eql(u8, base, repo_manifest_file_name) or
        directoryExists(path))
    {
        const resolved = try loadPackageRootFromPath(allocator, path);
        const target_kind: TargetKind = switch (resolved.project.manifest.kind) {
            .library => .library,
            .app => if (isExampleRoot(resolved.root_path)) .example else .executable,
        };
        return .{
            .root_path = resolved.root_path,
            .manifest_path = resolved.manifest_path,
            .source_path = resolved.entrypoint_path,
            .source_root = resolved.module_source_root,
            .project_name = resolved.project.manifest.name,
            .project = resolved.project,
            .package_kind = resolved.project.manifest.kind,
            .target_kind = target_kind,
        };
    }

    if (fileExists(path)) {
        return .{
            .source_path = try std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, path, allocator),
            .project_name = std.fs.path.stem(path),
            .target_kind = .source_file,
        };
    }

    return error.InvalidProjectPath;
}

fn resolveRootPath(allocator: std.mem.Allocator, path: []const u8) ![]u8 {
    if (isManifestPath(path)) {
        if (!fileExists(path)) return error.InvalidProjectPath;
        const directory = std.fs.path.dirname(path) orelse ".";
        return absolutize(allocator, directory);
    }

    if (directoryExists(path)) {
        return absolutize(allocator, path);
    }

    return error.InvalidProjectPath;
}

fn discoverManifestPath(allocator: std.mem.Allocator, root_path: []const u8) !?[]u8 {
    for (manifest_file_names) |name| {
        const candidate = try std.fs.path.join(allocator, &.{ root_path, name });
        if (fileExists(candidate)) return candidate;
        allocator.free(candidate);
    }
    return null;
}

fn isManifestPath(path: []const u8) bool {
    const base = std.fs.path.basename(path);
    for (manifest_file_names) |name| {
        if (std.mem.eql(u8, base, name)) return true;
    }
    return false;
}

fn absolutize(allocator: std.mem.Allocator, path: []const u8) ![]u8 {
    if (std.fs.path.isAbsolute(path)) return allocator.dupe(u8, path);
    return std.Io.Dir.cwd().realPathFileAlloc(std.Options.debug_io, path, allocator);
}

fn isExampleRoot(path: []const u8) bool {
    return std.mem.indexOf(u8, path, "/examples/") != null or
        std.mem.endsWith(u8, path, "/examples") or
        std.mem.indexOf(u8, path, "/Examples/") != null or
        std.mem.endsWith(u8, path, "/Examples");
}

fn moduleSourceRoot(allocator: std.mem.Allocator, root_path: []const u8) ![]u8 {
    return std.fs.path.join(allocator, &.{ root_path, "app" });
}

fn fileExists(path: []const u8) bool {
    if (std.fs.path.isAbsolute(path)) {
        var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch return false;
        file.close(std.Options.debug_io);
        return true;
    }

    var file = std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

fn directoryExists(path: []const u8) bool {
    if (std.fs.path.isAbsolute(path)) {
        var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
        dir.close(std.Options.debug_io);
        return true;
    }

    var dir = std.Io.Dir.cwd().openDir(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}
