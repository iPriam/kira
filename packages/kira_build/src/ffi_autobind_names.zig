const std = @import("std");

pub fn sanitizeIdentifier(name: []const u8) []const u8 {
    if (std.mem.eql(u8, name, "type")) return "type_value";
    if (std.mem.eql(u8, name, "class")) return "class_value";
    if (std.mem.eql(u8, name, "struct")) return "struct_value";
    if (std.mem.eql(u8, name, "annotation")) return "annotation_value";
    if (std.mem.eql(u8, name, "capability")) return "capability_value";
    if (std.mem.eql(u8, name, "function")) return "function_value";
    if (std.mem.eql(u8, name, "generated")) return "generated_value";
    if (std.mem.eql(u8, name, "overridable")) return "overridable_value";
    if (std.mem.eql(u8, name, "targets")) return "targets_value";
    if (std.mem.eql(u8, name, "uses")) return "uses_value";
    if (std.mem.eql(u8, name, "extends")) return "extends_value";
    if (std.mem.eql(u8, name, "override")) return "override_value";
    if (std.mem.eql(u8, name, "return")) return "return_value";
    if (std.mem.eql(u8, name, "switch")) return "switch_value";
    if (std.mem.eql(u8, name, "for")) return "for_value";
    if (std.mem.eql(u8, name, "if")) return "if_value";
    if (std.mem.eql(u8, name, "else")) return "else_value";
    if (std.mem.eql(u8, name, "let")) return "let_value";
    if (std.mem.eql(u8, name, "var")) return "var_value";
    if (std.mem.eql(u8, name, "import")) return "import_value";
    if (std.mem.eql(u8, name, "construct")) return "construct_value";
    return name;
}
