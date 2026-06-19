//! The VM dispatch loop, operating on `vm_prepare.PreparedModule`.
//!
//! Structure: a labeled-switch ("threaded") dispatch where every arm jumps
//! directly to the next instruction's arm via `continue :dispatch`. Compared to
//! the old `while (pc) { switch }` loop this gives each opcode its own indirect
//! branch (much better branch prediction) and removes the per-step pc bounds
//! check — the decode pass appends an implicit `ret`, so execution cannot fall
//! off the end of `code`.
//!
//! All ownership semantics (owned-slot tracking, transfers, borrows, drops)
//! are byte-for-byte the same as the previous interpreter; only the lookup
//! and dispatch machinery changed.

const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const builtins = @import("builtins.zig");
const ownership = @import("ownership.zig");
const helper_impl = @import("vm_helpers.zig");
const value_impl = @import("vm_values.zig");
const vm_prepare = @import("vm_prepare.zig");
const vm_mod = @import("vm.zig");

const Vm = vm_mod.Vm;
const Hooks = vm_mod.Hooks;
const ArrayObject = ownership.ArrayObject;
const PreparedModule = vm_prepare.PreparedModule;
const PreparedFunction = vm_prepare.PreparedFunction;

pub fn runPrepared(
    vm: *Vm,
    prepared: *const PreparedModule,
    function: *const PreparedFunction,
    args: []const runtime_abi.Value,
    writer: anytype,
    hooks: Hooks,
) anyerror!runtime_abi.Value {
    const decl = function.decl;
    const module = prepared.module;
    const code = function.code;
    const register_count: usize = decl.register_count;
    const local_count: usize = decl.local_count;

    const frame = try vm.acquireFrame(function.frame_size);
    const registers = frame.values[0..register_count];
    const register_owned = frame.owned[0..register_count];
    const locals = frame.values[register_count..][0..local_count];
    const local_owned = frame.owned[register_count..][0..local_count];
    defer {
        releaseTrackedSlots(vm, locals, local_owned);
        releaseTrackedSlots(vm, registers, register_owned);
        vm.releaseFrame(frame);
    }

    @memset(registers, .{ .void = {} });
    @memset(locals, .{ .void = {} });
    @memset(register_owned, false);
    @memset(local_owned, false);

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
            } else setSlotBorrowed(vm, &locals[index], &local_owned[index], arg);
        } else {
            switch (ownershipModeAt(decl.param_ownership, index)) {
                .owned, .move => setSlotOwned(vm, &locals[index], &local_owned[index], arg),
                .borrow_read, .borrow_mut, .copy => setSlotBorrowed(vm, &locals[index], &local_owned[index], arg),
            }
        }
    }

    var pc: usize = 0;
    dispatch: switch (code[pc]) {
        .const_int => |value| {
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = value.value });
            pc += 1;
            continue :dispatch code[pc];
        },
        .const_float => |value| {
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .float = value.value });
            pc += 1;
            continue :dispatch code[pc];
        },
        .const_string => |value| {
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .string = value.value });
            pc += 1;
            continue :dispatch code[pc];
        },
        .const_bool => |value| {
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .boolean = value.value });
            pc += 1;
            continue :dispatch code[pc];
        },
        .const_null_ptr => |value| {
            setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = 0 });
            pc += 1;
            continue :dispatch code[pc];
        },
        .const_function => |value| {
            const raw_ptr = switch (value.representation) {
                .callable_value => value.function_id,
                .native_callback => if (hooks.resolve_function) |resolve_function|
                    try helper_impl.resolveFunctionPointer(hooks, resolve_function, value.function_id)
                else
                    value.function_id,
            };
            runtime_abi.emitExecutionTrace("CALLABLE", "CONST_FUNCTION", "dst={d} fn={d} raw=0x{x} repr={s}", .{ value.dst, value.function_id, raw_ptr, @tagName(value.representation) });
            setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = raw_ptr });
            pc += 1;
            continue :dispatch code[pc];
        },
        .const_closure => |value| {
            const closure_ptr = try vm.allocateClosure(registers, value.function_id, value.captures, value.capture_ownership);
            runtime_abi.emitExecutionTrace("CALLABLE", "CONST_CLOSURE", "dst={d} fn={d} raw=0x{x} captures={d}", .{ value.dst, value.function_id, closure_ptr, value.captures.len });
            setSlotManaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = closure_ptr });
            pc += 1;
            continue :dispatch code[pc];
        },
        .alloc_struct => |value| {
            const type_index = function.alloc_type_index[pc];
            const struct_ptr = if (type_index != vm_prepare.no_type_index)
                try vm.allocateStructByDecl(module, module.types[type_index])
            else
                try vm.allocateStruct(module, value.type_name);
            setSlotManaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = struct_ptr });
            pc += 1;
            continue :dispatch code[pc];
        },
        .alloc_enum => |value| {
            setSlotManaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try vm.allocateEnum(value.enum_type_name, registers, value.discriminant, value.payload_src) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .alloc_native_state => |value| {
            const src_value = registers[value.src];
            if (src_value != .raw_ptr or src_value.raw_ptr == 0) {
                vm.rememberError("nativeState requires a valid Kira struct value");
                return error.RuntimeFailure;
            }
            setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try vm.allocateNativeState(module, value.type_name, value.type_id, src_value.raw_ptr) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .alloc_array => |value| {
            const len_value = registers[value.len];
            if (len_value != .integer or len_value.integer < 0) {
                vm.rememberError("array allocation requires a non-negative integer length");
                return error.RuntimeFailure;
            }
            setSlotManaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try vm.allocateArray(@intCast(len_value.integer)) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .add => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            // Fast path: integer + integer (the overwhelmingly common case) without
            // the helper call or the ownership hash lookup.
            if (lhs == .integer and rhs == .integer) {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = lhs.integer +% rhs.integer });
            } else {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], try value_impl.addValues(vm, lhs, rhs));
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .subtract => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            if (lhs == .integer and rhs == .integer) {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = lhs.integer -% rhs.integer });
            } else {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], try value_impl.subtractValues(vm, lhs, rhs));
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .multiply => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            if (lhs == .integer and rhs == .integer) {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = lhs.integer *% rhs.integer });
            } else {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], try value_impl.multiplyValues(vm, lhs, rhs));
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .divide => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], try value_impl.divideValues(vm, lhs, rhs));
            pc += 1;
            continue :dispatch code[pc];
        },
        .modulo => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], try value_impl.moduloValues(vm, lhs, rhs));
            pc += 1;
            continue :dispatch code[pc];
        },
        .compare => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            if (lhs == .integer and rhs == .integer) {
                const lhs_int = lhs.integer;
                const rhs_int = rhs.integer;
                const result = switch (value.op) {
                    .equal => lhs_int == rhs_int,
                    .not_equal => lhs_int != rhs_int,
                    .less => lhs_int < rhs_int,
                    .less_equal => lhs_int <= rhs_int,
                    .greater => lhs_int > rhs_int,
                    .greater_equal => lhs_int >= rhs_int,
                };
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .boolean = result });
            } else {
                setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], .{ .boolean = try value_impl.compareValues(vm, lhs, rhs, value.op) });
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .unary => |value| {
            setSlotPrimitive(vm, &registers[value.dst], &register_owned[value.dst], try value_impl.unaryValue(vm, registers[value.src], value.op));
            pc += 1;
            continue :dispatch code[pc];
        },
        .store_local => |value| {
            if (value.borrow) {
                // Reborrow (`var r = t` over a borrow): alias the source pointer as
                // a non-owning slot. Never clone — both bindings reference the same
                // storage and frame-exit cleanup must not free it (the borrow's owner
                // does). Mirrors the borrow-mut parameter binding path.
                setSlotBorrowed(vm, &locals[value.local], &local_owned[value.local], registers[value.src]);
            } else if (register_owned[value.src]) {
                transferSlot(
                    vm,
                    &locals[value.local],
                    &local_owned[value.local],
                    &registers[value.src],
                    &register_owned[value.src],
                );
            } else {
                const local_type = if (value.local < decl.local_types.len)
                    decl.local_types[value.local]
                else
                    bytecode.TypeRef{ .kind = .raw_ptr };
                if (local_type.kind == .ffi_struct) {
                    const stored = try vm.cloneBorrowedLocalValue(module, local_type, registers[value.src]);
                    setSlotOwned(vm, &locals[value.local], &local_owned[value.local], stored);
                } else {
                    setSlotBorrowed(vm, &locals[value.local], &local_owned[value.local], registers[value.src]);
                }
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .load_local => |value| {
            switch (value.ownership) {
                .move, .owned => {
                    transferSlot(
                        vm,
                        &registers[value.dst],
                        &register_owned[value.dst],
                        &locals[value.local],
                        &local_owned[value.local],
                    );
                    if (value.local < decl.local_types.len) {
                        const local_ty = decl.local_types[value.local];
                        if (local_ty.kind == .ffi_struct and locals[value.local] == .void) {
                            const type_name = local_ty.name orelse {
                                vm.rememberError("moved struct local requires a named type");
                                return error.RuntimeFailure;
                            };
                            setSlotOwned(
                                vm,
                                &locals[value.local],
                                &local_owned[value.local],
                                .{ .raw_ptr = try vm.allocateStruct(module, type_name) },
                            );
                        }
                    }
                },
                .borrow_read, .borrow_mut, .copy => setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], locals[value.local]),
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .local_ptr => |value| {
            setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = @intFromPtr(&locals[value.local]) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .subobject_ptr => |value| {
            const base = registers[value.base];
            if (base != .raw_ptr or base.raw_ptr == 0) {
                vm.rememberError("subobject access requires a valid struct pointer");
                return error.RuntimeFailure;
            }
            const base_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(base.raw_ptr);
            setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = @intFromPtr(base_ptr + value.offset) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .field_ptr => |value| {
            const base = registers[value.base];
            if (base != .raw_ptr or base.raw_ptr == 0) {
                vm.rememberFmt(
                    "field access requires a valid struct pointer: {s}.{d}",
                    .{ value.base_type_name, value.field_index },
                );
                return error.RuntimeFailure;
            }
            const base_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(base.raw_ptr);
            const field_index: usize = @intCast(value.field_index);
            const slot_ptr = base_ptr + field_index;
            if (value.field_ty.kind == .ffi_struct) {
                if (slot_ptr[0] != .raw_ptr or slot_ptr[0].raw_ptr == 0) {
                    vm.rememberError("nested struct field storage is invalid");
                    return error.RuntimeFailure;
                }
                setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = slot_ptr[0].raw_ptr });
            } else {
                setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = @intFromPtr(slot_ptr) });
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .recover_native_state => |value| {
            const state_value = registers[value.state];
            if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
                vm.rememberError("nativeRecover requires a valid native state token");
                return error.RuntimeFailure;
            }
            setSlotUnmanaged(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try vm.recoverNativeState(module, value.type_name, state_value.raw_ptr, value.type_id) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .native_state_field_get => |value| {
            const state_value = registers[value.state];
            if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
                vm.rememberError("native state field read requires a valid recovered state");
                return error.RuntimeFailure;
            }
            const payload: [*]const runtime_abi.BridgeValue = @ptrFromInt(state_value.raw_ptr);
            setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], runtime_abi.bridgeValueToValue(payload[@intCast(value.field_index)]));
            pc += 1;
            continue :dispatch code[pc];
        },
        .native_state_field_set => |value| {
            const state_value = registers[value.state];
            if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
                vm.rememberError("native state field write requires a valid recovered state");
                return error.RuntimeFailure;
            }
            const payload: [*]runtime_abi.BridgeValue = @ptrFromInt(state_value.raw_ptr);
            const field_index: usize = @intCast(value.field_index);
            const old = runtime_abi.bridgeValueToValue(payload[field_index]);
            const stored = if (register_owned[value.src])
                registers[value.src]
            else
                try vm.cloneBorrowedValueForStore(module, value.field_ty, registers[value.src]);
            payload[field_index] = runtime_abi.bridgeValueFromValue(stored);
            if (register_owned[value.src]) {
                register_owned[value.src] = false;
                registers[value.src] = .{ .void = {} };
            }
            vm.heap.dropValue(old);
            pc += 1;
            continue :dispatch code[pc];
        },
        .c_string_to_string => |value| {
            setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], try vm.copyCString(registers[value.src]));
            pc += 1;
            continue :dispatch code[pc];
        },
        .array_len => |value| {
            const array_value = registers[value.array];
            if (array_value != .raw_ptr or array_value.raw_ptr == 0) {
                vm.rememberError("array length requires a valid array handle");
                return error.RuntimeFailure;
            }
            const array_ptr: *const ArrayObject = @ptrFromInt(array_value.raw_ptr);
            setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = @intCast(array_ptr.len) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .string_len => |value| {
            const string_value = registers[value.string];
            if (string_value != .string) {
                vm.rememberError("string length requires a valid string value");
                return error.RuntimeFailure;
            }
            setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = @intCast(string_value.string.len) });
            pc += 1;
            continue :dispatch code[pc];
        },
        .array_get => |value| {
            const array_value = registers[value.array];
            const index_value = registers[value.index];
            if (array_value != .raw_ptr or array_value.raw_ptr == 0 or index_value != .integer or index_value.integer < 0) {
                vm.rememberError("array load requires a valid array handle and index");
                return error.RuntimeFailure;
            }
            const array_ptr: *const ArrayObject = @ptrFromInt(array_value.raw_ptr);
            const index: usize = @intCast(index_value.integer);
            if (index >= array_ptr.len) {
                vm.rememberError("array index is out of bounds");
                return error.RuntimeFailure;
            }
            const item_value = runtime_abi.bridgeValueToValue(array_ptr.items[index]);
            if (value.ty.kind == .ffi_struct and item_value == .raw_ptr and item_value.raw_ptr != 0) {
                const type_name = value.ty.name orelse {
                    vm.rememberError("array element struct type is missing a name");
                    return error.RuntimeFailure;
                };
                const copied = if (vm.isManagedStructPointer(item_value.raw_ptr))
                    try vm.cloneStructValue(module, type_name, item_value.raw_ptr)
                else
                    try vm.copyStructFromNativeLayout(module, type_name, item_value.raw_ptr);
                setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = copied });
            } else {
                setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], item_value);
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .array_set => |value| {
            const array_value = registers[value.array];
            const index_value = registers[value.index];
            if (array_value != .raw_ptr or array_value.raw_ptr == 0 or index_value != .integer or index_value.integer < 0) {
                vm.rememberError("array store requires a valid array handle and index");
                return error.RuntimeFailure;
            }
            const array_ptr: *ArrayObject = @ptrFromInt(array_value.raw_ptr);
            const index: usize = @intCast(index_value.integer);
            if (index >= array_ptr.len) {
                vm.rememberError("array index is out of bounds");
                return error.RuntimeFailure;
            }
            const stored = if (register_owned[value.src])
                registers[value.src]
            else
                try vm.cloneBorrowedManagedValueDynamic(module, registers[value.src]);
            vm.heap.replaceArrayItem(&array_ptr.items[index], stored);
            if (register_owned[value.src]) {
                register_owned[value.src] = false;
                registers[value.src] = .{ .void = {} };
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .array_append => |value| {
            const array_value = registers[value.array];
            if (array_value != .raw_ptr or array_value.raw_ptr == 0) {
                vm.rememberError("array append requires a valid array handle");
                return error.RuntimeFailure;
            }
            const array_ptr: *ArrayObject = @ptrFromInt(array_value.raw_ptr);
            const stored = if (register_owned[value.src])
                registers[value.src]
            else
                try vm.cloneBorrowedManagedValueDynamic(module, registers[value.src]);
            try vm.heap.appendArrayItem(array_ptr, stored);
            if (register_owned[value.src]) {
                register_owned[value.src] = false;
                registers[value.src] = .{ .void = {} };
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .enum_tag => |value| {
            const enum_value = registers[value.src];
            if (enum_value != .raw_ptr or enum_value.raw_ptr == 0) {
                vm.rememberError("enum tag access requires a valid enum value");
                return error.RuntimeFailure;
            }
            if (!vm.isManagedStructPointer(enum_value.raw_ptr)) {
                const native_words: [*]const u64 = @ptrFromInt(enum_value.raw_ptr);
                if (native_words[0] > std.math.maxInt(i64)) {
                    vm.rememberFmt(
                        "native enum tag is out of range: ptr=0x{x} tag={d}",
                        .{ enum_value.raw_ptr, native_words[0] },
                    );
                    return error.RuntimeFailure;
                }
                setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], .{ .integer = @intCast(native_words[0]) });
                pc += 1;
                continue :dispatch code[pc];
            }
            const enum_ptr: [*]align(1) const runtime_abi.Value = @ptrFromInt(enum_value.raw_ptr);
            if (enum_ptr[0] != .integer) {
                vm.rememberError("enum tag slot is not an integer");
                return error.RuntimeFailure;
            }
            setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], enum_ptr[0]);
            pc += 1;
            continue :dispatch code[pc];
        },
        .enum_payload => |value| {
            const enum_value = registers[value.src];
            if (enum_value != .raw_ptr or enum_value.raw_ptr == 0) {
                vm.rememberError("enum payload access requires a valid enum value");
                return error.RuntimeFailure;
            }
            if (!vm.isManagedStructPointer(enum_value.raw_ptr)) {
                const native_words: [*]const u64 = @ptrFromInt(enum_value.raw_ptr);
                const payload = try vm.enumPayloadFromNativeWord(module, value.payload_ty, native_words[1]);
                setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], payload);
                pc += 1;
                continue :dispatch code[pc];
            }
            const enum_ptr: [*]align(1) const runtime_abi.Value = @ptrFromInt(enum_value.raw_ptr);
            setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], enum_ptr[1]);
            pc += 1;
            continue :dispatch code[pc];
        },
        .load_indirect => |value| {
            const ptr = registers[value.ptr];
            if (ptr != .raw_ptr or ptr.raw_ptr == 0) {
                vm.rememberError("indirect load requires a valid pointer");
                return error.RuntimeFailure;
            }
            if (value.ty.kind == .ffi_struct) {
                const type_name = value.ty.name orelse {
                    vm.rememberError("struct load type is missing a name");
                    return error.RuntimeFailure;
                };
                const src_ptr = try vm.resolveStructValuePointer(type_name, ptr.raw_ptr);
                setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = if (src_ptr == 0) 0 else try vm.cloneStructValue(module, type_name, src_ptr) });
            } else if (value.ty.kind == .enum_instance) {
                const slot_ptr: *runtime_abi.Value = @ptrFromInt(ptr.raw_ptr);
                const enum_name = value.ty.name orelse {
                    vm.rememberError("enum load type is missing a name");
                    return error.RuntimeFailure;
                };
                setSlotOwned(vm, &registers[value.dst], &register_owned[value.dst], try vm.cloneEnumValue(module, enum_name, slot_ptr.*));
            } else {
                const slot_ptr: *runtime_abi.Value = @ptrFromInt(ptr.raw_ptr);
                setSlotBorrowed(vm, &registers[value.dst], &register_owned[value.dst], slot_ptr.*);
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .store_indirect => |value| {
            const ptr = registers[value.ptr];
            if (ptr != .raw_ptr or ptr.raw_ptr == 0) {
                vm.rememberError("indirect store requires a valid pointer");
                return error.RuntimeFailure;
            }
            if (value.ty.kind == .ffi_struct) {
                const type_name = value.ty.name orelse {
                    vm.rememberError("struct store type is missing a name");
                    return error.RuntimeFailure;
                };
                const dst_ptr = try vm.ensureStructDestinationPointer(module, type_name, ptr.raw_ptr);
                try vm.copyStructValueInto(module, type_name, dst_ptr, registers[value.src]);
            } else {
                const slot_ptr: *runtime_abi.Value = @ptrFromInt(ptr.raw_ptr);
                const stored = if (register_owned[value.src])
                    registers[value.src]
                else
                    try vm.cloneBorrowedValueForStore(module, value.ty, registers[value.src]);
                vm.heap.assignTransferred(slot_ptr, stored);
                if (register_owned[value.src]) {
                    register_owned[value.src] = false;
                    registers[value.src] = .{ .void = {} };
                }
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .copy_indirect => |value| {
            const dst_ptr_value = registers[value.dst_ptr];
            const src_ptr_value = registers[value.src_ptr];
            if (dst_ptr_value != .raw_ptr or src_ptr_value != .raw_ptr or dst_ptr_value.raw_ptr == 0 or src_ptr_value.raw_ptr == 0) {
                vm.rememberFmt(
                    "struct copy requires valid pointers: {s} dst_ok={d} src_ok={d}",
                    .{
                        value.type_name,
                        if (dst_ptr_value == .raw_ptr and dst_ptr_value.raw_ptr != 0) @as(u8, 1) else @as(u8, 0),
                        if (src_ptr_value == .raw_ptr and src_ptr_value.raw_ptr != 0) @as(u8, 1) else @as(u8, 0),
                    },
                );
                return error.RuntimeFailure;
            }
            const dst_ptr = try vm.ensureStructDestinationPointer(module, value.type_name, dst_ptr_value.raw_ptr);
            const src_ptr = try vm.resolveStructValuePointer(value.type_name, src_ptr_value.raw_ptr);
            try vm.copyStructValueInto(module, value.type_name, dst_ptr, .{ .raw_ptr = src_ptr });
            pc += 1;
            continue :dispatch code[pc];
        },
        .branch => |value| {
            const condition = registers[value.condition];
            if (condition != .boolean) {
                vm.rememberError("vm branch expects a boolean condition");
                return error.RuntimeFailure;
            }
            // Targets were resolved to direct pc offsets by the decode pass.
            pc = if (condition.boolean) value.true_label else value.false_label;
            continue :dispatch code[pc];
        },
        .jump => |value| {
            pc = value.label;
            continue :dispatch code[pc];
        },
        .label => {
            pc += 1;
            continue :dispatch code[pc];
        },
        .print => |value| {
            try builtins.printValue(writer, module, registers[value.src], value.ty);
            pc += 1;
            continue :dispatch code[pc];
        },
        .call_runtime => |value| {
            // The decode pass rewrote the function id into an index into
            // prepared.functions; sentinel indices keep the original runtime
            // failure semantics for missing functions and unresolved labels.
            const callee_index = value.function_id;
            if (callee_index >= prepared.functions.len) {
                if (callee_index == vm_prepare.trap_label_index) {
                    vm.rememberError("vm branch targets an unknown label");
                } else {
                    vm.rememberError("bytecode function id is out of range");
                }
                return error.RuntimeFailure;
            }
            const callee = &prepared.functions[callee_index];
            // Avoid a per-call heap allocation for the common small-arity case by
            // packing transferred args into a stack buffer; only spill to the heap
            // for unusually large argument lists.
            var arg_stack: [16]runtime_abi.Value = undefined;
            const spill = value.args.len > arg_stack.len;
            const call_args = if (spill) try vm.allocator.alloc(runtime_abi.Value, value.args.len) else arg_stack[0..value.args.len];
            defer if (spill) vm.allocator.free(call_args);
            fillTransferredArgs(call_args, registers, register_owned, value.args, callee.decl.param_ownership);
            const result = try runPrepared(vm, prepared, callee, call_args, writer, hooks);
            if (value.dst) |dst| setSlotOwned(vm, &registers[dst], &register_owned[dst], result) else vm.heap.dropValue(result);
            pc += 1;
            continue :dispatch code[pc];
        },
        .call_native => |value| {
            const callback = hooks.call_native orelse {
                vm.rememberError("vm native bridge was not installed");
                return error.RuntimeFailure;
            };
            var arg_stack: [16]runtime_abi.Value = undefined;
            const spill = value.args.len > arg_stack.len;
            const call_args = if (spill) try vm.allocator.alloc(runtime_abi.Value, value.args.len) else arg_stack[0..value.args.len];
            defer if (spill) vm.allocator.free(call_args);
            for (value.args, 0..) |register_index, index| call_args[index] = registers[register_index];
            var result = try callback(hooks.context, value.function_id, call_args);
            result = try vm.materializeNativeResult(module, value.return_ty, result);
            if (value.dst) |dst| setSlotOwned(vm, &registers[dst], &register_owned[dst], result) else vm.heap.dropValue(result);
            pc += 1;
            continue :dispatch code[pc];
        },
        .call_virtual => |value| {
            const receiver_value = registers[value.receiver];
            if (receiver_value != .raw_ptr or receiver_value.raw_ptr == 0) {
                vm.rememberError("virtual method call requires a valid class receiver");
                return error.RuntimeFailure;
            }
            const actual_type_name = vm.heap.getStructTypeName(receiver_value.raw_ptr) orelse value.static_type_name;
            const resolved_method = vm.resolveVirtualMethod(module, actual_type_name, value.method_name) orelse {
                vm.rememberError("virtual method could not be resolved on the concrete receiver type");
                return error.RuntimeFailure;
            };
            const adjusted_receiver = if (resolved_method.receiver_offset == 0)
                receiver_value.raw_ptr
            else
                @intFromPtr((@as([*]align(1) runtime_abi.Value, @ptrFromInt(receiver_value.raw_ptr)) + resolved_method.receiver_offset));
            const total_args = value.args.len + 1;
            var arg_stack: [17]runtime_abi.Value = undefined;
            const spill = total_args > arg_stack.len;
            const call_args = if (spill) try vm.allocator.alloc(runtime_abi.Value, total_args) else arg_stack[0..total_args];
            defer if (spill) vm.allocator.free(call_args);
            call_args[0] = .{ .raw_ptr = adjusted_receiver };
            for (value.args, 0..) |register_index, index| call_args[index + 1] = registers[register_index];

            const result = if (prepared.indexOfId(resolved_method.function_id)) |method_index|
                try runPrepared(vm, prepared, &prepared.functions[method_index], call_args, writer, hooks)
            else native_result: {
                const callback = hooks.call_native orelse {
                    vm.rememberError("vm native bridge was not installed");
                    return error.RuntimeFailure;
                };
                var native_value = try callback(hooks.context, resolved_method.function_id, call_args);
                native_value = try vm.materializeNativeResult(module, value.return_ty, native_value);
                break :native_result native_value;
            };
            if (value.dst) |dst| setSlotOwned(vm, &registers[dst], &register_owned[dst], result) else vm.heap.dropValue(result);
            pc += 1;
            continue :dispatch code[pc];
        },
        .call_value => |value| {
            const callee_value = registers[value.callee];
            if (callee_value != .raw_ptr) {
                vm.rememberFmt(
                    "indirect call requires a callable function value (callee_register={d}, tag={s})",
                    .{ value.callee, @tagName(callee_value) },
                );
                return error.RuntimeFailure;
            }
            var arg_stack: [16]runtime_abi.Value = undefined;
            const arg_spill = value.args.len > arg_stack.len;
            const call_args = if (arg_spill) try vm.allocator.alloc(runtime_abi.Value, value.args.len) else arg_stack[0..value.args.len];
            defer if (arg_spill) vm.allocator.free(call_args);
            fillTransferredArgs(call_args, registers, register_owned, value.args, value.param_ownership);
            const result = if (vm.heap.getClosure(callee_value.raw_ptr)) |closure| closure_call: {
                runtime_abi.emitExecutionTrace("CALLABLE", "INVOKE_CLOSURE", "raw=0x{x} fn={d} captures={d}", .{ callee_value.raw_ptr, closure.function_id, closure.captures.len });
                const total_args = call_args.len + closure.captures.len;
                var closure_stack: [24]runtime_abi.Value = undefined;
                const closure_spill = total_args > closure_stack.len;
                const closure_args = if (closure_spill) try vm.allocator.alloc(runtime_abi.Value, total_args) else closure_stack[0..total_args];
                defer if (closure_spill) vm.allocator.free(closure_args);
                @memcpy(closure_args[0..call_args.len], call_args);
                @memcpy(closure_args[call_args.len..], closure.captures);
                if (!closure.is_native) {
                    const callee_index = prepared.indexOfId(closure.function_id) orelse {
                        vm.rememberError("bytecode function id is out of range");
                        return error.RuntimeFailure;
                    };
                    break :closure_call try runPrepared(vm, prepared, &prepared.functions[callee_index], closure_args, writer, hooks);
                }
                const callback = hooks.call_native orelse {
                    vm.rememberError("native closure call requires a native call hook");
                    return error.RuntimeFailure;
                };
                break :closure_call try callback(hooks.context, closure.function_id, closure_args);
            } else if (callee_value.raw_ptr <= std.math.maxInt(u32)) direct: {
                const function_id: u32 = @intCast(callee_value.raw_ptr);
                if (prepared.indexOfId(function_id)) |callee_index| {
                    break :direct try runPrepared(vm, prepared, &prepared.functions[callee_index], call_args, writer, hooks);
                }
                const callback = hooks.call_native orelse {
                    vm.rememberError("vm native bridge was not installed");
                    return error.RuntimeFailure;
                };
                break :direct try callback(hooks.context, function_id, call_args);
            } else {
                vm.rememberError("indirect call received an unmanaged raw pointer that is not a runtime closure");
                return error.RuntimeFailure;
            };
            if (value.dst) |dst| setSlotOwned(vm, &registers[dst], &register_owned[dst], result) else vm.heap.dropValue(result);
            pc += 1;
            continue :dispatch code[pc];
        },
        .ret => |value| {
            const result = if (value.src) |src| registers[src] else runtime_abi.Value{ .void = {} };
            if (value.src) |src| {
                if (register_owned[src]) {
                    register_owned[src] = false;
                    registers[src] = .{ .void = {} };
                }
            }
            return result;
        },
        // Fused superinstructions (decode-produced; see vm_prepare.zig). Each
        // arm performs exactly what the original instruction sequence did,
        // minus the dead intermediate register writes.
        .fused_compare_branch => |value| {
            const lhs = registers[value.lhs];
            const rhs = registers[value.rhs];
            const taken = if (lhs == .integer and rhs == .integer)
                compareIntegers(lhs.integer, rhs.integer, value.op)
            else
                try value_impl.compareValues(vm, lhs, rhs, value.op);
            pc = if (taken) value.true_target else value.false_target;
            continue :dispatch code[pc];
        },
        .fused_compare_const_branch => |value| {
            const lhs = registers[value.lhs];
            const taken = if (lhs == .integer)
                compareIntegers(lhs.integer, value.imm, value.op)
            else
                try value_impl.compareValues(vm, lhs, .{ .integer = value.imm }, value.op);
            pc = if (taken) value.true_target else value.false_target;
            continue :dispatch code[pc];
        },
        .fused_cmp_local_const_branch => |value| {
            const lhs = locals[value.local];
            const taken = if (lhs == .integer)
                compareIntegers(lhs.integer, value.imm, value.op)
            else
                try value_impl.compareValues(vm, lhs, .{ .integer = value.imm }, value.op);
            pc = if (taken) value.true_target else value.false_target;
            continue :dispatch code[pc];
        },
        .fused_arith_locals_store => |value| {
            const lhs = locals[value.lhs_local];
            const rhs = locals[value.rhs_local];
            const result = if (lhs == .integer and rhs == .integer)
                runtime_abi.Value{ .integer = arithIntegers(lhs.integer, rhs.integer, value.kind) }
            else
                try arithValues(vm, lhs, rhs, value.kind);
            setSlotBorrowed(vm, &locals[value.dst_local], &local_owned[value.dst_local], result);
            pc += 1;
            continue :dispatch code[pc];
        },
        .fused_arith_local_const_store => |value| {
            const lhs = locals[value.lhs_local];
            const result = if (lhs == .integer)
                runtime_abi.Value{ .integer = arithIntegers(lhs.integer, value.imm, value.kind) }
            else
                try arithValues(vm, lhs, .{ .integer = value.imm }, value.kind);
            setSlotBorrowed(vm, &locals[value.dst_local], &local_owned[value.dst_local], result);
            pc += 1;
            continue :dispatch code[pc];
        },
        .fused_array_bind_local => |value| {
            const array_value = registers[value.array];
            const index_value = registers[value.index];
            if (array_value != .raw_ptr or array_value.raw_ptr == 0 or index_value != .integer or index_value.integer < 0) {
                vm.rememberError("array load requires a valid array handle and index");
                return error.RuntimeFailure;
            }
            const array_ptr: *const ArrayObject = @ptrFromInt(array_value.raw_ptr);
            const element_index: usize = @intCast(index_value.integer);
            if (element_index >= array_ptr.len) {
                vm.rememberError("array index is out of bounds");
                return error.RuntimeFailure;
            }
            const item_value = runtime_abi.bridgeValueToValue(array_ptr.items[element_index]);
            if (item_value != .raw_ptr or item_value.raw_ptr == 0) {
                vm.rememberFmt("struct copy requires valid pointers: {s} dst_ok=1 src_ok=0", .{value.type_name});
                return error.RuntimeFailure;
            }
            if (vm.isManagedStructPointer(item_value.raw_ptr)) {
                // Proven read-only binding over a stable array: alias the
                // element (the native backend never copies borrowed loop
                // elements either). The slot is non-owning, so the element is
                // not dropped when the binding is overwritten or released.
                setSlotBorrowed(vm, &locals[value.dst_local], &local_owned[value.dst_local], item_value);
            } else {
                // Native-layout element: materialize an owned runtime copy —
                // the same visible contents the clone + copy_indirect pair
                // used to produce.
                const copied = try vm.copyStructFromNativeLayout(module, value.type_name, item_value.raw_ptr);
                setSlotOwned(vm, &locals[value.dst_local], &local_owned[value.dst_local], .{ .raw_ptr = copied });
            }
            pc += 1;
            continue :dispatch code[pc];
        },
        .fused_arith_locals_ret => |value| {
            const lhs = locals[value.lhs_local];
            const rhs = locals[value.rhs_local];
            // Arith results are never heap-managed, so returning directly
            // matches ret's owned-register handling (nothing to transfer).
            if (lhs == .integer and rhs == .integer) {
                return .{ .integer = arithIntegers(lhs.integer, rhs.integer, value.kind) };
            }
            return try arithValues(vm, lhs, rhs, value.kind);
        },
    }
}

inline fn compareIntegers(lhs: i64, rhs: i64, op: bytecode.CompareOp) bool {
    return switch (op) {
        .equal => lhs == rhs,
        .not_equal => lhs != rhs,
        .less => lhs < rhs,
        .less_equal => lhs <= rhs,
        .greater => lhs > rhs,
        .greater_equal => lhs >= rhs,
    };
}

inline fn arithIntegers(lhs: i64, rhs: i64, kind: bytecode.ArithKind) i64 {
    return switch (kind) {
        .add => lhs +% rhs,
        .subtract => lhs -% rhs,
        .multiply => lhs *% rhs,
    };
}

fn arithValues(vm: *Vm, lhs: runtime_abi.Value, rhs: runtime_abi.Value, kind: bytecode.ArithKind) !runtime_abi.Value {
    return switch (kind) {
        .add => try value_impl.addValues(vm, lhs, rhs),
        .subtract => try value_impl.subtractValues(vm, lhs, rhs),
        .multiply => try value_impl.multiplyValues(vm, lhs, rhs),
    };
}

fn setSlotOwned(vm: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = vm.heap.isManagedValue(value);
    if (old_owned) vm.heap.dropValue(old);
}

// Fast paths for results whose ownership is statically known, avoiding the
// isManagedValue heap-membership hash lookup that setSlotOwned performs.
// `setSlotPrimitive`: const/arithmetic/compare results are never heap-managed.
// `setSlotManaged`: a freshly allocated+registered heap value is always owned.
inline fn setSlotPrimitive(vm: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = false;
    if (old_owned) vm.heap.dropValue(old);
}

inline fn setSlotManaged(vm: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = true;
    if (old_owned) vm.heap.dropValue(old);
}

fn setSlotBorrowed(vm: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = false;
    if (old_owned) vm.heap.dropValue(old);
}

fn setSlotUnmanaged(vm: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
    const old = slot.*;
    const old_owned = owned.*;
    slot.* = value;
    owned.* = false;
    if (old_owned) vm.heap.dropValue(old);
}

fn transferSlot(
    vm: *Vm,
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

fn fillTransferredArgs(
    values: []runtime_abi.Value,
    registers: []runtime_abi.Value,
    register_owned: []bool,
    argument_registers: []const u32,
    param_ownership: []const bytecode.OwnershipMode,
) void {
    for (argument_registers, 0..) |register_index, index| {
        values[index] = registers[register_index];
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

fn ownershipModeAt(values: []const bytecode.OwnershipMode, index: usize) bytecode.OwnershipMode {
    if (index < values.len) return values[index];
    return .owned;
}

fn releaseTrackedSlots(vm: *Vm, slots: []runtime_abi.Value, owned: []const bool) void {
    for (slots, owned) |slot, is_owned| if (is_owned) vm.heap.dropValue(slot);
}
