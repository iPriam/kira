const std = @import("std");
const source_pkg = @import("kira_source");
const types = @import("types.zig");
const ResolvedType = types.ResolvedType;
const OwnershipMode = types.OwnershipMode;
const FieldStorage = @import("hir.zig").FieldStorage;

pub const LocalBinding = struct {
    id: u32,
    ty: ResolvedType,
    storage: FieldStorage,
    ownership: OwnershipMode = .owned,
    initialized: bool = true,
    moved: bool = false,
    move_span: ?source_pkg.Span = null,
    decl_span: source_pkg.Span,
};

pub const Scope = struct {
    entries: std.StringHashMapUnmanaged(LocalBinding) = .{},

    pub fn put(self: *Scope, allocator: std.mem.Allocator, name: []const u8, binding: LocalBinding) !void {
        try self.entries.put(allocator, name, binding);
    }

    pub fn get(self: Scope, name: []const u8) ?LocalBinding {
        return self.entries.get(name);
    }

    pub fn deinit(self: *Scope, allocator: std.mem.Allocator) void {
        self.entries.deinit(allocator);
    }
};
