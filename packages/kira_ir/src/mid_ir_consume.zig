//! Value and place consumption for the Mid IR ownership checker: walking a Value
//! tree and recording how each place is used (read/borrow/move/write/drop), plus the
//! reborrow-binding and call-argument handling. These operate on `*Checker` and live
//! here to keep `mid_ir_check.zig` focused on control-flow traversal and diagnostics.
const std = @import("std");
const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");
const place_algebra = @import("mid_ir_place.zig");
const state_mod = @import("mid_ir_state.zig");
const check = @import("mid_ir_check.zig");

const Checker = check.Checker;
const PathUseKind = check.PathUseKind;
const PathAccess = check.PathAccess;
const State = state_mod.State;

const resolvePlace = state_mod.resolvePlace;
const aliasAccessLocalId = state_mod.aliasAccessLocalId;
const clearMovedPaths = state_mod.clearMovedPaths;
const rootLocalId = place_algebra.rootLocalId;
const placeRelation = place_algebra.placeRelation;

pub fn consumeBindingValue(
    self: *Checker,
    state: *State,
    local: mid.Local,
    value: mid.Value,
    is_reborrow: bool,
    span: source_pkg.Span,
) anyerror!void {
    if (state.locals.getPtr(local.id)) |local_state| {
        local_state.alias_kind = .none;
        local_state.alias_place = null;
        local_state.moved_paths.clearRetainingCapacity();
    }
    if (is_reborrow) {
        const unresolved_source = self.rawPlaceForValue(value) orelse {
            try self.emitOwnershipDiagnostic(
                "KIR002",
                "reborrow must resolve to a place",
                "This local was lowered as a reborrow, but the source is not a stable place.",
                span,
                "Use a stable local, field, or element when creating a reborrow alias.",
            );
            return;
        };
        const source_place = resolvePlace(state, unresolved_source) orelse unresolved_source;
        try self.ensurePlaceLive(state, source_place, span);
        try self.ensureNoConflictingAccess(state, .{
            .place = source_place,
            .kind = .borrow_mut,
            .span = span,
            .ignore_alias_local_id = aliasAccessLocalId(state, unresolved_source),
        });
        const local_state = state.locals.getPtr(local.id) orelse return;
        local_state.alias_kind = .reborrow;
        local_state.alias_place = source_place;
        local_state.availability = .live;
        return;
    }

    if (self.bindingMoveSource(state, value)) |source_place| {
        try self.movePlace(state, source_place, span);
        const local_state = state.locals.getPtr(local.id) orelse return;
        local_state.availability = .live;
        return;
    }

    try self.consumeValue(state, value, .read);
    const local_state = state.locals.getPtr(local.id) orelse return;
    local_state.availability = .live;
}

pub fn consumeValue(self: *Checker, state: *State, value: mid.Value, mode: PathUseKind) anyerror!void {
    switch (value) {
        .integer, .float, .string, .boolean, .null_ptr, .function_ref, .namespace_ref => {},
        .place => |node| switch (mode) {
            .move => try self.movePlace(state, node.place, node.place.span),
            .borrow_mut => try self.borrowPlace(state, node.place, .borrow_mut, node.place.span),
            .borrow_shared => try self.borrowPlace(state, node.place, .borrow_shared, node.place.span),
            .drop => try self.dropPlace(state, node.place, node.place.span),
            .write => try self.writePlace(state, node.place, node.place.span),
            .read => {
                try self.ensurePlaceLive(state, node.place, node.place.span);
                try self.ensureNoConflictingAccess(state, .{
                    .place = node.place,
                    .kind = .read,
                    .span = node.place.span,
                    .ignore_alias_local_id = aliasAccessLocalId(state, node.place),
                });
            },
        },
        .call => |node| {
            try self.consumeCallArgs(state, node.args, node.param_ownership);
        },
        .virtual_call => |node| {
            try self.consumeValue(state, node.receiver.*, self.effectiveUseKind(node.receiver.*, node.receiver_ownership));
            try self.consumeCallArgs(state, node.args, node.param_ownership);
        },
        .call_value => |node| {
            try self.consumeValue(state, node.callee.*, .read);
            try self.consumeCallArgs(state, node.args, node.param_ownership);
        },
        .callback => |node| {
            for (node.captures) |capture| {
                const source_place = self.resolveLocalPlace(state, capture.source_local_id) orelse continue;
                try self.consumeCapture(state, source_place, capture, capture.span);
            }
        },
        .construct => |node| {
            for (node.fields) |field| try self.consumeValue(state, field.value, .read);
        },
        .construct_enum_variant => |node| {
            if (node.payload) |payload| try self.consumeValue(state, payload.*, .read);
        },
        .array => |node| {
            for (node.elements) |element| try self.consumeValue(state, element, .read);
        },
        .builder_array => |node| try self.consumeBuilderBlock(state, node.builder),
        .binary => |node| {
            try self.consumeValue(state, node.lhs.*, .read);
            try self.consumeValue(state, node.rhs.*, .read);
        },
        .unary => |node| try self.consumeValue(state, node.operand.*, .read),
        .cast => |node| try self.consumeValue(state, node.operand.*, .read),
        .conditional => |node| {
            try self.consumeValue(state, node.condition.*, .read);
            var then_state = try state.clone();
            defer then_state.deinit();
            try self.consumeValue(&then_state, node.then_value.*, mode);
            var else_state = try state.clone();
            defer else_state.deinit();
            try self.consumeValue(&else_state, node.else_value.*, mode);
            try self.joinState(state, &then_state, &else_state);
        },
        .native_state, .native_user_data, .native_recover, .c_string_to_string, .array_len, .string_len => |node| try self.consumeValue(state, node.inner.*, .read),
        .opaque_member => |node| try self.consumeValue(state, node.object.*, .read),
        .opaque_index => |node| {
            try self.consumeValue(state, node.object.*, .read);
            try self.consumeValue(state, node.index.*, .read);
        },
    }
}

pub fn consumeBuilderBlock(self: *Checker, state: *State, builder: mid.BuilderBlock) anyerror!void {
    for (builder.items) |item| {
        switch (item) {
            .expr => |value| try self.consumeValue(state, value.value, .read),
            .if_item => |value| {
                try self.consumeValue(state, value.condition, .read);
                var then_state = try state.clone();
                defer then_state.deinit();
                try self.consumeBuilderBlock(&then_state, value.then_block);
                var else_state = try state.clone();
                defer else_state.deinit();
                if (value.else_block) |else_block| try self.consumeBuilderBlock(&else_state, else_block);
                try self.joinState(state, &then_state, &else_state);
            },
            .for_item => |value| {
                try self.consumeValue(state, value.iterator, .read);
                if (state.locals.getPtr(value.binding.id)) |binding| {
                    binding.availability = .live;
                    binding.alias_kind = .none;
                    binding.alias_place = null;
                    binding.move_span = null;
                    binding.moved_paths.clearRetainingCapacity();
                }
                try self.consumeBuilderBlock(state, value.body);
                try self.dropExplicitLocals(state, &.{value.binding}, false);
            },
            .switch_item => |value| {
                try self.consumeValue(state, value.subject, .read);
                for (value.cases) |case_node| {
                    try self.consumeValue(state, case_node.pattern, .read);
                    try self.consumeBuilderBlock(state, case_node.body);
                }
                if (value.default_block) |default_block| try self.consumeBuilderBlock(state, default_block);
            },
        }
    }
}

pub fn consumeCallArgs(self: *Checker, state: *State, args: []const mid.Value, ownership: []const model.OwnershipMode) anyerror!void {
    var accesses = std.array_list.Managed(PathAccess).init(self.allocator);
    defer accesses.deinit();

    for (args, 0..) |arg, index| {
        // When a callee's per-argument ownership is unknown (e.g. a virtual
        // call whose signature lookup came back short), default to a shared
        // borrow rather than `.owned`. Guessing `.owned` would let the
        // by-value move rule invalidate an argument the callee only borrows,
        // producing false "moved before use" errors on borrowed parameters.
        const mode = if (index < ownership.len) ownership[index] else model.OwnershipMode.borrow_read;
        const use_kind = self.effectiveUseKind(arg, mode);
        if (self.placeForValue(state, arg)) |place| {
            try accesses.append(.{ .place = place, .kind = use_kind, .span = place.span });
        }
    }

    for (accesses.items, 0..) |access, outer| {
        for (accesses.items[outer + 1 ..]) |other| {
            try self.ensureAccessesCompatible(access, other);
        }
    }

    for (args, 0..) |arg, index| {
        // When a callee's per-argument ownership is unknown (e.g. a virtual
        // call whose signature lookup came back short), default to a shared
        // borrow rather than `.owned`. Guessing `.owned` would let the
        // by-value move rule invalidate an argument the callee only borrows,
        // producing false "moved before use" errors on borrowed parameters.
        const mode = if (index < ownership.len) ownership[index] else model.OwnershipMode.borrow_read;
        const use_kind = self.effectiveUseKind(arg, mode);
        try self.consumeValue(state, arg, use_kind);
    }
}

pub fn consumeCapture(self: *Checker, state: *State, source_place: mid.Place, capture: mid.Capture, span: source_pkg.Span) anyerror!void {
    const use_kind: PathUseKind = switch (capture.ownership) {
        .borrow_read => .borrow_shared,
        .borrow_mut => .borrow_mut,
        // A by-value `.copy` capture reads the (Copy) value into the closure
        // environment without consuming the source. Semantics only ever assigns
        // `.copy` ownership to trivially-copyable captures (see
        // lower_shared_captures.zig), so the original place stays usable and may
        // be captured by additional closures — the core of the callback pattern.
        .copy => .read,
        .move, .owned => .move,
    };
    try self.consumeValue(state, .{ .place = .{ .place = source_place } }, use_kind);
    _ = span;
}

pub fn placeForValue(self: *Checker, state: *State, value: mid.Value) ?mid.Place {
    _ = self;
    return switch (value) {
        .place => |node| resolvePlace(state, node.place),
        else => null,
    };
}

pub fn rawPlaceForValue(self: *Checker, value: mid.Value) ?mid.Place {
    _ = self;
    return switch (value) {
        .place => |node| node.place,
        else => null,
    };
}

pub fn bindingMoveSource(self: *Checker, state: *State, value: mid.Value) ?mid.Place {
    const place = self.placeForValue(state, value) orelse return null;
    return switch (place.ty.kind) {
        .array => place,
        // Fieldless enums are copied by value, so binding one does not move its
        // source; only payload-carrying enums transfer ownership on bind.
        .enum_instance => if (self.isCopyableType(place.ty)) null else place,
        else => null,
    };
}

pub fn movePlace(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
    const ignore_alias_local_id = aliasAccessLocalId(state, place);
    const resolved = resolvePlace(state, place) orelse place;
    try self.ensurePlaceLive(state, resolved, span);
    try self.ensureNoConflictingAccess(state, .{
        .place = resolved,
        .kind = .move,
        .span = span,
        .ignore_alias_local_id = ignore_alias_local_id,
    });
    if (resolved.root == .return_slot) return;
    const root_state = state.locals.getPtr(rootLocalId(resolved.root) orelse return) orelse return;
    if (resolved.projections.len == 0) {
        root_state.availability = .moved;
        root_state.move_span = span;
        root_state.moved_paths.clearRetainingCapacity();
        return;
    }
    try root_state.moved_paths.append(resolved);
    if (root_state.move_span == null) root_state.move_span = span;
}

pub fn borrowPlace(self: *Checker, state: *State, place: mid.Place, kind: PathUseKind, span: source_pkg.Span) anyerror!void {
    const ignore_alias_local_id = aliasAccessLocalId(state, place);
    const resolved = resolvePlace(state, place) orelse place;
    try self.ensurePlaceLive(state, resolved, span);
    try self.ensureNoConflictingAccess(state, .{
        .place = resolved,
        .kind = kind,
        .span = span,
        .ignore_alias_local_id = ignore_alias_local_id,
    });
}

pub fn writePlace(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
    const ignore_alias_local_id = aliasAccessLocalId(state, place);
    const resolved = resolvePlace(state, place) orelse place;
    try self.ensureNoConflictingAccess(state, .{
        .place = resolved,
        .kind = .write,
        .span = span,
        .ignore_alias_local_id = ignore_alias_local_id,
    });
    if (resolved.root == .return_slot) return;
    const local_id = rootLocalId(resolved.root) orelse return;
    const root_state = state.locals.getPtr(local_id) orelse return;
    if (resolved.projections.len == 0) {
        root_state.availability = .live;
        root_state.move_span = null;
        root_state.alias_kind = .none;
        root_state.alias_place = null;
        root_state.moved_paths.clearRetainingCapacity();
        return;
    }
    root_state.availability = .live;
    clearMovedPaths(root_state, resolved);
}

pub fn dropPlace(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
    const ignore_alias_local_id = aliasAccessLocalId(state, place);
    const resolved = resolvePlace(state, place) orelse place;
    try self.ensurePlaceLive(state, resolved, span);
    try self.ensureNoConflictingAccess(state, .{
        .place = resolved,
        .kind = .drop,
        .span = span,
        .ignore_alias_local_id = ignore_alias_local_id,
    });
    if (resolved.root == .return_slot) return;
    const local_id = rootLocalId(resolved.root) orelse return;
    const root_state = state.locals.getPtr(local_id) orelse return;
    if (resolved.projections.len == 0) {
        root_state.availability = .moved;
        root_state.moved_paths.clearRetainingCapacity();
    } else {
        try root_state.moved_paths.append(resolved);
    }
}

pub fn ensurePlaceLive(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
    if (place.root == .return_slot) return;
    const local_id = rootLocalId(place.root) orelse return;
    const root_state = state.locals.get(local_id) orelse return;
    const local_name = root_state.local.name;
    switch (root_state.availability) {
        .live => {},
        .uninitialized => {
            if (self.failed) return;
            const message = try std.fmt.allocPrint(
                self.allocator,
                "In `{s}`, `{s}` is used here but does not hold a live value on every path that reaches this point.",
                .{ self.function_decl.name, local_name },
            );
            try self.emitOwnershipDiagnostic(
                "KIR002",
                "place is not initialized on every path",
                message,
                span,
                "Initialize the value on every control-flow path before using it.",
            );
            return;
        },
        .moved, .maybe_moved => {
            if (self.failed) return;
            const message = try std.fmt.allocPrint(
                self.allocator,
                "In `{s}`, `{s}` is used here after it was moved or dropped earlier in this control-flow path.",
                .{ self.function_decl.name, local_name },
            );
            try self.emitOwnershipDiagnostic(
                "KIR002",
                "place is moved or dropped before this use",
                message,
                span,
                "Avoid reusing the place after moving it, or reinitialize it before the next use.",
            );
            return;
        },
    }
    for (root_state.moved_paths.items) |moved_place| {
        const relation = placeRelation(place, moved_place);
        switch (relation) {
            .same, .ancestor, .descendant, .overlap => {
                if (self.failed) return;
                const moved_desc = try self.describeMovedPaths(local_name, &.{moved_place});
                defer self.allocator.free(moved_desc);
                const message = try std.fmt.allocPrint(
                    self.allocator,
                    "In `{s}`, `{s}` is used here but its {s} was already moved out, so it is not fully available.",
                    .{ self.function_decl.name, local_name, moved_desc },
                );
                try self.emitOwnershipDiagnostic(
                    "KIR002",
                    "place is only partially live after a move",
                    message,
                    span,
                    "Reinitialize the moved path or keep using only disjoint siblings.",
                );
                return;
            },
            .disjoint => {},
        }
    }
}

pub fn ensureNoConflictingAccess(self: *Checker, state: *State, access: PathAccess) anyerror!void {
    var it = state.locals.iterator();
    while (it.next()) |entry| {
        if (access.ignore_alias_local_id) |ignored| {
            if (entry.key_ptr.* == ignored) continue;
        }
        const local_state = entry.value_ptr;
        if (local_state.alias_place) |alias_place| {
            const existing_kind: PathUseKind = switch (local_state.alias_kind) {
                .reborrow => .borrow_mut,
                .none => continue,
            };
            try self.ensureAccessesCompatible(access, .{ .place = alias_place, .kind = existing_kind, .span = local_state.local.span });
        }
    }
}

pub fn ensureAccessesCompatible(self: *Checker, lhs: PathAccess, rhs: PathAccess) anyerror!void {
    const relation = placeRelation(lhs.place, rhs.place);
    if (relation == .disjoint) return;
    if (lhs.kind == .read and rhs.kind == .read) return;
    if (lhs.kind == .borrow_shared and rhs.kind == .borrow_shared) return;
    if ((lhs.kind == .read and rhs.kind == .borrow_shared) or (lhs.kind == .borrow_shared and rhs.kind == .read)) return;
    try self.emitOwnershipDiagnostic(
        "KIR002",
        "overlapping place access is not executable safely",
        "Two overlapping moves, borrows, or writes would require aliasing or drop behavior that Kira has not proven safe in Mid IR.",
        lhs.span,
        "Split the aggregate into disjoint fields, move the whole value instead, or sequence the operations so the first borrow or move ends before the second begins.",
    );
}

test {
    std.testing.refAllDecls(@This());
}
