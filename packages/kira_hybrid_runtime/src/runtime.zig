const std = @import("std");
const bytecode = @import("kira_bytecode");
const hybrid = @import("kira_hybrid_definition");
const native_bridge = @import("kira_native_bridge");
const runtime_abi = @import("kira_runtime_abi");
const binder = @import("binder.zig");
const vm_runtime = @import("kira_vm_runtime");

pub const HybridRuntime = struct {
    allocator: std.mem.Allocator,
    manifest: hybrid.HybridModuleManifest,
    module: bytecode.Module,
    vm: vm_runtime.Vm,
    bridge: native_bridge.NativeBridge,
    pending_callback_return_values: std.ArrayListUnmanaged(runtime_abi.Value) = .empty,
    pending_callback_native_structs: std.ArrayListUnmanaged(NativeStructReturn) = .empty,

    pub fn init(allocator: std.mem.Allocator, manifest: hybrid.HybridModuleManifest) !HybridRuntime {
        var bridge = native_bridge.NativeBridge.init(allocator);
        const descriptors = try buildRuntimeDescriptors(allocator, manifest);
        try binder.bindHybridSymbols(&bridge, manifest.native_library_path, descriptors);

        return .{
            .allocator = allocator,
            .manifest = manifest,
            .module = try bytecode.Module.readFromFile(allocator, manifest.bytecode_path),
            .vm = vm_runtime.Vm.init(allocator),
            .bridge = bridge,
        };
    }

    pub fn initFromCurrentProcess(allocator: std.mem.Allocator, manifest: hybrid.HybridModuleManifest) !HybridRuntime {
        var bridge = native_bridge.NativeBridge.init(allocator);
        const descriptors = try buildRuntimeDescriptors(allocator, manifest);
        try binder.bindHybridSymbolsInSelf(&bridge, descriptors);

        return .{
            .allocator = allocator,
            .manifest = manifest,
            .module = try bytecode.Module.readFromFile(allocator, manifest.bytecode_path),
            .vm = vm_runtime.Vm.init(allocator),
            .bridge = bridge,
        };
    }

    pub fn deinit(self: *HybridRuntime) void {
        self.cleanupPendingCallbackReturns();
        self.pending_callback_return_values.deinit(self.allocator);
        self.pending_callback_native_structs.deinit(self.allocator);
        self.vm.deinit();
        self.bridge.deinit();
    }

    pub fn run(self: *HybridRuntime) !void {
        try self.runWithWriter(DirectStdoutWriter{});
    }

    pub fn runWithWriter(self: *HybridRuntime, writer: anytype) !void {
        const Context = RuntimeContext(@TypeOf(writer));
        var runtime_context = Context{
            .runtime = self,
            .writer = writer,
        };
        native_bridge.installRuntimeInvoker(&runtime_context, runtimeInvoke(Context));
        defer native_bridge.clearRuntimeInvoker();

        switch (self.manifest.entry_execution) {
            .runtime => try self.invokeRuntime(&runtime_context, self.manifest.entry_function_id, &.{}, null),
            .native => _ = try callNativeFunction(self, self.manifest.entry_function_id, &.{}),
            .inherited => unreachable,
        }
    }

    fn invokeRuntime(self: *HybridRuntime, context: anytype, function_id: u32, args: []const runtime_abi.BridgeValue, out_result: ?*runtime_abi.BridgeValue) !void {
        const function_decl = self.module.findFunctionById(function_id) orelse return error.UnknownFunction;
        runtime_abi.emitExecutionTrace("CALLBACK", "ENTER", "native->runtime fn={s}({d}) args={d}", .{
            function_decl.name,
            function_id,
            args.len,
        });
        const runtime_args = try self.allocator.alloc(runtime_abi.Value, args.len);
        defer self.allocator.free(runtime_args);
        const materialized_args = try self.allocator.alloc(bool, args.len);
        defer self.allocator.free(materialized_args);
        @memset(materialized_args, false);
        defer {
            for (materialized_args, 0..) |materialized, index| {
                if (materialized) self.vm.releaseManagedValue(runtime_args[index]);
            }
        }
        const native_arg_ptrs = try self.allocator.alloc(usize, args.len);
        defer self.allocator.free(native_arg_ptrs);
        @memset(native_arg_ptrs, 0);
        for (args, 0..) |arg, index| {
            runtime_args[index] = runtime_abi.bridgeValueToValue(arg);
            if (index >= function_decl.param_count) continue;
            const local_ty = function_decl.local_types[index];
            runtime_abi.emitExecutionTrace("CALLBACK", "ARGTYPE", "fn={d} arg={d} kind={s} name={s} raw=0x{x}", .{
                function_id,
                index,
                @tagName(local_ty.kind),
                local_ty.name orelse "<none>",
                if (runtime_args[index] == .raw_ptr) runtime_args[index].raw_ptr else 0,
            });
            if (local_ty.kind == .raw_ptr and local_ty.name != null and isCallbackTypeName(local_ty.name.?) and runtime_args[index] == .raw_ptr and runtime_args[index].raw_ptr > std.math.maxInt(u32)) {
                if (self.vm.heap.getClosure(runtime_args[index].raw_ptr) != null) {
                    // Already a managed runtime closure pointer; do not reinterpret native memory.
                    continue;
                }
                const native_closure_ptr = runtime_abi.untagNativeClosurePointer(runtime_args[index].raw_ptr);
                const native_function_id: i64 = (@as(*const i64, @ptrFromInt(native_closure_ptr))).*;
                const runtime_present = if (native_function_id >= 0 and native_function_id <= std.math.maxInt(u32)) self.module.findFunctionById(@intCast(native_function_id)) != null else false;
                const manifest_function = if (native_function_id >= 0 and native_function_id <= std.math.maxInt(u32)) findFunction(self.manifest.functions, @intCast(native_function_id)) else null;
                runtime_abi.emitExecutionTrace("CALLBACK", "CLOSURE_ARG", "fn={d} arg={d} closure_fn={d} runtime_present={d} manifest_exec={s}", .{
                    function_id,
                    index,
                    native_function_id,
                    if (runtime_present) @as(i32, 1) else @as(i32, 0),
                    if (manifest_function) |item| @tagName(item.execution) else "<missing>",
                });
                const capture_types = if (manifest_function) |item|
                    try nativeClosureCaptureTypes(self.allocator, native_closure_ptr, item)
                else
                    null;
                defer if (capture_types) |items| self.allocator.free(items);
                runtime_args[index] = .{ .raw_ptr = try self.vm.materializeNativeClosure(&self.module, native_closure_ptr, capture_types) };
                materialized_args[index] = true;
                continue;
            }
            if (local_ty.kind != .ffi_struct or runtime_args[index] != .raw_ptr or runtime_args[index].raw_ptr == 0) continue;
            native_arg_ptrs[index] = runtime_args[index].raw_ptr;
            runtime_args[index] = .{ .raw_ptr = try self.vm.materializeNativeStruct(
                &self.module,
                local_ty.name orelse return error.RuntimeFailure,
                runtime_args[index].raw_ptr,
            ) };
            materialized_args[index] = true;
        }
        const result = try self.vm.runFunctionById(&self.module, function_decl.id, runtime_args, context.writer, .{
            .context = @as(?*anyopaque, @ptrCast(context)),
            .call_native = nativeCallHook(@TypeOf(context.*)),
            .resolve_function = resolveFunctionHook(@TypeOf(context.*)),
            .copy_struct_args_by_value = false,
        });
        for (native_arg_ptrs, 0..) |native_ptr, index| {
            if (native_ptr == 0) continue;
            const local_ty = function_decl.local_types[index];
            try self.vm.writeStructToNativeLayout(
                &self.module,
                local_ty.name orelse return error.RuntimeFailure,
                runtime_args[index].raw_ptr,
                native_ptr,
            );
        }

        var bridge_result = runtime_abi.bridgeValueFromValue(result);
        if (function_decl.return_type.kind == .ffi_struct and result == .raw_ptr and result.raw_ptr != 0) {
            const type_name = function_decl.return_type.name orelse return error.RuntimeFailure;
            const native_result = try self.vm.lowerStructToNativeLayout(
                &self.module,
                type_name,
                result.raw_ptr,
            );
            errdefer self.vm.destroyStructNativeLayout(&self.module, type_name, native_result);
            try self.pending_callback_native_structs.append(self.allocator, .{
                .type_name = type_name,
                .ptr = native_result,
            });
            bridge_result = runtime_abi.bridgeValueFromValue(.{ .raw_ptr = native_result });
        } else {
            self.vm.retainManagedValue(result);
            errdefer self.vm.releaseManagedValue(result);
            try self.pending_callback_return_values.append(self.allocator, result);
        }
        self.trimPendingCallbackReturns();
        if (out_result) |ptr| ptr.* = bridge_result;
        self.vm.releaseManagedValue(result);
        runtime_abi.emitExecutionTrace("CALLBACK", "RETURN", "runtime->native fn={s}({d}) tag={s}", .{
            function_decl.name,
            function_id,
            @tagName(bridge_result.tag),
        });
    }

    fn cleanupPendingCallbackReturns(self: *HybridRuntime) void {
        for (self.pending_callback_native_structs.items) |item| {
            self.vm.destroyStructNativeLayout(&self.module, item.type_name, item.ptr);
        }
        self.pending_callback_native_structs.clearRetainingCapacity();
        for (self.pending_callback_return_values.items) |value| {
            self.vm.releaseManagedValue(value);
        }
        self.pending_callback_return_values.clearRetainingCapacity();
    }

    fn trimPendingCallbackReturns(self: *HybridRuntime) void {
        const max_pending_callback_returns = 256;
        while (self.pending_callback_native_structs.items.len > max_pending_callback_returns) {
            const item = self.pending_callback_native_structs.orderedRemove(0);
            self.vm.destroyStructNativeLayout(&self.module, item.type_name, item.ptr);
        }
        while (self.pending_callback_return_values.items.len > max_pending_callback_returns) {
            const value = self.pending_callback_return_values.orderedRemove(0);
            self.vm.releaseManagedValue(value);
        }
    }

    fn resolveFunctionPointer(self: *HybridRuntime, function_id: u32) !usize {
        const function_decl = findFunction(self.manifest.functions, function_id) orelse return error.UnknownFunction;
        if (function_decl.execution != .native or function_decl.exported_name == null) return error.UnsupportedExecutableFeature;
        return self.bridge.resolveImplementationPointer(function_id);
    }
};

const NativeStructReturn = struct {
    type_name: []const u8,
    ptr: usize,
};

fn RuntimeContext(comptime Writer: type) type {
    return struct {
        runtime: *HybridRuntime,
        writer: Writer,
    };
}

fn runtimeInvoke(comptime Context: type) *const fn (?*anyopaque, u32, []const runtime_abi.BridgeValue, *runtime_abi.BridgeValue) anyerror!void {
    return struct {
        fn invoke(context: ?*anyopaque, function_id: u32, args: []const runtime_abi.BridgeValue, out_result: *runtime_abi.BridgeValue) !void {
            const runtime_context: *Context = @ptrCast(@alignCast(context orelse return error.MissingHybridContext));
            runtime_context.runtime.invokeRuntime(runtime_context, function_id, args, out_result) catch |err| {
                if (err == error.RuntimeFailure) {
                    if (runtime_context.runtime.vm.lastError()) |message| {
                        std.debug.panic("hybrid runtime failure in fn={d}: {s}", .{ function_id, message });
                    } else {
                        std.debug.panic("hybrid runtime failure in fn={d}: <no vm lastError>", .{function_id});
                    }
                }
                return err;
            };
        }
    }.invoke;
}

fn nativeCallHook(comptime Context: type) *const fn (?*anyopaque, u32, []const runtime_abi.Value) anyerror!runtime_abi.Value {
    return struct {
        fn invoke(context: ?*anyopaque, function_id: u32, args: []const runtime_abi.Value) !runtime_abi.Value {
            const runtime_context: *Context = @ptrCast(@alignCast(context orelse return error.MissingHybridContext));
            return callNative(runtime_context.runtime, function_id, args);
        }
    }.invoke;
}

fn resolveFunctionHook(comptime Context: type) *const fn (?*anyopaque, u32) anyerror!usize {
    return struct {
        fn resolve(context: ?*anyopaque, function_id: u32) !usize {
            const runtime_context: *Context = @ptrCast(@alignCast(context orelse return error.MissingHybridContext));
            return runtime_context.runtime.resolveFunctionPointer(function_id);
        }
    }.resolve;
}

fn buildRuntimeDescriptors(allocator: std.mem.Allocator, manifest: hybrid.HybridModuleManifest) ![]hybrid.BridgeDescriptor {
    var descriptors = std.array_list.Managed(hybrid.BridgeDescriptor).init(allocator);
    for (manifest.functions) |function_decl| {
        if (function_decl.execution != .native) continue;
        if (function_decl.exported_name == null) continue;
        try descriptors.append(.{
            .bridge_id = .init(function_decl.id),
            .function_id = .init(function_decl.id),
            .symbol_name = function_decl.exported_name.?,
            .source_execution = .runtime,
            .target_execution = .native,
            .calling_convention = .kira_hybrid,
        });
    }
    return descriptors.toOwnedSlice();
}

fn findFunction(functions: []const hybrid.FunctionManifest, function_id: u32) ?hybrid.FunctionManifest {
    for (functions) |function_decl| {
        if (function_decl.id == function_id) return function_decl;
    }
    return null;
}

fn nativeClosureCaptureTypes(allocator: std.mem.Allocator, native_ptr: usize, function_decl: hybrid.FunctionManifest) ![]bytecode.TypeRef {
    const raw_native_ptr = runtime_abi.untagNativeClosurePointer(native_ptr);
    const capture_count_ptr: *const i64 = @ptrFromInt(raw_native_ptr + 8);
    const capture_count_i64 = capture_count_ptr.*;
    if (capture_count_i64 < 0) return error.RuntimeFailure;
    const capture_count: usize = @intCast(capture_count_i64);
    if (capture_count > function_decl.param_types.len) return error.RuntimeFailure;

    const start = function_decl.param_types.len - capture_count;
    const result = try allocator.alloc(bytecode.TypeRef, capture_count);
    for (function_decl.param_types[start..], 0..) |param_type, index| {
        result[index] = convertManifestTypeRef(param_type);
    }
    return result;
}

fn convertManifestTypeRef(value: hybrid.TypeRef) bytecode.TypeRef {
    return .{
        .kind = switch (value.kind) {
            .void => .void,
            .integer => .integer,
            .float => .float,
            .string => .string,
            .boolean => .boolean,
            .construct_any => .construct_any,
            .array => .array,
            .raw_ptr => .raw_ptr,
            .ffi_struct => .ffi_struct,
            .enum_instance => .enum_instance,
        },
        .name = value.name,
        .construct_constraint = if (value.construct_constraint) |constraint| .{ .construct_name = constraint.construct_name } else null,
    };
}

fn isCallbackTypeName(name: []const u8) bool {
    return std.mem.indexOf(u8, name, "->") != null;
}

fn callNative(self: *HybridRuntime, function_id: u32, args: []const runtime_abi.Value) !runtime_abi.Value {
    try self.vm.beginNativeBoundary();
    defer self.vm.endNativeBoundary();

    const function_decl = findFunction(self.manifest.functions, function_id) orelse return error.UnknownFunction;
    const lowered_args = try self.allocator.alloc(runtime_abi.Value, args.len);
    defer self.allocator.free(lowered_args);
    const native_arg_ptrs = try self.allocator.alloc(usize, args.len);
    defer self.allocator.free(native_arg_ptrs);
    @memset(native_arg_ptrs, 0);
    defer {
        for (native_arg_ptrs, 0..) |native_ptr, index| {
            if (native_ptr == 0 or index >= function_decl.param_types.len) continue;
            if (ownershipTransfersToCallee(paramOwnershipAt(function_decl.param_ownership, index))) continue;
            const param_type = function_decl.param_types[index];
            switch (param_type.kind) {
                .ffi_struct => if (param_type.name) |name| {
                    self.vm.destroyStructNativeLayout(&self.module, name, native_ptr);
                },
                .array => self.vm.destroyArrayNativeLayout(
                    &self.module,
                    convertManifestTypeRef(param_type),
                    native_ptr,
                ),
                else => {},
            }
        }
    }

        for (args, 0..) |arg, index| {
            try self.vm.pinNativeBoundaryValue(arg);
            lowered_args[index] = arg;
            if (index >= function_decl.param_types.len) continue;
            const param_type = function_decl.param_types[index];
            if (arg != .raw_ptr or arg.raw_ptr == 0) continue;
            switch (param_type.kind) {
                .raw_ptr => {
                    if (param_type.name) |name| {
                        if (isCallbackTypeName(name) and self.vm.heap.getClosure(arg.raw_ptr) != null) {
                            lowered_args[index] = .{ .raw_ptr = try self.vm.exportRuntimeClosureToNative(&self.module, arg.raw_ptr) };
                        }
                    }
                },
                .ffi_struct => {
                    native_arg_ptrs[index] = try self.vm.lowerStructToNativeLayout(
                        &self.module,
                        param_type.name orelse return error.RuntimeFailure,
                        arg.raw_ptr,
                );
                lowered_args[index] = .{ .raw_ptr = native_arg_ptrs[index] };
            },
            .array => {
                native_arg_ptrs[index] = try self.vm.copyArrayToNativeLayout(
                    &self.module,
                    convertManifestTypeRef(param_type),
                    arg.raw_ptr,
                );
                lowered_args[index] = .{ .raw_ptr = native_arg_ptrs[index] };
            },
            else => {},
        }
    }

    const result = try self.bridge.call(function_id, lowered_args);
    self.vm.retainManagedValue(result);
    for (native_arg_ptrs, 0..) |native_ptr, index| {
        if (native_ptr == 0) continue;
        if (!ownershipSyncsBack(paramOwnershipAt(function_decl.param_ownership, index))) continue;
        const param_type = function_decl.param_types[index];
        switch (param_type.kind) {
            .ffi_struct => try self.vm.syncStructFromNativeLayout(
                &self.module,
                param_type.name orelse return error.RuntimeFailure,
                args[index].raw_ptr,
                native_ptr,
            ),
            .array => try self.vm.syncArrayFromNativeLayout(
                &self.module,
                convertManifestTypeRef(param_type),
                args[index].raw_ptr,
                native_ptr,
            ),
            else => {},
        }
    }

    return result;
}

fn paramOwnershipAt(param_ownership: []const hybrid.OwnershipMode, index: usize) hybrid.OwnershipMode {
    if (index < param_ownership.len) return param_ownership[index];
    return .owned;
}

fn ownershipTransfersToCallee(mode: hybrid.OwnershipMode) bool {
    return switch (mode) {
        .owned, .move => true,
        .borrow_read, .borrow_mut, .copy => false,
    };
}

fn ownershipSyncsBack(mode: hybrid.OwnershipMode) bool {
    return mode == .borrow_mut;
}

fn callNativeFunction(self: *HybridRuntime, function_id: u32, args: []const runtime_abi.Value) !runtime_abi.Value {
    try self.vm.beginNativeBoundary();
    defer self.vm.endNativeBoundary();

    const result = try self.bridge.call(function_id, args);
    self.vm.retainManagedValue(result);
    return result;
}

const DirectStdoutWriter = struct {
    pub fn writeAll(_: DirectStdoutWriter, bytes: []const u8) !void {
        var buffer: [1024]u8 = undefined;
        var writer = std.Io.File.stdout().writer(std.Options.debug_io, &buffer);
        defer writer.interface.flush() catch {};
        try writer.interface.writeAll(bytes);
    }

    pub fn writeByte(self: DirectStdoutWriter, byte: u8) !void {
        _ = self;
        if (@import("builtin").os.tag == .windows and byte == '\n') {
            try DirectStdoutWriter.writeAll(.{}, "\r\n");
            return;
        }
        var buffer = [1]u8{byte};
        try DirectStdoutWriter.writeAll(.{}, &buffer);
    }

    pub fn print(self: DirectStdoutWriter, comptime fmt: []const u8, args: anytype) !void {
        var buffer: [512]u8 = undefined;
        const rendered = try std.fmt.bufPrint(&buffer, fmt, args);
        try self.writeAll(rendered);
    }
};
