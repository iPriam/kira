const std = @import("std");
const builtin = @import("builtin");
const llvm_metadata = @import("llvm_metadata.zig");

pub fn extractArchive(
    allocator: std.mem.Allocator,
    archive_path: []const u8,
    archive_format: llvm_metadata.ArchiveFormat,
    destination_path: []const u8,
) !void {
    var destination_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, destination_path, .{});
    defer destination_dir.close(std.Options.debug_io);

    switch (archive_format) {
        .zip => try extractZip(archive_path, destination_dir),
        .tar_gz => try extractTarGz(allocator, archive_path, destination_path),
        .tar_xz => try extractTarXz(allocator, archive_path, destination_path),
    }
}

fn extractZip(archive_path: []const u8, destination_dir: std.Io.Dir) !void {
    const file = try std.Io.Dir.openFileAbsolute(std.Options.debug_io, archive_path, .{});
    defer file.close(std.Options.debug_io);

    var file_buffer: [16 * 1024]u8 = undefined;
    var reader = file.reader(std.Options.debug_io, &file_buffer);
    try std.zip.extract(destination_dir, &reader, .{
        .allow_backslashes = true,
        .verify_checksums = false,
    });
}

fn extractTarXz(
    allocator: std.mem.Allocator,
    archive_path: []const u8,
    destination_path: []const u8,
) !void {
    const file = try std.Io.Dir.openFileAbsolute(std.Options.debug_io, archive_path, .{});
    defer file.close(std.Options.debug_io);

    var file_buffer: [16 * 1024]u8 = undefined;
    var file_reader = file.reader(std.Options.debug_io, &file_buffer);

    const decompress_buffer = try allocator.alloc(u8, 32 * 1024);
    var xz = try std.compress.xz.Decompress.init(&file_reader.interface, allocator, decompress_buffer);
    defer xz.deinit();

    var destination_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, destination_path, .{});
    defer destination_dir.close(std.Options.debug_io);

    try std.tar.extract(std.Options.debug_io, destination_dir, &xz.reader, .{});
}

fn extractTarGz(
    _: std.mem.Allocator,
    archive_path: []const u8,
    destination_path: []const u8,
) !void {
    var child = try std.process.spawn(std.Options.debug_io, .{
        .argv = &.{ "tar", "-xzf", archive_path, "-C", destination_path },
        .expand_arg0 = .expand,
        .stdin = .ignore,
        .stdout = .ignore,
        .stderr = .inherit,
    });
    const term = try child.wait(std.Options.debug_io);
    if (term == .exited and term.exited == 0) return;
    return error.ExternalCommandFailed;
}

test "extractTarGz extracts archive with system tar" {
    if (builtin.os.tag == .windows) return error.SkipZigTest;

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.createDirPath(std.testing.io, "source/payload");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "source/payload/hello.txt", .data = "hello from tar.gz" });
    try tmp.dir.createDirPath(std.testing.io, "out");
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "sample.tar.gz", .data = "" });

    const archive_path = try tmp.dir.realPathFileAlloc(std.testing.io, "sample.tar.gz", std.testing.allocator);
    defer std.testing.allocator.free(archive_path);
    const source_path = try tmp.dir.realPathFileAlloc(std.testing.io, "source", std.testing.allocator);
    defer std.testing.allocator.free(source_path);
    const output_path = try tmp.dir.realPathFileAlloc(std.testing.io, "out", std.testing.allocator);
    defer std.testing.allocator.free(output_path);

    var create_child = try std.process.spawn(std.Options.debug_io, .{
        .argv = &.{ "tar", "-czf", archive_path, "-C", source_path, "." },
        .expand_arg0 = .expand,
        .stdin = .ignore,
        .stdout = .ignore,
        .stderr = .inherit,
    });
    const create_term = try create_child.wait(std.Options.debug_io);
    try std.testing.expectEqual(@as(std.process.Child.Term, .{ .exited = 0 }), create_term);

    try extractTarGz(std.testing.allocator, archive_path, output_path);

    const extracted = try tmp.dir.readFileAlloc(std.testing.io, "out/payload/hello.txt", std.testing.allocator, .limited(64));
    defer std.testing.allocator.free(extracted);
    try std.testing.expectEqualStrings("hello from tar.gz", extracted);
}
