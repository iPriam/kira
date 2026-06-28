//! Frame prologue and value-marshaling helpers for the VM dispatch loop.
//!
//! These cover the work `runPrepared` does *around* the threaded dispatch
//! switch — initializing pre-allocated struct-local storage, binding incoming
//! arguments into the frame with the correct ownership, and materializing array
//! elements that cross the native boundary. They are factored out of
//! `vm_interpreter.zig` so that file stays focused on the dispatch loop itself.
//!
//! Ownership semantics here are byte-for-byte identical to the inline code they
//! replaced.

const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const slot_impl = @import("vm_slot_utils.zig");
const vm_prepare = @import("vm_prepare.zig");
const vm_mod = @import("vm.zig");

const Vm = vm_mod.Vm;
const Hooks = vm_mod.Hooks;
const PreparedFunction = vm_prepare.PreparedFunction;
const ownershipModeAt = slot_impl.ownershipModeAt;
const setSlotBorrowed = slot_impl.setSlotBorrowed;
const setSlotOwned = slot_impl.setSlotOwned;

pub const PreparedArrayElement = struct {
    value: runtime_abi.Value,
    owned: bool,
};

/// Recover an array element into a runtime value, copying or materializing it
/// out of native layout when the element type requires it (struct/array/enum/
/// construct-any/native-closure). Primitive and already-managed elements are
/// returned as a borrow.
pub fn prepareArrayElement(
    vm: *Vm,
    module: *const bytecode.Module,
    element_ty: bytecode.TypeRef,
    item_value: runtime_abi.Value,
) !PreparedArrayElement {
    if (element_ty.kind == .ffi_struct and item_value == .raw_ptr and item_value.raw_ptr != 0) {
        const type_name = element_ty.name orelse {
            vm.rememberError("array element struct type is missing a name");
            return error.RuntimeFailure;
        };
        const copied = if (vm.isManagedStructPointer(item_value.raw_ptr))
            try vm.cloneStructValue(module, type_name, item_value.raw_ptr)
        else
            try vm.copyStructFromNativeLayout(module, type_name, item_value.raw_ptr);
        return .{
            .value = .{ .raw_ptr = copied },
            .owned = true,
        };
    }

    if (item_value != .raw_ptr or item_value.raw_ptr == 0) {
        return .{ .value = item_value, .owned = false };
    }

    const needs_materialize = switch (element_ty.kind) {
        .array => vm.heap.getArray(item_value.raw_ptr) == null,
        .enum_instance => !vm.heap.isManagedValue(item_value),
        .construct_any => !vm.isManagedStructPointer(item_value.raw_ptr),
        .raw_ptr => blk: {
            const name = element_ty.name orelse break :blk false;
            break :blk Vm.isCallbackTypeName(name) and vm.heap.getClosure(item_value.raw_ptr) == null and runtime_abi.isTaggedNativeClosurePointer(item_value.raw_ptr);
        },
        else => false,
    };
    if (!needs_materialize) return .{ .value = item_value, .owned = false };

    const materialized = try vm.materializeNativeStateValue(module, element_ty, item_value);
    return .{
        .value = materialized,
        .owned = vm.heap.isManagedValue(materialized) and !vm.heap.isManagedValue(item_value),
    };
}

/// Pre-allocate backing storage for struct locals that need it, owning the slot
/// so the struct is freed at frame exit. Mirrors the decode pass's
/// `struct_locals` collection.
pub fn initStructLocals(
    vm: *Vm,
    function: *const PreparedFunction,
    module: *const bytecode.Module,
    hooks: Hooks,
    locals: []runtime_abi.Value,
    local_owned: []bool,
) !void {
    for (function.struct_locals) |struct_local| {
        // A borrow/`borrow mut` struct parameter ALIASES the caller's struct
        // (so mutations propagate); the decode pass already excluded those. In
        // hybrid mode (copy_struct_args_by_value=false) no struct param gets a
        // private copy destination either.
        if (struct_local.param_only_when_copied and !hooks.copy_struct_args_by_value) continue;
        const type_name = struct_local.type_name orelse {
            vm.rememberError("struct local type is missing a name");
            return error.RuntimeFailure;
        };
        const struct_ptr = if (struct_local.type_index != vm_prepare.no_type_index)
            try vm.allocateStructByDecl(module, module.types[struct_local.type_index])
        else
            try vm.allocateStruct(module, type_name);
        setSlotOwned(vm, &locals[struct_local.local], &local_owned[struct_local.local], .{ .raw_ptr = struct_ptr });
    }
}

/// Bind incoming call arguments into the frame's local slots, honoring each
/// parameter's ownership mode and the hook-controlled struct-copy policy.
pub fn bindArguments(
    vm: *Vm,
    decl: *const bytecode.Function,
    module: *const bytecode.Module,
    hooks: Hooks,
    args: []const runtime_abi.Value,
    locals: []runtime_abi.Value,
    local_owned: []bool,
) !void {
    if (args.len != decl.param_count) {
        vm.rememberError("bytecode function call used the wrong number of arguments");
        return error.RuntimeFailure;
    }
    for (args, 0..) |arg, index| {
        if (decl.local_types[index].kind == .ffi_struct) {
            if (arg != .raw_ptr or arg.raw_ptr == 0) {
                vm.rememberFmt(
                    "struct argument requires a valid pointer (function={s}, arg={d}, tag={s})",
                    .{ decl.name, index, @tagName(arg) },
                );
                return error.RuntimeFailure;
            }
            const struct_mode = ownershipModeAt(decl.param_ownership, index);
            if (struct_mode == .borrow_read or struct_mode == .borrow_mut) {
                // Alias the caller's struct: a `borrow mut` callee mutates it in place and
                // the caller observes the change (matches the LLVM/native backend). The slot
                // is non-owning, so it is not freed at frame exit — the caller still owns it.
                setSlotBorrowed(vm, &locals[index], &local_owned[index], arg);
            } else if (hooks.copy_struct_args_by_value) {
                const type_name = decl.local_types[index].name orelse {
                    vm.rememberError("struct local type is missing a name");
                    return error.RuntimeFailure;
                };
                const type_decl = vm.findTypeCached(module, type_name) orelse {
                    vm.rememberError("struct type could not be resolved");
                    return error.RuntimeFailure;
                };
                const dst_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(locals[index].raw_ptr);
                const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(arg.raw_ptr);
                try vm.copyStruct(module, type_decl, dst_ptr, src_ptr);
            } else if ((struct_mode == .owned or struct_mode == .move) and vm.isManagedStructPointer(arg.raw_ptr)) {
                // Hybrid (copy_struct_args_by_value=false): an owned/move struct param is
                // ownership-transferred by the caller (owned struct args require `move` at the
                // call site, so the caller will NOT drop it). When the incoming value is a
                // managed VM struct, the callee is now the sole owner and must drop it at frame
                // exit — otherwise it leaks (the struct shell and any owned array/struct fields).
                // A native-layout struct (e.g. a sokol GraphicsFrame handed to a VM callback) is
                // not a managed pointer, so it stays borrowed below: native still owns it.
                setSlotOwned(vm, &locals[index], &local_owned[index], arg);
            } else setSlotBorrowed(vm, &locals[index], &local_owned[index], arg);
        } else {
            switch (ownershipModeAt(decl.param_ownership, index)) {
                .owned, .move => setSlotOwned(vm, &locals[index], &local_owned[index], arg),
                .borrow_read, .borrow_mut, .copy => setSlotBorrowed(vm, &locals[index], &local_owned[index], arg),
            }
        }
    }
}
