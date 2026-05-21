const std = @import("std");
const model = @import("kira_semantics_model");

pub const Signature = struct {
    params: []const model.ResolvedType,
    param_ownership: []const model.OwnershipMode,
    result: model.ResolvedType,
};

pub fn signatureText(
    allocator: std.mem.Allocator,
    params: []const model.ResolvedType,
    param_ownership: []const model.OwnershipMode,
    result: model.ResolvedType,
) ![]const u8 {
    var builder = std.array_list.Managed(u8).init(allocator);
    try builder.append('(');
    for (params, 0..) |param, index| {
        if (index != 0) try builder.appendSlice(", ");
        if (index < param_ownership.len) {
            try builder.appendSlice(ownershipPrefix(param_ownership[index]));
        }
        try builder.appendSlice(typeText(param));
    }
    try builder.appendSlice(") -> ");
    try builder.appendSlice(typeText(result));
    return builder.toOwnedSlice();
}

pub fn parseSignature(allocator: std.mem.Allocator, ty: model.ResolvedType) !?Signature {
    if (ty.kind != .callback or ty.name == null) return null;
    var parser = Parser{
        .allocator = allocator,
        .text = ty.name.?,
    };
    return try parser.parseFunctionType(true);
}

fn typeText(ty: model.ResolvedType) []const u8 {
    if (ty.name) |name| return name;
    return switch (ty.kind) {
        .void => "Void",
        .integer => "Int",
        .float => "Float",
        .boolean => "Bool",
        .string => "String",
        .c_string => "CString",
        .raw_ptr => "RawPtr",
        .callback => "Callback",
        .ffi_struct => "Struct",
        .named, .enum_instance => "Type",
        .construct_any => ty.name orelse "any Unknown",
        .array => "[]",
        .native_state => "NativeState",
        .native_state_view => "NativeStateView",
        .unknown => "Unknown",
    };
}

const Parser = struct {
    allocator: std.mem.Allocator,
    text: []const u8,
    index: usize = 0,

    fn parseFunctionType(self: *Parser, require_eof: bool) anyerror!Signature {
        self.skipSpaces();
        try self.expect('(');

        var params = std.array_list.Managed(model.ResolvedType).init(self.allocator);
        var param_ownership = std.array_list.Managed(model.OwnershipMode).init(self.allocator);
        self.skipSpaces();
        if (!self.peekIs(')')) {
            while (true) {
                const parsed = try self.parseTypeWithOwnership();
                try params.append(parsed.ty);
                try param_ownership.append(parsed.ownership);
                self.skipSpaces();
                if (!self.match(',')) break;
                self.skipSpaces();
            }
        }
        try self.expect(')');
        self.skipSpaces();
        if (!self.match('-') or !self.match('>')) return error.InvalidFunctionTypeSignature;
        self.skipSpaces();
        const result = (try self.parseTypeWithOwnership()).ty;
        self.skipSpaces();
        if (require_eof and self.index != self.text.len) return error.InvalidFunctionTypeSignature;
        return .{
            .params = try params.toOwnedSlice(),
            .param_ownership = try param_ownership.toOwnedSlice(),
            .result = result,
        };
    }

    fn parseTypeWithOwnership(self: *Parser) anyerror!struct { ty: model.ResolvedType, ownership: model.OwnershipMode } {
        self.skipSpaces();
        if (self.tryKeyword("borrow")) {
            self.skipSpaces();
            const ownership: model.OwnershipMode = if (self.tryKeyword("mut")) .borrow_mut else .borrow_read;
            self.skipSpaces();
            return .{ .ty = try self.parseType(), .ownership = ownership };
        }
        if (self.tryKeyword("move")) {
            self.skipSpaces();
            return .{ .ty = try self.parseType(), .ownership = .move };
        }
        if (self.tryKeyword("copy")) {
            self.skipSpaces();
            return .{ .ty = try self.parseType(), .ownership = .copy };
        }
        return .{ .ty = try self.parseType(), .ownership = .owned };
    }

    fn parseType(self: *Parser) anyerror!model.ResolvedType {
        self.skipSpaces();
        if (self.peekIs('(')) {
            const signature = try self.parseFunctionType(false);
            return .{
                .kind = .callback,
                .name = try signatureText(self.allocator, signature.params, signature.param_ownership, signature.result),
            };
        }

        if (self.match('[')) {
            const element = try self.parseType();
            self.skipSpaces();
            try self.expect(']');
            return .{ .kind = .array, .name = try typeTextOwned(self.allocator, element) };
        }

        const start = self.index;
        while (self.index < self.text.len) : (self.index += 1) {
            const ch = self.text[self.index];
            if (ch == ',' or ch == ')' or ch == ']' or std.ascii.isWhitespace(ch)) break;
        }
        if (self.index == start) return error.InvalidFunctionTypeSignature;
        const name = self.text[start..self.index];
        return resolvedPrimitiveOrNamed(self.allocator, name);
    }

    fn skipSpaces(self: *Parser) void {
        while (self.index < self.text.len and std.ascii.isWhitespace(self.text[self.index])) : (self.index += 1) {}
    }

    fn match(self: *Parser, ch: u8) bool {
        if (!self.peekIs(ch)) return false;
        self.index += 1;
        return true;
    }

    fn peekIs(self: *Parser, ch: u8) bool {
        return self.index < self.text.len and self.text[self.index] == ch;
    }

    fn tryKeyword(self: *Parser, keyword: []const u8) bool {
        if (self.index + keyword.len > self.text.len) return false;
        if (!std.mem.eql(u8, self.text[self.index .. self.index + keyword.len], keyword)) return false;
        const end = self.index + keyword.len;
        if (end < self.text.len and !std.ascii.isWhitespace(self.text[end]) and self.text[end] != '(' and self.text[end] != '[') return false;
        self.index = end;
        return true;
    }

    fn expect(self: *Parser, ch: u8) !void {
        if (!self.match(ch)) return error.InvalidFunctionTypeSignature;
    }
};

fn typeTextOwned(allocator: std.mem.Allocator, ty: model.ResolvedType) ![]const u8 {
    return switch (ty.kind) {
        .callback => allocator.dupe(u8, ty.name orelse "Callback"),
        .array => std.fmt.allocPrint(allocator, "[{s}]", .{ty.name orelse ""}),
        else => allocator.dupe(u8, typeText(ty)),
    };
}

fn ownershipPrefix(mode: model.OwnershipMode) []const u8 {
    return switch (mode) {
        .owned => "",
        .borrow_read => "borrow ",
        .borrow_mut => "borrow mut ",
        .move => "move ",
        .copy => "copy ",
    };
}

fn resolvedPrimitiveOrNamed(allocator: std.mem.Allocator, name: []const u8) !model.ResolvedType {
    if (std.mem.eql(u8, name, "Void")) return .{ .kind = .void };
    if (std.mem.eql(u8, name, "Int")) return .{ .kind = .integer };
    if (std.mem.eql(u8, name, "Float")) return .{ .kind = .float };
    if (std.mem.eql(u8, name, "Bool")) return .{ .kind = .boolean };
    if (std.mem.eql(u8, name, "String")) return .{ .kind = .string };
    if (std.mem.eql(u8, name, "CString")) return .{ .kind = .c_string, .name = "CString" };
    if (std.mem.eql(u8, name, "RawPtr")) return .{ .kind = .raw_ptr, .name = "RawPtr" };
    if (std.mem.eql(u8, name, "Unknown")) return .{ .kind = .unknown };
    return .{ .kind = .named, .name = try allocator.dupe(u8, name) };
}
