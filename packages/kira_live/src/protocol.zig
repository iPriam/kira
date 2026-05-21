const std = @import("std");

pub const LiveMessageKind = enum(u32) {
    hello = 1,
    runtime_info = 2,
    bundle_graph = 3,
    replace_bundle = 4,
    replace_module = 5,
    restart_required = 6,
    native_rebuild_started = 7,
    native_rebuild_finished = 8,
    diagnostics = 9,
    log_line = 10,
    heartbeat = 11,
    shutdown = 12,
    shutdown_ack = 13,
    reload_failed = 14,
};

pub const Frame = struct {
    kind: LiveMessageKind,
    payload: []const u8,
};

pub const ReplaceBundlePayload = struct {
    bundle_id: []const u8,
    files: []const FilePayload,

    pub const FilePayload = struct {
        relative_path: []const u8,
        bytes: []const u8,
    };
};

pub fn writeFrame(writer: anytype, kind: LiveMessageKind, payload: []const u8) !void {
    try writer.writeInt(u32, @intCast(payload.len), .little);
    try writer.writeInt(u32, @intFromEnum(kind), .little);
    try writer.writeAll(payload);
}

pub fn readFrame(allocator: std.mem.Allocator, reader: anytype) !Frame {
    const payload_len = try reader.takeInt(u32, .little);
    const raw_kind = try reader.takeInt(u32, .little);
    const payload = try allocator.alloc(u8, payload_len);
    try reader.readSliceAll(payload);
    return .{
        .kind = @enumFromInt(raw_kind),
        .payload = payload,
    };
}

pub fn encodeReplaceBundlePayload(
    allocator: std.mem.Allocator,
    bundle_id: []const u8,
    files: []const ReplaceBundlePayload.FilePayload,
) ![]u8 {
    var buffer: std.Io.Writer.Allocating = .init(allocator);
    defer buffer.deinit();
    const writer = &buffer.writer;
    try writeBytes(writer, bundle_id);
    try writer.writeInt(u32, @intCast(files.len), .little);
    for (files) |file| {
        try writeBytes(writer, file.relative_path);
        try writeBytes(writer, file.bytes);
    }
    return buffer.toOwnedSlice();
}

pub fn decodeReplaceBundlePayload(allocator: std.mem.Allocator, bytes: []const u8) !ReplaceBundlePayload {
    var reader_state = std.Io.Reader.fixed(bytes);
    const reader = &reader_state;
    const bundle_id = try readBytes(allocator, reader);
    const file_count = try reader.takeInt(u32, .little);
    const files = try allocator.alloc(ReplaceBundlePayload.FilePayload, file_count);
    for (files) |*file| {
        file.* = .{
            .relative_path = try readBytes(allocator, reader),
            .bytes = try readBytes(allocator, reader),
        };
    }
    return .{
        .bundle_id = bundle_id,
        .files = files,
    };
}

fn writeBytes(writer: anytype, bytes: []const u8) !void {
    try writer.writeInt(u32, @intCast(bytes.len), .little);
    try writer.writeAll(bytes);
}

fn readBytes(allocator: std.mem.Allocator, reader: anytype) ![]const u8 {
    const len = try reader.takeInt(u32, .little);
    const bytes = try allocator.alloc(u8, len);
    try reader.readSliceAll(bytes);
    return bytes;
}

test "replace bundle payload round-trips" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const payload = try encodeReplaceBundlePayload(
        arena.allocator(),
        "com.kira.ui_foundation",
        &.{
            .{ .relative_path = "KiraBundle.toml", .bytes = "bundle" },
            .{ .relative_path = "modules/app.main.kirbc", .bytes = "bc" },
        },
    );
    const decoded = try decodeReplaceBundlePayload(arena.allocator(), payload);
    try std.testing.expectEqualStrings("com.kira.ui_foundation", decoded.bundle_id);
    try std.testing.expectEqual(@as(usize, 2), decoded.files.len);
    try std.testing.expectEqualStrings("modules/app.main.kirbc", decoded.files[1].relative_path);
}
