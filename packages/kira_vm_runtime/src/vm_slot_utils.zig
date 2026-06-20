const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");

pub fn setSlotOwned(vm: anytype, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = vm.heap.isManagedValue(value);
    if (old_owned) vm.heap.dropValue(old);
}

pub fn setSlotPrimitive(vm: anytype, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = false;
    if (old_owned) vm.heap.dropValue(old);
}

pub fn setSlotManaged(vm: anytype, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = true;
    if (old_owned) vm.heap.dropValue(old);
}

pub fn setSlotBorrowed(vm: anytype, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = false;
    if (old_owned) vm.heap.dropValue(old);
}

pub fn setSlotUnmanaged(vm: anytype, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = false;
    if (old_owned) vm.heap.dropValue(old);
}

pub fn transferSlot(
    vm: anytype,
    dst: *runtime_abi.Value,
    dst_owned: *bool,
    src: *runtime_abi.Value,
    src_owned: *bool,
) void {
    const old = dst.*;
    const old_owned = dst_owned.*;
    dst.* = src.*;
    dst_owned.* = src_owned.*;
    if (src_owned.*) {
        src.* = .{ .void = {} };
        src_owned.* = false;
    }
    if (old_owned) vm.heap.dropValue(old);
}

pub fn fillTransferredArgs(
    values: []runtime_abi.Value,
    registers: []runtime_abi.Value,
    register_owned: []bool,
    argument_registers: []const u32,
    param_ownership: []const bytecode.OwnershipMode,
    param_types: []const bytecode.TypeRef,
    copy_struct_args_by_value: bool,
) void {
    for (argument_registers, 0..) |register_index, index| {
        values[index] = registers[register_index];
        // In copy-by-value (VM) mode a struct argument is passed as an
        // independent deep copy made by the callee (see bindArguments), and the
        // caller keeps ownership of the original — it is freed at the caller's
        // frame exit. Voiding the caller's register here would orphan that
        // original (a leak), and the callee cannot free it either because the
        // caller may have passed a borrow. So leave struct-arg registers alone
        // in copy-by-value mode regardless of the declared ownership mode.
        if (copy_struct_args_by_value and index < param_types.len and param_types[index].kind == .ffi_struct) continue;
        switch (ownershipModeAt(param_ownership, index)) {
            .owned, .move => {
                if (register_owned[register_index]) {
                    register_owned[register_index] = false;
                    registers[register_index] = .{ .void = {} };
                }
            },
            .borrow_read, .borrow_mut, .copy => {},
        }
    }
}

pub fn ownershipModeAt(values: []const bytecode.OwnershipMode, index: usize) bytecode.OwnershipMode {
    if (index < values.len) return values[index];
    return .owned;
}

pub fn releaseTrackedSlots(vm: anytype, slots: []runtime_abi.Value, owned: []const bool) void {
    for (slots, owned) |slot, is_owned| if (is_owned) vm.heap.dropValue(slot);
}
