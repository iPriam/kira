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

pub const ObjectKind = union(enum) {
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

pub const ObjectRecord = struct {
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

/// Purpose-built registry map for managed-object pointers.
///
/// The heap registry is the hottest data structure of allocation-heavy VM
/// workloads: every alloc registers, every drop removes, and every
/// is-managed/ownership check probes — including misses for foreign (native)
/// pointers. A general-purpose map (std.HashMap + Wyhash) showed up as the
/// dominant profile cost, so this is a specialized open-addressing table:
///
///   - keys are non-zero pointers; 0 marks an empty slot,
///   - one cheap two-multiply mix (Murmur3 finalizer) instead of Wyhash,
///   - split key/record arrays so probing touches a dense 8-byte key lane,
///   - linear probing with backward-shift deletion: no tombstones, so heavy
///     register/drop churn (one per object lifetime) never degrades probes
///     or forces tombstone-cleanup rehashes.
pub const PointerObjectMap = struct {
    keys: []usize = &.{},
    records: []ObjectRecord = &.{},
    len: usize = 0,

    const min_capacity: usize = 64;

    fn mix(key: usize) usize {
        var x: u64 = @intCast(key);
        x ^= x >> 33;
        x *%= 0xff51afd7ed558ccd;
        x ^= x >> 33;
        x *%= 0xc4ceb9fe1a85ec53;
        x ^= x >> 33;
        return @intCast(x);
    }

    pub fn deinit(self: *PointerObjectMap, allocator: std.mem.Allocator) void {
        allocator.free(self.keys);
        allocator.free(self.records);
        self.* = .{};
    }

    pub fn count(self: *const PointerObjectMap) usize {
        return self.len;
    }

    fn slotOf(self: *const PointerObjectMap, key: usize) ?usize {
        if (self.len == 0 or key == 0) return null;
        const mask = self.keys.len - 1;
        var index = mix(key) & mask;
        while (true) {
            const slot_key = self.keys[index];
            if (slot_key == key) return index;
            if (slot_key == 0) return null;
            index = (index + 1) & mask;
        }
    }

    pub fn contains(self: *const PointerObjectMap, key: usize) bool {
        return self.slotOf(key) != null;
    }

    pub fn getPtr(self: *const PointerObjectMap, key: usize) ?*ObjectRecord {
        const index = self.slotOf(key) orelse return null;
        return &self.records[index];
    }

    /// Insert or overwrite. `key` must be a non-zero pointer.
    pub fn put(self: *PointerObjectMap, allocator: std.mem.Allocator, key: usize, record: ObjectRecord) !void {
        std.debug.assert(key != 0);
        // Grow at 2/3 load; backward-shift deletion keeps clusters tight so a
        // moderately high load factor stays cheap.
        if (self.keys.len == 0 or self.len * 3 >= self.keys.len * 2) try self.grow(allocator);
        const mask = self.keys.len - 1;
        var index = mix(key) & mask;
        while (true) {
            const slot_key = self.keys[index];
            if (slot_key == 0) {
                self.keys[index] = key;
                self.records[index] = record;
                self.len += 1;
                return;
            }
            if (slot_key == key) {
                self.records[index] = record;
                return;
            }
            index = (index + 1) & mask;
        }
    }

    pub fn fetchRemove(self: *PointerObjectMap, key: usize) ?ObjectRecord {
        const found = self.slotOf(key) orelse return null;
        const removed = self.records[found];
        const mask = self.keys.len - 1;
        // Backward-shift deletion: pull cluster members whose probe path
        // crosses the hole back into it, leaving no tombstone behind.
        var hole = found;
        var index = (found + 1) & mask;
        while (self.keys[index] != 0) {
            const ideal = mix(self.keys[index]) & mask;
            if (((index -% ideal) & mask) >= ((index -% hole) & mask)) {
                self.keys[hole] = self.keys[index];
                self.records[hole] = self.records[index];
                hole = index;
            }
            index = (index + 1) & mask;
        }
        self.keys[hole] = 0;
        self.len -= 1;
        return removed;
    }

    fn grow(self: *PointerObjectMap, allocator: std.mem.Allocator) !void {
        const new_capacity = @max(min_capacity, self.keys.len * 2);
        const new_keys = try allocator.alloc(usize, new_capacity);
        errdefer allocator.free(new_keys);
        const new_records = try allocator.alloc(ObjectRecord, new_capacity);
        @memset(new_keys, 0);
        const mask = new_capacity - 1;
        for (self.keys, 0..) |key, old_index| {
            if (key == 0) continue;
            var index = mix(key) & mask;
            while (new_keys[index] != 0) index = (index + 1) & mask;
            new_keys[index] = key;
            new_records[index] = self.records[old_index];
        }
        allocator.free(self.keys);
        allocator.free(self.records);
        self.keys = new_keys;
        self.records = new_records;
    }

    pub const Entry = struct { key: usize, record: *ObjectRecord };

    pub const Iterator = struct {
        map: *const PointerObjectMap,
        slot: usize = 0,

        /// Iteration order is unspecified. The iterator is invalidated by any
        /// map mutation (matching std.HashMap semantics).
        pub fn next(self: *Iterator) ?Entry {
            while (self.slot < self.map.keys.len) {
                const index = self.slot;
                self.slot += 1;
                if (self.map.keys[index] != 0) {
                    return .{ .key = self.map.keys[index], .record = &self.map.records[index] };
                }
            }
            return null;
        }
    };

    pub fn iterator(self: *const PointerObjectMap) Iterator {
        return .{ .map = self };
    }
};

pub const Heap = struct {
    allocator: std.mem.Allocator,
    objects: PointerObjectMap,
    pin_frames: std.ArrayListUnmanaged(PinFrame) = .empty,
    stats: HeapStats = .{},
    // Free-list caches for the allocation shapes the VM churns hardest:
    // struct field slices, array backing stores, and the array/closure object
    // headers. Every pooled block is a real allocator allocation of exactly
    // the bucket size, so pooled and non-pooled call sites stay
    // interchangeable — a block handed out by the pool may be released with
    // allocator.free and vice versa; the pool only adds reuse, never a new
    // allocation shape.
    value_slice_pools: [max_pooled_slice_len + 1]std.ArrayListUnmanaged([*]runtime_abi.Value) = @splat(.empty),
    bridge_slice_pools: [max_pooled_slice_len + 1]std.ArrayListUnmanaged([*]runtime_abi.BridgeValue) = @splat(.empty),
    array_object_pool: std.ArrayListUnmanaged(*ArrayObject) = .empty,
    closure_object_pool: std.ArrayListUnmanaged(*ClosureObject) = .empty,

    const max_pooled_slice_len = 16;
    const max_pool_entries = 1024;

    pub fn allocValueSlice(self: *Heap, len: usize) ![]runtime_abi.Value {
        if (len >= 1 and len <= max_pooled_slice_len) {
            if (self.value_slice_pools[len].pop()) |ptr| return ptr[0..len];
        }
        return self.allocator.alloc(runtime_abi.Value, len);
    }

    pub fn freeValueSlice(self: *Heap, slice: []runtime_abi.Value) void {
        if (slice.len >= 1 and slice.len <= max_pooled_slice_len) {
            const pool = &self.value_slice_pools[slice.len];
            if (pool.items.len < max_pool_entries) {
                pool.append(self.allocator, slice.ptr) catch self.allocator.free(slice);
                return;
            }
        }
        self.allocator.free(slice);
    }

    pub fn allocBridgeSlice(self: *Heap, len: usize) ![]runtime_abi.BridgeValue {
        if (len >= 1 and len <= max_pooled_slice_len) {
            if (self.bridge_slice_pools[len].pop()) |ptr| return ptr[0..len];
        }
        return self.allocator.alloc(runtime_abi.BridgeValue, len);
    }

    pub fn freeBridgeSlice(self: *Heap, slice: []runtime_abi.BridgeValue) void {
        if (slice.len >= 1 and slice.len <= max_pooled_slice_len) {
            const pool = &self.bridge_slice_pools[slice.len];
            if (pool.items.len < max_pool_entries) {
                pool.append(self.allocator, slice.ptr) catch self.allocator.free(slice);
                return;
            }
        }
        self.allocator.free(slice);
    }

    pub fn allocArrayObject(self: *Heap) !*ArrayObject {
        return self.array_object_pool.pop() orelse self.allocator.create(ArrayObject);
    }

    pub fn freeArrayObject(self: *Heap, object: *ArrayObject) void {
        if (self.array_object_pool.items.len < max_pool_entries) {
            self.array_object_pool.append(self.allocator, object) catch self.allocator.destroy(object);
            return;
        }
        self.allocator.destroy(object);
    }

    pub fn allocClosureObject(self: *Heap) !*ClosureObject {
        return self.closure_object_pool.pop() orelse self.allocator.create(ClosureObject);
    }

    pub fn freeClosureObject(self: *Heap, object: *ClosureObject) void {
        if (self.closure_object_pool.items.len < max_pool_entries) {
            self.closure_object_pool.append(self.allocator, object) catch self.allocator.destroy(object);
            return;
        }
        self.allocator.destroy(object);
    }

    fn drainPools(self: *Heap) void {
        for (&self.value_slice_pools, 0..) |*pool, len| {
            for (pool.items) |ptr| self.allocator.free(ptr[0..len]);
            pool.deinit(self.allocator);
        }
        for (&self.bridge_slice_pools, 0..) |*pool, len| {
            for (pool.items) |ptr| self.allocator.free(ptr[0..len]);
            pool.deinit(self.allocator);
        }
        for (self.array_object_pool.items) |object| self.allocator.destroy(object);
        self.array_object_pool.deinit(self.allocator);
        for (self.closure_object_pool.items) |object| self.allocator.destroy(object);
        self.closure_object_pool.deinit(self.allocator);
    }

    pub fn init(allocator: std.mem.Allocator) Heap {
        return .{
            .allocator = allocator,
            .objects = .{},
            .pin_frames = .empty,
        };
    }

    pub fn deinit(self: *Heap) void {
        while (self.pin_frames.items.len != 0) self.endBoundaryPinScope();
        self.pin_frames.deinit(self.allocator);
        while (self.objects.count() != 0) {
            var iterator = self.objects.iterator();
            if (iterator.next()) |entry| self.dropPtr(entry.key);
        }
        self.objects.deinit(self.allocator);
        self.drainPools();
    }

    pub fn registerArray(self: *Heap, object: *ArrayObject) !usize {
        const ptr = @intFromPtr(object);
        try self.objects.put(self.allocator, ptr, .{ .kind = .{ .array = object } });
        self.recordAlloc(.array);
        return ptr;
    }

    pub fn registerClosure(self: *Heap, object: *ClosureObject) !usize {
        const ptr = @intFromPtr(object);
        try self.objects.put(self.allocator, ptr, .{ .kind = .{ .closure = object } });
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
            owned_fields = try self.allocValueSlice(1);
            owned_fields[0] = .{ .void = {} };
        }
        const ptr = @intFromPtr(owned_fields.ptr);
        try self.objects.put(self.allocator, ptr, .{ .origin = origin, .kind = .{ .struct_fields = .{
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
        try self.objects.put(self.allocator, ptr, .{ .kind = .{ .string_bytes = bytes } });
        self.recordAlloc(.string_bytes);
    }

    pub fn beginBoundaryPinScope(self: *Heap) !void {
        try self.pin_frames.append(self.allocator, .{});
    }

    pub fn endBoundaryPinScope(self: *Heap) void {
        if (self.pin_frames.items.len == 0) return;
        const frame = self.pin_frames.pop().?;
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
            runtime_alloc: usize = 0,
            native_materialize: usize = 0,
        };
        var counts = std.StringHashMap(TypeStats).init(std.heap.page_allocator);
        defer counts.deinit();

        var iterator = self.objects.iterator();
        while (iterator.next()) |entry| {
            switch (entry.record.kind) {
                .struct_fields => |value| {
                    const result = counts.getOrPut(value.type_name) catch continue;
                    if (!result.found_existing) result.value_ptr.* = .{};
                    result.value_ptr.current += 1;
                    switch (entry.record.origin) {
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
                "Kira runtime memory detail: structType={s} current={d} runtimeAlloc={d} nativeMaterialize={d}\n",
                .{
                    entry.key_ptr.*,
                    entry.value_ptr.current,
                    entry.value_ptr.runtime_alloc,
                    entry.value_ptr.native_materialize,
                },
            );
        }
    }

    pub fn dropValue(self: *Heap, value: runtime_abi.Value) void {
        if (value == .raw_ptr) self.dropPtr(value.raw_ptr);
        if (value == .string and value.string.len != 0) self.dropPtr(@intFromPtr(value.string.ptr));
    }

    pub fn isManagedValue(self: *const Heap, value: runtime_abi.Value) bool {
        return switch (value) {
            .raw_ptr => |ptr| self.objects.contains(ptr),
            .string => |bytes| bytes.len != 0 and self.objects.contains(@intFromPtr(bytes.ptr)),
            else => false,
        };
    }

    /// Single-probe record lookup for hot paths that would otherwise chain
    /// several `getClosure`/`getArray`/`getStructTypeName` probes on the same
    /// key. The pointer is only valid until the next map mutation.
    pub fn getRecord(self: *const Heap, ptr: usize) ?*const ObjectRecord {
        return self.objects.getPtr(ptr);
    }

    pub fn getClosure(self: *const Heap, ptr: usize) ?*const ClosureObject {
        const record = self.objects.getPtr(ptr) orelse return null;
        return switch (record.kind) {
            .closure => |closure| closure,
            else => null,
        };
    }

    pub fn getArray(self: *const Heap, ptr: usize) ?*const ArrayObject {
        const record = self.objects.getPtr(ptr) orelse return null;
        return switch (record.kind) {
            .array => |array| array,
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

    pub fn assignTransferred(self: *Heap, slot: *runtime_abi.Value, value: runtime_abi.Value) void {
        const old = slot.*;
        slot.* = value;
        self.dropValue(old);
    }

    pub fn assignBorrowed(self: *Heap, slot: *runtime_abi.Value, value: runtime_abi.Value) void {
        const old = slot.*;
        slot.* = value;
        self.dropValue(old);
    }

    pub fn dropSlots(self: *Heap, slots: []runtime_abi.Value) void {
        for (slots) |*slot| self.assignTransferred(slot, .{ .void = {} });
    }

    pub fn replaceArrayItem(self: *Heap, slot: *runtime_abi.BridgeValue, value: runtime_abi.Value) void {
        const old = runtime_abi.bridgeValueToValue(slot.*);
        slot.* = runtime_abi.bridgeValueFromValue(value);
        self.dropValue(old);
    }

    pub fn appendArrayItem(self: *Heap, object: *ArrayObject, value: runtime_abi.Value) !void {
        const old_items = object.items[0..@max(object.len, 1)];
        const new_len = object.len + 1;
        // Invariant: the items allocation is always exactly max(len, 1) elements
        // (every alloc/free site in the VM relies on it). Growing in place keeps
        // that invariant while skipping the copy+free; size-class allocators
        // (smp) accept most of these, so repeated appends amortize without
        // changing the ArrayObject ABI shared with native code.
        if (self.allocator.resize(old_items, new_len)) {
            object.items[object.len] = runtime_abi.bridgeValueFromValue(value);
            object.len = new_len;
            return;
        }
        const new_items = try self.allocBridgeSlice(new_len);
        @memcpy(new_items[0..object.len], old_items[0..object.len]);
        new_items[object.len] = runtime_abi.bridgeValueFromValue(value);
        self.freeBridgeSlice(old_items);
        object.items = new_items.ptr;
        object.len = new_len;
    }

    fn dropPtr(self: *Heap, ptr: usize) void {
        if (ptr == 0) return;
        const removed = self.objects.fetchRemove(ptr) orelse return;
        self.recordFree(removed.kind);
        self.destroy(removed.kind);
    }

    fn pinValueRecursive(self: *Heap, value: runtime_abi.Value, frame: *PinFrame, visited: *std.AutoHashMapUnmanaged(usize, void)) !void {
        if (value != .raw_ptr or value.raw_ptr == 0) return;
        const ptr = value.raw_ptr;
        const record = self.objects.getPtr(ptr) orelse return;
        if (visited.contains(ptr)) return;
        try visited.put(self.allocator, ptr, {});
        if (!frame.pinned.contains(ptr)) {
            try frame.pinned.put(self.allocator, ptr, {});
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
                for (items[0..object.len]) |item| self.dropValue(runtime_abi.bridgeValueToValue(item));
                self.freeBridgeSlice(items);
                self.freeArrayObject(object);
            },
            .closure => |closure| {
                self.dropSlots(closure.captures);
                self.freeValueSlice(closure.captures);
                self.freeClosureObject(closure);
            },
            .struct_fields => |value| {
                self.dropSlots(value.fields);
                self.freeValueSlice(value.fields);
            },
            .string_bytes => |bytes| self.allocator.free(bytes),
        }
    }
};

test "boundary pin scopes do not own managed graphs" {
    var heap = Heap.init(std.testing.allocator);
    defer heap.deinit();

    const nested_fields = try std.testing.allocator.alloc(runtime_abi.Value, 1);
    nested_fields[0] = .{ .integer = 7 };
    const nested_ptr = try heap.registerStruct("Nested", nested_fields);

    const fields = try std.testing.allocator.alloc(runtime_abi.Value, 1);
    fields[0] = .{ .raw_ptr = nested_ptr };
    const root_ptr = try heap.registerStruct("Root", fields);

    try heap.beginBoundaryPinScope();
    defer heap.endBoundaryPinScope();
    try heap.pinBoundaryValue(.{ .raw_ptr = root_ptr });

    heap.dropValue(.{ .raw_ptr = root_ptr });
    try std.testing.expectEqual(@as(usize, 0), heap.count());
}

test "empty structs use managed non-zero storage" {
    var heap = Heap.init(std.testing.allocator);
    defer heap.deinit();

    const fields = try std.testing.allocator.alloc(runtime_abi.Value, 0);
    const ptr = try heap.registerStruct("Empty", fields);

    try std.testing.expect(ptr != 0);
    try std.testing.expect(heap.isManagedValue(.{ .raw_ptr = ptr }));
    heap.dropValue(.{ .raw_ptr = ptr });
    try std.testing.expectEqual(@as(usize, 0), heap.count());
}
