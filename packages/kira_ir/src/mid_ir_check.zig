const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");

pub fn checkProgram(
    allocator: std.mem.Allocator,
    program: mid.Program,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !?mid.CheckedProgram {
    for (program.functions) |function_decl| {
        if (function_decl.is_extern) continue;
        var checker = Checker.init(allocator, program, function_decl, out_diagnostics);
        try checker.checkFunction();
        if (checker.failed) return null;
    }
    return .{ .program = program };
}

const Control = enum {
    next,
    break_loop,
    continue_loop,
    returned,
};

const LocalAvailability = enum {
    uninitialized,
    live,
    moved,
    maybe_moved,
};

const AliasKind = enum {
    none,
    reborrow,
};

const PlaceRelation = enum {
    same,
    ancestor,
    descendant,
    disjoint,
    overlap,
};

const PathUseKind = enum {
    read,
    borrow_shared,
    borrow_mut,
    move,
    write,
    drop,
};

const PathAccess = struct {
    place: mid.Place,
    kind: PathUseKind,
    span: source_pkg.Span,
    ignore_alias_local_id: ?u32 = null,
};

const LocalState = struct {
    local: mid.Local,
    availability: LocalAvailability = .uninitialized,
    alias_kind: AliasKind = .none,
    alias_place: ?mid.Place = null,
    move_span: ?source_pkg.Span = null,
    moved_paths: std.array_list.Managed(mid.Place) = undefined,

    fn init(allocator: std.mem.Allocator, local: mid.Local, initially_live: bool) LocalState {
        return .{
            .local = local,
            .availability = if (initially_live) .live else .uninitialized,
            .moved_paths = std.array_list.Managed(mid.Place).init(allocator),
        };
    }

    fn clone(self: LocalState, allocator: std.mem.Allocator) !LocalState {
        var cloned = self;
        cloned.moved_paths = std.array_list.Managed(mid.Place).init(allocator);
        try cloned.moved_paths.appendSlice(self.moved_paths.items);
        return cloned;
    }

    fn deinit(self: *LocalState) void {
        self.moved_paths.deinit();
    }
};

const State = struct {
    allocator: std.mem.Allocator,
    locals: std.AutoHashMapUnmanaged(u32, LocalState) = .{},

    fn init(allocator: std.mem.Allocator) State {
        return .{ .allocator = allocator };
    }

    fn clone(self: *const State) !State {
        var cloned = State.init(self.allocator);
        var it = self.locals.iterator();
        while (it.next()) |entry| {
            try cloned.locals.put(self.allocator, entry.key_ptr.*, try entry.value_ptr.clone(self.allocator));
        }
        return cloned;
    }

    fn deinit(self: *State) void {
        var it = self.locals.iterator();
        while (it.next()) |entry| entry.value_ptr.deinit();
        self.locals.deinit(self.allocator);
    }

    fn putLocal(self: *State, local: mid.Local, initially_live: bool) !void {
        try self.locals.put(self.allocator, local.id, LocalState.init(self.allocator, local, initially_live));
    }
};

const Checker = struct {
    allocator: std.mem.Allocator,
    program: mid.Program,
    function_decl: mid.Function,
    diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    failed: bool = false,

    fn init(
        allocator: std.mem.Allocator,
        program: mid.Program,
        function_decl: mid.Function,
        diagnostics_list: *std.array_list.Managed(diagnostics.Diagnostic),
    ) Checker {
        return .{
            .allocator = allocator,
            .program = program,
            .function_decl = function_decl,
            .diagnostics = diagnostics_list,
        };
    }

    fn checkFunction(self: *Checker) anyerror!void {
        var state = State.init(self.allocator);
        defer state.deinit();

        for (self.function_decl.locals) |local| {
            const live = local.is_parameter or local.is_capture;
            try state.putLocal(local, live);
        }

        const result = try self.checkBlock(&state, self.function_decl.body);
        if (!self.failed and result == .next) {
            try self.dropScopeLocals(&state, self.function_decl.body, false);
        }
    }

    fn checkBlock(self: *Checker, state: *State, block: mid.Block) anyerror!Control {
        for (block.statements) |statement| {
            const control = try self.checkStatement(state, statement);
            switch (control) {
                .next => {},
                else => {
                    try self.dropScopeLocals(state, block, control == .returned);
                    return control;
                },
            }
        }
        try self.dropScopeLocals(state, block, false);
        return .next;
    }

    fn checkStatement(self: *Checker, state: *State, statement: mid.Statement) anyerror!Control {
        return switch (statement) {
            .let_stmt => |node| blk: {
                const value = if (node.value) |value| value else {
                    if (state.locals.getPtr(node.local.id)) |local_state| {
                        local_state.availability = .uninitialized;
                        local_state.alias_kind = .none;
                        local_state.alias_place = null;
                        local_state.move_span = null;
                        local_state.moved_paths.clearRetainingCapacity();
                    }
                    break :blk .next;
                };
                try self.consumeBindingValue(state, node.local, value, node.is_reborrow, node.span);
                break :blk .next;
            },
            .assign_stmt => |node| blk: {
                try self.consumeValue(state, node.value, .read);
                try self.writePlace(state, node.target, node.span);
                break :blk .next;
            },
            .expr_stmt => |node| blk: {
                try self.consumeValue(state, node.value, .read);
                break :blk .next;
            },
            .if_stmt => |node| try self.checkIf(state, node),
            .while_stmt => |node| try self.checkWhile(state, node),
            .for_stmt => |node| try self.checkFor(state, node),
            .break_stmt => .break_loop,
            .continue_stmt => .continue_loop,
            .match_stmt => |node| try self.checkMatch(state, node),
            .switch_stmt => |node| try self.checkSwitch(state, node),
            .return_stmt => |node| blk: {
                if (node.value) |value| {
                    const mode: PathUseKind = switch (self.function_decl.return_ownership) {
                        .borrow_read => .borrow_shared,
                        .borrow_mut => .borrow_mut,
                        .move, .owned => .move,
                        .copy => .read,
                    };
                    try self.consumeValue(state, value, mode);
                }
                break :blk .returned;
            },
        };
    }

    fn checkIf(self: *Checker, state: *State, node: mid.IfStatement) anyerror!Control {
        try self.consumeValue(state, node.condition, .read);
        var then_state = try state.clone();
        defer then_state.deinit();
        const then_control = try self.checkBlock(&then_state, node.then_block);

        var else_state = try state.clone();
        defer else_state.deinit();
        const else_control = if (node.else_block) |else_block|
            try self.checkBlock(&else_state, else_block)
        else
            .next;

        if (then_control == .next and else_control == .next) {
            try self.joinState(state, &then_state, &else_state);
            return .next;
        }
        if (then_control == .returned and else_control == .returned) return .returned;
        if (then_control == .returned) {
            state.deinit();
            state.* = try else_state.clone();
            return else_control;
        }
        if (else_control == .returned) {
            state.deinit();
            state.* = try then_state.clone();
            return then_control;
        }
        try self.joinState(state, &then_state, &else_state);
        return .next;
    }

    fn checkWhile(self: *Checker, state: *State, node: mid.WhileStatement) anyerror!Control {
        var header = try state.clone();
        defer header.deinit();

        var changed = true;
        var iteration_count: usize = 0;
        while (changed and iteration_count < 8) : (iteration_count += 1) {
            changed = false;
            var body_state = try header.clone();
            defer body_state.deinit();
            try self.consumeValue(&body_state, node.condition, .read);
            const body_control = try self.checkBlock(&body_state, node.body);
            if (body_control == .returned) break;
            var joined = try header.clone();
            defer joined.deinit();
            try self.joinState(&joined, &header, &body_state);
            changed = try self.stateDiffers(&header, &joined);
            if (changed) {
                header.deinit();
                header = try joined.clone();
            }
        }

        var exit_state = try header.clone();
        defer exit_state.deinit();
        try self.consumeValue(&exit_state, node.condition, .read);
        state.deinit();
        state.* = try exit_state.clone();
        return .next;
    }

    fn checkFor(self: *Checker, state: *State, node: mid.ForStatement) anyerror!Control {
        try self.consumeValue(state, node.iterator, .read);
        var loop_state = try state.clone();
        defer loop_state.deinit();
        if (loop_state.locals.getPtr(node.binding.id)) |binding| {
            binding.availability = .live;
            binding.alias_kind = .none;
            binding.alias_place = null;
            binding.moved_paths.clearRetainingCapacity();
        }
        const control = try self.checkBlock(&loop_state, node.body);
        if (control == .returned) return .returned;
        state.deinit();
        state.* = try loop_state.clone();
        return .next;
    }

    fn checkMatch(self: *Checker, state: *State, node: mid.MatchStatement) anyerror!Control {
        try self.consumeValue(state, node.subject, .read);
        var merged: ?State = null;
        defer if (merged) |*m| m.deinit();
        var saw_fallthrough = false;
        var all_returned = true;
        for (node.arms) |arm| {
            var arm_state = try state.clone();
            defer arm_state.deinit();
            try self.activatePatternLocals(&arm_state, arm.bound_locals);
            if (arm.guard) |guard| try self.consumeValue(&arm_state, guard, .read);
            const control = try self.checkBlock(&arm_state, arm.body);
            try self.dropExplicitLocals(&arm_state, arm.bound_locals, control == .returned);
            if (control == .next) {
                if (merged == null) {
                    merged = try arm_state.clone();
                } else {
                    try self.joinState(&(merged.?), &(merged.?), &arm_state);
                }
                saw_fallthrough = true;
                all_returned = false;
            } else if (control != .returned) {
                all_returned = false;
            }
        }
        if (all_returned) return .returned;
        if (saw_fallthrough and merged != null) {
            state.deinit();
            state.* = try merged.?.clone();
        }
        return .next;
    }

    fn checkSwitch(self: *Checker, state: *State, node: mid.SwitchStatement) anyerror!Control {
        try self.consumeValue(state, node.subject, .read);
        var merged: ?State = null;
        defer if (merged) |*m| m.deinit();
        var saw_fallthrough = false;
        var all_returned = true;

        for (node.cases) |case_node| {
            var case_state = try state.clone();
            defer case_state.deinit();
            try self.consumeValue(&case_state, case_node.pattern, .read);
            const control = try self.checkBlock(&case_state, case_node.body);
            if (control == .next) {
                if (merged == null) {
                    merged = try case_state.clone();
                } else {
                    try self.joinState(&(merged.?), &(merged.?), &case_state);
                }
                saw_fallthrough = true;
                all_returned = false;
            } else if (control != .returned) {
                all_returned = false;
            }
        }
        if (node.default_block) |default_block| {
            var default_state = try state.clone();
            defer default_state.deinit();
            const control = try self.checkBlock(&default_state, default_block);
            if (control == .next) {
                if (merged == null) {
                    merged = try default_state.clone();
                } else {
                    try self.joinState(&(merged.?), &(merged.?), &default_state);
                }
                saw_fallthrough = true;
                all_returned = false;
            } else if (control != .returned) {
                all_returned = false;
            }
        } else {
            all_returned = false;
        }
        if (all_returned) return .returned;
        if (saw_fallthrough and merged != null) {
            state.deinit();
            state.* = try merged.?.clone();
        }
        return .next;
    }

    fn consumeBindingValue(
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

    fn consumeValue(self: *Checker, state: *State, value: mid.Value, mode: PathUseKind) anyerror!void {
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

    fn consumeBuilderBlock(self: *Checker, state: *State, builder: mid.BuilderBlock) anyerror!void {
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

    fn consumeCallArgs(self: *Checker, state: *State, args: []const mid.Value, ownership: []const model.OwnershipMode) anyerror!void {
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

    fn consumeCapture(self: *Checker, state: *State, source_place: mid.Place, capture: mid.Capture, span: source_pkg.Span) anyerror!void {
        const use_kind: PathUseKind = switch (capture.ownership) {
            .borrow_read => .borrow_shared,
            .borrow_mut => .borrow_mut,
            .move, .owned, .copy => .move,
        };
        try self.consumeValue(state, .{ .place = .{ .place = source_place } }, use_kind);
        _ = span;
    }

    fn placeForValue(self: *Checker, state: *State, value: mid.Value) ?mid.Place {
        _ = self;
        return switch (value) {
            .place => |node| resolvePlace(state, node.place),
            else => null,
        };
    }

    fn rawPlaceForValue(self: *Checker, value: mid.Value) ?mid.Place {
        _ = self;
        return switch (value) {
            .place => |node| node.place,
            else => null,
        };
    }

    fn bindingMoveSource(self: *Checker, state: *State, value: mid.Value) ?mid.Place {
        const place = self.placeForValue(state, value) orelse return null;
        return switch (place.ty.kind) {
            .array => place,
            // Fieldless enums are copied by value, so binding one does not move its
            // source; only payload-carrying enums transfer ownership on bind.
            .enum_instance => if (self.isCopyableType(place.ty)) null else place,
            else => null,
        };
    }

    fn movePlace(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
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

    fn borrowPlace(self: *Checker, state: *State, place: mid.Place, kind: PathUseKind, span: source_pkg.Span) anyerror!void {
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

    fn writePlace(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
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

    fn dropPlace(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
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

    fn ensurePlaceLive(self: *Checker, state: *State, place: mid.Place, span: source_pkg.Span) anyerror!void {
        if (place.root == .return_slot) return;
        const local_id = rootLocalId(place.root) orelse return;
        const root_state = state.locals.get(local_id) orelse return;
        switch (root_state.availability) {
            .live => {},
            .uninitialized => {
                try self.emitOwnershipDiagnostic(
                    "KIR002",
                    "place is not initialized on every path",
                    "This place does not hold a live value here.",
                    span,
                    "Initialize the value on every control-flow path before using it.",
                );
                return;
            },
            .moved, .maybe_moved => {
                try self.emitOwnershipDiagnostic(
                    "KIR002",
                    "place is moved or dropped before this use",
                    "This use reads a place that was moved or dropped earlier in the current control-flow state.",
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
                    try self.emitOwnershipDiagnostic(
                        "KIR002",
                        "place is only partially live after a move",
                        "A moved child or ancestor overlaps this use, so the place is not fully available here.",
                        span,
                        "Reinitialize the moved path or keep using only disjoint siblings.",
                    );
                    return;
                },
                .disjoint => {},
            }
        }
    }

    fn ensureNoConflictingAccess(self: *Checker, state: *State, access: PathAccess) anyerror!void {
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

    fn ensureAccessesCompatible(self: *Checker, lhs: PathAccess, rhs: PathAccess) anyerror!void {
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

    fn dropScopeLocals(self: *Checker, state: *State, block: mid.Block, returned: bool) anyerror!void {
        if (returned) return;
        const scoped = scopedLocalIds(self.allocator, block);
        defer self.allocator.free(scoped);
        try self.dropLocalIds(state, scoped);
    }

    fn dropExplicitLocals(self: *Checker, state: *State, locals: []const mid.Local, returned: bool) anyerror!void {
        if (returned) return;
        var ids = std.array_list.Managed(u32).init(self.allocator);
        defer ids.deinit();
        for (locals) |local| try ids.append(local.id);
        try self.dropLocalIds(state, ids.items);
    }

    fn dropLocalIds(self: *Checker, state: *State, local_ids: []const u32) anyerror!void {
        for (local_ids) |local_id| {
            const local_state = state.locals.getPtr(local_id) orelse continue;
            if (local_state.local.ownership != .borrow_read and local_state.local.ownership != .borrow_mut) {
                if (local_state.moved_paths.items.len != 0 and local_state.availability == .live) {
                    try self.emitOwnershipDiagnostic(
                        "KIR003",
                        "scope would drop an incompletely moved value",
                        "This scope exits while an owned aggregate still has moved children, so Kira cannot build an honest drop plan.",
                        local_state.move_span orelse local_state.local.span,
                        "Write the moved field back, replace the whole value, or move the whole aggregate instead of leaving a child path moved.",
                    );
                    return;
                }
            }
            resetScopedLocal(local_state);
        }
    }

    fn joinState(self: *Checker, out_state: *State, lhs: *State, rhs: *State) anyerror!void {
        _ = self;
        var keys = std.AutoHashMapUnmanaged(u32, void){};
        defer keys.deinit(out_state.allocator);

        var lhs_it = lhs.locals.iterator();
        while (lhs_it.next()) |entry| try keys.put(out_state.allocator, entry.key_ptr.*, {});
        var rhs_it = rhs.locals.iterator();
        while (rhs_it.next()) |entry| try keys.put(out_state.allocator, entry.key_ptr.*, {});

        var next = State.init(out_state.allocator);
        defer next.deinit();

        var key_it = keys.iterator();
        while (key_it.next()) |entry| {
            const local_id = entry.key_ptr.*;
            const left = lhs.locals.get(local_id) orelse rhs.locals.get(local_id).?;
            const right = rhs.locals.get(local_id) orelse lhs.locals.get(local_id).?;
            var merged = try left.clone(out_state.allocator);
            merged.availability = joinAvailability(left.availability, right.availability);
            if (left.alias_kind != right.alias_kind or !placesEqualOptional(left.alias_place, right.alias_place)) {
                merged.alias_kind = .none;
                merged.alias_place = null;
            }
            merged.moved_paths.clearRetainingCapacity();
            try merged.moved_paths.appendSlice(left.moved_paths.items);
            for (right.moved_paths.items) |moved_place| {
                if (!placeSliceContains(merged.moved_paths.items, moved_place)) try merged.moved_paths.append(moved_place);
            }
            try next.locals.put(out_state.allocator, local_id, merged);
        }

        out_state.deinit();
        out_state.* = try next.clone();
    }

    fn stateDiffers(self: *Checker, lhs: *State, rhs: *State) anyerror!bool {
        _ = self;
        if (lhs.locals.count() != rhs.locals.count()) return true;
        var it = lhs.locals.iterator();
        while (it.next()) |entry| {
            const other = rhs.locals.get(entry.key_ptr.*) orelse return true;
            if (entry.value_ptr.availability != other.availability) return true;
            if (entry.value_ptr.alias_kind != other.alias_kind) return true;
            if (!placesEqualOptional(entry.value_ptr.alias_place, other.alias_place)) return true;
            if (entry.value_ptr.moved_paths.items.len != other.moved_paths.items.len) return true;
        }
        return false;
    }

    fn resolveLocalPlace(self: *Checker, state: *State, local_id: u32) ?mid.Place {
        _ = self;
        return if (state.locals.get(local_id)) |local_state|
            if (local_state.alias_place) |alias_place|
                alias_place
            else
                .{
                    .root = if (local_state.local.is_capture) .{ .capture = local_state.local.id } else .{ .local = local_state.local.id },
                    .ty = local_state.local.ty,
                    .span = local_state.local.span,
                }
        else
            null;
    }

    fn activatePatternLocals(self: *Checker, state: *State, locals: []const mid.Local) !void {
        _ = self;
        for (locals) |local| {
            const local_state = state.locals.getPtr(local.id) orelse continue;
            local_state.availability = .live;
            local_state.alias_kind = .none;
            local_state.alias_place = null;
            local_state.move_span = null;
            local_state.moved_paths.clearRetainingCapacity();
        }
    }

    fn emitOwnershipDiagnostic(
        self: *Checker,
        code: []const u8,
        title: []const u8,
        message: []const u8,
        span: source_pkg.Span,
        help: []const u8,
    ) anyerror!void {
        self.failed = true;
        try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
            .severity = .@"error",
            .code = code,
            .domain = "lowering",
            .phase = "lowering",
            .title = title,
            .message = message,
            .labels = &.{diagnostics.primaryLabel(span, title)},
            .help = help,
        });
    }

    /// Rust by-value semantics: handing a non-`Copy` value to an owned/move
    /// parameter transfers ownership, so a movable place argument (a local or a
    /// struct field) is moved out of its source. This is what lets the checker
    /// forbid reusing a moved field or the whole aggregate afterward instead of
    /// leaving the source aliased into the callee (the latent use-after-free
    /// behind the native enum-copy crash). Copyable values are duplicated and stay
    /// live. Indexed places (`arr[i]`) cannot be moved out of in place — mirroring
    /// Rust's "cannot move out of indexed content" — so they are cloned (read), as
    /// are fresh temporaries that own no source storage.
    fn effectiveUseKind(self: *const Checker, value: mid.Value, ownership: model.OwnershipMode) PathUseKind {
        return switch (ownership) {
            .borrow_read => .borrow_shared,
            .borrow_mut => .borrow_mut,
            .copy => .read,
            .move, .owned => if (self.isCopyableType(valueType(value))) .read else if (isMovablePlaceValue(value)) .move else .read,
        };
    }

    /// A value type that is duplicated rather than moved when passed by value:
    /// trivially copyable scalars and fieldless enums (every variant payload-less).
    /// The runtime already copies such enums by value, so treating them as moves
    /// would over-reject without preventing any real use-after-free.
    fn isCopyableType(self: *const Checker, ty: model.ResolvedType) bool {
        if (isTriviallyCopyableType(ty)) return true;
        return self.isFieldlessEnumType(ty);
    }

    fn isFieldlessEnumType(self: *const Checker, ty: model.ResolvedType) bool {
        if (ty.kind != .enum_instance) return false;
        const name = ty.name orelse return false;
        for (self.program.source_program.enums) |enum_decl| {
            if (!std.mem.eql(u8, enum_decl.name, name)) continue;
            for (enum_decl.variants) |variant| {
                if (variant.payload_ty != null) return false;
            }
            return true;
        }
        return false;
    }
};

fn resolvePlace(state: *State, place: mid.Place) ?mid.Place {
    const local_id = rootLocalId(place.root) orelse return place;
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

fn aliasAccessLocalId(state: *State, place: mid.Place) ?u32 {
    const local_id = rootLocalId(place.root) orelse return null;
    const local_state = state.locals.get(local_id) orelse return null;
    return if (local_state.alias_kind != .none) local_id else null;
}

fn rootLocalId(root: mid.Place.Root) ?u32 {
    return switch (root) {
        .local => |id| id,
        .capture => |id| id,
        .return_slot => null,
    };
}

/// A place argument that can be moved out of its source: a local or a chain of
/// struct-field projections. A projection chain that reaches through an array
/// element (`arr[i]`) is excluded, because Kira (like Rust) cannot leave a hole
/// in indexed content; such reads are cloned instead of moved.
fn isMovablePlaceValue(value: mid.Value) bool {
    return switch (value) {
        .place => |node| !placeHasIndexProjection(node.place),
        else => false,
    };
}

fn placeHasIndexProjection(place: mid.Place) bool {
    for (place.projections) |projection| {
        if (projection == .index) return true;
    }
    return false;
}

fn valueType(value: mid.Value) model.ResolvedType {
    return switch (value) {
        .integer => |node| node.ty,
        .float => |node| node.ty,
        .string => |node| node.ty,
        .boolean => |node| node.ty,
        .null_ptr => |node| node.ty,
        .function_ref => |node| node.ty,
        .place => |node| node.place.ty,
        .namespace_ref => |node| node.ty,
        .call => |node| node.ty,
        .virtual_call => |node| node.ty,
        .callback => |node| node.ty,
        .call_value => |node| node.ty,
        .construct => |node| node.ty,
        .construct_enum_variant => |node| node.ty,
        .array => |node| node.ty,
        .builder_array => |node| node.ty,
        .binary => |node| node.ty,
        .unary => |node| node.ty,
        .conditional => |node| node.ty,
        .native_state => |node| node.ty,
        .native_user_data => |node| node.ty,
        .native_recover => |node| node.ty,
        .c_string_to_string => |node| node.ty,
        .array_len => |node| node.ty,
        .string_len => |node| node.ty,
        .opaque_member => |node| node.ty,
        .opaque_index => |node| node.ty,
    };
}

fn isTriviallyCopyableType(ty: model.ResolvedType) bool {
    return switch (ty.kind) {
        .void, .integer, .float, .boolean, .c_string, .raw_ptr => true,
        else => false,
    };
}

fn joinAvailability(lhs: LocalAvailability, rhs: LocalAvailability) LocalAvailability {
    if (lhs == rhs) return lhs;
    if (lhs == .live and rhs == .uninitialized) return .maybe_moved;
    if (lhs == .uninitialized and rhs == .live) return .maybe_moved;
    if (lhs == .moved or rhs == .moved) return .maybe_moved;
    if (lhs == .maybe_moved or rhs == .maybe_moved) return .maybe_moved;
    return .maybe_moved;
}

fn scopedLocalIds(allocator: std.mem.Allocator, block: mid.Block) []u32 {
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

fn clearMovedPaths(root_state: *LocalState, assigned_place: mid.Place) void {
    var index: usize = 0;
    while (index < root_state.moved_paths.items.len) {
        const moved_place = root_state.moved_paths.items[index];
        const relation = placeRelation(moved_place, assigned_place);
        switch (relation) {
            .same, .ancestor, .descendant => _ = root_state.moved_paths.swapRemove(index),
            .disjoint, .overlap => index += 1,
        }
    }
}

fn resetScopedLocal(local_state: *LocalState) void {
    local_state.availability = .uninitialized;
    local_state.alias_kind = .none;
    local_state.alias_place = null;
    local_state.move_span = null;
    local_state.moved_paths.clearRetainingCapacity();
}

fn placeRelation(lhs: mid.Place, rhs: mid.Place) PlaceRelation {
    if (!rootsEqual(lhs.root, rhs.root)) return .disjoint;
    var index: usize = 0;
    while (index < lhs.projections.len and index < rhs.projections.len) : (index += 1) {
        const lhs_projection = lhs.projections[index];
        const rhs_projection = rhs.projections[index];
        switch (lhs_projection) {
            .field => |lhs_field| switch (rhs_projection) {
                .field => |rhs_field| {
                    if (lhs_field.field_index == rhs_field.field_index) continue;
                    return .disjoint;
                },
                else => return .overlap,
            },
            .index => |lhs_index| switch (rhs_projection) {
                .index => |rhs_index| {
                    if (lhs_index.index != null and rhs_index.index != null and lhs_index.index.? == rhs_index.index.?) continue;
                    return .overlap;
                },
                else => return .overlap,
            },
            .parent_view => |lhs_parent| switch (rhs_projection) {
                .parent_view => |rhs_parent| {
                    if (lhs_parent.offset == rhs_parent.offset) continue;
                    return .overlap;
                },
                else => return .overlap,
            },
        }
    }
    if (lhs.projections.len == rhs.projections.len) return .same;
    if (lhs.projections.len < rhs.projections.len) return .ancestor;
    return .descendant;
}

fn rootsEqual(lhs: mid.Place.Root, rhs: mid.Place.Root) bool {
    return switch (lhs) {
        .local => |id| switch (rhs) {
            .local => |other| id == other,
            else => false,
        },
        .capture => |id| switch (rhs) {
            .capture => |other| id == other,
            else => false,
        },
        .return_slot => rhs == .return_slot,
    };
}

fn placesEqual(lhs: mid.Place, rhs: mid.Place) bool {
    if (!rootsEqual(lhs.root, rhs.root)) return false;
    if (lhs.projections.len != rhs.projections.len) return false;
    for (lhs.projections, rhs.projections) |lhs_projection, rhs_projection| {
        switch (lhs_projection) {
            .field => |lhs_field| switch (rhs_projection) {
                .field => |rhs_field| {
                    if (lhs_field.field_index != rhs_field.field_index) return false;
                },
                else => return false,
            },
            .index => |lhs_index| switch (rhs_projection) {
                .index => |rhs_index| {
                    if (lhs_index.index != rhs_index.index) return false;
                },
                else => return false,
            },
            .parent_view => |lhs_parent| switch (rhs_projection) {
                .parent_view => |rhs_parent| {
                    if (lhs_parent.offset != rhs_parent.offset) return false;
                },
                else => return false,
            },
        }
    }
    return true;
}

fn placesEqualOptional(lhs: ?mid.Place, rhs: ?mid.Place) bool {
    if (lhs == null and rhs == null) return true;
    if (lhs == null or rhs == null) return false;
    return placesEqual(lhs.?, rhs.?);
}

fn placeSliceContains(items: []const mid.Place, needle: mid.Place) bool {
    for (items) |item| {
        if (placesEqual(item, needle)) return true;
    }
    return false;
}
