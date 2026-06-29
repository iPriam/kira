const std = @import("std");

pub const DirectStdoutWriter = struct {
    pub fn writeAll(_: DirectStdoutWriter, bytes: []const u8) !void {
        var buffer: [1024]u8 = undefined;
        var writer = std.Io.File.stdout().writerStreaming(std.Options.debug_io, &buffer);
        try writer.interface.writeAll(bytes);
        // Propagate the flush error: a swallowed flush makes a write look successful even when
        // the buffered bytes never reached the stream.
        try writer.interface.flush();
    }

    pub fn writeByte(self: DirectStdoutWriter, byte: u8) !void {
        _ = self;
        if (@import("builtin").os.tag == .windows and byte == '\n') {
            try DirectStdoutWriter.writeAll(.{}, "\r\n");
            return;
        }
        var buffer = [1]u8{byte};
        try DirectStdoutWriter.writeAll(.{}, &buffer);
    }

    pub fn print(self: DirectStdoutWriter, comptime fmt: []const u8, args: anytype) !void {
        var buffer: [512]u8 = undefined;
        const rendered = try std.fmt.bufPrint(&buffer, fmt, args);
        try self.writeAll(rendered);
    }
};
