//! Native-state opcode handlers, extracted from the interpreter dispatch loop.
//!
//! `recover_native_state` / `native_state_field_get` / `native_state_field_set`
//! implement reads and writes against a recovered native-state token (either a
//! VM-managed `NativeStateBox` carrying a runtime or bridge payload, or a raw
//! pointer to a contiguous value array). They are a cohesive, comparatively cold
//! group, so they live here to keep `vm_interpreter.zig` focused on the hot
//! dispatch path (and under Core Law #5's size limit). Each returns to the
//! dispatch loop, which advances the program counter.

const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const vm_mod = @import("vm.zig");
const slot_impl = @import("vm_slot_utils.zig");

const Vm = vm_mod.Vm;
const NativeStateBox = vm_mod.NativeStateBox;
const setSlotUnmanaged = slot_impl.setSlotUnmanaged;
const setSlotBorrowed = slot_impl.setSlotBorrowed;
const setSlotOwned = slot_impl.setSlotOwned;

fn consumesRuntimeOwnership(field_ty: bytecode.TypeRef) bool {
    return switch (field_ty.kind) {
        .construct_any, .string => false,
        .raw_ptr => if (field_ty.name) |name| Vm.isCallbackTypeName(name) else false,
        .ffi_struct, .array, .enum_instance => true,
        else => false,
    };
}

pub fn recoverNativeState(
    vm: *Vm,
    module: *const bytecode.Module,
    registers: []runtime_abi.Value,
    register_owned: []bool,
    value: anytype,
) !void {
    const state_value = registers[value.state];
    if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
        vm.rememberError("nativeRecover requires a valid native state token");
        return error.RuntimeFailure;
    }
    setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try vm.recoverNativeState(module, value.type_name, state_value.raw_ptr, value.type_id) });
}

pub fn nativeStateFieldGet(
    vm: *Vm,
    module: *const bytecode.Module,
    registers: []runtime_abi.Value,
    register_owned: []bool,
    value: anytype,
) !void {
    const state_value = registers[value.state];
    if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
        vm.rememberError("native state field read requires a valid recovered state");
        return error.RuntimeFailure;
    }
    if (vm.native_state_boxes.contains(state_value.raw_ptr)) {
        const box: *const NativeStateBox = @ptrFromInt(state_value.raw_ptr);
        const payload_ptr = if (box.runtime_payload != 0) box.runtime_payload else box.payload;
        if (payload_ptr == 0) {
            vm.rememberError("native state field read requires a valid state payload");
            return error.RuntimeFailure;
        }
        if (box.runtime_payload != 0) {
            const payload: [*]const runtime_abi.Value = @ptrFromInt(payload_ptr);
            setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], payload[@intCast(value.field_index)]);
        } else {
            const payload: [*]const runtime_abi.BridgeValue = @ptrFromInt(payload_ptr);
            const materialized = try vm.materializeNativeStateValue(module, value.field_ty, runtime_abi.bridgeValueToValue(payload[@intCast(value.field_index)]));
            setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], materialized);
        }
    } else {
        const payload: [*]const runtime_abi.Value = @ptrFromInt(state_value.raw_ptr);
        setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], payload[@intCast(value.field_index)]);
    }
}

pub fn nativeStateFieldSet(
    vm: *Vm,
    module: *const bytecode.Module,
    registers: []runtime_abi.Value,
    register_owned: []bool,
    value: anytype,
) !void {
    const state_value = registers[value.state];
    if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
        vm.rememberError("native state field write requires a valid recovered state");
        return error.RuntimeFailure;
    }
    const field_index: usize = @intCast(value.field_index);
    if (vm.native_state_boxes.contains(state_value.raw_ptr)) {
        const box: *NativeStateBox = @ptrFromInt(state_value.raw_ptr);
        if (box.runtime_payload != 0) {
            const payload: [*]runtime_abi.Value = @ptrFromInt(box.runtime_payload);
            const old = payload[field_index];
            const stored = if (register_owned[value.src])
                registers[value.src]
            else
                try vm.cloneBorrowedValueForStore(module, value.field_ty, registers[value.src]);
            payload[field_index] = stored;
            if (register_owned[value.src]) {
                register_owned[value.src] = false;
                registers[value.src] = .{ .void = {} };
            }
            vm.heap.dropValue(old);
        } else {
            if (box.payload == 0) {
                vm.rememberError("native state field write requires a valid state payload");
                return error.RuntimeFailure;
            }
            const payload: [*]runtime_abi.BridgeValue = @ptrFromInt(box.payload);
            const old = runtime_abi.bridgeValueToValue(payload[field_index]);
            const source = registers[value.src];
            const stored = try vm.preserveNativeStateValue(module, value.field_ty, source);
            payload[field_index] = runtime_abi.bridgeValueFromValue(stored);
            if (register_owned[value.src]) {
                if (consumesRuntimeOwnership(value.field_ty)) {
                    vm.heap.dropValue(source);
                }
                register_owned[value.src] = false;
                registers[value.src] = .{ .void = {} };
            }
            vm.destroyPreservedNativeStateValue(module, value.field_ty, old);
        }
    } else {
        const payload: [*]runtime_abi.Value = @ptrFromInt(state_value.raw_ptr);
        const old = payload[field_index];
        const stored = if (register_owned[value.src])
            registers[value.src]
        else
            try vm.cloneBorrowedValueForStore(module, value.field_ty, registers[value.src]);
        payload[field_index] = stored;
        if (register_owned[value.src]) {
            register_owned[value.src] = false;
            registers[value.src] = .{ .void = {} };
        }
        vm.heap.dropValue(old);
    }
}
