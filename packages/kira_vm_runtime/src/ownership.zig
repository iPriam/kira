const std = @import("std");
const runtime_abi = @import("kira_runtime_abi");

pub const ArrayObject = extern struct {
    len: usize,
    items: [*]runtime_abi.BridgeValue,
};

pub const ClosureObject = struct {
    function_id: u32,
    is_native: bool = false,
    captures: []runtime_abi.Value,
};

const ObjectKind = union(enum) {
    array: *ArrayObject,
    closure: *ClosureObject,
    struct_fields: StructFieldsObject,
    string_bytes: []u8,
};

pub const ObjectOrigin = enum {
    runtime_alloc,
    native_materialize,
};

pub const StructFieldsObject = struct {
    type_name: []const u8,
    fields: []runtime_abi.Value,
};

const ObjectRecord = struct {
    ref_count: usize,
    pin_count: usize = 0,
    origin: ObjectOrigin = .runtime_alloc,
    kind: ObjectKind,
};

pub const HeapStats = struct {
    arrays_current: usize = 0,
    arrays_peak: usize = 0,
    arrays_allocated: usize = 0,
    arrays_freed: usize = 0,
    closures_current: usize = 0,
    closures_peak: usize = 0,
    closures_allocated: usize = 0,
    closures_freed: usize = 0,
    structs_current: usize = 0,
    structs_peak: usize = 0,
    structs_allocated: usize = 0,
    structs_freed: usize = 0,
    strings_current: usize = 0,
    strings_peak: usize = 0,
    strings_allocated: usize = 0,
    strings_freed: usize = 0,
};

const PinFrame = struct {
    pinned: std.AutoHashMapUnmanaged(usize, void) = .{},
};

pub const Heap = struct {
    allocator: std.mem.Allocator,
    objects: std.AutoHashMap(usize, ObjectRecord),
    pin_frames: std.ArrayListUnmanaged(PinFrame) = .empty,
    stats: HeapStats = .{},

    pub fn init(allocator: std.mem.Allocator) Heap {
        return .{
            .allocator = allocator,
            .objects = std.AutoHashMap(usize, ObjectRecord).init(allocator),
            .pin_frames = .empty,
        };
    }

    pub fn deinit(self: *Heap) void {
        while (self.pin_frames.items.len != 0) self.endBoundaryPinScope();
        while (self.objects.count() != 0) {
            var iterator = self.objects.iterator();
            if (iterator.next()) |entry| self.releasePtr(entry.key_ptr.*);
        }
        self.objects.deinit();
    }

    pub fn registerArray(self: *Heap, object: *ArrayObject) !usize {
        const ptr = @intFromPtr(object);
        try self.objects.put(ptr, .{ .ref_count = 1, .kind = .{ .array = object } });
        self.recordAlloc(.array);
        return ptr;
    }

    pub fn registerClosure(self: *Heap, object: *ClosureObject) !usize {
        const ptr = @intFromPtr(object);
        try self.objects.put(ptr, .{ .ref_count = 1, .kind = .{ .closure = object } });
        self.recordAlloc(.closure);
        return ptr;
    }

    pub fn registerStruct(self: *Heap, type_name: []const u8, fields: []runtime_abi.Value) !usize {
        return self.registerStructWithOrigin(type_name, fields, .runtime_alloc);
    }

    pub fn registerStructWithOrigin(self: *Heap, type_name: []const u8, fields: []runtime_abi.Value, origin: ObjectOrigin) !usize {
        var owned_fields = fields;
        if (owned_fields.len == 0) {
            self.allocator.free(owned_fields);
            owned_fields = try self.allocator.alloc(runtime_abi.Value, 1);
            owned_fields[0] = .{ .void = {} };
        }
        const ptr = @intFromPtr(owned_fields.ptr);
        try self.objects.put(ptr, .{ .ref_count = 1, .origin = origin, .kind = .{ .struct_fields = .{
            .type_name = type_name,
            .fields = owned_fields,
        } } });
        self.recordAlloc(.struct_fields);
        return ptr;
    }

    pub fn registerString(self: *Heap, bytes: []u8) !void {
        if (bytes.len == 0) {
            self.allocator.free(bytes);
            return;
        }
        const ptr = @intFromPtr(bytes.ptr);
        try self.objects.put(ptr, .{ .ref_count = 1, .kind = .{ .string_bytes = bytes } });
        self.recordAlloc(.string_bytes);
    }

    pub fn beginBoundaryPinScope(self: *Heap) !void {
        try self.pin_frames.append(self.allocator, .{});
    }

    pub fn endBoundaryPinScope(self: *Heap) void {
        if (self.pin_frames.items.len == 0) return;
        const frame = self.pin_frames.pop().?;
        var iterator = frame.pinned.iterator();
        while (iterator.next()) |entry| self.unpinPtr(entry.key_ptr.*);
        var mutable = frame;
        mutable.pinned.deinit(self.allocator);
    }

    pub fn pinBoundaryValue(self: *Heap, value: runtime_abi.Value) !void {
        if (self.pin_frames.items.len == 0) return;
        var visited: std.AutoHashMapUnmanaged(usize, void) = .{};
        defer visited.deinit(self.allocator);
        const frame = &self.pin_frames.items[self.pin_frames.items.len - 1];
        try self.pinValueRecursive(value, frame, &visited);
    }

    pub fn count(self: *const Heap) usize {
        return self.objects.count();
    }

    pub fn emitCurrentTypeReport(self: *const Heap) void {
        const TypeStats = struct {
            current: usize = 0,
            ref_total: usize = 0,
            pin_total: usize = 0,
            zero_ref_pinned: usize = 0,
            runtime_alloc: usize = 0,
            native_materialize: usize = 0,
        };
        var counts = std.StringHashMap(TypeStats).init(std.heap.page_allocator);
        defer counts.deinit();

        var iterator = self.objects.iterator();
        while (iterator.next()) |entry| {
            switch (entry.value_ptr.kind) {
                .struct_fields => |value| {
                    const result = counts.getOrPut(value.type_name) catch continue;
                    if (!result.found_existing) result.value_ptr.* = .{};
                    result.value_ptr.current += 1;
                    result.value_ptr.ref_total += entry.value_ptr.ref_count;
                    result.value_ptr.pin_total += entry.value_ptr.pin_count;
                    if (entry.value_ptr.ref_count == 0 and entry.value_ptr.pin_count != 0) result.value_ptr.zero_ref_pinned += 1;
                    switch (entry.value_ptr.origin) {
                        .runtime_alloc => result.value_ptr.runtime_alloc += 1,
                        .native_materialize => result.value_ptr.native_materialize += 1,
                    }
                },
                .array, .closure, .string_bytes => {},
            }
        }

        var count_iterator = counts.iterator();
        while (count_iterator.next()) |entry| {
            std.debug.print(
                "Kira runtime memory detail: structType={s} current={d} refTotal={d} pinTotal={d} zeroRefPinned={d} runtimeAlloc={d} nativeMaterialize={d}\n",
                .{
                    entry.key_ptr.*,
                    entry.value_ptr.current,
                    entry.value_ptr.ref_total,
                    entry.value_ptr.pin_total,
                    entry.value_ptr.zero_ref_pinned,
                    entry.value_ptr.runtime_alloc,
                    entry.value_ptr.native_materialize,
                },
            );
        }
    }

    pub fn retainValue(self: *Heap, value: runtime_abi.Value) void {
        if (value == .raw_ptr) self.retainPtr(value.raw_ptr);
        if (value == .string and value.string.len != 0) self.retainPtr(@intFromPtr(value.string.ptr));
    }

    pub fn releaseValue(self: *Heap, value: runtime_abi.Value) void {
        if (value == .raw_ptr) self.releasePtr(value.raw_ptr);
        if (value == .string and value.string.len != 0) self.releasePtr(@intFromPtr(value.string.ptr));
    }

    pub fn isManagedValue(self: *const Heap, value: runtime_abi.Value) bool {
        return switch (value) {
            .raw_ptr => |ptr| self.objects.contains(ptr),
            .string => |bytes| bytes.len != 0 and self.objects.contains(@intFromPtr(bytes.ptr)),
            else => false,
        };
    }

    pub fn getClosure(self: *const Heap, ptr: usize) ?*const ClosureObject {
        const record = self.objects.getPtr(ptr) orelse return null;
        return switch (record.kind) {
            .closure => |closure| closure,
            else => null,
        };
    }

    pub fn getStructTypeName(self: *const Heap, ptr: usize) ?[]const u8 {
        const record = self.objects.getPtr(ptr) orelse return null;
        return switch (record.kind) {
            .struct_fields => |value| value.type_name,
            else => null,
        };
    }

    pub fn assignOwned(self: *Heap, slot: *runtime_abi.Value, value: runtime_abi.Value) void {
        const old = slot.*;
        slot.* = value;
        self.releaseValue(old);
    }

    pub fn assignBorrowed(self: *Heap, slot: *runtime_abi.Value, value: runtime_abi.Value) void {
        self.retainValue(value);
        const old = slot.*;
        slot.* = value;
        self.releaseValue(old);
    }

    pub fn releaseSlots(self: *Heap, slots: []runtime_abi.Value) void {
        for (slots) |*slot| self.assignOwned(slot, .{ .void = {} });
    }

    pub fn replaceArrayItem(self: *Heap, slot: *runtime_abi.BridgeValue, value: runtime_abi.Value) void {
        self.retainValue(value);
        const old = runtime_abi.bridgeValueToValue(slot.*);
        slot.* = runtime_abi.bridgeValueFromValue(value);
        self.releaseValue(old);
    }

    pub fn appendArrayItem(self: *Heap, object: *ArrayObject, value: runtime_abi.Value) !void {
        const old_items = object.items[0..@max(object.len, 1)];
        const new_len = object.len + 1;
        const new_items = try self.allocator.alloc(runtime_abi.BridgeValue, new_len);
        for (old_items[0..object.len], 0..) |item, index| {
            new_items[index] = item;
        }
        self.retainValue(value);
        new_items[object.len] = runtime_abi.bridgeValueFromValue(value);
        self.allocator.free(old_items);
        object.items = new_items.ptr;
        object.len = new_len;
    }

    fn retainPtr(self: *Heap, ptr: usize) void {
        if (ptr == 0) return;
        if (self.objects.getPtr(ptr)) |record| record.ref_count += 1;
    }

    fn releasePtr(self: *Heap, ptr: usize) void {
        if (ptr == 0) return;
        const record_ptr = self.objects.getPtr(ptr) orelse return;
        if (record_ptr.ref_count == 0) return;
        record_ptr.ref_count -= 1;
        if (record_ptr.ref_count != 0 or record_ptr.pin_count != 0) {
            return;
        }
        const removed = self.objects.fetchRemove(ptr) orelse return;
        self.recordFree(removed.value.kind);
        self.destroy(removed.value.kind);
    }

    fn pinValueRecursive(self: *Heap, value: runtime_abi.Value, frame: *PinFrame, visited: *std.AutoHashMapUnmanaged(usize, void)) !void {
        if (value != .raw_ptr or value.raw_ptr == 0) return;
        const ptr = value.raw_ptr;
        const record = self.objects.getPtr(ptr) orelse return;
        if (visited.contains(ptr)) return;
        try visited.put(self.allocator, ptr, {});
        if (!frame.pinned.contains(ptr)) {
            try frame.pinned.put(self.allocator, ptr, {});
            record.pin_count += 1;
        }
        switch (record.kind) {
            .array => |object| {
                for (object.items[0..object.len]) |item| try self.pinValueRecursive(runtime_abi.bridgeValueToValue(item), frame, visited);
            },
            .closure => |closure| {
                for (closure.captures) |capture| try self.pinValueRecursive(capture, frame, visited);
            },
            .struct_fields => |struct_fields| {
                for (struct_fields.fields) |field| try self.pinValueRecursive(field, frame, visited);
            },
            .string_bytes => {},
        }
    }

    fn unpinPtr(self: *Heap, ptr: usize) void {
        if (ptr == 0) return;
        const record_ptr = self.objects.getPtr(ptr) orelse return;
        if (record_ptr.pin_count == 0) return;
        record_ptr.pin_count -= 1;
        if (record_ptr.pin_count != 0 or record_ptr.ref_count != 0) return;
        const removed = self.objects.fetchRemove(ptr) orelse return;
        self.recordFree(removed.value.kind);
        self.destroy(removed.value.kind);
    }

    const StatsKind = enum {
        array,
        closure,
        struct_fields,
        string_bytes,
    };

    fn recordAlloc(self: *Heap, kind: StatsKind) void {
        switch (kind) {
            .array => {
                self.stats.arrays_current += 1;
                self.stats.arrays_allocated += 1;
                self.stats.arrays_peak = @max(self.stats.arrays_peak, self.stats.arrays_current);
            },
            .closure => {
                self.stats.closures_current += 1;
                self.stats.closures_allocated += 1;
                self.stats.closures_peak = @max(self.stats.closures_peak, self.stats.closures_current);
            },
            .struct_fields => {
                self.stats.structs_current += 1;
                self.stats.structs_allocated += 1;
                self.stats.structs_peak = @max(self.stats.structs_peak, self.stats.structs_current);
            },
            .string_bytes => {
                self.stats.strings_current += 1;
                self.stats.strings_allocated += 1;
                self.stats.strings_peak = @max(self.stats.strings_peak, self.stats.strings_current);
            },
        }
    }

    fn recordFree(self: *Heap, kind: ObjectKind) void {
        switch (kind) {
            .array => {
                if (self.stats.arrays_current > 0) self.stats.arrays_current -= 1;
                self.stats.arrays_freed += 1;
            },
            .closure => {
                if (self.stats.closures_current > 0) self.stats.closures_current -= 1;
                self.stats.closures_freed += 1;
            },
            .struct_fields => {
                if (self.stats.structs_current > 0) self.stats.structs_current -= 1;
                self.stats.structs_freed += 1;
            },
            .string_bytes => {
                if (self.stats.strings_current > 0) self.stats.strings_current -= 1;
                self.stats.strings_freed += 1;
            },
        }
    }

    fn destroy(self: *Heap, kind: ObjectKind) void {
        switch (kind) {
            .array => |object| {
                const items = object.items[0..@max(object.len, 1)];
                for (items[0..object.len]) |item| self.releaseValue(runtime_abi.bridgeValueToValue(item));
                self.allocator.free(items);
                self.allocator.destroy(object);
            },
            .closure => |closure| {
                self.releaseSlots(closure.captures);
                self.allocator.free(closure.captures);
                self.allocator.destroy(closure);
            },
            .struct_fields => |value| {
                self.releaseSlots(value.fields);
                self.allocator.free(value.fields);
            },
            .string_bytes => |bytes| self.allocator.free(bytes),
        }
    }
};

test "boundary pin scopes preserve managed graphs until unpinned" {
    var heap = Heap.init(std.testing.allocator);
    defer heap.deinit();

    const nested_fields = try std.testing.allocator.alloc(runtime_abi.Value, 1);
    nested_fields[0] = .{ .integer = 7 };
    const nested_ptr = try heap.registerStruct("Nested", nested_fields);

    const fields = try std.testing.allocator.alloc(runtime_abi.Value, 1);
    fields[0] = .{ .raw_ptr = nested_ptr };
    const root_ptr = try heap.registerStruct("Root", fields);

    heap.releaseValue(.{ .raw_ptr = nested_ptr });
    try heap.beginBoundaryPinScope();
    defer heap.endBoundaryPinScope();
    try heap.pinBoundaryValue(.{ .raw_ptr = root_ptr });

    heap.releaseValue(.{ .raw_ptr = root_ptr });
    try std.testing.expectEqual(@as(usize, 2), heap.count());
}

test "unpin destroys zero-ref objects" {
    var heap = Heap.init(std.testing.allocator);
    defer heap.deinit();

    const fields = try std.testing.allocator.alloc(runtime_abi.Value, 1);
    fields[0] = .{ .integer = 1 };
    const ptr = try heap.registerStruct("Value", fields);

    try heap.beginBoundaryPinScope();
    try heap.pinBoundaryValue(.{ .raw_ptr = ptr });
    heap.releaseValue(.{ .raw_ptr = ptr });
    try std.testing.expectEqual(@as(usize, 1), heap.count());
    heap.endBoundaryPinScope();

    try std.testing.expectEqual(@as(usize, 0), heap.count());
}

test "empty structs use managed non-zero storage" {
    var heap = Heap.init(std.testing.allocator);
    defer heap.deinit();

    const fields = try std.testing.allocator.alloc(runtime_abi.Value, 0);
    const ptr = try heap.registerStruct("Empty", fields);

    try std.testing.expect(ptr != 0);
    try std.testing.expect(heap.isManagedValue(.{ .raw_ptr = ptr }));
    heap.releaseValue(.{ .raw_ptr = ptr });
    try std.testing.expectEqual(@as(usize, 0), heap.count());
}
