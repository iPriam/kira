const std = @import("std");
const Diagnostic = @import("diagnostic.zig").Diagnostic;
const Severity = @import("diagnostic.zig").Severity;
const SourceFile = @import("kira_source").SourceFile;
const Label = @import("label.zig").Label;

pub fn render(writer: anytype, source: *const SourceFile, diagnostic: Diagnostic) !void {
    try renderHeader(writer, diagnostic);
    try writer.print("  {s}\n", .{diagnostic.message});

    if (diagnostic.primaryLabel()) |label| {
        try renderPrimaryLabel(writer, source, label);

        for (diagnostic.labels) |secondary| {
            if (secondary.kind != .secondary) continue;
            try renderSecondaryLabel(writer, source, secondary);
        }
    } else {
        try writer.print("  --> {s}\n", .{source.path});
    }

    if (diagnostic.help) |help| {
        try writer.print("  help: {s}\n", .{help});
    }
    if (diagnostic.suggestion) |suggestion| {
        try writer.print("  suggestion: {s}\n", .{suggestion.message});
    }
    for (diagnostic.notes) |note| {
        try writer.print("  note: {s}\n", .{note});
    }
}

pub fn renderAll(writer: anytype, source: *const SourceFile, diagnostics: []const Diagnostic) !void {
    for (diagnostics, 0..) |diagnostic, index| {
        if (index > 0) try writer.writeByte('\n');
        try render(writer, source, diagnostic);
    }
}

fn renderPrimaryLabel(writer: anytype, fallback_source: *const SourceFile, label: Label) !void {
    if (try loadAlternateSourceForLabel(label, fallback_source)) |alternate_source| {
        var owned_source = alternate_source;
        defer owned_source.deinit();
        try renderPrimaryLabelWithSource(writer, &owned_source, label);
        return;
    }
    try renderPrimaryLabelWithSource(writer, fallback_source, label);
}

fn renderPrimaryLabelWithSource(writer: anytype, source: *const SourceFile, label: Label) !void {
    const clamped_start = @min(label.span.start, source.text.len);
    const clamped_end = @min(@max(label.span.end, clamped_start), source.text.len);
    const location = source.line_map.lineColumn(clamped_start);
    const line_index = source.line_map.lineIndex(clamped_start);
    const bounds = source.line_map.lineBounds(line_index, source.text);
    const safe_end = @max(bounds.end, bounds.start);
    const snippet = source.text[bounds.start..safe_end];
    const line_number = line_index + 1;
    const highlight_base = @min(@max(clamped_start, bounds.start), safe_end);
    const highlight_start = highlight_base - bounds.start;
    const line_end = safe_end;
    const requested_end = if (clamped_end > clamped_start) clamped_end else @min(clamped_start + 1, source.text.len);
    const highlight_end = @max(highlight_base, @min(requested_end, line_end));
    const caret_width = @max(@as(usize, 1), highlight_end - highlight_base);

    try writer.print("  --> {s}:{d}:{d}\n", .{ source.path, location.line, location.column });
    try writer.print("   {d} | {s}\n", .{ line_number, snippet });
    try writer.writeAll("     | ");
    try writeSpaces(writer, highlight_start);
    try writeCarets(writer, caret_width);
    if (label.message.len > 0) {
        try writer.print(" {s}", .{label.message});
    }
    try writer.writeByte('\n');
}

fn renderSecondaryLabel(writer: anytype, fallback_source: *const SourceFile, label: Label) !void {
    if (try loadAlternateSourceForLabel(label, fallback_source)) |alternate_source| {
        var owned_source = alternate_source;
        defer owned_source.deinit();
        const location = owned_source.line_map.lineColumn(@min(label.span.start, owned_source.text.len));
        try writer.print(
            "     = related: {s}:{d}:{d}: {s}\n",
            .{ owned_source.path, location.line, location.column, label.message },
        );
        return;
    }

    const location = fallback_source.line_map.lineColumn(@min(label.span.start, fallback_source.text.len));
    try writer.print(
        "     = related: {s}:{d}:{d}: {s}\n",
        .{ fallback_source.path, location.line, location.column, label.message },
    );
}

fn loadAlternateSourceForLabel(label: Label, fallback_source: *const SourceFile) !?SourceFile {
    const source_path = label.span.source_path orelse return null;
    if (std.mem.eql(u8, source_path, fallback_source.path)) return null;
    // A label may point at a span the compiler synthesized (e.g. macro-generated declarations),
    // whose `source_path` is not a real file on disk. Degrade to the fallback source for a
    // missing path rather than aborting the whole diagnostic with an internal FileNotFound — but
    // still surface a genuine read failure (permissions, I/O) instead of silently hiding it.
    return SourceFile.fromPath(fallback_source.allocator, source_path) catch |err| switch (err) {
        error.FileNotFound => null,
        else => err,
    };
}

fn renderHeader(writer: anytype, diagnostic: Diagnostic) !void {
    if (diagnostic.code) |code| {
        try writer.print("{s}[{s}]: {s}\n", .{ severityName(diagnostic.severity), code, diagnostic.title });
        return;
    }
    try writer.print("{s}: {s}\n", .{ severityName(diagnostic.severity), diagnostic.title });
}

fn severityName(severity: Severity) []const u8 {
    return switch (severity) {
        .@"error" => "error",
        .warning => "warning",
        .note => "note",
    };
}

fn writeSpaces(writer: anytype, count: usize) !void {
    for (0..count) |_| try writer.writeByte(' ');
}

fn writeCarets(writer: anytype, count: usize) !void {
    for (0..count) |_| try writer.writeByte('^');
}

test "renders labeled diagnostics with notes and help" {
    const diagnostics = @import("root.zig");
    const source_pkg = @import("kira_source");

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(allocator, "sample.kira", "function main() {\n  let x = ;\n}\n");
    const diagnostic = diagnostics.Diagnostic{
        .severity = .@"error",
        .code = "KPAR001",
        .title = "expected expression",
        .message = "Kira expected an expression after '='.",
        .labels = &.{
            diagnostics.primaryLabel(source_pkg.Span.init(28, 29), "expression expected here"),
        },
        .help = "Insert a value after '=' or remove the assignment.",
        .notes = &.{"Parsing stopped after this statement."},
    };

    var buffer: [1024]u8 = undefined;
    var stream = std.Io.Writer.fixed(&buffer);
    try render(&stream, &source, diagnostic);

    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "error[KPAR001]: expected expression") != null);
    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "sample.kira:2:11") != null);
    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "help: Insert a value after '=' or remove the assignment.") != null);
    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "note: Parsing stopped after this statement.") != null);
}

test "renders labels whose synthetic source path is not a real file" {
    // A label may carry a span the compiler synthesized (e.g. a macro-generated declaration),
    // whose source_path names no file on disk. Rendering must degrade to the primary source
    // instead of aborting the whole diagnostic with an internal FileNotFound.
    const diagnostics = @import("root.zig");
    const source_pkg = @import("kira_source");

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(allocator, "sample.kira", "function main() {\n}\n");
    const diagnostic = diagnostics.Diagnostic{
        .severity = .@"error",
        .code = "KSEM012",
        .title = "unknown local name",
        .message = "Kira could not find a local binding named 'x'.",
        .labels = &.{
            diagnostics.primaryLabel(
                // A syntactically valid but absent path (no OS-reserved characters) so
                // SourceFile.fromPath fails with error.FileNotFound on every platform, exercising
                // the intended missing-source fallback rather than an invalid-path error.
                source_pkg.Span.withSource(3, 4, "kira-macro-synthetic/does-not-exist.kira"),
                "unknown local name",
            ),
        },
    };

    var buffer: [1024]u8 = undefined;
    var stream = std.Io.Writer.fixed(&buffer);
    // Must not return error.FileNotFound; the diagnostic renders against the fallback source.
    try render(&stream, &source, diagnostic);

    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "error[KSEM012]: unknown local name") != null);
    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "unknown local name") != null);
}

test "renders zero-length eof labels without overflowing" {
    const diagnostics = @import("root.zig");
    const source_pkg = @import("kira_source");

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(allocator, "sample.kira", "function main() {\n}\n");
    const eof = source.text.len;
    const diagnostic = diagnostics.Diagnostic{
        .severity = .@"error",
        .code = "KSEM001",
        .title = "missing @Main entrypoint",
        .message = "This module cannot run because no function is marked with @Main.",
        .labels = &.{
            diagnostics.primaryLabel(source_pkg.Span.init(eof, eof), "file ends here"),
        },
    };

    var buffer: [1024]u8 = undefined;
    var stream = std.Io.Writer.fixed(&buffer);
    try render(&stream, &source, diagnostic);

    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "error[KSEM001]: missing @Main entrypoint") != null);
    try std.testing.expect(std.mem.indexOf(u8, stream.buffered(), "^ file ends here") != null);
}
