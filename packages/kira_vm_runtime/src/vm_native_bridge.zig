//! Native-layout bridge for the VM.
//!
//! Everything that copies, materializes, synchronizes, or destroys values
//! across the VM <-> native representation boundary lives here: struct/array/
//! enum native-layout copies, native-state preservation and recovery, and
//! closure export/materialization. Functions take the owning `Vm` as their
//! first parameter; `Vm` keeps thin method wrappers for the public surface
//! (used by the hybrid runtime, the interpreter, and vm_helpers), so the
//! call-site API is unchanged.

const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const native_layout = @import("native_layout.zig");
const helper_impl = @import("vm_helpers.zig");
const construct_any = @import("vm_construct_any.zig");
const ownership = @import("ownership.zig");
const vm_mod = @import("vm.zig");

const Vm = vm_mod.Vm;
const NativeStateBox = vm_mod.NativeStateBox;
const ArrayObject = ownership.ArrayObject;
const ClosureObject = ownership.ClosureObject;

pub fn materializeNativeStruct(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) !usize {
    runtime_abi.emitExecutionTrace("BRIDGE", "MATERIALIZE", "native->runtime type={s} ptr=0x{x}", .{ type_name, native_ptr });
    return copyStructFromNativeLayout(self, module, type_name, native_ptr);
}

pub fn materializeNativeClosure(self: *Vm, module: *const bytecode.Module, native_ptr: usize, external_capture_types: ?[]const bytecode.TypeRef) !usize {
    if (native_ptr == 0) return 0;
    if (self.heap.getClosure(native_ptr) != null) return native_ptr;
    const raw_native_ptr = runtime_abi.untagNativeClosurePointer(native_ptr);
    const function_id_ptr: *const i64 = @ptrFromInt(raw_native_ptr);
    const capture_count_ptr: *const i64 = @ptrFromInt(raw_native_ptr + 8);
    const function_id_i64 = function_id_ptr.*;
    const capture_count_i64 = capture_count_ptr.*;
    if (function_id_i64 < 0 or function_id_i64 > std.math.maxInt(u32)) {
        return native_ptr;
    }
    if (capture_count_i64 < 0) {
        self.rememberError("native closure capture count is negative");
        return error.RuntimeFailure;
    }

    const capture_count: usize = @intCast(capture_count_i64);
    const function_id: u32 = @intCast(function_id_i64);
    const function_decl = module.findFunctionById(function_id);
    const native_slots: [*]const runtime_abi.BridgeValue = @ptrFromInt(raw_native_ptr + 16);
    const closure = try self.allocator.create(ClosureObject);
    errdefer self.allocator.destroy(closure);
    const captures = try self.allocator.alloc(runtime_abi.Value, capture_count);
    for (captures) |*capture| capture.* = .{ .void = {} };
    var initialized: usize = 0;
    errdefer {
        self.heap.dropSlots(captures[0..initialized]);
        self.allocator.free(captures);
    }
    for (0..capture_count) |index| {
        var capture_value = runtime_abi.bridgeValueToValue(native_slots[index]);
        var capture_is_owned = false;
        if (function_decl) |decl| {
            const param_index = decl.param_count - @as(u32, @intCast(capture_count)) + @as(u32, @intCast(index));
            const capture_ty = decl.local_types[param_index];
            if (capture_ty.kind == .ffi_struct and capture_value == .raw_ptr and capture_value.raw_ptr != 0) {
                capture_value = .{ .raw_ptr = try copyStructFromNativeLayout(self, module, capture_ty.name orelse {
                    self.rememberError("native closure capture type is missing a name");
                    return error.RuntimeFailure;
                }, capture_value.raw_ptr) };
                capture_is_owned = true;
            } else if (capture_ty.kind == .raw_ptr) {
                capture_value = try materializeCallbackValueFromNative(self, module, capture_ty, capture_value);
                capture_is_owned = true;
            }
        } else if (external_capture_types) |capture_types| {
            if (index >= capture_types.len) {
                self.rememberError("native closure capture metadata is incomplete");
                return error.RuntimeFailure;
            }
            const capture_ty = capture_types[index];
            if (capture_ty.kind == .ffi_struct and capture_value == .raw_ptr and capture_value.raw_ptr != 0) {
                capture_value = .{ .raw_ptr = try copyStructFromNativeLayout(self, module, capture_ty.name orelse {
                    self.rememberError("native closure capture type is missing a name");
                    return error.RuntimeFailure;
                }, capture_value.raw_ptr) };
                capture_is_owned = true;
            } else if (capture_ty.kind == .raw_ptr) {
                capture_value = try materializeCallbackValueFromNative(self, module, capture_ty, capture_value);
                capture_is_owned = true;
            }
        }
        if (capture_is_owned) {
            self.heap.assignTransferred(&captures[index], capture_value);
        } else {
            self.heap.assignBorrowed(&captures[index], capture_value);
        }
        initialized += 1;
    }
    closure.* = .{
        .function_id = function_id,
        .is_native = function_decl == null,
        .captures = captures,
    };
    runtime_abi.emitExecutionTrace("BRIDGE", "MATERIALIZE", "native->runtime closure fn={d} captures={d} ptr=0x{x}", .{ closure.function_id, capture_count, raw_native_ptr });
    return self.heap.registerClosure(closure);
}

pub fn lowerStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) !usize {
    runtime_abi.emitExecutionTrace("BRIDGE", "COPY", "runtime->native type={s} ptr=0x{x}", .{ type_name, runtime_ptr });
    return copyStructToNativeLayout(self, module, type_name, runtime_ptr);
}

pub fn writeStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) !void {
    runtime_abi.emitExecutionTrace("BRIDGE", "COPY", "sync runtime->native type={s} src=0x{x} dst=0x{x}", .{ type_name, runtime_ptr, native_ptr });
    try copyStructToNativeLayoutInto(self, module, type_name, runtime_ptr, native_ptr);
}

pub fn copyArrayToNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize) anyerror!usize {
    if (self.heap.getArray(runtime_array_ptr) == null) {
        self.rememberFmt(
            "array native copy was handed an unmanaged/dangling array pointer: ty={s} ptr=0x{x}",
            .{ array_ty.name orelse "?", runtime_array_ptr },
        );
        return error.RuntimeFailure;
    }
    const source: *const ArrayObject = @ptrFromInt(runtime_array_ptr);
    const object = try self.allocator.create(ArrayObject);
    const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
    for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
    object.* = .{
        .len = source.len,
        .items = items.ptr,
    };
    recordNativeArrayAlloc(self);
    errdefer destroyArrayNativeLayout(self, module, array_ty, @intFromPtr(object));

    const element_ty = try self.arrayElementType(module, array_ty);
    for (source.items[0..source.len], 0..) |item, index| {
        const value = runtime_abi.bridgeValueToValue(item);
        items[index] = runtime_abi.bridgeValueFromValue(try copyValueToNativeLayout(self, module, element_ty, value));
    }
    return @intFromPtr(object);
}

pub fn copyArrayFromNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize) anyerror!usize {
    const source: *const ArrayObject = @ptrFromInt(native_array_ptr);
    const object = try self.allocator.create(ArrayObject);
    errdefer self.allocator.destroy(object);
    const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
    const element_ty = try self.arrayElementType(module, array_ty);
    for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
    var initialized: usize = 0;
    errdefer {
        for (items[0..initialized]) |item| self.heap.dropValue(runtime_abi.bridgeValueToValue(item));
        self.allocator.free(items);
    }
    for (source.items[0..source.len], 0..) |item, index| {
        const value = runtime_abi.bridgeValueToValue(item);
        items[index] = runtime_abi.bridgeValueFromValue(try copyValueFromNativeLayout(self, module, element_ty, value));
        initialized += 1;
    }
    object.* = .{
        .len = source.len,
        .items = items.ptr,
    };
    return try self.heap.registerArray(object);
}

pub fn writeArrayToNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize, native_array_ptr: usize) anyerror!void {
    const source = self.heap.getArray(runtime_array_ptr) orelse {
        self.rememberFmt(
            "array native writeback was handed an unmanaged/dangling runtime array pointer: ty={s} ptr=0x{x}",
            .{ array_ty.name orelse "?", runtime_array_ptr },
        );
        return error.RuntimeFailure;
    };
    const destination: *ArrayObject = @ptrFromInt(native_array_ptr);

    const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
    const element_ty = try self.arrayElementType(module, array_ty);
    for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
    var initialized: usize = 0;
    errdefer {
        for (items[0..initialized]) |item| {
            destroyNativeLayoutValueWithOwner(self, module, element_ty, runtime_abi.bridgeValueToValue(item), .vm);
        }
        self.allocator.free(items);
    }
    for (source.items[0..source.len], 0..) |item, index| {
        const value = runtime_abi.bridgeValueToValue(item);
        items[index] = runtime_abi.bridgeValueFromValue(try copyValueToNativeLayout(self, module, element_ty, value));
        initialized += 1;
    }

    const old_items = destination.items[0..@max(destination.len, 1)];
    for (old_items[0..destination.len]) |item| {
        destroyNativeLayoutValueWithOwner(self, module, element_ty, runtime_abi.bridgeValueToValue(item), .vm);
    }
    self.allocator.free(old_items);
    destination.len = source.len;
    destination.items = items.ptr;
}

pub fn syncArrayFromNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize, native_array_ptr: usize) anyerror!void {
    const source: *const ArrayObject = @ptrFromInt(native_array_ptr);
    const destination: *ArrayObject = @ptrFromInt(runtime_array_ptr);

    const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
    const element_ty = try self.arrayElementType(module, array_ty);
    for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
    var initialized: usize = 0;
    errdefer {
        for (items[0..initialized]) |item| self.heap.dropValue(runtime_abi.bridgeValueToValue(item));
        self.allocator.free(items);
    }
    for (source.items[0..source.len], 0..) |item, index| {
        const value = runtime_abi.bridgeValueToValue(item);
        items[index] = runtime_abi.bridgeValueFromValue(try copyValueFromNativeLayout(self, module, element_ty, value));
        initialized += 1;
    }

    const old_items = destination.items[0..@max(destination.len, 1)];
    for (old_items[0..destination.len]) |item| self.heap.dropValue(runtime_abi.bridgeValueToValue(item));
    self.allocator.free(old_items);
    destination.len = source.len;
    destination.items = items.ptr;
}

pub fn syncStructFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) !void {
    runtime_abi.emitExecutionTrace("BRIDGE", "COPY", "sync native->runtime type={s} src=0x{x} dst=0x{x}", .{ type_name, native_ptr, runtime_ptr });
    try copyStructFromNativeLayoutInto(self, module, type_name, runtime_ptr, native_ptr);
}

pub fn destroyArrayNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize) void {
    destroyArrayNativeLayoutWithOwner(self, module, array_ty, native_array_ptr, .vm);
}

pub fn destroyOwnedArrayNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize) void {
    destroyArrayNativeLayoutWithOwner(self, module, array_ty, native_array_ptr, .c);
}

fn destroyArrayNativeLayoutWithOwner(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize, owner: NativeLayoutOwner) void {
    if (native_array_ptr == 0) return;
    const object: *ArrayObject = @ptrFromInt(native_array_ptr);
    const items = object.items[0..@max(object.len, 1)];
    const element_ty = self.arrayElementType(module, array_ty) catch .{ .kind = .raw_ptr };
    // Elements keep the per-value `owner`: a native-owned array holds native-owned
    // elements (e.g. `kira_struct_alloc`'d struct elements with their 8-byte header),
    // which must be released by their own type-specific scheme.
    for (items[0..object.len]) |item| {
        destroyNativeLayoutValueWithOwner(self, module, element_ty, runtime_abi.bridgeValueToValue(item), owner);
    }
    // The array BLOCK (the `ArrayObject` and its `items` buffer) is always owned by
    // `self.allocator`, for BOTH owners: VM-built native arrays come straight from
    // `self.allocator` (`copyArrayToNativeLayout`), and native-built arrays come from
    // `kira_array_alloc` -> `kira_bridge_alloc`, whose installed hook
    // (`kira_hybrid_install_array_allocator`) is wired to this same allocator. (Unlike
    // `kira_struct_alloc`, the array helpers route through the installed VM allocator,
    // not raw libc.) Freeing the block with the C allocator hands libc an smp pointer
    // -> "pointer being freed was not allocated".
    self.allocator.free(items);
    self.allocator.destroy(object);
    recordNativeArrayFree(self);
}

pub fn destroyStructNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
    destroyStructNativeLayoutWithOwner(self, module, type_name, native_ptr, .vm);
}

pub fn destroyOwnedStructNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
    destroyStructNativeLayoutWithOwner(self, module, type_name, native_ptr, .c);
}

fn destroyStructNativeLayoutWithOwner(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize, owner: NativeLayoutOwner) void {
    if (native_ptr == 0) return;
    destroyStructNativeLayoutFieldsWithOwner(self, module, type_name, native_ptr, owner);
    const layout = native_layout.structLayout(module, type_name) catch return;
    const word_count = @max(1, std.math.divCeil(usize, layout.size, @sizeOf(u64)) catch unreachable);
    const words: [*]u64 = @ptrFromInt(native_ptr);
    const free_owner: NativeLayoutOwner = if (std.c.getenv("KIRA_RESULT_VM_FREE") != null) .vm else owner;
    switch (free_owner) {
        // VM-allocated native-layout structs (`copyStructToNativeLayout`) are a bare
        // `self.allocator.alloc(u64, ...)` with no header, so the payload pointer IS
        // the base.
        .vm => self.allocator.free(words[0..word_count]),
        // Native-owned structs (a `@Native`/native function result, or an owned heap
        // element of a native-returned array/enum) are produced by `kira_struct_alloc`
        // (runtime_helpers.c), which `malloc`s an 8-byte type-id header in front of the
        // payload and returns `base + 8`. The matching deallocator is `kira_struct_free`
        // = `free(ptr - 8)`. Freeing the payload pointer directly hands libc a non-base
        // address ("pointer being freed was not allocated"), so free the real malloc
        // base. The same `raw_ptr - @sizeOf(u64)` header convention is read in
        // vm_construct_any.zig.
        .c => std.c.free(@ptrFromInt(native_ptr - @sizeOf(u64))),
    }
    recordNativeStructFree(self);
}

pub fn destroyNativeLayoutValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) void {
    destroyNativeLayoutValueWithOwner(self, module, ty, value, .vm);
}

fn destroyNativeLayoutValueWithOwner(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value, owner: NativeLayoutOwner) void {
    switch (ty.kind) {
        .ffi_struct => {
            if (value == .raw_ptr) {
                if (ty.name) |name| destroyStructNativeLayoutWithOwner(self, module, name, value.raw_ptr, owner);
            }
        },
        .array => {
            if (value == .raw_ptr) destroyArrayNativeLayoutWithOwner(self, module, ty, value.raw_ptr, owner);
        },
        .enum_instance => if (ty.name) |name| {
            if (value == .raw_ptr) destroyEnumNativeLayoutWithOwner(self, module, name, value.raw_ptr, owner);
        },
        .construct_any => self.heap.dropValue(value),
        else => {},
    }
}

pub fn allocateNativeState(self: *Vm, module: *const bytecode.Module, type_name: []const u8, type_id: u64, src_payload: usize) !usize {
    const type_decl = self.findTypeCached(module, type_name) orelse {
        self.rememberError("native state type could not be resolved");
        return error.RuntimeFailure;
    };
    const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(src_payload);
    const native_payload = try self.allocator.alloc(runtime_abi.BridgeValue, type_decl.fields.len);
    for (type_decl.fields, 0..) |field_decl, index| {
        if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG preserveNativeState type={s} field[{d}] kind={s} name={s}\n", .{ type_name, index, @tagName(field_decl.ty.kind), field_decl.ty.name orelse "?" });
        const native_value = try preserveNativeStateValue(self, module, field_decl.ty, src_ptr[index]);
        native_payload[index] = runtime_abi.bridgeValueFromValue(native_value);
    }

    const box = try self.allocator.create(NativeStateBox);
    box.* = NativeStateBox.init(module, type_name, type_id, type_decl.fields.len, @intFromPtr(native_payload.ptr));
    try self.native_state_boxes.put(@intFromPtr(box), {});
    return @intFromPtr(box);
}

pub fn recoverNativeState(self: *Vm, module: *const bytecode.Module, type_name: []const u8, state_token: usize, expected_type_id: u64) !usize {
    if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG recoverNativeState type={s} token=0x{x}\n", .{ type_name, state_token });
    self.native_layout_stats.native_state_recovers += 1;
    const box: *NativeStateBox = @ptrFromInt(state_token);
    if (box.type_id != expected_type_id) {
        self.rememberError("nativeRecover used a userdata token for the wrong state type");
        return error.RuntimeFailure;
    }
    if (box.runtime_payload == 0 and box.payload != 0) {
        self.native_layout_stats.native_state_materializations += 1;
        const result = try self.native_state_materialized_types.getOrPut(type_name);
        if (!result.found_existing) result.value_ptr.* = 0;
        result.value_ptr.* += 1;
        box.runtime_payload = try materializeNativeStatePayload(self, module, type_name, box.payload);
        if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG recover {s}: materialized, destroying box.payload\n", .{type_name});
        if (std.c.getenv("KIRA_SKIP_STATE_DESTROY") == null) destroyNativeStatePayload(self, module, type_name, box.payload);
        if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG recover {s}: destroyed box.payload\n", .{type_name});
        box.payload = 0;
    }
    if (box.runtime_payload == 0) {
        self.rememberError("nativeRecover used a userdata token with no state payload");
        return error.RuntimeFailure;
    }
    if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG recover {s}: returning runtime_payload\n", .{type_name});
    return box.runtime_payload;
}

pub fn materializeNativeStatePayload(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_payload_ptr: usize) !usize {
    const type_decl = self.findTypeCached(module, type_name) orelse {
        self.rememberError("native state type could not be resolved");
        return error.RuntimeFailure;
    };
    const native_payload: [*]const runtime_abi.BridgeValue = @ptrFromInt(native_payload_ptr);
    const runtime_payload = try self.allocator.alloc(runtime_abi.Value, type_decl.fields.len);
    for (type_decl.fields, 0..) |field_decl, index| {
        if (std.c.getenv("KIRA_DBG") != null) {
            const v = runtime_abi.bridgeValueToValue(native_payload[index]);
            std.debug.print("DBG materialize type={s} field[{d}] kind={s} name={s} raw=0x{x}\n", .{ type_name, index, @tagName(field_decl.ty.kind), field_decl.ty.name orelse "?", if (v == .raw_ptr) v.raw_ptr else 0 });
        }
        runtime_payload[index] = try materializeNativeStateValue(self, module, field_decl.ty, runtime_abi.bridgeValueToValue(native_payload[index]));
    }
    return @intFromPtr(runtime_payload.ptr);
}

pub fn preserveNativeStateValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) !runtime_abi.Value {
    return switch (ty.kind) {
        // `construct_any` is normally passed across the native boundary verbatim
        // (the managed VM pointer stays valid for the duration of a synchronous
        // call). Native *state*, however, outlives the frame that built it: the
        // builder returns and frame cleanup frees the boxed value, leaving the
        // stored pointer dangling (a hybrid use-after-free that surfaces when the
        // app's stored root is later recovered and dispatched on — e.g. the KiraUI
        // `KiraAppInternal { root: Widget }` rootBuilder). Deep-clone so the state
        // box owns an independent copy that frame cleanup cannot free.
        .construct_any => try self.cloneBorrowedManagedValueDynamic(module, value),
        .ffi_struct, .array, .enum_instance, .raw_ptr => try copyValueToNativeLayout(self, module, ty, value),
        else => value,
    };
}

pub fn materializeNativeStateValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) !runtime_abi.Value {
    return switch (ty.kind) {
        // Recovery materializes the box payload into a fresh runtime payload and
        // then destroys the box payload (see `recoverNativeState`). Clone the
        // preserved `construct_any` so the recovered value is independent of the
        // box copy that is about to be dropped, mirroring `preserveNativeStateValue`.
        .construct_any => if (self.heap.isManagedValue(value))
            try self.cloneBorrowedManagedValueDynamic(module, value)
        else
            try copyValueFromNativeLayout(self, module, ty, value),
        .ffi_struct, .array, .enum_instance => try copyValueFromNativeLayout(self, module, ty, value),
        .raw_ptr => try materializeCallbackValueFromNative(self, module, ty, value),
        else => value,
    };
}

pub fn destroyNativeStatePayload(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_payload_ptr: usize) void {
    if (native_payload_ptr == 0) return;
    const type_decl = self.findTypeCached(module, type_name) orelse return;
    const native_payload: [*]const runtime_abi.BridgeValue = @ptrFromInt(native_payload_ptr);
    for (type_decl.fields, 0..) |field_decl, index| {
        destroyPreservedNativeStateValue(self, module, field_decl.ty, runtime_abi.bridgeValueToValue(native_payload[index]));
    }
    self.allocator.free(native_payload[0..type_decl.fields.len]);
}

pub fn destroyMaterializedNativeStatePayload(self: *Vm, runtime_payload_ptr: usize, field_count: usize) void {
    if (runtime_payload_ptr == 0) return;
    const runtime_payload: [*]runtime_abi.Value = @ptrFromInt(runtime_payload_ptr);
    self.heap.dropSlots(runtime_payload[0..field_count]);
    self.allocator.free(runtime_payload[0..field_count]);
}

pub fn deinitTrackedNativeStates(self: *Vm) void {
    var iterator = self.native_state_boxes.iterator();
    while (iterator.next()) |entry| {
        const box: *NativeStateBox = @ptrFromInt(entry.key_ptr.*);
        if (box.payload != 0) {
            destroyNativeStatePayload(self, box.module, box.typeName(), box.payload);
        }
        if (box.runtime_payload != 0) {
            destroyMaterializedNativeStatePayload(self, box.runtime_payload, box.field_count);
        }
        self.allocator.destroy(box);
    }
    self.native_state_boxes.deinit();
}

pub fn destroyPreservedNativeStateValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) void {
    switch (ty.kind) {
        .ffi_struct => {
            if (value != .raw_ptr or value.raw_ptr == 0) return;
            destroyStructNativeLayout(self, module, ty.name orelse return, value.raw_ptr);
        },
        .array => {
            if (value != .raw_ptr or value.raw_ptr == 0) return;
            destroyArrayNativeLayout(self, module, ty, value.raw_ptr);
        },
        .enum_instance => {
            if (value != .raw_ptr or value.raw_ptr == 0) return;
            destroyEnumNativeLayout(self, module, ty.name orelse return, value.raw_ptr);
        },
        .construct_any => self.heap.dropValue(value),
        else => {},
    }
}

pub fn copyValueToNativeLayout(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
    return switch (ty.kind) {
        .ffi_struct => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk .{ .raw_ptr = try copyStructToNativeLayout(self, module, ty.name orelse {
                self.rememberError("array element struct type is missing a name");
                return error.RuntimeFailure;
            }, value.raw_ptr) };
        },
        .array => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk .{ .raw_ptr = try copyArrayToNativeLayout(self, module, ty, value.raw_ptr) };
        },
        .enum_instance => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk .{ .raw_ptr = try copyEnumToNativeLayout(self, module, ty.name orelse {
                self.rememberError("enum type is missing a name");
                return error.RuntimeFailure;
            }, value.raw_ptr) };
        },
        .construct_any => blk: {
            break :blk value;
        },
        .raw_ptr => blk: {
            if (ty.name) |name| {
                if (Vm.isCallbackTypeName(name) and value == .raw_ptr and value.raw_ptr != 0 and self.heap.getClosure(value.raw_ptr) != null) {
                    break :blk .{ .raw_ptr = try exportRuntimeClosureToNative(self, module, value.raw_ptr) };
                }
            }
            break :blk value;
        },
        else => value,
    };
}

pub fn copyValueFromNativeLayout(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
    return switch (ty.kind) {
        .ffi_struct => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk .{ .raw_ptr = try copyStructFromNativeLayout(self, module, ty.name orelse {
                self.rememberError("array element struct type is missing a name");
                return error.RuntimeFailure;
            }, value.raw_ptr) };
        },
        .array => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk .{ .raw_ptr = try copyArrayFromNativeLayout(self, module, ty, value.raw_ptr) };
        },
        .enum_instance => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk .{ .raw_ptr = try copyEnumFromNativeLayout(self, module, ty.name orelse {
                self.rememberError("enum type is missing a name");
                return error.RuntimeFailure;
            }, value.raw_ptr) };
        },
        .construct_any => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk try construct_any.materializeFromNativeIfNeeded(self, module, ty, value.raw_ptr);
        },
        .raw_ptr => try materializeCallbackValueFromNative(self, module, ty, value),
        else => value,
    };
}

pub fn materializeCallbackValueFromNative(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
    if (ty.kind != .raw_ptr) return value;
    const name = ty.name orelse return value;
    if (!Vm.isCallbackTypeName(name)) return value;
    if (value != .raw_ptr or value.raw_ptr == 0) return value;
    if (self.heap.getClosure(value.raw_ptr) != null) return value;
    if (!runtime_abi.isTaggedNativeClosurePointer(value.raw_ptr)) return value;
    return .{ .raw_ptr = try materializeNativeClosure(self, module, value.raw_ptr, null) };
}

pub fn exportRuntimeClosureToNative(self: *Vm, module: *const bytecode.Module, closure_ptr: usize) !usize {
    // No pointer-keyed dedup: a consumed closure's pointer can be reused by a
    // later, different closure, so caching by `closure_ptr` would hand the new
    // closure the stale native block (FF1 — captured closures crossing the bridge
    // dispatching to the wrong body). Always export a fresh block.
    const closure = self.heap.getClosure(closure_ptr) orelse {
        self.rememberError("callback value is not a valid runtime closure");
        return error.RuntimeFailure;
    };
    const function_decl = module.findFunctionById(closure.function_id) orelse {
        self.rememberError("runtime closure function could not be resolved");
        return error.RuntimeFailure;
    };
    if (closure.captures.len > function_decl.param_count) {
        self.rememberError("runtime closure capture metadata is inconsistent");
        return error.RuntimeFailure;
    }

    const param_count: usize = function_decl.param_count;
    const capture_types = function_decl.local_types[param_count - closure.captures.len .. param_count];
    const byte_len = 16 + closure.captures.len * @sizeOf(runtime_abi.BridgeValue);
    const word_count = @max(1, std.math.divCeil(usize, byte_len, @sizeOf(u64)) catch unreachable);
    const words = try self.allocator.alloc(u64, word_count);
    errdefer self.allocator.free(words);
    @memset(words, 0);

    const raw_ptr = @intFromPtr(words.ptr);
    const header: [*]u64 = @ptrFromInt(raw_ptr);
    header[0] = closure.function_id;
    header[1] = closure.captures.len;

    const retained_captures = try self.allocator.alloc(runtime_abi.Value, closure.captures.len);
    errdefer self.allocator.free(retained_captures);
    const slots: [*]runtime_abi.BridgeValue = @ptrFromInt(raw_ptr + 16);
    for (closure.captures, 0..) |capture, index| {
        const lowered = try copyValueToNativeLayout(self, module, capture_types[index], capture);
        retained_captures[index] = lowered;
        slots[index] = runtime_abi.bridgeValueFromValue(lowered);
    }

    try self.exported_native_closures.append(self.allocator, .{
        .native_ptr = raw_ptr,
        .captures = retained_captures,
    });
    return runtime_abi.tagNativeClosurePointer(raw_ptr);
}

fn resolveManagedEnumSlots(self: *Vm, runtime_ptr: usize) ?[*]align(1) const runtime_abi.Value {
    var candidate = runtime_ptr;
    var depth: usize = 0;
    while (candidate != 0 and depth < 8) : (depth += 1) {
        const slots: [*]align(1) const runtime_abi.Value = @ptrFromInt(candidate);
        if (slots[0] == .integer) return slots;
        if (slots[0] == .raw_ptr and slots[0].raw_ptr != 0 and slots[0].raw_ptr != candidate) {
            candidate = slots[0].raw_ptr;
            continue;
        }
        if (self.isManagedStructPointer(candidate)) break;
        return null;
    }
    return null;
}

pub fn copyEnumToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) anyerror!usize {
    const src = resolveManagedEnumSlots(self, runtime_ptr) orelse {
        self.rememberError("enum native copy requires a managed enum value");
        return error.RuntimeFailure;
    };
    if (src[0] != .integer) {
        self.rememberError("enum native copy requires an integer tag slot");
        return error.RuntimeFailure;
    }
    const words = try self.allocator.alloc(u64, 2);
    errdefer self.allocator.free(words);
    const discriminant: u32 = @intCast(src[0].integer);
    const payload_ty = enumPayloadType(self, module, type_name, discriminant) orelse {
        self.rememberFmt(
            "enum native copy could not resolve discriminant: type={s} tag={d} ptr=0x{x}",
            .{ type_name, discriminant, runtime_ptr },
        );
        return error.RuntimeFailure;
    };
    words[0] = @as(u64, @intCast(discriminant));
    words[1] = try enumPayloadToNativeWord(self, module, payload_ty, src[1]);
    return @intFromPtr(words.ptr);
}

/// Lower a VM enum value into a libc-`malloc`'d 16-byte native enum block
/// `{ i64 tag, i64 payload }` whose ownership transfers to native code.
///
/// Unlike `copyEnumToNativeLayout` (which allocates with the VM allocator and is freed
/// VM-side by `destroyEnumNativeLayout`), the block produced here is freed by the native
/// C-API backend itself: an enum stored into a native struct field is freed via
/// `kira_destroy_raw_ptr` (libc `free`) in that struct's `release_contents`, and cloned
/// via `kira_enum_clone` (libc `malloc`+memcpy) on struct copy. The VM runner uses
/// `std.heap.smp_allocator`, so handing native a VM-allocator pointer to `free` aborts
/// with "pointer being freed was not allocated". Allocating on the C heap keeps the
/// clone/free pair heap-consistent. Payloads are inline (the native enum layout carries
/// no out-of-line payload — `kira_destroy_raw_ptr` does not recurse), matching
/// `kira_enum_clone`'s verbatim 16-byte copy.
pub fn lowerEnumToNativeOwned(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) anyerror!usize {
    const src = resolveManagedEnumSlots(self, runtime_ptr) orelse {
        self.rememberError("enum native lowering requires a managed enum value");
        return error.RuntimeFailure;
    };
    if (src[0] != .integer) {
        self.rememberFmt(
            "enum native lowering requires an integer tag slot: type={s} slot0={s} slot1={s} ptr=0x{x}",
            .{ type_name, @tagName(src[0]), @tagName(src[1]), runtime_ptr },
        );
        return error.RuntimeFailure;
    }
    const discriminant: u32 = @intCast(src[0].integer);
    const payload_ty = enumPayloadType(self, module, type_name, discriminant) orelse {
        self.rememberFmt(
            "enum native lowering could not resolve discriminant: type={s} tag={d} ptr=0x{x}",
            .{ type_name, discriminant, runtime_ptr },
        );
        return error.RuntimeFailure;
    };
    const words = try std.heap.c_allocator.alloc(u64, 2);
    errdefer std.heap.c_allocator.free(words);
    words[0] = @as(u64, @intCast(discriminant));
    words[1] = try enumPayloadToNativeWord(self, module, payload_ty, src[1]);
    return @intFromPtr(words.ptr);
}

pub fn copyEnumFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) anyerror!usize {
    if (self.isManagedStructPointer(native_ptr)) {
        const cloned = try self.cloneEnumValue(module, type_name, .{ .raw_ptr = native_ptr });
        return if (cloned == .raw_ptr) cloned.raw_ptr else 0;
    }
    const resolved_ptr = try resolveNativeEnumLayoutPointer(self, module, type_name, native_ptr);
    const words: [*]const u64 = @ptrFromInt(resolved_ptr);
    const native_variant = enumNativeVariant(self, module, type_name, words[0]) orelse {
        const runtime_slots: [*]align(1) const runtime_abi.Value = @ptrFromInt(resolved_ptr);
        if (runtime_slots[0] == .integer) {
            const discriminant: u32 = @intCast(runtime_slots[0].integer);
            if (enumPayloadType(self, module, type_name, discriminant)) |payload_ty| {
                const slots = try self.allocator.alloc(runtime_abi.Value, 2);
                errdefer self.allocator.free(slots);
                slots[0] = runtime_slots[0];
                slots[1] = switch (payload_ty.kind) {
                    .ffi_struct => blk: {
                        if (runtime_slots[1] != .raw_ptr or runtime_slots[1].raw_ptr == 0) break :blk runtime_slots[1];
                        break :blk .{ .raw_ptr = try self.cloneStructValue(module, payload_ty.name orelse type_name, runtime_slots[1].raw_ptr) };
                    },
                    .array => try self.cloneArrayValueDeep(module, try self.arrayElementType(module, payload_ty), runtime_slots[1]),
                    .enum_instance => try self.cloneEnumValue(module, payload_ty.name orelse type_name, runtime_slots[1]),
                    else => runtime_slots[1],
                };
                return self.heap.registerStruct(type_name, slots);
            }
        }
        self.rememberFmt(
            "enum native copy found an invalid discriminant: type={s} tag={d} ptr=0x{x}",
            .{ type_name, words[0], resolved_ptr },
        );
        return error.RuntimeFailure;
    };
    const slots = try self.allocator.alloc(runtime_abi.Value, 2);
    errdefer self.allocator.free(slots);
    slots[0] = .{ .integer = @intCast(native_variant.discriminant) };
    slots[1] = try enumPayloadFromNativeWord(self, module, native_variant.payload_ty, words[1]);
    return self.heap.registerStruct(type_name, slots);
}

pub fn resolveNativeEnumLayoutPointer(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) anyerror!usize {
    var candidate = native_ptr;
    var depth: usize = 0;
    while (depth < 4) : (depth += 1) {
        const words: [*]const u64 = @ptrFromInt(candidate);
        if (enumNativeVariant(self, module, type_name, words[0]) != null) return candidate;
        const next_candidate: usize = @intCast(words[0]);
        if (next_candidate == 0 or next_candidate == candidate or next_candidate % @alignOf(u64) != 0) break;
        candidate = next_candidate;
    }
    return native_ptr;
}

pub fn destroyEnumNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
    destroyEnumNativeLayoutWithOwner(self, module, type_name, native_ptr, .vm);
}

fn destroyEnumNativeLayoutWithOwner(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize, owner: NativeLayoutOwner) void {
    if (native_ptr == 0) return;
    const words: [*]u64 = @ptrFromInt(native_ptr);
    if (enumNativeVariant(self, module, type_name, words[0])) |native_variant| {
        destroyEnumNativePayloadWithOwner(self, module, native_variant.payload_ty, words[1], owner);
    }
    const native_words: []u64 = words[0..2];
    switch (owner) {
        .vm => self.allocator.free(native_words),
        .c => std.heap.c_allocator.free(native_words),
    }
}

pub fn destroyOwnedEnumNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
    destroyEnumNativeLayoutWithOwner(self, module, type_name, native_ptr, .c);
}

const EnumNativeVariant = struct {
    discriminant: u32,
    payload_ty: bytecode.TypeRef,
};

const NativeLayoutOwner = enum {
    vm,
    c,
};

pub fn enumNativeVariant(self: *Vm, module: *const bytecode.Module, type_name: []const u8, word: u64) ?EnumNativeVariant {
    if (word > std.math.maxInt(u32)) return null;
    const discriminant: u32 = @intCast(word);
    const payload_ty = enumPayloadType(self, module, type_name, discriminant) orelse return null;
    return .{
        .discriminant = discriminant,
        .payload_ty = payload_ty,
    };
}

pub fn enumPayloadType(self: *Vm, module: *const bytecode.Module, type_name: []const u8, discriminant: u32) ?bytecode.TypeRef {
    const enum_decl = self.findEnumCached(module, type_name) orelse return null;
    for (enum_decl.variants) |variant| {
        if (variant.discriminant == discriminant) return variant.payload_ty orelse bytecode.TypeRef{ .kind = .void };
    }
    return null;
}

pub fn enumPayloadToNativeWord(self: *Vm, module: *const bytecode.Module, payload_ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!u64 {
    return switch (payload_ty.kind) {
        .void => 0,
        .integer => if (value == .integer) @as(u64, @bitCast(value.integer)) else 0,
        .boolean => if (value == .boolean and value.boolean) 1 else 0,
        .float => if (value == .float) @as(u64, @bitCast(value.float)) else 0,
        .string => blk: {
            if (value != .string) break :blk 0;
            const boxed = try self.allocator.create(runtime_abi.BridgeString);
            boxed.* = .{ .ptr = if (value.string.len == 0) null else value.string.ptr, .len = value.string.len };
            break :blk @intFromPtr(boxed);
        },
        .raw_ptr, .construct_any => if (value == .raw_ptr) value.raw_ptr else 0,
        .ffi_struct, .array, .enum_instance => blk: {
            const copied = try copyValueToNativeLayout(self, module, payload_ty, value);
            break :blk if (copied == .raw_ptr) copied.raw_ptr else 0;
        },
    };
}

pub fn enumPayloadFromNativeWord(self: *Vm, module: *const bytecode.Module, payload_ty: bytecode.TypeRef, word: u64) anyerror!runtime_abi.Value {
    return switch (payload_ty.kind) {
        .void => .{ .void = {} },
        .integer => .{ .integer = @as(i64, @bitCast(word)) },
        .boolean => .{ .boolean = word != 0 },
        .float => .{ .float = @as(f64, @bitCast(word)) },
        .string => blk: {
            if (word == 0) break :blk runtime_abi.Value{ .string = "" };
            const boxed: *const runtime_abi.BridgeString = @ptrFromInt(@as(usize, @intCast(word)));
            break :blk runtime_abi.Value{ .string = if (boxed.ptr) |ptr| ptr[0..boxed.len] else "" };
        },
        .raw_ptr, .construct_any => .{ .raw_ptr = @intCast(word) },
        .ffi_struct, .array, .enum_instance => try copyValueFromNativeLayout(self, module, payload_ty, .{ .raw_ptr = @intCast(word) }),
    };
}

pub fn destroyEnumNativePayload(self: *Vm, module: *const bytecode.Module, payload_ty: bytecode.TypeRef, word: u64) void {
    destroyEnumNativePayloadWithOwner(self, module, payload_ty, word, .vm);
}

fn destroyEnumNativePayloadWithOwner(self: *Vm, module: *const bytecode.Module, payload_ty: bytecode.TypeRef, word: u64, owner: NativeLayoutOwner) void {
    if (word == 0) return;
    switch (payload_ty.kind) {
        .ffi_struct => destroyStructNativeLayoutWithOwner(self, module, payload_ty.name orelse return, @intCast(word), owner),
        .array => destroyArrayNativeLayoutWithOwner(self, module, payload_ty, @intCast(word), owner),
        .enum_instance => destroyEnumNativeLayoutWithOwner(self, module, payload_ty.name orelse return, @intCast(word), owner),
        .string => switch (owner) {
            .vm => self.allocator.destroy(@as(*runtime_abi.BridgeString, @ptrFromInt(@as(usize, @intCast(word))))),
            .c => std.heap.c_allocator.destroy(@as(*runtime_abi.BridgeString, @ptrFromInt(@as(usize, @intCast(word))))),
        },
        else => {},
    }
}

fn isFlatScalarKind(kind: bytecode.TypeRef.Kind) bool {
    return switch (kind) {
        .void, .integer, .float, .boolean => true,
        else => false,
    };
}

/// Whether the native layout produced for a callback return of `return_ty` (whose
/// managed runtime value is at `runtime_ptr`) owns ALL of its data — so the
/// managed VM value can be dropped immediately after lowering: a Rust-style move
/// into native ownership, with no lingering alias and no per-call retention.
///
/// Returns false (the managed value MUST stay alive, because the native copy
/// borrows its bytes) whenever any payload / field / element is a `string`,
/// `raw_ptr`, or `construct_any`, or is a nested aggregate this conservative,
/// allocation-free check does not descend into. Flat-scalar aggregates
/// (an enum with a scalar/void payload, a struct of scalars, an array of scalars)
/// are deep-copied wholesale by the lowering, so they are self-contained.
pub fn nativeReturnIsSelfContained(
    self: *Vm,
    module: *const bytecode.Module,
    return_ty: bytecode.TypeRef,
    runtime_ptr: usize,
) bool {
    return switch (return_ty.kind) {
        .enum_instance => blk: {
            const type_name = return_ty.name orelse break :blk false;
            const slots = resolveManagedEnumSlots(self, runtime_ptr) orelse break :blk false;
            if (slots[0] != .integer) break :blk false;
            const payload_ty = enumPayloadType(self, module, type_name, @intCast(slots[0].integer)) orelse break :blk false;
            break :blk isFlatScalarKind(payload_ty.kind);
        },
        .array => blk: {
            const element_ty = self.arrayElementType(module, return_ty) catch break :blk false;
            break :blk isFlatScalarKind(element_ty.kind);
        },
        .ffi_struct => blk: {
            const type_name = return_ty.name orelse break :blk false;
            const type_decl = self.findTypeCached(module, type_name) orelse break :blk false;
            for (type_decl.fields) |field_decl| {
                if (!isFlatScalarKind(field_decl.ty.kind)) break :blk false;
            }
            break :blk true;
        },
        else => false,
    };
}

pub fn materializeNativeResult(
    self: *Vm,
    module: *const bytecode.Module,
    return_ty: bytecode.TypeRef,
    value: runtime_abi.Value,
) !runtime_abi.Value {
    return materializeNativeResultWithOwner(self, module, return_ty, value, .vm);
}

pub fn materializeNativeResultFromC(
    self: *Vm,
    module: *const bytecode.Module,
    return_ty: bytecode.TypeRef,
    value: runtime_abi.Value,
) !runtime_abi.Value {
    return materializeNativeResultWithOwner(self, module, return_ty, value, .c);
}

fn materializeNativeResultWithOwner(
    self: *Vm,
    module: *const bytecode.Module,
    return_ty: bytecode.TypeRef,
    value: runtime_abi.Value,
    owner: NativeLayoutOwner,
) !runtime_abi.Value {
    const leak_results = std.c.getenv("KIRA_LEAK_RESULTS") != null;
    return switch (return_ty.kind) {
        .ffi_struct => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) {
                self.rememberError("native struct result requires a valid pointer");
                return error.RuntimeFailure;
            }
            const type_name = return_ty.name orelse {
                self.rememberError("native struct result is missing a type name");
                return error.RuntimeFailure;
            };
            // Release the native result storage even if materialization fails.
            errdefer if (!leak_results) destroyStructNativeLayoutWithOwner(self, module, type_name, value.raw_ptr, owner);
            const copied = try copyStructFromNativeLayout(self, module, type_name, value.raw_ptr);
            if (!leak_results) destroyStructNativeLayoutWithOwner(self, module, type_name, value.raw_ptr, owner);
            break :blk .{ .raw_ptr = copied };
        },
        .array => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            // Release the native result storage even if materialization fails.
            errdefer destroyArrayNativeLayoutWithOwner(self, module, return_ty, value.raw_ptr, owner);
            const copied = try copyArrayFromNativeLayout(self, module, return_ty, value.raw_ptr);
            if (!leak_results) destroyArrayNativeLayoutWithOwner(self, module, return_ty, value.raw_ptr, owner);
            break :blk .{ .raw_ptr = copied };
        },
        .enum_instance => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            const type_name = return_ty.name orelse {
                self.rememberError("native enum result is missing a type name");
                return error.RuntimeFailure;
            };
            // Release the native result storage even if materialization fails.
            errdefer if (!leak_results) destroyEnumNativeLayoutWithOwner(self, module, type_name, value.raw_ptr, owner);
            const copied = try copyEnumFromNativeLayout(self, module, type_name, value.raw_ptr);
            if (!leak_results) destroyEnumNativeLayoutWithOwner(self, module, type_name, value.raw_ptr, owner);
            break :blk .{ .raw_ptr = copied };
        },
        .construct_any => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
            break :blk try construct_any.materializeFromNativeIfNeeded(self, module, return_ty, value.raw_ptr);
        },
        .raw_ptr => try materializeCallbackValueFromNative(self, module, return_ty, value),
        else => value,
    };
}

pub fn copyStructFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) anyerror!usize {
    const type_decl = self.findTypeCached(module, type_name) orelse {
        self.rememberError("struct type could not be resolved");
        return error.RuntimeFailure;
    };
    const fields = try self.allocator.alloc(runtime_abi.Value, type_decl.fields.len);
    for (fields) |*field| field.* = .{ .void = {} };
    var initialized: usize = 0;
    errdefer {
        for (fields[0..initialized]) |field| self.heap.dropValue(field);
        self.allocator.free(fields);
    }
    const dbg = std.c.getenv("KIRA_DBG") != null;
    if (dbg) std.debug.print("DBG copyStructFromNative type={s} fields={d} native=0x{x}\n", .{ type_name, type_decl.fields.len, native_ptr });
    for (type_decl.fields, 0..) |field_decl, index| {
        if (dbg) std.debug.print("DBG   readField type={s} field[{d}]={s} kind={s} name={s}\n", .{ type_name, index, field_decl.name, @tagName(field_decl.ty.kind), field_decl.ty.name orelse "?" });
        fields[index] = try helper_impl.readNativeFieldValue(self, module, type_name, field_decl, index, native_ptr);
        initialized += 1;
    }
    return self.heap.registerStructWithOrigin(type_name, fields, .native_materialize);
}

pub fn copyStructFromNativeLayoutInto(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) anyerror!void {
    const type_decl = self.findTypeCached(module, type_name) orelse {
        self.rememberError("struct type could not be resolved");
        return error.RuntimeFailure;
    };
    const fields: [*]align(1) runtime_abi.Value = @ptrFromInt(runtime_ptr);
    for (type_decl.fields, 0..) |field_decl, index| {
        const offset = try native_layout.fieldOffset(module, type_name, index);
        const address = native_ptr + offset;
        switch (field_decl.ty.kind) {
            .ffi_struct => {
                const nested_name = field_decl.ty.name orelse {
                    self.rememberError("nested struct field type is missing a name");
                    return error.RuntimeFailure;
                };
                if (fields[index] != .raw_ptr or fields[index].raw_ptr == 0) {
                    const old = fields[index];
                    fields[index] = .{ .raw_ptr = try self.allocateStruct(module, nested_name) };
                    self.heap.dropValue(old);
                }
                try copyStructFromNativeLayoutInto(self, module, nested_name, fields[index].raw_ptr, address);
            },
            .array => {
                const native_array_ptr = (@as(*const usize, @ptrFromInt(address))).*;
                if (native_array_ptr == 0) {
                    const old = fields[index];
                    fields[index] = .{ .raw_ptr = 0 };
                    self.heap.dropValue(old);
                    continue;
                }
                if (fields[index] == .raw_ptr and fields[index].raw_ptr != 0) {
                    try syncArrayFromNativeLayout(self, module, field_decl.ty, fields[index].raw_ptr, native_array_ptr);
                    continue;
                }
                const old = fields[index];
                fields[index] = .{ .raw_ptr = try copyArrayFromNativeLayout(self, module, field_decl.ty, native_array_ptr) };
                self.heap.dropValue(old);
            },
            else => {
                const old = fields[index];
                fields[index] = try helper_impl.readNativeFieldValue(self, module, type_name, field_decl, index, native_ptr);
                self.heap.dropValue(old);
            },
        }
    }
}

pub fn copyStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) anyerror!usize {
    const layout = try native_layout.structLayout(module, type_name);
    const word_count = @max(1, std.math.divCeil(usize, layout.size, @sizeOf(u64)) catch unreachable);
    const words = try self.allocator.alloc(u64, word_count);
    @memset(std.mem.sliceAsBytes(words), 0);
    recordNativeStructAlloc(self);
    errdefer destroyStructNativeLayout(self, module, type_name, @intFromPtr(words.ptr));
    try copyStructToNativeLayoutInto(self, module, type_name, runtime_ptr, @intFromPtr(words.ptr));
    return @intFromPtr(words.ptr);
}

pub fn copyStructToNativeLayoutInto(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) anyerror!void {
    const type_decl = self.findTypeCached(module, type_name) orelse {
        self.rememberError("struct type could not be resolved");
        return error.RuntimeFailure;
    };
    if (runtime_abi.isTaggedNativeClosurePointer(runtime_ptr) or !self.isManagedStructPointer(runtime_ptr)) {
        self.rememberFmt(
            "struct native copy was handed a non-struct (tagged/dangling) pointer: type={s} ptr=0x{x}",
            .{ type_name, runtime_ptr },
        );
        return error.RuntimeFailure;
    }
    const fields: [*]align(1) const runtime_abi.Value = @ptrFromInt(runtime_ptr);
    for (type_decl.fields, 0..) |field_decl, index| {
        const offset = try native_layout.fieldOffset(module, type_name, index);
        releaseNativeFieldBeforeOverwrite(self, module, field_decl.ty, native_ptr + offset);
        try helper_impl.writeNativeFieldValue(self, module, field_decl.ty, fields[index], native_ptr + offset);
    }
}

fn releaseNativeFieldBeforeOverwrite(self: *Vm, module: *const bytecode.Module, field_ty: bytecode.TypeRef, address: usize) void {
    switch (field_ty.kind) {
        .enum_instance => {
            const old_ptr = (@as(*const usize, @ptrFromInt(address))).*;
            if (old_ptr != 0) {
                destroyEnumNativeLayoutWithOwner(self, module, field_ty.name orelse return, old_ptr, .c);
                (@as(*usize, @ptrFromInt(address))).* = 0;
            }
        },
        .array => {
            const old_ptr = (@as(*const usize, @ptrFromInt(address))).*;
            if (old_ptr != 0) {
                destroyArrayNativeLayoutWithOwner(self, module, field_ty, old_ptr, .vm);
                (@as(*usize, @ptrFromInt(address))).* = 0;
            }
        },
        .ffi_struct => {
            destroyStructNativeLayoutFieldsWithOwner(self, module, field_ty.name orelse return, address, .c);
        },
        else => {},
    }
}

pub fn destroyStructNativeLayoutFields(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
    destroyStructNativeLayoutFieldsWithOwner(self, module, type_name, native_ptr, .vm);
}

fn destroyStructNativeLayoutFieldsWithOwner(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize, owner: NativeLayoutOwner) void {
    const type_decl = self.findTypeCached(module, type_name) orelse return;
    for (type_decl.fields, 0..) |field_decl, index| {
        const offset = native_layout.fieldOffset(module, type_name, index) catch continue;
        const address = native_ptr + offset;
        switch (field_decl.ty.kind) {
            .array => {
                const array_ptr = (@as(*const usize, @ptrFromInt(address))).*;
                destroyArrayNativeLayoutWithOwner(self, module, field_decl.ty, array_ptr, owner);
                // Clear the slot so a subsequent overwrite (releaseNativeFieldBeforeOverwrite ->
                // re-entry on the same address) does not free this now-stale pointer again.
                (@as(*usize, @ptrFromInt(address))).* = 0;
            },
            .ffi_struct => if (field_decl.ty.name) |nested_name| {
                destroyStructNativeLayoutFieldsWithOwner(self, module, nested_name, address, owner);
            },
            .enum_instance => {
                const enum_ptr = (@as(*const usize, @ptrFromInt(address))).*;
                destroyEnumNativeLayoutWithOwner(self, module, field_decl.ty.name orelse return, enum_ptr, owner);
                (@as(*usize, @ptrFromInt(address))).* = 0;
            },
            .construct_any => {},
            else => {},
        }
    }
}

pub fn recordNativeArrayAlloc(self: *Vm) void {
    self.native_layout_stats.arrays_current += 1;
    self.native_layout_stats.arrays_allocated += 1;
    self.native_layout_stats.arrays_peak = @max(self.native_layout_stats.arrays_peak, self.native_layout_stats.arrays_current);
}

pub fn recordNativeArrayFree(self: *Vm) void {
    if (self.native_layout_stats.arrays_current > 0) self.native_layout_stats.arrays_current -= 1;
    self.native_layout_stats.arrays_freed += 1;
}

pub fn recordNativeStructAlloc(self: *Vm) void {
    self.native_layout_stats.structs_current += 1;
    self.native_layout_stats.structs_allocated += 1;
    self.native_layout_stats.structs_peak = @max(self.native_layout_stats.structs_peak, self.native_layout_stats.structs_current);
}

pub fn recordNativeStructFree(self: *Vm) void {
    if (self.native_layout_stats.structs_current > 0) self.native_layout_stats.structs_current -= 1;
    self.native_layout_stats.structs_freed += 1;
}
