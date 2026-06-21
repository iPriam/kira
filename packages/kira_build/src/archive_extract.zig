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
