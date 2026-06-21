const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");
const place_algebra = @import("mid_ir_place.zig");
const state_mod = @import("mid_ir_state.zig");
const consume = @import("mid_ir_consume.zig");

// Place/value algebra and dataflow-state types live in sibling modules; alias them
// here so the checker body can reference them unqualified.
const PlaceRelation = place_algebra.PlaceRelation;
const rootLocalId = place_algebra.rootLocalId;
const isMovablePlaceValue = place_algebra.isMovablePlaceValue;
const placeHasIndexProjection = place_algebra.placeHasIndexProjection;
const valueType = place_algebra.valueType;
const isTriviallyCopyableType = place_algebra.isTriviallyCopyableType;
const placeRelation = place_algebra.placeRelation;
const rootsEqual = place_algebra.rootsEqual;
const placesEqual = place_algebra.placesEqual;
const placesEqualOptional = place_algebra.placesEqualOptional;
const placeSliceContains = place_algebra.placeSliceContains;

const LocalAvailability = state_mod.LocalAvailability;
const AliasKind = state_mod.AliasKind;
const LocalState = state_mod.LocalState;
const State = state_mod.State;
const resolvePlace = state_mod.resolvePlace;
const aliasAccessLocalId = state_mod.aliasAccessLocalId;
const joinAvailability = state_mod.joinAvailability;
const scopedLocalIds = state_mod.scopedLocalIds;
const clearMovedPaths = state_mod.clearMovedPaths;
const resetScopedLocal = state_mod.resetScopedLocal;

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

pub const PathUseKind = enum {
    read,
    borrow_shared,
    borrow_mut,
    move,
    write,
    drop,
};

pub const PathAccess = struct {
    place: mid.Place,
    kind: PathUseKind,
    span: source_pkg.Span,
    ignore_alias_local_id: ?u32 = null,
};

pub const Checker = struct {
    // Value/place consumption lives in `mid_ir_consume.zig`; re-expose those
    // functions as methods so call sites read as `self.consumeValue(...)`.
    pub const consumeBindingValue = consume.consumeBindingValue;
    pub const consumeValue = consume.consumeValue;
    pub const consumeBuilderBlock = consume.consumeBuilderBlock;
    pub const consumeCallArgs = consume.consumeCallArgs;
    pub const consumeCapture = consume.consumeCapture;
    pub const placeForValue = consume.placeForValue;
    pub const rawPlaceForValue = consume.rawPlaceForValue;
    pub const bindingMoveSource = consume.bindingMoveSource;
    pub const movePlace = consume.movePlace;
    pub const borrowPlace = consume.borrowPlace;
    pub const writePlace = consume.writePlace;
    pub const dropPlace = consume.dropPlace;
    pub const ensurePlaceLive = consume.ensurePlaceLive;
    pub const ensureNoConflictingAccess = consume.ensureNoConflictingAccess;
    pub const ensureAccessesCompatible = consume.ensureAccessesCompatible;

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

    fn dropScopeLocals(self: *Checker, state: *State, block: mid.Block, returned: bool) anyerror!void {
        if (returned) return;
        const scoped = scopedLocalIds(self.allocator, block);
        defer self.allocator.free(scoped);
        try self.dropLocalIds(state, scoped);
    }

    pub fn dropExplicitLocals(self: *Checker, state: *State, locals: []const mid.Local, returned: bool) anyerror!void {
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

    pub fn joinState(self: *Checker, out_state: *State, lhs: *State, rhs: *State) anyerror!void {
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

    pub fn resolveLocalPlace(self: *Checker, state: *State, local_id: u32) ?mid.Place {
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

    pub fn emitOwnershipDiagnostic(
        self: *Checker,
        code: []const u8,
        title: []const u8,
        message: []const u8,
        span: source_pkg.Span,
        help: []const u8,
    ) anyerror!void {
        // Report only the first ownership error per function. A single fatal
        // diagnostic keeps the failure deterministic (later branches and overlapping
        // pairs would otherwise pile on redundant errors at the same root cause).
        if (self.failed) return;
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
    pub fn effectiveUseKind(self: *const Checker, value: mid.Value, ownership: model.OwnershipMode) PathUseKind {
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
    pub fn isCopyableType(self: *const Checker, ty: model.ResolvedType) bool {
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

