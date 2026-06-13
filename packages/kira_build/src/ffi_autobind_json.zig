const std = @import("std");

pub fn objectString(object: std.json.ObjectMap, key: []const u8) ?[]const u8 {
    const value = object.get(key) orelse return null;
    if (value != .string) return null;
    return value.string;
}

pub fn objectBool(object: std.json.ObjectMap, key: []const u8) bool {
    const value = object.get(key) orelse return false;
    return value == .bool and value.bool;
}

pub fn objectQualType(object: std.json.ObjectMap) ?[]const u8 {
    const type_value = object.get("type") orelse return null;
    if (type_value != .object) return null;
    return objectString(type_value.object, "qualType");
}

pub fn cloneStrings(allocator: std.mem.Allocator, values: []const []const u8) ![]const []const u8 {
    var list = std.array_list.Managed([]const u8).init(allocator);
    for (values) |value| {
        try list.append(try allocator.dupe(u8, value));
    }
    return list.toOwnedSlice();
}
