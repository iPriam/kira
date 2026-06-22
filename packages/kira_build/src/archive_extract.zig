const std = @import("std");
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
    allocator: std.mem.Allocator,
    archive_path: []const u8,
    destination_path: []const u8,
) !void {
    const file = try std.Io.Dir.openFileAbsolute(std.Options.debug_io, archive_path, .{});
    defer file.close(std.Options.debug_io);

    var file_buffer: [16 * 1024]u8 = undefined;
    var file_reader = file.reader(std.Options.debug_io, &file_buffer);

    // flate asserts the window buffer is at least `max_window_len` bytes; a
    // smaller buffer caused the earlier in-process attempt to be abandoned for
    // a `tar` subprocess, which in turn returned OutOfMemory in CI.
    const decompress_buffer = try allocator.alloc(u8, std.compress.flate.max_window_len);
    defer allocator.free(decompress_buffer);
    var gz = std.compress.flate.Decompress.init(&file_reader.interface, .gzip, decompress_buffer);

    var destination_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, destination_path, .{});
    defer destination_dir.close(std.Options.debug_io);

    try std.tar.extract(std.Options.debug_io, destination_dir, &gz.reader, .{});
}

test "extractTarGz extracts a gzip tarball in-process" {
    // Build a small gzip tarball entirely in-process so the regression test
    // exercises the real decompression path on every platform without relying
    // on an external `tar` binary.
    var window: [std.compress.flate.max_window_len]u8 = undefined;
    var archive_storage: [16 * 1024]u8 = undefined;
    var archive_writer = std.Io.Writer.fixed(&archive_storage);
    var compress = try std.compress.flate.Compress.init(&archive_writer, &window, .gzip, .default);

    var tar_writer: std.tar.Writer = .{ .underlying_writer = &compress.writer };
    try tar_writer.writeFileBytes("payload/hello.txt", "hello from tar.gz", .{});
    try tar_writer.finishPedantically();
    try compress.finish();

    const archive_bytes = archive_writer.buffered();

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();

    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "sample.tar.gz", .data = archive_bytes });
    try tmp.dir.createDirPath(std.testing.io, "out");

    const archive_path = try tmp.dir.realPathFileAlloc(std.testing.io, "sample.tar.gz", std.testing.allocator);
    defer std.testing.allocator.free(archive_path);
    const output_path = try tmp.dir.realPathFileAlloc(std.testing.io, "out", std.testing.allocator);
    defer std.testing.allocator.free(output_path);

    try extractTarGz(std.testing.allocator, archive_path, output_path);

    const extracted = try tmp.dir.readFileAlloc(std.testing.io, "out/payload/hello.txt", std.testing.allocator, .limited(64));
    defer std.testing.allocator.free(extracted);
    try std.testing.expectEqualStrings("hello from tar.gz", extracted);
}
