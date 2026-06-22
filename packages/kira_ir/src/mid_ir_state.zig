//! Per-path dataflow state for the Mid IR ownership checker: the availability and
//! move/alias status of each local, plus the small state transforms (join, scope
//! reset, moved-path maintenance) and place resolution through reborrow aliases.
const std = @import("std");
const source_pkg = @import("kira_source");
const mid = @import("mid_ir.zig");
const place_algebra = @import("mid_ir_place.zig");

pub const LocalAvailability = enum {
    uninitialized,
    live,
    moved,
    maybe_moved,
};

pub const AliasKind = enum {
    none,
    reborrow,
};

pub const LocalState = struct {
    local: mid.Local,
    availability: LocalAvailability = .uninitialized,
    alias_kind: AliasKind = .none,
    alias_place: ?mid.Place = null,
    move_span: ?source_pkg.Span = null,
    moved_paths: std.array_list.Managed(mid.Place) = undefined,

    pub fn init(allocator: std.mem.Allocator, local: mid.Local, initially_live: bool) LocalState {
        return .{
            .local = local,
            .availability = if (initially_live) .live else .uninitialized,
            .moved_paths = std.array_list.Managed(mid.Place).init(allocator),
        };
    }

    pub fn clone(self: LocalState, allocator: std.mem.Allocator) !LocalState {
        var cloned = self;
        cloned.moved_paths = std.array_list.Managed(mid.Place).init(allocator);
        try cloned.moved_paths.appendSlice(self.moved_paths.items);
        return cloned;
    }

    pub fn deinit(self: *LocalState) void {
        self.moved_paths.deinit();
    }
};

pub const State = struct {
    allocator: std.mem.Allocator,
    locals: std.AutoHashMapUnmanaged(u32, LocalState) = .{},

    pub fn init(allocator: std.mem.Allocator) State {
        return .{ .allocator = allocator };
    }

    pub fn clone(self: *const State) !State {
        var cloned = State.init(self.allocator);
        var it = self.locals.iterator();
        while (it.next()) |entry| {
            try cloned.locals.put(self.allocator, entry.key_ptr.*, try entry.value_ptr.clone(self.allocator));
        }
        return cloned;
    }

    pub fn deinit(self: *State) void {
        var it = self.locals.iterator();
        while (it.next()) |entry| entry.value_ptr.deinit();
        self.locals.deinit(self.allocator);
    }

    pub fn putLocal(self: *State, local: mid.Local, initially_live: bool) !void {
        try self.locals.put(self.allocator, local.id, LocalState.init(self.allocator, local, initially_live));
    }
};

pub fn resolvePlace(state: *State, place: mid.Place) ?mid.Place {
    const local_id = place_algebra.rootLocalId(place.root) orelse return place;
    const local_state = state.locals.get(local_id) orelse return place;
    const alias_place = local_state.alias_place orelse return place;
    var projections = std.array_list.Managed(mid.Projection).init(state.allocator);
    defer projections.deinit();
    projections.appendSlice(alias_place.projections) catch return alias_place;
    projections.appendSlice(place.projections) catch return alias_place;
    return .{
        .root = alias_place.root,
        .projections = projections.toOwnedSlice() catch alias_place.projections,
        .ty = place.ty,
        .span = place.span,
    };
}

pub fn aliasAccessLocalId(state: *State, place: mid.Place) ?u32 {
    const local_id = place_algebra.rootLocalId(place.root) orelse return null;
    const local_state = state.locals.get(local_id) orelse return null;
    return if (local_state.alias_kind != .none) local_id else null;
}

pub fn joinAvailability(lhs: LocalAvailability, rhs: LocalAvailability) LocalAvailability {
    if (lhs == rhs) return lhs;
    if (lhs == .live and rhs == .uninitialized) return .maybe_moved;
    if (lhs == .uninitialized and rhs == .live) return .maybe_moved;
    if (lhs == .moved or rhs == .moved) return .maybe_moved;
    if (lhs == .maybe_moved or rhs == .maybe_moved) return .maybe_moved;
    return .maybe_moved;
}

pub fn scopedLocalIds(allocator: std.mem.Allocator, block: mid.Block) []u32 {
    var items = std.array_list.Managed(u32).init(allocator);
    for (block.statements) |statement| {
        switch (statement) {
            .let_stmt => |node| items.append(node.local.id) catch {},
            .for_stmt => |node| items.append(node.binding.id) catch {},
            else => {},
        }
    }
    return items.toOwnedSlice() catch &.{};
}

pub fn clearMovedPaths(root_state: *LocalState, assigned_place: mid.Place) void {
    var index: usize = 0;
    while (index < root_state.moved_paths.items.len) {
        const moved_place = root_state.moved_paths.items[index];
        const relation = place_algebra.placeRelation(moved_place, assigned_place);
        switch (relation) {
            .same, .ancestor, .descendant => _ = root_state.moved_paths.swapRemove(index),
            .disjoint, .overlap => index += 1,
        }
    }
}

pub fn resetScopedLocal(local_state: *LocalState) void {
    local_state.availability = .uninitialized;
    local_state.alias_kind = .none;
    local_state.alias_place = null;
    local_state.move_span = null;
    local_state.moved_paths.clearRetainingCapacity();
}

test {
    std.testing.refAllDecls(@This());
}
