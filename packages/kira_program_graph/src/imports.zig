const std = @import("std");
const package_manager = @import("kira_package_manager");
const syntax = @import("kira_syntax_model");
const paths = @import("paths.zig");

pub const ImportResolution = struct {
    display_name: []u8,
    candidates: [][]u8,
    exists: bool,
};

pub fn resolveImportPath(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    module_name: syntax.ast.QualifiedName,
    module_map: package_manager.ModuleMap,
) !ImportResolution {
    const display_name = try qualifiedNameDisplay(allocator, module_name);
    var candidates_list = std.array_list.Managed([]u8).init(allocator);

    if (bestOwnerForImport(module_map, module_name) == null) {
        try appendLocalModuleCandidates(allocator, source_path, module_name, &candidates_list);
    }

    if (bestOwnerForImport(module_map, module_name)) |owner| {
        const relative_slash = try qualifiedRelativeAfterPrefix(allocator, owner.module_root, module_name, '/');
        defer allocator.free(relative_slash);
        const relative_backslash = try qualifiedRelativeAfterPrefix(allocator, owner.module_root, module_name, '\\');
        defer allocator.free(relative_backslash);
        try appendRootedModuleCandidates(allocator, &candidates_list, owner.source_root, relative_slash, '/');
        try appendRootedModuleCandidates(allocator, &candidates_list, owner.source_root, relative_backslash, '\\');
    }

    const candidates = try candidates_list.toOwnedSlice();
    return .{
        .display_name = display_name,
        .candidates = candidates,
        .exists = firstExistingCandidate(candidates) != null,
    };
}

pub fn packageRootOwnerForImport(
    module_map: package_manager.ModuleMap,
    module_name: syntax.ast.QualifiedName,
) ?package_manager.ModuleMap.ModuleOwner {
    for (module_map.owners) |owner| {
        const depth = qualifiedPrefixDepth(owner.module_root, module_name);
        if (depth == 0) continue;
        if (depth == module_name.segments.len) return owner;
    }
    return null;
}

pub fn firstExistingCandidate(candidates: [][]u8) ?[]const u8 {
    for (candidates) |candidate| {
        if (paths.fileExists(candidate)) return candidate;
    }
    return null;
}

pub fn resolvedCandidateNotes(allocator: std.mem.Allocator, candidates: [][]u8) ![]const []const u8 {
    const notes = try allocator.alloc([]const u8, candidates.len);
    for (candidates, 0..) |candidate, index| {
        notes[index] = try std.fmt.allocPrint(allocator, "looked for {s}", .{candidate});
    }
    return notes;
}

pub fn qualifiedNameDisplay(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName) ![]u8 {
    return joinQualifiedName(allocator, name, ".");
}

pub fn ownerForSourcePath(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    module_map: package_manager.ModuleMap,
) !?package_manager.ModuleMap.ModuleOwner {
    if (module_map.owners.len == 0) return null;

    const canonical_source = try paths.canonicalizeExistingPath(allocator, source_path);
    defer allocator.free(canonical_source);

    for (module_map.owners) |owner| {
        const canonical_root = try paths.canonicalizeSourceRoot(allocator, owner.source_root);
        defer allocator.free(canonical_root);
        if (paths.pathWithinRoot(canonical_source, canonical_root)) return owner;

        const bindings_root = try paths.bindingsRootForSourceRoot(allocator, canonical_root);
        defer allocator.free(bindings_root);
        if (paths.pathWithinRoot(canonical_source, bindings_root)) return owner;
    }
    return null;
}

pub fn sourcePathIsAllowed(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    module_map: package_manager.ModuleMap,
) !bool {
    if (module_map.owners.len == 0) return true;
    return (try ownerForSourcePath(allocator, source_path, module_map)) != null;
}

fn bestOwnerForImport(
    module_map: package_manager.ModuleMap,
    module_name: syntax.ast.QualifiedName,
) ?package_manager.ModuleMap.ModuleOwner {
    var best_owner: ?package_manager.ModuleMap.ModuleOwner = null;
    var best_depth: usize = 0;
    for (module_map.owners) |owner| {
        const depth = qualifiedPrefixDepth(owner.module_root, module_name);
        if (depth == 0) continue;
        if (depth > best_depth) {
            best_depth = depth;
            best_owner = owner;
        }
    }
    return best_owner;
}

fn appendRootedModuleCandidates(
    allocator: std.mem.Allocator,
    candidates: *std.array_list.Managed([]u8),
    source_root: []const u8,
    relative: []const u8,
    comptime separator: u8,
) !void {
    if (relative.len == 0) {
        if (separator == '/') {
            try candidates.append(try std.fmt.allocPrint(allocator, "{s}/main.kira", .{source_root}));
            try candidates.append(try std.fmt.allocPrint(allocator, "{s}/*.kira", .{source_root}));
        } else {
            try candidates.append(try std.fmt.allocPrint(allocator, "{s}\\main.kira", .{source_root}));
            try candidates.append(try std.fmt.allocPrint(allocator, "{s}\\*.kira", .{source_root}));
        }
        return;
    }

    if (separator == '/') {
        try candidates.append(try std.fmt.allocPrint(allocator, "{s}/{s}.kira", .{ source_root, relative }));
        try candidates.append(try std.fmt.allocPrint(allocator, "{s}/{s}/main.kira", .{ source_root, relative }));
    } else {
        try candidates.append(try std.fmt.allocPrint(allocator, "{s}\\{s}.kira", .{ source_root, relative }));
        try candidates.append(try std.fmt.allocPrint(allocator, "{s}\\{s}\\main.kira", .{ source_root, relative }));
    }
}

fn appendLocalModuleCandidates(
    allocator: std.mem.Allocator,
    source_path: []const u8,
    module_name: syntax.ast.QualifiedName,
    candidates: *std.array_list.Managed([]u8),
) !void {
    const source_dir = std.fs.path.dirname(source_path) orelse return;

    const relative_slash = try qualifiedNameRelativePath(allocator, module_name, '/');
    defer allocator.free(relative_slash);
    const relative_backslash = try qualifiedNameRelativePath(allocator, module_name, '\\');
    defer allocator.free(relative_backslash);

    try appendRootedModuleCandidates(allocator, candidates, source_dir, relative_slash, '/');
    try appendRootedModuleCandidates(allocator, candidates, source_dir, relative_backslash, '\\');
}

fn qualifiedPrefixDepth(prefix: []const u8, module_name: syntax.ast.QualifiedName) usize {
    var parts = std.mem.splitScalar(u8, prefix, '.');
    var depth: usize = 0;
    while (parts.next()) |part| {
        if (depth >= module_name.segments.len) return 0;
        if (!std.mem.eql(u8, part, module_name.segments[depth].text)) return 0;
        depth += 1;
    }
    return depth;
}

fn qualifiedRelativeAfterPrefix(
    allocator: std.mem.Allocator,
    prefix: []const u8,
    module_name: syntax.ast.QualifiedName,
    comptime separator: u8,
) ![]u8 {
    const depth = qualifiedPrefixDepth(prefix, module_name);
    if (depth == 0 or depth > module_name.segments.len) return error.InvalidArguments;
    if (depth == module_name.segments.len) return allocator.dupe(u8, "");

    var builder = std.array_list.Managed(u8).init(allocator);
    for (module_name.segments[depth..], 0..) |segment, index| {
        if (index != 0) try builder.append(separator);
        try builder.appendSlice(segment.text);
    }
    return builder.toOwnedSlice();
}

fn qualifiedNameRelativePath(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName, comptime separator: u8) ![]u8 {
    const sep = [_]u8{separator};
    return joinQualifiedName(allocator, name, &sep);
}

fn joinQualifiedName(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName, separator: []const u8) ![]u8 {
    var builder = std.array_list.Managed(u8).init(allocator);
    for (name.segments, 0..) |segment, index| {
        if (index != 0) try builder.appendSlice(separator);
        try builder.appendSlice(segment.text);
    }
    return builder.toOwnedSlice();
}

test "same-package import resolves sibling app files from source directory" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Project/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Project/app/main.kira", .data = "" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Project/app/support.kira", .data = "" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Project/app", allocator);
    defer allocator.free(app_root);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Project/app/main.kira", allocator);
    defer allocator.free(source_path);

    var segments = [_]syntax.ast.NameSegment{.{ .text = "support", .span = .{ .start = 0, .end = 7 } }};
    const module_name: syntax.ast.QualifiedName = .{ .segments = segments[0..], .span = .{ .start = 0, .end = 7 } };
    const owners = [_]package_manager.ModuleMap.ModuleOwner{.{
        .module_root = "App",
        .package_name = "App",
        .source_root = app_root,
    }};

    const resolved = try resolveImportPath(allocator, source_path, module_name, .{ .owners = owners[0..] });
    defer allocator.free(resolved.display_name);
    defer {
        for (resolved.candidates) |candidate| allocator.free(candidate);
        allocator.free(resolved.candidates);
    }

    try std.testing.expect(resolved.exists);
    try std.testing.expect(resolved.candidates.len >= 2);
}

test "same-package import resolves inside canonical app root from source directory" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Package/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/app/main.kira", .data = "" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/app/support.kira", .data = "" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app", allocator);
    defer allocator.free(app_root);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app/main.kira", allocator);
    defer allocator.free(source_path);

    var segments = [_]syntax.ast.NameSegment{.{ .text = "support", .span = .{ .start = 0, .end = 7 } }};
    const module_name: syntax.ast.QualifiedName = .{ .segments = segments[0..], .span = .{ .start = 0, .end = 7 } };
    const owners = [_]package_manager.ModuleMap.ModuleOwner{.{
        .module_root = "Package",
        .package_name = "Package",
        .source_root = app_root,
    }};

    const resolved = try resolveImportPath(allocator, source_path, module_name, .{ .owners = owners[0..] });
    defer allocator.free(resolved.display_name);
    defer {
        for (resolved.candidates) |candidate| allocator.free(candidate);
        allocator.free(resolved.candidates);
    }

    try std.testing.expect(resolved.exists);
    try std.testing.expect(resolved.candidates.len >= 2);
}

test "local import ignores package root outside app" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "Package/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/app/main.kira", .data = "" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Package/support.kira", .data = "" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app", allocator);
    defer allocator.free(app_root);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "Package/app/main.kira", allocator);
    defer allocator.free(source_path);

    var segments = [_]syntax.ast.NameSegment{.{ .text = "support", .span = .{ .start = 0, .end = 7 } }};
    const module_name: syntax.ast.QualifiedName = .{ .segments = segments[0..], .span = .{ .start = 0, .end = 7 } };
    const owners = [_]package_manager.ModuleMap.ModuleOwner{.{
        .module_root = "Package",
        .package_name = "Package",
        .source_root = app_root,
    }};

    const resolved = try resolveImportPath(allocator, source_path, module_name, .{ .owners = owners[0..] });
    defer allocator.free(resolved.display_name);
    defer {
        for (resolved.candidates) |candidate| allocator.free(candidate);
        allocator.free(resolved.candidates);
    }

    try std.testing.expect(!resolved.exists);
}

test "dependency import ignores dependency root outside app" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "App/app");
    try tmp.dir.createDirPath(std.testing.io, "Dep/app");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "App/app/main.kira", .data = "" });
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "Dep/Helper.kira", .data = "" });

    const app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app", allocator);
    defer allocator.free(app_root);
    const dep_app_root = try tmp.dir.realPathFileAlloc(std.testing.io, "Dep/app", allocator);
    defer allocator.free(dep_app_root);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "App/app/main.kira", allocator);
    defer allocator.free(source_path);

    var segments = [_]syntax.ast.NameSegment{
        .{ .text = "Dep", .span = .{ .start = 0, .end = 3 } },
        .{ .text = "Helper", .span = .{ .start = 4, .end = 10 } },
    };
    const module_name: syntax.ast.QualifiedName = .{ .segments = segments[0..], .span = .{ .start = 0, .end = 10 } };
    const owners = [_]package_manager.ModuleMap.ModuleOwner{
        .{ .module_root = "App", .package_name = "App", .source_root = app_root },
        .{ .module_root = "Dep", .package_name = "Dep", .source_root = dep_app_root },
    };

    const resolved = try resolveImportPath(allocator, source_path, module_name, .{ .owners = owners[0..] });
    defer allocator.free(resolved.display_name);
    defer {
        for (resolved.candidates) |candidate| allocator.free(candidate);
        allocator.free(resolved.candidates);
    }

    try std.testing.expect(!resolved.exists);
}
