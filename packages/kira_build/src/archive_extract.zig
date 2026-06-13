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

    const decompress_buffer = try allocator.alloc(u8, 32 * 1024);
    defer allocator.free(decompress_buffer);
    var gz = std.compress.flate.Decompress.init(&file_reader.interface, .gzip, decompress_buffer);

    var destination_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, destination_path, .{});
    defer destination_dir.close(std.Options.debug_io);

    try std.tar.extract(std.Options.debug_io, destination_dir, &gz.reader, .{});
}
