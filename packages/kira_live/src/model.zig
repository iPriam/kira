const std = @import("std");
const RunnerKind = @import("runner_kind.zig").RunnerKind;

pub const BundleSpec = struct {
    id: []const u8,
    package_name: []const u8,
    package_root: []const u8,
    version: []const u8,
    kind: []const u8,
    module_root: []const u8,
    manifest_rel_path: []const u8,
    bytecode_rel_path: []const u8,
    hybrid_rel_path: []const u8,
    executable: bool,
    validation_root: []const u8,
};

pub const BundleGraph = struct {
    target_path: []const u8,
    target_package: []const u8,
    validation_app_path: []const u8,
    main_bundle_id: []const u8,
    bundles: []const BundleSpec,

    pub fn writeToml(self: BundleGraph, writer: anytype) !void {
        try writer.writeAll("[target]\n");
        try writer.print("path = \"{s}\"\n", .{self.target_path});
        try writer.print("package = \"{s}\"\n", .{self.target_package});
        try writer.print("validation_app = \"{s}\"\n", .{self.validation_app_path});
        try writer.print("main_bundle = \"{s}\"\n", .{self.main_bundle_id});
        for (self.bundles) |bundle| {
            try writer.writeAll("\n[[bundle]]\n");
            try writeBundleSpec(writer, bundle);
        }
    }
};

pub const BundleManifest = struct {
    id: []const u8,
    package_name: []const u8,
    version: []const u8,
    kind: []const u8,
    module_root: []const u8,
    bytecode_rel_path: []const u8,
    hybrid_rel_path: []const u8,
    executable: bool,

    pub fn writeToml(self: BundleManifest, writer: anytype) !void {
        try writer.writeAll("[bundle]\n");
        try writer.print("id = \"{s}\"\n", .{self.id});
        try writer.print("package = \"{s}\"\n", .{self.package_name});
        try writer.print("version = \"{s}\"\n", .{self.version});
        try writer.print("kind = \"{s}\"\n", .{self.kind});
        try writer.print("module_root = \"{s}\"\n", .{self.module_root});
        try writer.print("executable = {s}\n", .{if (self.executable) "true" else "false"});
        try writer.writeAll("\n[paths]\n");
        try writer.print("bytecode = \"{s}\"\n", .{self.bytecode_rel_path});
        try writer.print("hybrid = \"{s}\"\n", .{self.hybrid_rel_path});
        try writer.writeAll("assets = \"assets\"\n");
    }

    pub fn parse(allocator: std.mem.Allocator, text: []const u8) !BundleManifest {
        var manifest = BundleManifest{
            .id = "",
            .package_name = "",
            .version = "0.1.0",
            .kind = "library",
            .module_root = "",
            .bytecode_rel_path = "",
            .hybrid_rel_path = "",
            .executable = false,
        };
        var section: []const u8 = "";
        var lines = std.mem.splitScalar(u8, text, '\n');
        while (lines.next()) |raw_line| {
            const line = trimComment(raw_line);
            if (line.len == 0) continue;
            if (isSectionHeader(line)) {
                section = line[1 .. line.len - 1];
                continue;
            }
            const kv = try splitKeyValue(line);
            if (std.mem.eql(u8, section, "bundle")) {
                if (std.mem.eql(u8, kv.key, "id")) manifest.id = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "package")) manifest.package_name = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "version")) manifest.version = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "kind")) manifest.kind = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "module_root")) manifest.module_root = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "executable")) manifest.executable = std.mem.eql(u8, kv.value, "true");
                continue;
            }
            if (std.mem.eql(u8, section, "paths")) {
                if (std.mem.eql(u8, kv.key, "bytecode")) manifest.bytecode_rel_path = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "hybrid")) manifest.hybrid_rel_path = try parseOwnedString(allocator, kv.value);
            }
        }
        return manifest;
    }
};

pub const RunnerManifest = struct {
    kind: RunnerKind,
    name: []const u8,
    bundle_id: []const u8,
    version: []const u8,
    target_path: []const u8,
    package_name: []const u8,
    validation_app_path: []const u8,
    bundles_path: []const u8,
    local_cache_path: []const u8,
    main_bundle_id: []const u8,
    server_host: []const u8,
    server_port: u16,
    native_contract_hash: []const u8,

    pub fn writeToml(self: RunnerManifest, writer: anytype) !void {
        try writer.writeAll("[runtime]\n");
        try writer.print("kind = \"{s}\"\n", .{self.kind.manifestName()});
        try writer.print("name = \"{s}\"\n", .{self.name});
        try writer.print("bundle_id = \"{s}\"\n", .{self.bundle_id});
        try writer.print("version = \"{s}\"\n", .{self.version});
        try writer.writeAll("\n[target]\n");
        try writer.print("path = \"{s}\"\n", .{self.target_path});
        try writer.print("package = \"{s}\"\n", .{self.package_name});
        try writer.print("validation_app = \"{s}\"\n", .{self.validation_app_path});
        try writer.writeAll("\n[paths]\n");
        try writer.print("bundles = \"{s}\"\n", .{self.bundles_path});
        try writer.print("local_cache = \"{s}\"\n", .{self.local_cache_path});
        try writer.print("main_bundle = \"{s}\"\n", .{self.main_bundle_id});
        try writer.writeAll("\n[abi]\n");
        try writer.writeAll("bytecode = 1\n");
        try writer.writeAll("hostcall = 1\n");
        try writer.print("native_contract_hash = \"{s}\"\n", .{self.native_contract_hash});
        try writer.writeAll("\n[server]\n");
        try writer.print("host = \"{s}\"\n", .{self.server_host});
        try writer.print("port = {d}\n", .{self.server_port});
    }

    pub fn parse(allocator: std.mem.Allocator, text: []const u8) !RunnerManifest {
        var manifest = RunnerManifest{
            .kind = .desktop_dynamic_host,
            .name = "",
            .bundle_id = "",
            .version = "0.1.0",
            .target_path = "",
            .package_name = "",
            .validation_app_path = "",
            .bundles_path = "",
            .local_cache_path = "",
            .main_bundle_id = "",
            .server_host = "127.0.0.1",
            .server_port = 0,
            .native_contract_hash = "",
        };
        var section: []const u8 = "";
        var lines = std.mem.splitScalar(u8, text, '\n');
        while (lines.next()) |raw_line| {
            const line = trimComment(raw_line);
            if (line.len == 0) continue;
            if (isSectionHeader(line)) {
                section = line[1 .. line.len - 1];
                continue;
            }
            const kv = try splitKeyValue(line);
            if (std.mem.eql(u8, section, "runtime")) {
                if (std.mem.eql(u8, kv.key, "kind")) manifest.kind = RunnerKind.parse(try parseOwnedString(allocator, kv.value)) orelse return error.InvalidManifest;
                if (std.mem.eql(u8, kv.key, "name")) manifest.name = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "bundle_id")) manifest.bundle_id = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "version")) manifest.version = try parseOwnedString(allocator, kv.value);
                continue;
            }
            if (std.mem.eql(u8, section, "target")) {
                if (std.mem.eql(u8, kv.key, "path")) manifest.target_path = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "package")) manifest.package_name = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "validation_app")) manifest.validation_app_path = try parseOwnedString(allocator, kv.value);
                continue;
            }
            if (std.mem.eql(u8, section, "paths")) {
                if (std.mem.eql(u8, kv.key, "bundles")) manifest.bundles_path = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "local_cache")) manifest.local_cache_path = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "main_bundle")) manifest.main_bundle_id = try parseOwnedString(allocator, kv.value);
                continue;
            }
            if (std.mem.eql(u8, section, "abi")) {
                if (std.mem.eql(u8, kv.key, "native_contract_hash")) manifest.native_contract_hash = try parseOwnedString(allocator, kv.value);
                continue;
            }
            if (std.mem.eql(u8, section, "server")) {
                if (std.mem.eql(u8, kv.key, "host")) manifest.server_host = try parseOwnedString(allocator, kv.value);
                if (std.mem.eql(u8, kv.key, "port")) manifest.server_port = try std.fmt.parseInt(u16, kv.value, 10);
            }
        }
        return manifest;
    }
};

fn writeBundleSpec(writer: anytype, bundle: BundleSpec) !void {
    try writer.print("id = \"{s}\"\n", .{bundle.id});
    try writer.print("package = \"{s}\"\n", .{bundle.package_name});
    try writer.print("package_root = \"{s}\"\n", .{bundle.package_root});
    try writer.print("version = \"{s}\"\n", .{bundle.version});
    try writer.print("kind = \"{s}\"\n", .{bundle.kind});
    try writer.print("module_root = \"{s}\"\n", .{bundle.module_root});
    try writer.print("manifest = \"{s}\"\n", .{bundle.manifest_rel_path});
    try writer.print("bytecode = \"{s}\"\n", .{bundle.bytecode_rel_path});
    try writer.print("hybrid = \"{s}\"\n", .{bundle.hybrid_rel_path});
    try writer.print("validation_root = \"{s}\"\n", .{bundle.validation_root});
    try writer.print("executable = {s}\n", .{if (bundle.executable) "true" else "false"});
}

fn isSectionHeader(line: []const u8) bool {
    return line.len >= 3 and line[0] == '[' and line[line.len - 1] == ']';
}

fn splitKeyValue(line: []const u8) !struct { key: []const u8, value: []const u8 } {
    const index = std.mem.indexOfScalar(u8, line, '=') orelse return error.InvalidManifest;
    return .{
        .key = std.mem.trim(u8, line[0..index], " \t\r"),
        .value = std.mem.trim(u8, line[index + 1 ..], " \t\r"),
    };
}

fn parseOwnedString(allocator: std.mem.Allocator, value: []const u8) ![]const u8 {
    if (value.len < 2 or value[0] != '"' or value[value.len - 1] != '"') return error.InvalidManifest;
    return allocator.dupe(u8, value[1 .. value.len - 1]);
}

fn trimComment(raw_line: []const u8) []const u8 {
    const line = std.mem.trim(u8, raw_line, " \t\r");
    if (line.len == 0) return "";
    if (line[0] == '#') return "";
    return line;
}

test "RunnerManifest round-trips core fields" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var buffer: [4096]u8 = undefined;
    var writer = std.Io.Writer.fixed(&buffer);
    const manifest = RunnerManifest{
        .kind = .xcode_ios,
        .name = "UiFoundationLiveRunner",
        .bundle_id = "com.kira.live.ui-foundation",
        .version = "0.1.0",
        .target_path = "/tmp/ui-foundation",
        .package_name = "KiraUIFoundation",
        .validation_app_path = "/tmp/ui-foundation/Examples/basic-foundation-app",
        .bundles_path = "../../bundles",
        .local_cache_path = "Resources/live-cache",
        .main_bundle_id = "com.kira.basic_foundation_app",
        .server_host = "127.0.0.1",
        .server_port = 4242,
        .native_contract_hash = "abc123",
    };
    try manifest.writeToml(&writer);
    const parsed = try RunnerManifest.parse(arena.allocator(), writer.buffered());
    try std.testing.expectEqual(RunnerKind.xcode_ios, parsed.kind);
    try std.testing.expectEqualStrings("UiFoundationLiveRunner", parsed.name);
    try std.testing.expectEqualStrings("com.kira.basic_foundation_app", parsed.main_bundle_id);
    try std.testing.expectEqual(@as(u16, 4242), parsed.server_port);
}
