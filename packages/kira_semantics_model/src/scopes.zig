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
    /// Top-level fields moved out of this binding (`let x = obj.field` on an
    /// aliasing aggregate). A binding with any moved field cannot be used as a
    /// whole until the field is re-initialized (`obj.field = ...`), mirroring
    /// Rust's partial-move rules. Stored as field names because the move model
    /// tracks one level below the binding root.
    moved_fields: std.ArrayListUnmanaged([]const u8) = .empty,

    pub fn fieldMoved(self: *const LocalBinding, name: []const u8) bool {
        for (self.moved_fields.items) |field| {
            if (std.mem.eql(u8, field, name)) return true;
        }
        return false;
    }

    pub fn hasMovedFields(self: *const LocalBinding) bool {
        return self.moved_fields.items.len > 0;
    }

    pub fn markFieldMoved(self: *LocalBinding, allocator: std.mem.Allocator, name: []const u8) !void {
        if (self.fieldMoved(name)) return;
        try self.moved_fields.append(allocator, name);
    }

    pub fn clone(self: LocalBinding, allocator: std.mem.Allocator) !LocalBinding {
        var cloned = self;
        cloned.moved_fields = .empty;
        try cloned.moved_fields.appendSlice(allocator, self.moved_fields.items);
        return cloned;
    }

    pub fn deinit(self: *LocalBinding, allocator: std.mem.Allocator) void {
        self.moved_fields.deinit(allocator);
    }

    /// Re-initialize a moved field (`obj.field = ...`), making it whole again.
    pub fn clearFieldMoved(self: *LocalBinding, name: []const u8) void {
        var index: usize = 0;
        while (index < self.moved_fields.items.len) {
            if (std.mem.eql(u8, self.moved_fields.items[index], name)) {
                _ = self.moved_fields.swapRemove(index);
            } else {
                index += 1;
            }
        }
    }

    pub fn replaceMovedFields(self: *LocalBinding, allocator: std.mem.Allocator, fields: []const []const u8) !void {
        self.moved_fields.clearRetainingCapacity();
        try self.moved_fields.appendSlice(allocator, fields);
    }

    pub fn clearMoveState(self: *LocalBinding) void {
        self.moved = false;
        self.move_span = null;
        self.moved_fields.clearRetainingCapacity();
    }
};

pub const Scope = struct {
    entries: std.StringHashMapUnmanaged(LocalBinding) = .{},

    pub fn put(self: *Scope, allocator: std.mem.Allocator, name: []const u8, binding: LocalBinding) !void {
        try self.entries.put(allocator, name, binding);
    }

    pub fn get(self: Scope, name: []const u8) ?LocalBinding {
        return self.entries.get(name);
    }

    pub fn clone(self: Scope, allocator: std.mem.Allocator) !Scope {
        var cloned = Scope{};
        var iterator = self.entries.iterator();
        while (iterator.next()) |entry| {
            try cloned.put(allocator, entry.key_ptr.*, try entry.value_ptr.clone(allocator));
        }
        return cloned;
    }

    pub fn deinit(self: *Scope, allocator: std.mem.Allocator) void {
        var iterator = self.entries.valueIterator();
        while (iterator.next()) |binding| {
            binding.deinit(allocator);
        }
        self.entries.deinit(allocator);
    }
};
