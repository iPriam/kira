const std = @import("std");

pub fn main(init: std.process.Init.Minimal) !void {
    var gpa = std.heap.DebugAllocator(.{}){};
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    const args = try init.args.toSlice(allocator);
    if (args.len != 3) return error.InvalidArguments;

    const root = args[1];
    const port = try std.fmt.parseInt(u16, args[2], 10);
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{});
    defer io_impl.deinit();
    const io = io_impl.io();

    const bind_address = try std.Io.net.IpAddress.parse("127.0.0.1", port);
    var server = try std.Io.net.IpAddress.listen(&bind_address, io, .{
        .reuse_address = true,
        .mode = .stream,
        .protocol = .tcp,
    });
    defer server.deinit(io);

    while (true) {
        var stream = try server.accept(io);
        serveOne(allocator, root, io, stream) catch {};
        stream.close(io);
    }
}

fn serveOne(allocator: std.mem.Allocator, root: []const u8, io: std.Io, stream: std.Io.net.Stream) !void {
    var request_buffer: [4096]u8 = undefined;
    var reader = std.Io.net.Stream.Reader.init(stream, io, &request_buffer);
    const read_len = try reader.interface.readSliceShort(&request_buffer);
    if (read_len == 0) return;
    const request = request_buffer[0..read_len];
    const first_line_end = std.mem.indexOfScalar(u8, request, '\n') orelse request.len;
    const first_line = std.mem.trim(u8, request[0..first_line_end], "\r");
    var parts = std.mem.splitScalar(u8, first_line, ' ');
    const method = parts.next() orelse "";
    const raw_path = parts.next() orelse "/";
    var writer_buffer: [4096]u8 = undefined;
    var writer = std.Io.net.Stream.Writer.init(stream, io, &writer_buffer);
    if (!std.mem.eql(u8, method, "GET")) return writeStatus(&writer.interface, "405 Method Not Allowed", "method not allowed\n");
    if (std.mem.indexOf(u8, raw_path, "..") != null) return writeStatus(&writer.interface, "403 Forbidden", "forbidden\n");

    const rel_path = if (std.mem.eql(u8, raw_path, "/")) "index.html" else if (raw_path.len != 0 and raw_path[0] == '/') raw_path[1..] else raw_path;
    const file_path = try std.fs.path.join(allocator, &.{ root, rel_path });
    defer allocator.free(file_path);
    const data = std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, file_path, allocator, .limited(32 * 1024 * 1024)) catch |err| switch (err) {
        error.FileNotFound => return writeStatus(&writer.interface, "404 Not Found", "not found\n"),
        else => return err,
    };
    defer allocator.free(data);

    const content_type = contentType(file_path);
    var header_buffer: [256]u8 = undefined;
    const header = try std.fmt.bufPrint(&header_buffer, "HTTP/1.1 200 OK\r\ncontent-length: {d}\r\ncontent-type: {s}\r\nconnection: close\r\n\r\n", .{ data.len, content_type });
    try writer.interface.writeAll(header);
    try writer.interface.writeAll(data);
    try writer.interface.flush();
}

fn writeStatus(writer: *std.Io.Writer, status: []const u8, body: []const u8) !void {
    var header_buffer: [256]u8 = undefined;
    const header = try std.fmt.bufPrint(&header_buffer, "HTTP/1.1 {s}\r\ncontent-length: {d}\r\ncontent-type: text/plain\r\nconnection: close\r\n\r\n", .{ status, body.len });
    try writer.writeAll(header);
    try writer.writeAll(body);
    try writer.flush();
}

fn contentType(path: []const u8) []const u8 {
    if (std.mem.endsWith(u8, path, ".html")) return "text/html; charset=utf-8";
    if (std.mem.endsWith(u8, path, ".js")) return "text/javascript; charset=utf-8";
    if (std.mem.endsWith(u8, path, ".wasm")) return "application/wasm";
    if (std.mem.endsWith(u8, path, ".json")) return "application/json; charset=utf-8";
    return "application/octet-stream";
}
