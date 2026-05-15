const std = @import("std");

pub const Type = enum {
    void,
    integer,
    float,
    boolean,
    string,
    c_string,
    raw_ptr,
    callback,
    ffi_struct,
    named,
    enum_instance,
    construct_any,
    array,
    native_state,
    native_state_view,
    unknown,
};

pub const OwnershipMode = enum {
    owned,
    borrow_read,
    borrow_mut,
    move,
    copy,
};

pub const ConstructConstraint = struct {
    construct_name: []const u8,

    pub fn eql(self: ConstructConstraint, other: ConstructConstraint) bool {
        return std.mem.eql(u8, self.construct_name, other.construct_name);
    }
};

pub const ResolvedType = struct {
    kind: Type,
    name: ?[]const u8 = null,
    construct_constraint: ?ConstructConstraint = null,

    pub fn eql(self: ResolvedType, other: ResolvedType) bool {
        if (self.kind != other.kind) return false;
        if (self.construct_constraint) |constraint| {
            const other_constraint = other.construct_constraint orelse return false;
            if (!constraint.eql(other_constraint)) return false;
        } else if (other.construct_constraint != null) {
            return false;
        }
        const require_exact_name = switch (self.kind) {
            .named, .enum_instance, .ffi_struct, .callback, .construct_any, .array, .native_state, .native_state_view => true,
            else => false,
        };
        if (self.name == null and other.name == null) return true;
        if (self.name == null or other.name == null) return !require_exact_name;
        return std.mem.eql(u8, self.name.?, other.name.?);
    }

    pub fn plain(kind: Type) ResolvedType {
        return .{ .kind = kind };
    }
};
