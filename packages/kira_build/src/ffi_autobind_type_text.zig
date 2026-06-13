const std = @import("std");

pub fn cleanCType(qual_type: []const u8) []const u8 {
    var text = std.mem.trim(u8, qual_type, " ");
    if (std.mem.indexOf(u8, text, " __attribute__((")) |attr_start| {
        text = std.mem.trimEnd(u8, text[0..attr_start], " ");
    }
    while (std.mem.endsWith(u8, text, "*const")) {
        text = std.mem.trimEnd(u8, text[0 .. text.len - "const".len], " ");
    }
    while (std.mem.endsWith(u8, text, " const")) {
        text = std.mem.trimEnd(u8, text[0 .. text.len - " const".len], " ");
    }
    return text;
}

pub fn isUnsupportedAnonymousAggregate(qual_type: []const u8) bool {
    const text = cleanCType(qual_type);
    if (!(std.mem.startsWith(u8, text, "union ") or std.mem.startsWith(u8, text, "struct "))) return false;
    return std.mem.indexOf(u8, text, "::(anonymous at") != null;
}

pub fn trimPointerTarget(qual_type: []const u8) []const u8 {
    var trimmed = cleanCType(qual_type);
    while (std.mem.endsWith(u8, trimmed, "*")) {
        trimmed = cleanCType(std.mem.trimEnd(u8, trimmed[0 .. trimmed.len - 1], " "));
    }
    if (std.mem.startsWith(u8, trimmed, "const ")) trimmed = trimmed["const ".len..];
    if (std.mem.startsWith(u8, trimmed, "struct ")) trimmed = trimmed["struct ".len..];
    return trimmed;
}
