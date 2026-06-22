const std = @import("std");
const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");

pub fn cloneScope(allocator: std.mem.Allocator, scope: model.Scope) !model.Scope {
    return scope.clone(allocator);
}

pub fn mergeIfState(
    allocator: std.mem.Allocator,
    scope: *model.Scope,
    then_scope: model.Scope,
    else_scope: ?model.Scope,
) !void {
    var iterator = scope.entries.iterator();
    while (iterator.next()) |entry| {
        const then_binding = then_scope.get(entry.key_ptr.*) orelse continue;
        if (then_binding.id != entry.value_ptr.id) continue;

        if (else_scope) |resolved_else| {
            const else_binding = resolved_else.get(entry.key_ptr.*) orelse continue;
            if (else_binding.id != entry.value_ptr.id) continue;
            try replaceBindingMoveState(allocator, entry.value_ptr, &.{ then_binding, else_binding });
            entry.value_ptr.initialized = entry.value_ptr.initialized or (then_binding.initialized and else_binding.initialized);
        } else {
            try replaceBindingMoveState(allocator, entry.value_ptr, &.{ entry.value_ptr.*, then_binding });
        }
    }
}

pub fn mergeSwitchState(
    allocator: std.mem.Allocator,
    scope: *model.Scope,
    case_scopes: []const model.Scope,
    default_scope: ?model.Scope,
) !void {
    var iterator = scope.entries.iterator();
    while (iterator.next()) |entry| {
        var merged_fields: std.ArrayListUnmanaged([]const u8) = .empty;
        defer merged_fields.deinit(allocator);

        var moved = false;
        var move_span: ?source_pkg.Span = null;

        if (default_scope) |resolved_default| {
            const default_binding = resolved_default.get(entry.key_ptr.*) orelse continue;
            if (default_binding.id != entry.value_ptr.id) continue;
            try appendBindingMoveState(allocator, &merged_fields, &moved, &move_span, default_binding);
        } else {
            try appendBindingMoveState(allocator, &merged_fields, &moved, &move_span, entry.value_ptr.*);
        }

        var initialized_in_all_cases = default_scope != null;
        for (case_scopes) |case_scope| {
            const case_binding = case_scope.get(entry.key_ptr.*) orelse {
                initialized_in_all_cases = false;
                continue;
            };
            if (case_binding.id != entry.value_ptr.id) {
                initialized_in_all_cases = false;
                continue;
            }
            try appendBindingMoveState(allocator, &merged_fields, &moved, &move_span, case_binding);
            if (default_scope != null and !case_binding.initialized) initialized_in_all_cases = false;
        }

        entry.value_ptr.moved = moved;
        entry.value_ptr.move_span = if (moved or merged_fields.items.len != 0) move_span else null;
        try entry.value_ptr.replaceMovedFields(allocator, merged_fields.items);
        if (initialized_in_all_cases) entry.value_ptr.initialized = true;
    }
}

pub fn mergeLoopState(
    allocator: std.mem.Allocator,
    scope: *model.Scope,
    body_scope: model.Scope,
) !void {
    var iterator = scope.entries.iterator();
    while (iterator.next()) |entry| {
        const body_binding = body_scope.get(entry.key_ptr.*) orelse continue;
        if (body_binding.id != entry.value_ptr.id) continue;
        try replaceBindingMoveState(allocator, entry.value_ptr, &.{ entry.value_ptr.*, body_binding });
    }
}

pub fn mergeMatchState(
    allocator: std.mem.Allocator,
    scope: *model.Scope,
    arm_scopes: []const model.Scope,
    include_original_path: bool,
) !void {
    var iterator = scope.entries.iterator();
    while (iterator.next()) |entry| {
        var merged_fields: std.ArrayListUnmanaged([]const u8) = .empty;
        defer merged_fields.deinit(allocator);

        var moved = false;
        var move_span: ?source_pkg.Span = null;
        if (include_original_path) {
            try appendBindingMoveState(allocator, &merged_fields, &moved, &move_span, entry.value_ptr.*);
        }
        for (arm_scopes) |arm_scope| {
            const arm_binding = arm_scope.get(entry.key_ptr.*) orelse continue;
            if (arm_binding.id != entry.value_ptr.id) continue;
            try appendBindingMoveState(allocator, &merged_fields, &moved, &move_span, arm_binding);
        }

        entry.value_ptr.moved = moved;
        entry.value_ptr.move_span = if (moved or merged_fields.items.len != 0) move_span else null;
        try entry.value_ptr.replaceMovedFields(allocator, merged_fields.items);
    }
}

fn replaceBindingMoveState(
    allocator: std.mem.Allocator,
    binding: *model.LocalBinding,
    sources: []const model.LocalBinding,
) !void {
    var merged_fields: std.ArrayListUnmanaged([]const u8) = .empty;
    defer merged_fields.deinit(allocator);

    var moved = false;
    var move_span: ?source_pkg.Span = null;
    for (sources) |source| {
        try appendBindingMoveState(allocator, &merged_fields, &moved, &move_span, source);
    }

    binding.moved = moved;
    binding.move_span = if (moved or merged_fields.items.len != 0) move_span else null;
    try binding.replaceMovedFields(allocator, merged_fields.items);
}

fn appendBindingMoveState(
    allocator: std.mem.Allocator,
    merged_fields: *std.ArrayListUnmanaged([]const u8),
    moved: *bool,
    move_span: *?source_pkg.Span,
    binding: model.LocalBinding,
) !void {
    if (binding.moved) {
        moved.* = true;
        if (move_span.* == null) move_span.* = binding.move_span;
    }
    if (!binding.hasMovedFields()) return;
    if (move_span.* == null) move_span.* = binding.move_span;
    for (binding.moved_fields.items) |field| {
        try appendUniqueField(allocator, merged_fields, field);
    }
}

fn appendUniqueField(
    allocator: std.mem.Allocator,
    fields: *std.ArrayListUnmanaged([]const u8),
    field: []const u8,
) !void {
    for (fields.items) |existing| {
        if (std.mem.eql(u8, existing, field)) return;
    }
    try fields.append(allocator, field);
}
