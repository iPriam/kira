const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const builtins = @import("builtins.zig");
const ownership = @import("ownership.zig");
const native_layout = @import("native_layout.zig");
const helper_impl = @import("vm_helpers.zig");
const value_impl = @import("vm_values.zig");

const ArrayObject = ownership.ArrayObject;
const ClosureObject = ownership.ClosureObject;

const NativeStateBox = extern struct { type_id: u64, payload: usize, runtime_payload: usize };

pub const NativeCallHook = *const fn (?*anyopaque, u32, []const runtime_abi.Value) anyerror!runtime_abi.Value;
pub const ResolveFunctionHook = *const fn (?*anyopaque, u32) anyerror!usize;

pub const Hooks = struct { context: ?*anyopaque = null, call_native: ?NativeCallHook = null, resolve_function: ?ResolveFunctionHook = null, copy_struct_args_by_value: bool = true };

pub const Vm = struct {
    allocator: std.mem.Allocator,
    heap: ownership.Heap,
    last_error_buffer: [256]u8 = [_]u8{0} ** 256,
    last_error_len: usize = 0,

    pub fn init(allocator: std.mem.Allocator) Vm {
        return .{ .allocator = allocator, .heap = ownership.Heap.init(allocator) };
    }

    pub fn deinit(self: *Vm) void {
        self.heap.deinit();
    }

    pub fn managedObjectCount(self: *const Vm) usize {
        return self.heap.count();
    }

    pub fn releaseManagedValue(self: *Vm, value: runtime_abi.Value) void {
        self.heap.releaseValue(value);
    }

    pub fn retainManagedValue(self: *Vm, value: runtime_abi.Value) void {
        self.heap.retainValue(value);
    }

    pub fn beginNativeBoundary(self: *Vm) !void {
        try self.heap.beginBoundaryPinScope();
    }

    pub fn endNativeBoundary(self: *Vm) void {
        self.heap.endBoundaryPinScope();
    }

    pub fn pinNativeBoundaryValue(self: *Vm, value: runtime_abi.Value) !void {
        try self.heap.pinBoundaryValue(value);
    }

    pub fn runMain(self: *Vm, module: *const bytecode.Module, writer: anytype) anyerror!void {
        const entry_function_id = module.entry_function_id orelse {
            self.rememberError("bytecode module has no runtime entrypoint");
            return error.RuntimeFailure;
        };
        const result = try self.runFunctionById(module, entry_function_id, &.{}, writer, .{});
        self.heap.releaseValue(result);
    }

    pub fn runFunctionById(
        self: *Vm,
        module: *const bytecode.Module,
        function_id: u32,
        args: []const runtime_abi.Value,
        writer: anytype,
        hooks: Hooks,
    ) anyerror!runtime_abi.Value {
        const function_decl = module.findFunctionById(function_id) orelse {
            self.rememberError("bytecode function id is out of range");
            return error.RuntimeFailure;
        };
        return self.runFunction(module, function_decl, args, writer, hooks);
    }

    pub fn lastError(self: *const Vm) ?[]const u8 {
        if (self.last_error_len == 0) return null;
        return self.last_error_buffer[0..self.last_error_len];
    }

    pub fn materializeNativeStruct(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) !usize {
        runtime_abi.emitExecutionTrace("BRIDGE", "MATERIALIZE", "native->runtime type={s} ptr=0x{x}", .{ type_name, native_ptr });
        return self.copyStructFromNativeLayout(module, type_name, native_ptr);
    }

    pub fn materializeNativeClosure(self: *Vm, module: *const bytecode.Module, native_ptr: usize, external_capture_types: ?[]const bytecode.TypeRef) !usize {
        if (native_ptr == 0) return 0;
        if (self.heap.getClosure(native_ptr) != null) return native_ptr;
        const function_id_ptr: *const i64 = @ptrFromInt(native_ptr);
        const capture_count_ptr: *const i64 = @ptrFromInt(native_ptr + 8);
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
        const native_slots: [*]const runtime_abi.BridgeValue = @ptrFromInt(native_ptr + 16);
        const closure = try self.allocator.create(ClosureObject);
        const captures = try self.allocator.alloc(runtime_abi.Value, capture_count);
        for (captures) |*capture| capture.* = .{ .void = {} };
        for (0..capture_count) |index| {
            var capture_value = runtime_abi.bridgeValueToValue(native_slots[index]);
            if (function_decl) |decl| {
                const param_index = decl.param_count - @as(u32, @intCast(capture_count)) + @as(u32, @intCast(index));
                const capture_ty = decl.local_types[param_index];
                if (capture_ty.kind == .ffi_struct and capture_value == .raw_ptr and capture_value.raw_ptr != 0) {
                    capture_value = .{ .raw_ptr = try self.copyStructFromNativeLayout(module, capture_ty.name orelse {
                        self.rememberError("native closure capture type is missing a name");
                        return error.RuntimeFailure;
                    }, capture_value.raw_ptr) };
                }
            } else if (external_capture_types) |capture_types| {
                if (index >= capture_types.len) {
                    self.rememberError("native closure capture metadata is incomplete");
                    return error.RuntimeFailure;
                }
                const capture_ty = capture_types[index];
                if (capture_ty.kind == .ffi_struct and capture_value == .raw_ptr and capture_value.raw_ptr != 0) {
                    capture_value = .{ .raw_ptr = try self.copyStructFromNativeLayout(module, capture_ty.name orelse {
                        self.rememberError("native closure capture type is missing a name");
                        return error.RuntimeFailure;
                    }, capture_value.raw_ptr) };
                }
            }
            self.heap.assignBorrowed(&captures[index], capture_value);
        }
        closure.* = .{
            .function_id = function_id,
            .is_native = function_decl == null,
            .captures = captures,
        };
        runtime_abi.emitExecutionTrace("BRIDGE", "MATERIALIZE", "native->runtime closure fn={d} captures={d} ptr=0x{x}", .{ closure.function_id, capture_count, native_ptr });
        return self.heap.registerClosure(closure);
    }

    pub fn lowerStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) !usize {
        runtime_abi.emitExecutionTrace("BRIDGE", "COPY", "runtime->native type={s} ptr=0x{x}", .{ type_name, runtime_ptr });
        return self.copyStructToNativeLayout(module, type_name, runtime_ptr);
    }

    pub fn writeStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) !void {
        runtime_abi.emitExecutionTrace("BRIDGE", "COPY", "sync runtime->native type={s} src=0x{x} dst=0x{x}", .{ type_name, runtime_ptr, native_ptr });
        try self.copyStructToNativeLayoutInto(module, type_name, runtime_ptr, native_ptr);
    }

    pub fn copyArrayToNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize) anyerror!usize {
        const source: *const ArrayObject = @ptrFromInt(runtime_array_ptr);
        const object = try self.allocator.create(ArrayObject);
        const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
        const element_ty = try self.arrayElementType(module, array_ty);
        for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
        for (source.items[0..source.len], 0..) |item, index| {
            const value = runtime_abi.bridgeValueToValue(item);
            items[index] = runtime_abi.bridgeValueFromValue(try self.copyValueToNativeLayout(module, element_ty, value));
        }
        object.* = .{
            .len = source.len,
            .items = items.ptr,
        };
        return @intFromPtr(object);
    }

    pub fn copyArrayFromNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize) anyerror!usize {
        const source: *const ArrayObject = @ptrFromInt(native_array_ptr);
        const object = try self.allocator.create(ArrayObject);
        const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
        const element_ty = try self.arrayElementType(module, array_ty);
        for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
        for (source.items[0..source.len], 0..) |item, index| {
            const value = runtime_abi.bridgeValueToValue(item);
            items[index] = runtime_abi.bridgeValueFromValue(try self.copyValueFromNativeLayout(module, element_ty, value));
        }
        object.* = .{
            .len = source.len,
            .items = items.ptr,
        };
        return try self.heap.registerArray(object);
    }

    pub fn syncArrayFromNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize, native_array_ptr: usize) anyerror!void {
        const source: *const ArrayObject = @ptrFromInt(native_array_ptr);
        const destination: *ArrayObject = @ptrFromInt(runtime_array_ptr);
        const old_items = destination.items[0..@max(destination.len, 1)];
        for (old_items[0..destination.len]) |item| self.heap.releaseValue(runtime_abi.bridgeValueToValue(item));

        const items = try self.allocator.alloc(runtime_abi.BridgeValue, @max(source.len, 1));
        const element_ty = try self.arrayElementType(module, array_ty);
        for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
        for (source.items[0..source.len], 0..) |item, index| {
            const value = runtime_abi.bridgeValueToValue(item);
            items[index] = runtime_abi.bridgeValueFromValue(try self.copyValueFromNativeLayout(module, element_ty, value));
        }
        self.allocator.free(old_items);
        destination.len = source.len;
        destination.items = items.ptr;
    }

    pub fn syncStructFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) !void {
        runtime_abi.emitExecutionTrace("BRIDGE", "COPY", "sync native->runtime type={s} src=0x{x} dst=0x{x}", .{ type_name, native_ptr, runtime_ptr });
        try self.copyStructFromNativeLayoutInto(module, type_name, runtime_ptr, native_ptr);
    }

    fn runFunction(
        self: *Vm,
        module: *const bytecode.Module,
        function_decl: bytecode.Function,
        args: []const runtime_abi.Value,
        writer: anytype,
        hooks: Hooks,
    ) anyerror!runtime_abi.Value {
        const registers = try self.allocator.alloc(runtime_abi.Value, function_decl.register_count);
        const register_owned = try self.allocator.alloc(bool, function_decl.register_count);
        defer {
            self.releaseTrackedSlots(registers, register_owned);
            self.allocator.free(register_owned);
            self.allocator.free(registers);
        }
        const locals = try self.allocator.alloc(runtime_abi.Value, function_decl.local_count);
        const local_owned = try self.allocator.alloc(bool, function_decl.local_count);
        defer {
            self.releaseTrackedSlots(locals, local_owned);
            self.allocator.free(local_owned);
            self.allocator.free(locals);
        }
        const label_offsets = try helper_impl.buildLabelOffsets(self.allocator, function_decl.instructions);
        defer self.allocator.free(label_offsets);

        for (registers) |*slot| slot.* = .{ .void = {} };
        for (locals) |*slot| slot.* = .{ .void = {} };
        @memset(register_owned, false);
        @memset(local_owned, false);
        for (function_decl.local_types, 0..) |local_ty, index| {
            if (local_ty.kind != .ffi_struct) continue;
            if (index < function_decl.param_count and !hooks.copy_struct_args_by_value) continue;
            const type_name = local_ty.name orelse {
                self.rememberError("struct local type is missing a name");
                return error.RuntimeFailure;
            };
            self.setSlotOwned(&locals[index], &local_owned[index], .{ .raw_ptr = try self.allocateStruct(module, type_name) });
        }
        if (args.len != function_decl.param_count) {
            self.rememberError("bytecode function call used the wrong number of arguments");
            return error.RuntimeFailure;
        }
        for (args, 0..) |arg, index| {
            if (function_decl.local_types[index].kind == .ffi_struct) {
                if (arg != .raw_ptr or arg.raw_ptr == 0) {
                    self.rememberError("struct argument requires a valid pointer");
                    return error.RuntimeFailure;
                }
                if (hooks.copy_struct_args_by_value) {
                    const type_name = function_decl.local_types[index].name orelse {
                        self.rememberError("struct local type is missing a name");
                        return error.RuntimeFailure;
                    };
                    const type_decl = helper_impl.findType(module, type_name) orelse {
                        self.rememberError("struct type could not be resolved");
                        return error.RuntimeFailure;
                    };
                    const dst_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(locals[index].raw_ptr);
                    const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(arg.raw_ptr);
                    try self.copyStruct(module, type_decl, dst_ptr, src_ptr);
                } else self.setSlotBorrowed(&locals[index], &local_owned[index], arg);
            } else {
                self.setSlotBorrowed(&locals[index], &local_owned[index], arg);
            }
        }

        var pc: usize = 0;
        while (pc < function_decl.instructions.len) {
            const inst = function_decl.instructions[pc];
            switch (inst) {
                .const_int => |value| self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .integer = value.value }),
                .const_float => |value| self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .float = value.value }),
                .const_string => |value| self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .string = value.value }),
                .const_bool => |value| self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .boolean = value.value }),
                .const_null_ptr => |value| self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = 0 }),
                .const_function => |value| {
                    const raw_ptr = switch (value.representation) {
                        .callable_value => value.function_id,
                        .native_callback => if (hooks.resolve_function) |resolve_function|
                            try helper_impl.resolveFunctionPointer(hooks, resolve_function, value.function_id)
                        else
                            value.function_id,
                    };
                    runtime_abi.emitExecutionTrace("CALLABLE", "CONST_FUNCTION", "dst={d} fn={d} raw=0x{x} repr={s}", .{ value.dst, value.function_id, raw_ptr, @tagName(value.representation) });
                    self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = raw_ptr });
                },
                .const_closure => |value| {
                    const closure_ptr = try self.allocateClosure(registers, value.function_id, value.captures);
                    runtime_abi.emitExecutionTrace("CALLABLE", "CONST_CLOSURE", "dst={d} fn={d} raw=0x{x} captures={d}", .{ value.dst, value.function_id, closure_ptr, value.captures.len });
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = closure_ptr });
                },
                .alloc_struct => |value| self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try self.allocateStruct(module, value.type_name) }),
                .alloc_enum => |value| self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try self.allocateEnum(value.enum_type_name, registers, value.discriminant, value.payload_src) }),
                .alloc_native_state => |value| {
                    const src_value = registers[value.src];
                    if (src_value != .raw_ptr or src_value.raw_ptr == 0) {
                        self.rememberError("nativeState requires a valid Kira struct value");
                        return error.RuntimeFailure;
                    }
                    self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try self.allocateNativeState(module, value.type_name, value.type_id, src_value.raw_ptr) });
                },
                .alloc_array => |value| {
                    const len_value = registers[value.len];
                    if (len_value != .integer or len_value.integer < 0) {
                        self.rememberError("array allocation requires a non-negative integer length");
                        return error.RuntimeFailure;
                    }
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try self.allocateArray(@intCast(len_value.integer)) });
                },
                .add => |value| {
                    const lhs = registers[value.lhs];
                    const rhs = registers[value.rhs];
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try value_impl.addValues(self, lhs, rhs));
                },
                .subtract => |value| {
                    const lhs = registers[value.lhs];
                    const rhs = registers[value.rhs];
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try value_impl.subtractValues(self, lhs, rhs));
                },
                .multiply => |value| {
                    const lhs = registers[value.lhs];
                    const rhs = registers[value.rhs];
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try value_impl.multiplyValues(self, lhs, rhs));
                },
                .divide => |value| {
                    const lhs = registers[value.lhs];
                    const rhs = registers[value.rhs];
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try value_impl.divideValues(self, lhs, rhs));
                },
                .modulo => |value| {
                    const lhs = registers[value.lhs];
                    const rhs = registers[value.rhs];
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try value_impl.moduloValues(self, lhs, rhs));
                },
                .compare => |value| {
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .boolean = try value_impl.compareValues(self, registers[value.lhs], registers[value.rhs], value.op) });
                },
                .unary => |value| {
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try value_impl.unaryValue(self, registers[value.src], value.op));
                },
                .store_local => |value| self.setSlotBorrowed(&locals[value.local], &local_owned[value.local], registers[value.src]),
                .load_local => |value| self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], locals[value.local]),
                .local_ptr => |value| self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = @intFromPtr(&locals[value.local]) }),
                .subobject_ptr => |value| {
                    const base = registers[value.base];
                    if (base != .raw_ptr or base.raw_ptr == 0) {
                        self.rememberError("subobject access requires a valid struct pointer");
                        return error.RuntimeFailure;
                    }
                    const base_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(base.raw_ptr);
                    self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = @intFromPtr(base_ptr + value.offset) });
                },
                .field_ptr => |value| {
                    const base = registers[value.base];
                    if (base != .raw_ptr or base.raw_ptr == 0) {
                        self.rememberError("field access requires a valid struct pointer");
                        return error.RuntimeFailure;
                    }
                    const base_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(base.raw_ptr);
                    const field_index: usize = @intCast(value.field_index);
                    const slot_ptr = base_ptr + field_index;
                    if (value.field_ty.kind == .ffi_struct) {
                        if (slot_ptr[0] != .raw_ptr or slot_ptr[0].raw_ptr == 0) {
                            self.rememberError("nested struct field storage is invalid");
                            return error.RuntimeFailure;
                        }
                        self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = slot_ptr[0].raw_ptr });
                    } else {
                        self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = @intFromPtr(slot_ptr) });
                    }
                },
                .recover_native_state => |value| {
                    const state_value = registers[value.state];
                    if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
                        self.rememberError("nativeRecover requires a valid native state token");
                        return error.RuntimeFailure;
                    }
                    self.setSlotUnmanaged(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try self.recoverNativeState(module, value.type_name, state_value.raw_ptr, value.type_id) });
                },
                .native_state_field_get => |value| {
                    const state_value = registers[value.state];
                    if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
                        self.rememberError("native state field read requires a valid recovered state");
                        return error.RuntimeFailure;
                    }
                    const payload: [*]const runtime_abi.BridgeValue = @ptrFromInt(state_value.raw_ptr);
                    self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], runtime_abi.bridgeValueToValue(payload[@intCast(value.field_index)]));
                    _ = value.field_ty;
                },
                .native_state_field_set => |value| {
                    const state_value = registers[value.state];
                    if (state_value != .raw_ptr or state_value.raw_ptr == 0) {
                        self.rememberError("native state field write requires a valid recovered state");
                        return error.RuntimeFailure;
                    }
                    const payload: [*]runtime_abi.BridgeValue = @ptrFromInt(state_value.raw_ptr);
                    payload[@intCast(value.field_index)] = runtime_abi.bridgeValueFromValue(registers[value.src]);
                    _ = value.field_ty;
                },
                .c_string_to_string => |value| {
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], try self.copyCString(registers[value.src]));
                },
                .array_len => |value| {
                    const array_value = registers[value.array];
                    if (array_value != .raw_ptr or array_value.raw_ptr == 0) {
                        self.rememberError("array length requires a valid array handle");
                        return error.RuntimeFailure;
                    }
                    const array_ptr: *const ArrayObject = @ptrFromInt(array_value.raw_ptr);
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], .{ .integer = @intCast(array_ptr.len) });
                },
                .array_get => |value| {
                    const array_value = registers[value.array];
                    const index_value = registers[value.index];
                    if (array_value != .raw_ptr or array_value.raw_ptr == 0 or index_value != .integer or index_value.integer < 0) {
                        self.rememberError("array load requires a valid array handle and index");
                        return error.RuntimeFailure;
                    }
                    const array_ptr: *const ArrayObject = @ptrFromInt(array_value.raw_ptr);
                    const index: usize = @intCast(index_value.integer);
                    if (index >= array_ptr.len) {
                        self.rememberError("array index is out of bounds");
                        return error.RuntimeFailure;
                    }
                    var item_value = runtime_abi.bridgeValueToValue(array_ptr.items[index]);
                    if (value.ty.kind == .ffi_struct and item_value == .raw_ptr and item_value.raw_ptr != 0 and !self.isManagedStructPointer(item_value.raw_ptr)) {
                        item_value = .{ .raw_ptr = try self.copyStructFromNativeLayout(module, value.ty.name orelse {
                            self.rememberError("array element struct type is missing a name");
                            return error.RuntimeFailure;
                        }, item_value.raw_ptr) };
                        self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], item_value);
                    } else {
                        self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], item_value);
                    }
                },
                .array_set => |value| {
                    const array_value = registers[value.array];
                    const index_value = registers[value.index];
                    if (array_value != .raw_ptr or array_value.raw_ptr == 0 or index_value != .integer or index_value.integer < 0) {
                        self.rememberError("array store requires a valid array handle and index");
                        return error.RuntimeFailure;
                    }
                    const array_ptr: *ArrayObject = @ptrFromInt(array_value.raw_ptr);
                    const index: usize = @intCast(index_value.integer);
                    if (index >= array_ptr.len) {
                        self.rememberError("array index is out of bounds");
                        return error.RuntimeFailure;
                    }
                    self.heap.replaceArrayItem(&array_ptr.items[index], registers[value.src]);
                },
                .array_append => |value| {
                    const array_value = registers[value.array];
                    if (array_value != .raw_ptr or array_value.raw_ptr == 0) {
                        self.rememberError("array append requires a valid array handle");
                        return error.RuntimeFailure;
                    }
                    const array_ptr: *ArrayObject = @ptrFromInt(array_value.raw_ptr);
                    try self.heap.appendArrayItem(array_ptr, registers[value.src]);
                },
                .enum_tag => |value| {
                    const enum_value = registers[value.src];
                    if (enum_value != .raw_ptr or enum_value.raw_ptr == 0) {
                        self.rememberError("enum tag access requires a valid enum value");
                        return error.RuntimeFailure;
                    }
                    const enum_ptr: [*]align(1) const runtime_abi.Value = @ptrFromInt(enum_value.raw_ptr);
                    if (enum_ptr[0] != .integer) {
                        self.rememberError("enum tag slot is not an integer");
                        return error.RuntimeFailure;
                    }
                    self.setSlotOwned(&registers[value.dst], &register_owned[value.dst], enum_ptr[0]);
                },
                .enum_payload => |value| {
                    const enum_value = registers[value.src];
                    if (enum_value != .raw_ptr or enum_value.raw_ptr == 0) {
                        self.rememberError("enum payload access requires a valid enum value");
                        return error.RuntimeFailure;
                    }
                    const enum_ptr: [*]align(1) const runtime_abi.Value = @ptrFromInt(enum_value.raw_ptr);
                    self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], enum_ptr[1]);
                    _ = value.payload_ty;
                },
                .load_indirect => |value| {
                    const ptr = registers[value.ptr];
                    if (ptr != .raw_ptr or ptr.raw_ptr == 0) {
                        self.rememberError("indirect load requires a valid pointer");
                        return error.RuntimeFailure;
                    }
                    if (value.ty.kind == .ffi_struct) {
                        const type_name = value.ty.name orelse {
                            self.rememberError("struct load type is missing a name");
                            return error.RuntimeFailure;
                        };
                        self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], .{ .raw_ptr = try self.resolveStructValuePointer(type_name, ptr.raw_ptr) });
                    } else {
                        const slot_ptr: *runtime_abi.Value = @ptrFromInt(ptr.raw_ptr);
                        self.setSlotBorrowed(&registers[value.dst], &register_owned[value.dst], slot_ptr.*);
                    }
                },
                .store_indirect => |value| {
                    const ptr = registers[value.ptr];
                    if (ptr != .raw_ptr or ptr.raw_ptr == 0) {
                        self.rememberError("indirect store requires a valid pointer");
                        return error.RuntimeFailure;
                    }
                    if (value.ty.kind == .ffi_struct) {
                        const type_name = value.ty.name orelse {
                            self.rememberError("struct store type is missing a name");
                            return error.RuntimeFailure;
                        };
                        const dst_ptr = try self.ensureStructDestinationPointer(module, type_name, ptr.raw_ptr);
                        try self.copyStructValueInto(module, type_name, dst_ptr, registers[value.src]);
                    } else {
                        const slot_ptr: *runtime_abi.Value = @ptrFromInt(ptr.raw_ptr);
                        self.heap.assignBorrowed(slot_ptr, registers[value.src]);
                    }
                },
                .copy_indirect => |value| {
                    const dst_ptr_value = registers[value.dst_ptr];
                    const src_ptr_value = registers[value.src_ptr];
                    if (dst_ptr_value != .raw_ptr or src_ptr_value != .raw_ptr or dst_ptr_value.raw_ptr == 0 or src_ptr_value.raw_ptr == 0) {
                        self.rememberError("struct copy requires valid pointers");
                        return error.RuntimeFailure;
                    }
                    const dst_ptr = try self.ensureStructDestinationPointer(module, value.type_name, dst_ptr_value.raw_ptr);
                    const src_ptr = try self.resolveStructValuePointer(value.type_name, src_ptr_value.raw_ptr);
                    try self.copyStructValueInto(module, value.type_name, dst_ptr, .{ .raw_ptr = src_ptr });
                },
                .branch => |value| {
                    const condition = registers[value.condition];
                    if (condition != .boolean) {
                        self.rememberError("vm branch expects a boolean condition");
                        return error.RuntimeFailure;
                    }
                    pc = try helper_impl.resolveLabelOffset(label_offsets, if (condition.boolean) value.true_label else value.false_label);
                    continue;
                },
                .jump => |value| {
                    pc = try helper_impl.resolveLabelOffset(label_offsets, value.label);
                    continue;
                },
                .label => {},
                .print => |value| try builtins.printValue(writer, module, registers[value.src], value.ty),
                .call_runtime => |value| {
                    const call_args = try helper_impl.collectArgs(self.allocator, registers, value.args);
                    defer self.allocator.free(call_args);
                    const result = try self.runFunctionById(module, value.function_id, call_args, writer, hooks);
                    if (value.dst) |dst| self.setSlotOwned(&registers[dst], &register_owned[dst], result) else self.heap.releaseValue(result);
                },
                .call_native => |value| {
                    const callback = hooks.call_native orelse {
                        self.rememberError("vm native bridge was not installed");
                        return error.RuntimeFailure;
                    };
                    const call_args = try helper_impl.collectArgs(self.allocator, registers, value.args);
                    defer self.allocator.free(call_args);
                    var result = try callback(hooks.context, value.function_id, call_args);
                    result = try self.materializeNativeResult(module, value.return_ty, result);
                    if (value.dst) |dst| self.setSlotOwned(&registers[dst], &register_owned[dst], result) else self.heap.releaseValue(result);
                },
                .call_virtual => |value| {
                    const receiver_value = registers[value.receiver];
                    if (receiver_value != .raw_ptr or receiver_value.raw_ptr == 0) {
                        self.rememberError("virtual method call requires a valid class receiver");
                        return error.RuntimeFailure;
                    }
                    const actual_type_name = self.heap.getStructTypeName(receiver_value.raw_ptr) orelse value.static_type_name;
                    const resolved_method = self.resolveVirtualMethod(module, actual_type_name, value.method_name) orelse {
                        self.rememberError("virtual method could not be resolved on the concrete receiver type");
                        return error.RuntimeFailure;
                    };
                    const adjusted_receiver = if (resolved_method.receiver_offset == 0)
                        receiver_value.raw_ptr
                    else
                        @intFromPtr((@as([*]align(1) runtime_abi.Value, @ptrFromInt(receiver_value.raw_ptr)) + resolved_method.receiver_offset));
                    const explicit_args = try helper_impl.collectArgs(self.allocator, registers, value.args);
                    defer self.allocator.free(explicit_args);
                    var call_args = try self.allocator.alloc(runtime_abi.Value, explicit_args.len + 1);
                    defer self.allocator.free(call_args);
                    call_args[0] = .{ .raw_ptr = adjusted_receiver };
                    @memcpy(call_args[1..], explicit_args);

                    const result = if (module.findFunctionById(resolved_method.function_id) != null)
                        try self.runFunctionById(module, resolved_method.function_id, call_args, writer, hooks)
                    else native_result: {
                        const callback = hooks.call_native orelse {
                            self.rememberError("vm native bridge was not installed");
                            return error.RuntimeFailure;
                        };
                        var native_value = try callback(hooks.context, resolved_method.function_id, call_args);
                        native_value = try self.materializeNativeResult(module, value.return_ty, native_value);
                        break :native_result native_value;
                    };
                    if (value.dst) |dst| self.setSlotOwned(&registers[dst], &register_owned[dst], result) else self.heap.releaseValue(result);
                },
                .call_value => |value| {
                    const callee_value = registers[value.callee];
                    if (callee_value != .raw_ptr) {
                        self.rememberError("indirect call requires a callable function value");
                        return error.RuntimeFailure;
                    }
                    const call_args = try helper_impl.collectArgs(self.allocator, registers, value.args);
                    defer self.allocator.free(call_args);
                    const result = if (self.heap.getClosure(callee_value.raw_ptr)) |closure| closure_call: {
                        runtime_abi.emitExecutionTrace("CALLABLE", "INVOKE_CLOSURE", "raw=0x{x} fn={d} captures={d}", .{ callee_value.raw_ptr, closure.function_id, closure.captures.len });
                        var closure_args = try self.allocator.alloc(runtime_abi.Value, call_args.len + closure.captures.len);
                        defer self.allocator.free(closure_args);
                        @memcpy(closure_args[0..call_args.len], call_args);
                        @memcpy(closure_args[call_args.len..], closure.captures);
                        if (!closure.is_native) {
                            break :closure_call try self.runFunctionById(module, closure.function_id, closure_args, writer, hooks);
                        }
                        const callback = hooks.call_native orelse {
                            self.rememberError("native closure call requires a native call hook");
                            return error.RuntimeFailure;
                        };
                        break :closure_call try callback(hooks.context, closure.function_id, closure_args);
                    } else if (callee_value.raw_ptr <= std.math.maxInt(u32)) direct: {
                        const function_id: u32 = @intCast(callee_value.raw_ptr);
                        if (module.findFunctionById(function_id) != null) {
                            break :direct try self.runFunctionById(module, function_id, call_args, writer, hooks);
                        }
                        const callback = hooks.call_native orelse {
                            self.rememberError("vm native bridge was not installed");
                            return error.RuntimeFailure;
                        };
                        break :direct try callback(hooks.context, function_id, call_args);
                    } else {
                        self.rememberError("indirect call received an unmanaged raw pointer that is not a runtime closure");
                        return error.RuntimeFailure;
                    };
                    if (value.dst) |dst| self.setSlotOwned(&registers[dst], &register_owned[dst], result) else self.heap.releaseValue(result);
                },
                .ret => |value| {
                    const result = if (value.src) |src| registers[src] else runtime_abi.Value{ .void = {} };
                    if (value.src == null or register_owned[value.src.?]) self.heap.retainValue(result);
                    return result;
                },
            }
            pc += 1;
        }
        return .{ .void = {} };
    }

    pub fn rememberError(self: *Vm, message: []const u8) void {
        const length = @min(message.len, self.last_error_buffer.len);
        @memcpy(self.last_error_buffer[0..length], message[0..length]);
        self.last_error_len = length;
    }

    fn setSlotOwned(self: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
        const old = slot.*;
        const old_owned = owned.*;
        slot.* = value;
        owned.* = self.heap.isManagedValue(value);
        if (old_owned) self.heap.releaseValue(old);
    }

    fn setSlotBorrowed(self: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
        const new_owned = self.heap.isManagedValue(value);
        if (new_owned) self.heap.retainValue(value);
        const old = slot.*;
        const old_owned = owned.*;
        slot.* = value;
        owned.* = new_owned;
        if (old_owned) self.heap.releaseValue(old);
    }

    fn setSlotUnmanaged(self: *Vm, slot: *runtime_abi.Value, owned: *bool, value: runtime_abi.Value) void {
        const old = slot.*;
        const old_owned = owned.*;
        slot.* = value;
        owned.* = false;
        if (old_owned) self.heap.releaseValue(old);
    }

    fn releaseTrackedSlots(self: *Vm, slots: []runtime_abi.Value, owned: []const bool) void {
        for (slots, owned) |slot, is_owned| if (is_owned) self.heap.releaseValue(slot);
    }

    fn copyCString(self: *Vm, value: runtime_abi.Value) !runtime_abi.Value {
        if (value != .raw_ptr or value.raw_ptr == 0) return .{ .string = "" };
        const source: [*:0]const u8 = @ptrFromInt(value.raw_ptr);
        const bytes = std.mem.span(source);
        if (bytes.len == 0) return .{ .string = "" };
        const owned = try self.allocator.dupe(u8, bytes);
        errdefer self.allocator.free(owned);
        try self.heap.registerString(owned);
        return .{ .string = owned };
    }

    fn allocateClosure(self: *Vm, registers: []const runtime_abi.Value, function_id: u32, capture_registers: []const u32) !usize {
        const closure = try self.allocator.create(ClosureObject);
        const captures = try self.allocator.alloc(runtime_abi.Value, capture_registers.len);
        for (captures) |*capture| capture.* = .{ .void = {} };
        for (capture_registers, 0..) |reg, index| self.heap.assignBorrowed(&captures[index], registers[reg]);
        closure.* = .{ .function_id = function_id, .captures = captures };
        return self.heap.registerClosure(closure);
    }

    fn allocateStruct(self: *Vm, module: *const bytecode.Module, type_name: []const u8) !usize {
        const type_decl = helper_impl.findType(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        const fields = try self.allocator.alloc(runtime_abi.Value, type_decl.fields.len);
        for (type_decl.fields, 0..) |field_decl, index| {
            fields[index] = try self.zeroValueForType(module, field_decl.ty);
        }
        return self.heap.registerStruct(type_name, fields);
    }

    fn allocateNativeState(self: *Vm, module: *const bytecode.Module, type_name: []const u8, type_id: u64, src_payload: usize) !usize {
        const type_decl = helper_impl.findType(module, type_name) orelse {
            self.rememberError("native state type could not be resolved");
            return error.RuntimeFailure;
        };
        const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(src_payload);
        const native_payload = try self.allocator.alloc(runtime_abi.BridgeValue, type_decl.fields.len);
        for (type_decl.fields, 0..) |field_decl, index| {
            var native_value = src_ptr[index];
            if (field_decl.ty.kind == .ffi_struct and native_value == .raw_ptr and native_value.raw_ptr != 0) {
                native_value = .{ .raw_ptr = try self.copyStructToNativeLayout(
                    module,
                    field_decl.ty.name orelse return error.RuntimeFailure,
                    native_value.raw_ptr,
                ) };
            }
            native_payload[index] = runtime_abi.bridgeValueFromValue(native_value);
        }

        const box = try self.allocator.create(NativeStateBox);
        box.* = .{
            .type_id = type_id,
            .payload = @intFromPtr(native_payload.ptr),
            .runtime_payload = 0,
        };
        return @intFromPtr(box);
    }

    fn recoverNativeState(self: *Vm, module: *const bytecode.Module, type_name: []const u8, state_token: usize, expected_type_id: u64) !usize {
        const box: *NativeStateBox = @ptrFromInt(state_token);
        if (box.type_id != expected_type_id) {
            self.rememberError("nativeRecover used a userdata token for the wrong state type");
            return error.RuntimeFailure;
        }
        if (box.runtime_payload == 0 and box.payload != 0) {
            box.runtime_payload = try self.materializeNativeStatePayload(module, type_name, box.payload);
        }
        if (box.runtime_payload == 0) {
            self.rememberError("nativeRecover used a userdata token with no state payload");
            return error.RuntimeFailure;
        }
        return box.runtime_payload;
    }

    fn materializeNativeStatePayload(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_payload_ptr: usize) !usize {
        const type_decl = helper_impl.findType(module, type_name) orelse {
            self.rememberError("native state type could not be resolved");
            return error.RuntimeFailure;
        };
        const native_payload: [*]const runtime_abi.BridgeValue = @ptrFromInt(native_payload_ptr);
        const runtime_payload = try self.allocator.alloc(runtime_abi.BridgeValue, type_decl.fields.len);
        for (type_decl.fields, 0..) |field_decl, index| {
            var value = runtime_abi.bridgeValueToValue(native_payload[index]);
            if (field_decl.ty.kind == .ffi_struct and value == .raw_ptr and value.raw_ptr != 0) {
                value = .{ .raw_ptr = try self.copyStructFromNativeLayout(
                    module,
                    field_decl.ty.name orelse return error.RuntimeFailure,
                    value.raw_ptr,
                ) };
            }
            runtime_payload[index] = runtime_abi.bridgeValueFromValue(value);
        }
        return @intFromPtr(runtime_payload.ptr);
    }

    fn zeroValueForType(self: *Vm, module: *const bytecode.Module, value_type: bytecode.TypeRef) anyerror!runtime_abi.Value {
        return switch (value_type.kind) {
            .void => .{ .void = {} },
            .integer => .{ .integer = 0 },
            .float => .{ .float = 0.0 },
            .string => .{ .string = "" },
            .boolean => .{ .boolean = false },
            .construct_any, .array, .raw_ptr, .enum_instance => .{ .raw_ptr = 0 },
            .ffi_struct => blk: {
                const nested_name = value_type.name orelse {
                    self.rememberError("struct field type is missing a name");
                    return error.RuntimeFailure;
                };
                break :blk .{ .raw_ptr = try self.allocateStruct(module, nested_name) };
            },
        };
    }

    fn allocateArray(self: *Vm, len: usize) !usize {
        const object = try self.allocator.create(ArrayObject);
        const items = try self.allocator.alloc(runtime_abi.BridgeValue, if (len == 0) 1 else len);
        for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
        object.* = .{
            .len = len,
            .items = items.ptr,
        };
        return self.heap.registerArray(object);
    }

    fn arrayElementType(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef) !bytecode.TypeRef {
        _ = self;
        const name = array_ty.name orelse return .{ .kind = .raw_ptr };
        return resolveTypeText(module, name);
    }

    fn copyValueToNativeLayout(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        return switch (ty.kind) {
            .ffi_struct => blk: {
                if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
                break :blk .{ .raw_ptr = try self.copyStructToNativeLayout(module, ty.name orelse {
                    self.rememberError("array element struct type is missing a name");
                    return error.RuntimeFailure;
                }, value.raw_ptr) };
            },
            .array => blk: {
                if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
                break :blk .{ .raw_ptr = try self.copyArrayToNativeLayout(module, ty, value.raw_ptr) };
            },
            else => value,
        };
    }

    fn resolveTypeText(module: *const bytecode.Module, text: []const u8) bytecode.TypeRef {
        if (std.mem.eql(u8, text, "Void")) return .{ .kind = .void };
        if (std.mem.eql(u8, text, "Bool")) return .{ .kind = .boolean, .name = "Bool" };
        if (std.mem.eql(u8, text, "String")) return .{ .kind = .string };
        if (std.mem.eql(u8, text, "Float") or std.mem.eql(u8, text, "F64")) return .{ .kind = .float, .name = "F64" };
        if (std.mem.eql(u8, text, "F32")) return .{ .kind = .float, .name = "F32" };
        if (std.mem.eql(u8, text, "Int") or std.mem.eql(u8, text, "I64")) return .{ .kind = .integer, .name = "I64" };
        if (std.mem.eql(u8, text, "I8") or std.mem.eql(u8, text, "I16") or std.mem.eql(u8, text, "I32") or
            std.mem.eql(u8, text, "U8") or std.mem.eql(u8, text, "U16") or std.mem.eql(u8, text, "U32"))
        {
            return .{ .kind = .integer, .name = text };
        }
        if (std.mem.eql(u8, text, "RawPtr") or std.mem.endsWith(u8, text, "_ptr")) return .{ .kind = .raw_ptr, .name = text };
        if (text.len >= 2 and text[0] == '[' and text[text.len - 1] == ']') return .{ .kind = .array, .name = text[1 .. text.len - 1] };
        if (helper_impl.findType(module, text) != null) return .{ .kind = .ffi_struct, .name = text };
        for (module.enums) |enum_decl| {
            if (std.mem.eql(u8, enum_decl.name, text)) return .{ .kind = .enum_instance, .name = text };
        }
        return .{ .kind = .raw_ptr, .name = text };
    }

    fn copyValueFromNativeLayout(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        return switch (ty.kind) {
            .ffi_struct => blk: {
                if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
                break :blk .{ .raw_ptr = try self.copyStructFromNativeLayout(module, ty.name orelse {
                    self.rememberError("array element struct type is missing a name");
                    return error.RuntimeFailure;
                }, value.raw_ptr) };
            },
            .array => blk: {
                if (value != .raw_ptr or value.raw_ptr == 0) break :blk .{ .raw_ptr = 0 };
                break :blk .{ .raw_ptr = try self.copyArrayFromNativeLayout(module, ty, value.raw_ptr) };
            },
            else => value,
        };
    }

    fn allocateEnum(self: *Vm, enum_type_name: []const u8, registers: []const runtime_abi.Value, discriminant: u32, payload_src: ?u32) !usize {
        const slots = try self.allocator.alloc(runtime_abi.Value, 2);
        slots[0] = .{ .integer = @as(i64, @intCast(discriminant)) };
        slots[1] = if (payload_src) |src| registers[src] else .{ .void = {} };
        return self.heap.registerStruct(enum_type_name, slots);
    }

    fn typeFieldCount(self: *Vm, module: *const bytecode.Module, type_name: []const u8) ?usize {
        _ = self;
        const type_decl = helper_impl.findType(module, type_name) orelse return null;
        return type_decl.fields.len;
    }

    fn managedStructTypeName(self: *Vm, ptr: usize) ?[]const u8 {
        return self.heap.getStructTypeName(ptr);
    }

    fn isManagedStructPointer(self: *Vm, ptr: usize) bool {
        return self.managedStructTypeName(ptr) != null;
    }

    fn resolveStructValuePointer(self: *Vm, expected_type_name: []const u8, ptr: usize) !usize {
        if (ptr == 0) {
            self.rememberError("struct value pointer is null");
            return error.RuntimeFailure;
        }
        if (self.isManagedStructPointer(ptr)) return ptr;
        _ = expected_type_name;

        const slot_ptr: *const runtime_abi.Value = @ptrFromInt(ptr);
        const value = slot_ptr.*;
        if (value != .raw_ptr) {
            self.rememberError("struct pointer slot does not contain a struct value");
            return error.RuntimeFailure;
        }
        if (value.raw_ptr == 0) return 0;
        if (!self.isManagedStructPointer(value.raw_ptr)) {
            self.rememberError("struct pointer slot does not contain a managed struct value");
            return error.RuntimeFailure;
        }
        return value.raw_ptr;
    }

    fn ensureStructDestinationPointer(self: *Vm, module: *const bytecode.Module, expected_type_name: []const u8, ptr: usize) !usize {
        if (ptr == 0) {
            self.rememberError("struct destination pointer is null");
            return error.RuntimeFailure;
        }
        if (self.isManagedStructPointer(ptr)) return ptr;
        if (self.managedStructTypeName(ptr)) |_| {
            self.rememberError("struct destination type does not match the expected type");
            return error.RuntimeFailure;
        }

        const slot_ptr: *runtime_abi.Value = @ptrFromInt(ptr);
        if (slot_ptr.* == .raw_ptr and slot_ptr.raw_ptr != 0) {
            if (!self.isManagedStructPointer(slot_ptr.raw_ptr)) {
                self.rememberError("struct destination slot does not contain a managed struct value");
                return error.RuntimeFailure;
            }
            return slot_ptr.raw_ptr;
        }

        const old = slot_ptr.*;
        slot_ptr.* = .{ .raw_ptr = try self.allocateStruct(module, expected_type_name) };
        self.heap.releaseValue(old);
        return slot_ptr.raw_ptr;
    }

    fn copyStructValueInto(
        self: *Vm,
        module: *const bytecode.Module,
        type_name: []const u8,
        dst_raw_ptr: usize,
        src_value: runtime_abi.Value,
    ) !void {
        const type_decl = helper_impl.findType(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        const dst_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(dst_raw_ptr);
        if (src_value == .raw_ptr and src_value.raw_ptr != 0) {
            const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(src_value.raw_ptr);
            try self.copyStruct(module, type_decl, dst_ptr, src_ptr);
            return;
        }
        if (src_value == .raw_ptr and src_value.raw_ptr == 0) {
            const default_ptr = try self.allocateStruct(module, type_name);
            defer self.heap.releaseValue(.{ .raw_ptr = default_ptr });
            const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(default_ptr);
            try self.copyStruct(module, type_decl, dst_ptr, src_ptr);
            return;
        }
        self.rememberError("struct copy source must be a struct value");
        return error.RuntimeFailure;
    }

    fn resolveVirtualMethod(
        self: *Vm,
        module: *const bytecode.Module,
        actual_type_name: []const u8,
        method_name: []const u8,
    ) ?bytecode.MethodMember {
        _ = self;
        const type_decl = helper_impl.findType(module, actual_type_name) orelse return null;
        for (type_decl.methods) |method_decl| {
            if (std.mem.eql(u8, method_decl.name, method_name)) return method_decl;
        }
        return null;
    }

    fn copyStruct(
        self: *Vm,
        module: *const bytecode.Module,
        type_decl: bytecode.TypeDecl,
        dst_ptr: [*]align(1) runtime_abi.Value,
        src_ptr: [*]align(1) runtime_abi.Value,
    ) !void {
        for (type_decl.fields, 0..) |field_decl, index| {
            if (field_decl.ty.kind == .ffi_struct) {
                const nested_name = field_decl.ty.name orelse {
                    self.rememberError("struct field type is missing a name");
                    return error.RuntimeFailure;
                };
                const nested_type = helper_impl.findType(module, nested_name) orelse {
                    self.rememberError("struct type could not be resolved");
                    return error.RuntimeFailure;
                };
                if (src_ptr[index] != .raw_ptr) {
                    self.rememberError("nested struct copy source must be a pointer");
                    return error.RuntimeFailure;
                }
                if (dst_ptr[index] != .raw_ptr or dst_ptr[index].raw_ptr == 0) {
                    const old = dst_ptr[index];
                    dst_ptr[index] = .{ .raw_ptr = try self.allocateStruct(module, nested_name) };
                    self.heap.releaseValue(old);
                }
                if (src_ptr[index].raw_ptr == 0) {
                    // Treat null nested pointers as zero/default nested structs.
                    const old = dst_ptr[index];
                    dst_ptr[index] = .{ .raw_ptr = try self.allocateStruct(module, nested_name) };
                    self.heap.releaseValue(old);
                    continue;
                }
                const nested_dst: [*]align(1) runtime_abi.Value = @ptrFromInt(dst_ptr[index].raw_ptr);
                const nested_src: [*]align(1) runtime_abi.Value = @ptrFromInt(src_ptr[index].raw_ptr);
                try self.copyStruct(module, nested_type, nested_dst, nested_src);
            } else {
                self.heap.retainValue(src_ptr[index]);
                const old = dst_ptr[index];
                dst_ptr[index] = src_ptr[index];
                self.heap.releaseValue(old);
            }
        }
    }

    fn materializeNativeResult(
        self: *Vm,
        module: *const bytecode.Module,
        return_ty: bytecode.TypeRef,
        value: runtime_abi.Value,
    ) !runtime_abi.Value {
        if (return_ty.kind != .ffi_struct) return value;
        if (value != .raw_ptr or value.raw_ptr == 0) {
            self.rememberError("native struct result requires a valid pointer");
            return error.RuntimeFailure;
        }
        return .{ .raw_ptr = try self.copyStructFromNativeLayout(module, return_ty.name orelse {
            self.rememberError("native struct result is missing a type name");
            return error.RuntimeFailure;
        }, value.raw_ptr) };
    }

    pub fn copyStructFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) anyerror!usize {
        const type_decl = helper_impl.findType(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        const fields = try self.allocator.alloc(runtime_abi.Value, type_decl.fields.len);
        for (type_decl.fields, 0..) |field_decl, index| {
            fields[index] = try helper_impl.readNativeFieldValue(self, module, type_name, field_decl, index, native_ptr);
        }
        return self.heap.registerStruct(type_name, fields);
    }

    fn copyStructFromNativeLayoutInto(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) anyerror!void {
        const type_decl = helper_impl.findType(module, type_name) orelse {
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
                        self.heap.releaseValue(old);
                    }
                    try self.copyStructFromNativeLayoutInto(module, nested_name, fields[index].raw_ptr, address);
                },
                .array => {
                    const native_array_ptr = (@as(*const usize, @ptrFromInt(address))).*;
                    if (native_array_ptr == 0) {
                        const old = fields[index];
                        fields[index] = .{ .raw_ptr = 0 };
                        self.heap.releaseValue(old);
                        continue;
                    }
                    if (fields[index] == .raw_ptr and fields[index].raw_ptr != 0) {
                        try self.syncArrayFromNativeLayout(module, field_decl.ty, fields[index].raw_ptr, native_array_ptr);
                        continue;
                    }
                    const old = fields[index];
                    fields[index] = .{ .raw_ptr = try self.copyArrayFromNativeLayout(module, field_decl.ty, native_array_ptr) };
                    self.heap.releaseValue(old);
                },
                else => {
                    const old = fields[index];
                    fields[index] = try helper_impl.readNativeFieldValue(self, module, type_name, field_decl, index, native_ptr);
                    self.heap.releaseValue(old);
                },
            }
        }
    }

    fn copyStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) anyerror!usize {
        const layout = try native_layout.structLayout(module, type_name);
        const word_count = @max(1, std.math.divCeil(usize, layout.size, @sizeOf(u64)) catch unreachable);
        const words = try self.allocator.alloc(u64, word_count);
        @memset(std.mem.sliceAsBytes(words), 0);
        try self.copyStructToNativeLayoutInto(module, type_name, runtime_ptr, @intFromPtr(words.ptr));
        return @intFromPtr(words.ptr);
    }

    pub fn copyStructToNativeLayoutInto(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) anyerror!void {
        const type_decl = helper_impl.findType(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        const fields: [*]align(1) const runtime_abi.Value = @ptrFromInt(runtime_ptr);
        for (type_decl.fields, 0..) |field_decl, index| {
            const offset = try native_layout.fieldOffset(module, type_name, index);
            try helper_impl.writeNativeFieldValue(self, module, field_decl.ty, fields[index], native_ptr + offset);
        }
    }
};

test "executes nested runtime calls" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{},
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .register_count = 1,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .call_runtime = .{ .function_id = 1 } },
                    .{ .ret = .{ .src = null } },
                },
            },
            .{
                .id = 1,
                .name = "helper",
                .param_count = 0,
                .register_count = 1,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .const_int = .{ .dst = 0, .value = 42 } },
                    .{ .print = .{ .src = 0, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .ret = .{ .src = null } },
                },
            },
        },
        .entry_function_id = 0,
    };

    var buffer: [128]u8 = undefined;
    var stream = std.Io.Writer.fixed(&buffer);
    try vm.runMain(&module, &stream);
    try std.testing.expectEqualStrings("42\n", stream.buffered());
}

test "materializes native closure struct captures using external metadata" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const backend_fields = [_]bytecode.Field{
        .{ .name = "id", .ty = .{ .kind = .integer, .name = "U32" } },
    };
    const pipeline_fields = [_]bytecode.Field{
        .{ .name = "handle", .ty = .{ .kind = .ffi_struct, .name = "BackendPipelineHandle" } },
        .{ .name = "id", .ty = .{ .kind = .integer, .name = "U32" } },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "BackendPipelineHandle", .fields = @constCast(&backend_fields) },
        .{ .name = "RenderPipeline", .fields = @constCast(&pipeline_fields) },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = &.{},
        .entry_function_id = null,
    };

    const native_pipeline = try arena.allocator().alloc(u8, 8);
    @memset(native_pipeline, 0);
    (@as(*u32, @ptrFromInt(@intFromPtr(native_pipeline.ptr)))).* = 65537;
    (@as(*u32, @ptrFromInt(@intFromPtr(native_pipeline.ptr) + 4))).* = 65537;

    const NativeClosure = extern struct {
        function_id: i64,
        capture_count: i64,
        captures: [1]runtime_abi.BridgeValue,
    };
    const native_closure = try arena.allocator().create(NativeClosure);
    native_closure.* = .{
        .function_id = 457,
        .capture_count = 1,
        .captures = .{runtime_abi.bridgeValueFromValue(.{ .raw_ptr = @intFromPtr(native_pipeline.ptr) })},
    };
    const capture_types = [_]bytecode.TypeRef{
        .{ .kind = .ffi_struct, .name = "RenderPipeline" },
    };

    const closure_ptr = try vm.materializeNativeClosure(&module, @intFromPtr(native_closure), &capture_types);
    const closure = vm.heap.getClosure(closure_ptr) orelse return error.TestExpectedEqual;
    try std.testing.expectEqual(@as(usize, 1), closure.captures.len);
    try std.testing.expect(closure.captures[0] == .raw_ptr);

    const pipeline_values: [*]const runtime_abi.Value = @ptrFromInt(closure.captures[0].raw_ptr);
    try std.testing.expect(pipeline_values[0] == .raw_ptr);
    try std.testing.expect(pipeline_values[1] == .integer);
    try std.testing.expectEqual(@as(i64, 65537), pipeline_values[1].integer);

    const handle_values: [*]const runtime_abi.Value = @ptrFromInt(pipeline_values[0].raw_ptr);
    try std.testing.expect(handle_values[0] == .integer);
    try std.testing.expectEqual(@as(i64, 65537), handle_values[0].integer);
}

test "prints struct values" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{
            .{
                .name = "Color",
                .fields = &.{
                    .{ .name = "r", .ty = .{ .kind = .integer, .name = "I64" } },
                    .{ .name = "g", .ty = .{ .kind = .integer, .name = "I64" } },
                    .{ .name = "b", .ty = .{ .kind = .integer, .name = "I64" } },
                },
            },
        },
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .register_count = 8,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .alloc_struct = .{ .dst = 0, .type_name = "Color" } },
                    .{ .field_ptr = .{ .dst = 1, .base = 0, .base_type_name = "Color", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .const_int = .{ .dst = 2, .value = 255 } },
                    .{ .store_indirect = .{ .ptr = 1, .src = 2, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .field_ptr = .{ .dst = 3, .base = 0, .base_type_name = "Color", .field_index = 1, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .const_int = .{ .dst = 4, .value = 0 } },
                    .{ .store_indirect = .{ .ptr = 3, .src = 4, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .field_ptr = .{ .dst = 5, .base = 0, .base_type_name = "Color", .field_index = 2, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .const_int = .{ .dst = 6, .value = 0 } },
                    .{ .store_indirect = .{ .ptr = 5, .src = 6, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .print = .{ .src = 0, .ty = .{ .kind = .ffi_struct, .name = "Color" } } },
                    .{ .ret = .{ .src = null } },
                },
            },
        },
        .entry_function_id = 0,
    };

    var buffer: [256]u8 = undefined;
    var stream = std.Io.Writer.fixed(&buffer);
    try vm.runMain(&module, &stream);
    try std.testing.expectEqualStrings("Color(r: 255, g: 0, b: 0)\n", stream.buffered());
}

test "resolves function constants through hooks" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{},
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .register_count = 1,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .const_function = .{ .dst = 0, .function_id = 7 } },
                    .{ .ret = .{ .src = 0 } },
                },
            },
        },
        .entry_function_id = 0,
    };

    const result = try vm.runFunctionById(&module, 0, &.{}, std.io.null_writer, .{
        .resolve_function = struct {
            fn resolve(_: ?*anyopaque, function_id: u32) !usize {
                return 0x1000 + function_id;
            }
        }.resolve,
    });

    try std.testing.expectEqual(@as(usize, 0x1007), result.raw_ptr);
}

test "copies struct arguments by value for runtime calls" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{
            .{
                .name = "Pair",
                .fields = &.{
                    .{ .name = "left", .ty = .{ .kind = .integer, .name = "I64" } },
                    .{ .name = "right", .ty = .{ .kind = .integer, .name = "I64" } },
                },
            },
        },
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .register_count = 6,
                .local_count = 1,
                .local_types = &.{.{ .kind = .ffi_struct, .name = "Pair" }},
                .instructions = &.{
                    .{ .alloc_struct = .{ .dst = 0, .type_name = "Pair" } },
                    .{ .field_ptr = .{ .dst = 1, .base = 0, .base_type_name = "Pair", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .const_int = .{ .dst = 2, .value = 1 } },
                    .{ .store_indirect = .{ .ptr = 1, .src = 2, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .store_local = .{ .local = 0, .src = 0 } },
                    .{ .call_runtime = .{ .function_id = 1, .args = &.{0} } },
                    .{ .field_ptr = .{ .dst = 3, .base = 0, .base_type_name = "Pair", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .load_indirect = .{ .dst = 4, .ptr = 3, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .ret = .{ .src = 4 } },
                },
            },
            .{
                .id = 1,
                .name = "mutate",
                .param_count = 1,
                .register_count = 3,
                .local_count = 1,
                .local_types = &.{.{ .kind = .ffi_struct, .name = "Pair" }},
                .instructions = &.{
                    .{ .load_local = .{ .dst = 0, .local = 0 } },
                    .{ .field_ptr = .{ .dst = 1, .base = 0, .base_type_name = "Pair", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .const_int = .{ .dst = 2, .value = 99 } },
                    .{ .store_indirect = .{ .ptr = 1, .src = 2, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .ret = .{ .src = null } },
                },
            },
        },
        .entry_function_id = 0,
    };

    const result = try vm.runFunctionById(&module, 0, &.{}, std.io.null_writer, .{});
    try std.testing.expectEqual(@as(i64, 1), result.integer);
}

test "copyStruct tolerates null nested ffi struct pointers" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{
            .{
                .name = "Child",
                .fields = &.{.{ .name = "x", .ty = .{ .kind = .integer, .name = "I64" } }},
            },
            .{
                .name = "Parent",
                .fields = &.{.{ .name = "child", .ty = .{ .kind = .ffi_struct, .name = "Child" } }},
            },
        },
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .register_count = 7,
                .local_count = 1,
                .local_types = &.{.{ .kind = .ffi_struct, .name = "Parent" }},
                .instructions = &.{
                    .{ .alloc_struct = .{ .dst = 0, .type_name = "Parent" } },
                    .{ .field_ptr = .{ .dst = 1, .base = 0, .base_type_name = "Parent", .field_index = 0, .field_ty = .{ .kind = .ffi_struct, .name = "Child" } } },
                    .{ .const_ptr = .{ .dst = 2, .value = 0 } },
                    .{ .store_indirect = .{ .ptr = 1, .src = 2, .ty = .{ .kind = .ffi_struct, .name = "Child" } } },
                    .{ .call_runtime = .{ .function_id = 1, .args = &.{0} } },
                    .{ .ret = .{ .src = null } },
                },
            },
            .{
                .id = 1,
                .name = "touch",
                .param_count = 1,
                .register_count = 4,
                .local_count = 1,
                .local_types = &.{.{ .kind = .ffi_struct, .name = "Parent" }},
                .instructions = &.{
                    .{ .load_local = .{ .dst = 0, .local = 0 } },
                    .{ .field_ptr = .{ .dst = 1, .base = 0, .base_type_name = "Parent", .field_index = 0, .field_ty = .{ .kind = .ffi_struct, .name = "Child" } } },
                    .{ .load_indirect = .{ .dst = 2, .ptr = 1, .ty = .{ .kind = .ffi_struct, .name = "Child" } } },
                    .{ .field_ptr = .{ .dst = 3, .base = 2, .base_type_name = "Child", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .const_int = .{ .dst = 2, .value = 7 } },
                    .{ .store_indirect = .{ .ptr = 3, .src = 2, .ty = .{ .kind = .integer, .name = "I64" } } },
                    .{ .ret = .{ .src = null } },
                },
            },
        },
        .entry_function_id = 0,
    };

    try vm.runMain(&module, std.io.null_writer);
}

test "construct any values survive nested runtime calls without leaking" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const any_widget = bytecode.TypeRef{
        .kind = .construct_any,
        .name = "any Widget",
        .construct_constraint = .{ .construct_name = "Widget" },
    };
    const module = bytecode.Module{
        .constructs = &.{.{ .name = "Widget" }},
        .construct_implementations = &.{
            .{ .type_name = "Button", .construct_constraint = .{ .construct_name = "Widget" }, .fields = &.{}, .has_content = false, .lifecycle_hooks = &.{} },
            .{ .type_name = "Label", .construct_constraint = .{ .construct_name = "Widget" }, .fields = &.{}, .has_content = false, .lifecycle_hooks = &.{} },
        },
        .types = &.{
            .{ .name = "Button", .fields = &.{} },
            .{ .name = "Label", .fields = &.{} },
        },
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .register_count = 2,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .alloc_struct = .{ .dst = 0, .type_name = "Button" } },
                    .{ .call_runtime = .{ .function_id = 1, .args = &.{0} } },
                    .{ .alloc_struct = .{ .dst = 0, .type_name = "Label" } },
                    .{ .call_runtime = .{ .function_id = 1, .args = &.{0} } },
                    .{ .ret = .{ .src = null } },
                },
            },
            .{
                .id = 1,
                .name = "forward",
                .param_count = 1,
                .return_type = any_widget,
                .register_count = 2,
                .local_count = 1,
                .local_types = &.{any_widget},
                .instructions = &.{
                    .{ .load_local = .{ .dst = 0, .local = 0 } },
                    .{ .call_runtime = .{ .function_id = 2, .args = &.{0}, .dst = 1 } },
                    .{ .ret = .{ .src = 1 } },
                },
            },
            .{
                .id = 2,
                .name = "identity",
                .param_count = 1,
                .return_type = any_widget,
                .register_count = 1,
                .local_count = 1,
                .local_types = &.{any_widget},
                .instructions = &.{
                    .{ .load_local = .{ .dst = 0, .local = 0 } },
                    .{ .ret = .{ .src = 0 } },
                },
            },
        },
        .entry_function_id = 0,
    };

    try vm.runMain(&module, std.io.null_writer);
    try std.testing.expectEqual(@as(usize, 0), vm.heap.count());
}

test "native state recovery mutates persistent payload" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{.{
            .name = "CounterState",
            .fields = &.{.{ .name = "count", .ty = .{ .kind = .integer, .name = "I64" } }},
        }},
        .functions = &.{.{
            .id = 0,
            .name = "main",
            .param_count = 0,
            .register_count = 9,
            .local_count = 0,
            .local_types = &.{},
            .instructions = &.{
                .{ .alloc_struct = .{ .dst = 0, .type_name = "CounterState" } },
                .{ .alloc_native_state = .{ .dst = 1, .src = 0, .type_name = "CounterState", .type_id = 77 } },
                .{ .recover_native_state = .{ .dst = 2, .state = 1, .type_name = "CounterState", .type_id = 77 } },
                .{ .field_ptr = .{ .dst = 3, .base = 2, .base_type_name = "CounterState", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .const_int = .{ .dst = 4, .value = 9 } },
                .{ .store_indirect = .{ .ptr = 3, .src = 4, .ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .recover_native_state = .{ .dst = 5, .state = 1, .type_name = "CounterState", .type_id = 77 } },
                .{ .field_ptr = .{ .dst = 6, .base = 5, .base_type_name = "CounterState", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .load_indirect = .{ .dst = 7, .ptr = 6, .ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .ret = .{ .src = 7 } },
            },
        }},
        .entry_function_id = 0,
    };

    const result = try vm.runFunctionById(&module, 0, &.{}, std.io.null_writer, .{});
    try std.testing.expectEqual(@as(i64, 9), result.integer);
}

test "native state recovery validates the expected type id" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = &.{.{
            .name = "CounterState",
            .fields = &.{.{ .name = "count", .ty = .{ .kind = .integer, .name = "I64" } }},
        }},
        .functions = &.{.{
            .id = 0,
            .name = "main",
            .param_count = 0,
            .register_count = 3,
            .local_count = 0,
            .local_types = &.{},
            .instructions = &.{
                .{ .alloc_struct = .{ .dst = 0, .type_name = "CounterState" } },
                .{ .alloc_native_state = .{ .dst = 1, .src = 0, .type_name = "CounterState", .type_id = 77 } },
                .{ .recover_native_state = .{ .dst = 2, .state = 1, .type_name = "CounterState", .type_id = 88 } },
                .{ .ret = .{ .src = null } },
            },
        }},
        .entry_function_id = 0,
    };

    try std.testing.expectError(error.RuntimeFailure, vm.runFunctionById(&module, 0, &.{}, std.io.null_writer, .{}));
    try std.testing.expect(std.mem.indexOf(u8, vm.lastError().?, "wrong state type") != null);
}
