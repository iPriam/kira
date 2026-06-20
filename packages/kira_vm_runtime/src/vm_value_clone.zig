const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const ArrayObject = @import("ownership.zig").ArrayObject;
const construct_any = @import("vm_construct_any.zig");

pub fn cloneBorrowedValueForStore(
    vm: anytype,
    module: *const bytecode.Module,
    value_type: bytecode.TypeRef,
    value: runtime_abi.Value,
) anyerror!runtime_abi.Value {
    if (!vm.heap.isManagedValue(value)) {
        return switch (value_type.kind) {
            .construct_any => if (value == .raw_ptr and value.raw_ptr != 0)
                try construct_any.materializeFromNativeIfNeeded(vm, module, value_type, value.raw_ptr)
            else
                value,
            else => value,
        };
    }

    return switch (value_type.kind) {
        .array => try vm.cloneArrayValueDeep(module, try vm.arrayElementType(module, value_type), value),
        .enum_instance => try vm.cloneEnumValue(module, value_type.name orelse {
            vm.rememberError("enum store type is missing a name");
            return error.RuntimeFailure;
        }, value),
        .ffi_struct => blk: {
            if (value != .raw_ptr or value.raw_ptr == 0) break :blk value;
            const type_name = value_type.name orelse {
                vm.rememberError("struct store type is missing a name");
                return error.RuntimeFailure;
            };
            const copied = if (vm.isManagedStructPointer(value.raw_ptr))
                try vm.cloneStructValue(module, type_name, value.raw_ptr)
            else
                try vm.copyStructFromNativeLayout(module, type_name, value.raw_ptr);
            break :blk runtime_abi.Value{ .raw_ptr = copied };
        },
        .construct_any => if (value == .raw_ptr and value.raw_ptr != 0)
            try cloneBorrowedManagedValueDynamic(vm, module, value)
        else
            value,
        .string => if (value.string.len == 0) value else blk: {
            const owned = try vm.allocator.dupe(u8, value.string);
            errdefer vm.allocator.free(owned);
            try vm.heap.registerString(owned);
            break :blk runtime_abi.Value{ .string = owned };
        },
        .raw_ptr => blk: {
            const name = value_type.name orelse break :blk value;
            if (!isCallbackTypeName(name) or value != .raw_ptr or value.raw_ptr == 0) break :blk value;
            break :blk runtime_abi.Value{ .raw_ptr = try cloneClosureValue(vm, module, value.raw_ptr) };
        },
        else => value,
    };
}

pub fn cloneBorrowedManagedValueDynamic(vm: anytype, module: *const bytecode.Module, value: runtime_abi.Value) anyerror!runtime_abi.Value {
    return switch (value) {
        .string => |bytes| if (bytes.len == 0 or !vm.heap.isManagedValue(value)) value else blk: {
            const owned = try vm.allocator.dupe(u8, bytes);
            errdefer vm.allocator.free(owned);
            try vm.heap.registerString(owned);
            break :blk runtime_abi.Value{ .string = owned };
        },
        .raw_ptr => |ptr| blk: {
            const record = vm.heap.getRecord(ptr) orelse break :blk value;
            switch (record.kind) {
                .closure => break :blk runtime_abi.Value{ .raw_ptr = try cloneClosureValue(vm, module, ptr) },
                .array => |array| break :blk runtime_abi.Value{ .raw_ptr = try cloneArrayValueDynamic(vm, module, array) },
                .struct_fields => |struct_fields| {
                    const type_name = struct_fields.type_name;
                    if (vm.findTypeCached(module, type_name) != null) {
                        break :blk runtime_abi.Value{ .raw_ptr = try vm.cloneStructValue(module, type_name, ptr) };
                    }
                    if (vm.findEnumCached(module, type_name) != null) {
                        break :blk try vm.cloneEnumValue(module, type_name, value);
                    }
                    break :blk value;
                },
                .string_bytes => break :blk value,
            }
        },
        else => value,
    };
}

pub fn cloneBorrowedLocalValue(
    vm: anytype,
    module: *const bytecode.Module,
    value_type: bytecode.TypeRef,
    value: runtime_abi.Value,
) anyerror!runtime_abi.Value {
    return cloneBorrowedValueForStore(vm, module, value_type, value);
}

pub fn cloneClosureValue(vm: anytype, module: *const bytecode.Module, closure_ptr: usize) anyerror!usize {
    const source = vm.heap.getClosure(closure_ptr) orelse {
        vm.rememberError("callback store source is not a valid closure");
        return error.RuntimeFailure;
    };
    const captures = try vm.allocator.alloc(runtime_abi.Value, source.captures.len);
    for (captures) |*capture| capture.* = .{ .void = {} };
    var initialized: usize = 0;
    errdefer {
        vm.heap.dropSlots(captures[0..initialized]);
        vm.allocator.free(captures);
    }

    const function_decl = module.findFunctionById(source.function_id);
    const capture_types = if (function_decl) |decl| blk: {
        if (source.captures.len > decl.param_count) {
            vm.rememberError("closure capture metadata is inconsistent");
            return error.RuntimeFailure;
        }
        const start = decl.param_count - source.captures.len;
        break :blk decl.local_types[start..decl.param_count];
    } else null;

    for (source.captures, 0..) |capture, index| {
        const cloned = if (capture_types) |types|
            try cloneBorrowedValueForStore(vm, module, types[index], capture)
        else
            capture;
        vm.heap.assignTransferred(&captures[index], cloned);
        initialized += 1;
    }

    const clone = try vm.allocator.create(@TypeOf(source.*));
    errdefer vm.allocator.destroy(clone);
    clone.* = .{
        .function_id = source.function_id,
        .is_native = source.is_native,
        .captures = captures,
    };
    return vm.heap.registerClosure(clone);
}

fn cloneArrayValueDynamic(vm: anytype, module: *const bytecode.Module, source: *const ArrayObject) anyerror!usize {
    const object = try vm.heap.allocArrayObject();
    errdefer vm.heap.freeArrayObject(object);
    const items = try vm.heap.allocBridgeSlice(@max(source.len, 1));
    for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
    var initialized: usize = 0;
    errdefer {
        for (items[0..initialized]) |item| vm.heap.dropValue(runtime_abi.bridgeValueToValue(item));
        vm.heap.freeBridgeSlice(items);
    }
    for (source.items[0..source.len], 0..) |item, index| {
        const cloned = try cloneBorrowedManagedValueDynamic(vm, module, runtime_abi.bridgeValueToValue(item));
        items[index] = runtime_abi.bridgeValueFromValue(cloned);
        initialized += 1;
    }
    object.* = .{
        .len = source.len,
        .items = items.ptr,
    };
    return vm.heap.registerArray(object);
}

fn isCallbackTypeName(name: []const u8) bool {
    return std.mem.indexOf(u8, name, "->") != null;
}
