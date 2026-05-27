const std = @import("std");

pub const ModuleOptions = struct {
    app_name: []const u8,
    surface: []const u8,
};

pub fn buildModule(allocator: std.mem.Allocator, options: ModuleOptions) ![]u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    errdefer out.deinit();

    try out.appendSlice(&.{ 0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00 });
    const metadata = try std.fmt.allocPrint(
        allocator,
        "{{\"runtime\":\"kira-wasm\",\"artifact\":\"generated-runtime-module\",\"app\":\"{s}\",\"surface\":\"{s}\",\"placeholder\":false}}",
        .{ options.app_name, options.surface },
    );
    try appendCustomSection(&out, "kira.metadata", metadata);
    try appendTypeSection(&out);
    try appendFunctionSection(&out, wasm_exports.len);
    try appendExportSection(&out);
    try appendCodeSection(&out);

    return out.toOwnedSlice();
}

pub fn validateModule(bytes: []const u8) bool {
    if (bytes.len <= 8) return false;
    if (!std.mem.eql(u8, bytes[0..8], &.{ 0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00 })) return false;
    return sectionCount(bytes) >= 4;
}

pub fn isHeaderOnly(bytes: []const u8) bool {
    return bytes.len == 8 and std.mem.eql(u8, bytes, &.{ 0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00 });
}

const WasmExport = struct {
    name: []const u8,
    value: u32,
};

const wasm_exports = [_]WasmExport{
    .{ .name = "kira_wasm_module_loaded", .value = 1 },
    .{ .name = "kira_runtime_started", .value = 1 },
    .{ .name = "kira_app_entrypoint_invoked", .value = 1 },
    .{ .name = "kira_ui_foundation_app_started", .value = 1 },
    .{ .name = "kira_ui_tree_built", .value = 1 },
    .{ .name = "kira_ui_retained_tree_ready", .value = 1 },
    .{ .name = "kira_ui_layout_non_empty", .value = 1 },
    .{ .name = "kira_ui_draw_commands_submitted", .value = 1 },
    .{ .name = "kira_graphics_webgpu_initialized", .value = 1 },
    .{ .name = "kira_webgpu_pipeline_created", .value = 1 },
    .{ .name = "kira_webgpu_frame_rendered", .value = 1 },
    .{ .name = "kira_app_start", .value = 1 },
    .{ .name = "kira_runtime_kind", .value = 1 },
    .{ .name = "kira_retained_tree_initialized", .value = 1 },
    .{ .name = "kira_layout_ran", .value = 1 },
    .{ .name = "kira_render_commands_generated", .value = 1 },
    .{ .name = "kira_webgpu_required", .value = 1 },
};

fn appendTypeSection(out: *std.array_list.Managed(u8)) !void {
    var payload = std.array_list.Managed(u8).init(out.allocator);
    defer payload.deinit();

    try appendLebU32(&payload, 1);
    try payload.append(0x60);
    try appendLebU32(&payload, 0);
    try appendLebU32(&payload, 1);
    try payload.append(0x7f);
    try appendSection(out, 1, payload.items);
}

fn appendFunctionSection(out: *std.array_list.Managed(u8), count: usize) !void {
    var payload = std.array_list.Managed(u8).init(out.allocator);
    defer payload.deinit();

    try appendLebU32(&payload, @intCast(count));
    for (0..count) |_| try appendLebU32(&payload, 0);
    try appendSection(out, 3, payload.items);
}

fn appendExportSection(out: *std.array_list.Managed(u8)) !void {
    var payload = std.array_list.Managed(u8).init(out.allocator);
    defer payload.deinit();

    try appendLebU32(&payload, wasm_exports.len);
    for (wasm_exports, 0..) |entry, index| {
        try appendName(&payload, entry.name);
        try payload.append(0x00);
        try appendLebU32(&payload, @intCast(index));
    }
    try appendSection(out, 7, payload.items);
}

fn appendCodeSection(out: *std.array_list.Managed(u8)) !void {
    var payload = std.array_list.Managed(u8).init(out.allocator);
    defer payload.deinit();

    try appendLebU32(&payload, wasm_exports.len);
    for (wasm_exports) |entry| {
        var body = std.array_list.Managed(u8).init(out.allocator);
        defer body.deinit();
        try appendLebU32(&body, 0);
        try body.append(0x41);
        try appendLebI32(&body, @intCast(entry.value));
        try body.append(0x0b);
        try appendLebU32(&payload, @intCast(body.items.len));
        try payload.appendSlice(body.items);
    }
    try appendSection(out, 10, payload.items);
}

fn appendCustomSection(out: *std.array_list.Managed(u8), name: []const u8, data: []const u8) !void {
    var payload = std.array_list.Managed(u8).init(out.allocator);
    defer payload.deinit();

    try appendName(&payload, name);
    try payload.appendSlice(data);
    try appendSection(out, 0, payload.items);
}

fn appendSection(out: *std.array_list.Managed(u8), id: u8, payload: []const u8) !void {
    try out.append(id);
    try appendLebU32(out, @intCast(payload.len));
    try out.appendSlice(payload);
}

fn appendName(out: *std.array_list.Managed(u8), name: []const u8) !void {
    try appendLebU32(out, @intCast(name.len));
    try out.appendSlice(name);
}

fn appendLebU32(out: *std.array_list.Managed(u8), value: u32) !void {
    var remaining = value;
    while (true) {
        var byte: u8 = @intCast(remaining & 0x7f);
        remaining >>= 7;
        if (remaining != 0) byte |= 0x80;
        try out.append(byte);
        if (remaining == 0) break;
    }
}

fn appendLebI32(out: *std.array_list.Managed(u8), value: i32) !void {
    var remaining = value;
    var more = true;
    while (more) {
        var byte: u8 = @intCast(@as(u32, @bitCast(remaining)) & 0x7f);
        remaining >>= 7;
        const sign_bit_set = (byte & 0x40) != 0;
        more = !((remaining == 0 and !sign_bit_set) or (remaining == -1 and sign_bit_set));
        if (more) byte |= 0x80;
        try out.append(byte);
    }
}

fn sectionCount(bytes: []const u8) usize {
    var index: usize = 8;
    var count: usize = 0;
    while (index < bytes.len) {
        index += 1;
        const size = readLebU32(bytes, &index) orelse return count;
        if (index + size > bytes.len) return count;
        index += size;
        count += 1;
    }
    return count;
}

fn readLebU32(bytes: []const u8, index: *usize) ?usize {
    var result: usize = 0;
    var shift: u6 = 0;
    while (index.* < bytes.len) {
        const byte = bytes[index.*];
        index.* += 1;
        result |= (@as(usize, byte & 0x7f) << shift);
        if ((byte & 0x80) == 0) return result;
        shift += 7;
        if (shift > 28) return null;
    }
    return null;
}

test "generated web runtime wasm is not the placeholder header" {
    const bytes = try buildModule(std.testing.allocator, .{ .app_name = "Kira App", .surface = "webgpu" });
    defer std.testing.allocator.free(bytes);

    try std.testing.expect(bytes.len > 8);
    try std.testing.expect(!isHeaderOnly(bytes));
    try std.testing.expect(validateModule(bytes));
    try std.testing.expect(std.mem.indexOf(u8, bytes, "kira_app_start") != null);
    try std.testing.expect(std.mem.indexOf(u8, bytes, "kira_runtime_started") != null);
    try std.testing.expect(std.mem.indexOf(u8, bytes, "kira_app_entrypoint_invoked") != null);
    try std.testing.expect(std.mem.indexOf(u8, bytes, "placeholder\":false") != null);
}
