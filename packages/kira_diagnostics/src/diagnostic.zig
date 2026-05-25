const std = @import("std");
const Label = @import("label.zig").Label;

pub const Severity = enum {
    @"error",
    warning,
    note,
};

pub const Suggestion = struct {
    message: []const u8,
};

pub const Diagnostic = struct {
    severity: Severity,
    title: []const u8,
    message: []const u8,
    code: ?[]const u8 = null,
    domain: ?[]const u8 = null,
    phase: ?[]const u8 = null,
    labels: []const Label = &.{},
    notes: []const []const u8 = &.{},
    help: ?[]const u8 = null,
    suggestion: ?Suggestion = null,

    pub fn primaryLabel(self: Diagnostic) ?Label {
        for (self.labels) |label| {
            if (label.kind == .primary) return label;
        }
        return if (self.labels.len > 0) self.labels[0] else null;
    }
};

pub fn single(severity: Severity, message: []const u8, label: Label) Diagnostic {
    return .{
        .severity = severity,
        .title = message,
        .message = message,
        .labels = &.{label},
    };
}

pub fn hasErrors(items: []const Diagnostic) bool {
    for (items) |item| {
        if (item.severity == .@"error") return true;
    }
    return false;
}

pub fn appendOwned(
    allocator: std.mem.Allocator,
    list: *std.array_list.Managed(Diagnostic),
    diagnostic: Diagnostic,
) !void {
    const owned_labels = if (diagnostic.labels.len == 0)
        &.{}
    else
        try allocator.dupe(Label, diagnostic.labels);
    const owned_notes = if (diagnostic.notes.len == 0)
        &.{}
    else
        try allocator.dupe([]const u8, diagnostic.notes);

    try list.append(.{
        .severity = diagnostic.severity,
        .title = diagnostic.title,
        .message = diagnostic.message,
        .code = diagnostic.code,
        .domain = diagnostic.domain,
        .phase = diagnostic.phase,
        .labels = owned_labels,
        .notes = owned_notes,
        .help = diagnostic.help,
        .suggestion = diagnostic.suggestion,
    });
}

test "appendOwned copies label and note slices" {
    const source_pkg = @import("kira_source");

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var list = std.array_list.Managed(Diagnostic).init(allocator);

    var label_storage = [_]Label{
        .{
            .kind = .primary,
            .span = source_pkg.Span.init(0, 0),
            .message = "copied label",
        },
    };
    var note_storage = [_][]const u8{"copied note"};

    try appendOwned(allocator, &list, .{
        .severity = .@"error",
        .title = "owned diagnostic",
        .message = "diagnostic message",
        .labels = label_storage[0..],
        .notes = note_storage[0..],
    });

    label_storage[0].message = "mutated label";
    note_storage[0] = "mutated note";

    try std.testing.expectEqualStrings("copied label", list.items[0].labels[0].message);
    try std.testing.expectEqualStrings("copied note", list.items[0].notes[0]);
}
