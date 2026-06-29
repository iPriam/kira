const std = @import("std");
const bytecode = @import("kira_bytecode");
const hybrid = @import("kira_hybrid_definition");
const native_bridge = @import("kira_native_bridge");
const runtime_abi = @import("kira_runtime_abi");
const binder = @import("binder.zig");
const vm_runtime = @import("kira_vm_runtime");
const native_calls = @import("native_calls.zig");
const DirectStdoutWriter = @import("direct_stdout_writer.zig").DirectStdoutWriter;

pub const HybridRuntime = struct {
    allocator: std.mem.Allocator,
    manifest: hybrid.HybridModuleManifest,
    module: bytecode.Module,
    vm: vm_runtime.Vm,
    bridge: native_bridge.NativeBridge,
    pending_callback_return_values: std.ArrayListUnmanaged(runtime_abi.Value) = .empty,
    pending_callback_native_arrays: std.ArrayListUnmanaged(NativeArrayReturn) = .empty,
    pending_callback_native_enums: std.ArrayListUnmanaged(NativeEnumReturn) = .empty,
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
        self.pending_callback_native_arrays.deinit(self.allocator);
        self.pending_callback_native_enums.deinit(self.allocator);
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
            .native => _ = try native_calls.callNativeFunction(self, self.manifest.entry_function_id, &.{}),
            .inherited => unreachable,
        }
    }

    /// Invoke a single @Runtime function by id, writing its output to `writer`,
    /// with the @Native bridge installed exactly as `run` sets it up. Used by
    /// `kira test --backend hybrid` to run the synthesized pure-Kira test driver
    /// (so its @Native/FFI calls bridge) and capture its PASS/FAIL/SKIP output.
    pub fn runFunctionWithWriter(self: *HybridRuntime, function_id: u32, writer: anytype) !void {
        const Context = RuntimeContext(@TypeOf(writer));
        var runtime_context = Context{
            .runtime = self,
            .writer = writer,
        };
        native_bridge.installRuntimeInvoker(&runtime_context, runtimeInvoke(Context));
        defer native_bridge.clearRuntimeInvoker();
        try self.invokeRuntime(&runtime_context, function_id, &.{}, null);
    }

    /// Run a zero-argument @Runtime function by id THROUGH THE BRIDGE (the
    /// @Native invoker installed exactly as `run` sets it up) and return its
    /// managed VM `runtime_abi.Value`. Unlike `runFunctionWithWriter` this hands
    /// back the value instead of writing markers, and unlike `invokeRuntime` it
    /// does not lower the result into a native-layout copy — the caller wants the
    /// managed VM value to decode in Kira terms (used by `kira test` to evaluate a
    /// trap test's `__expect()` whose body may call @Native/FFI). The returned
    /// value is owned by the caller; drop it via `self.vm.dropManagedValue`.
    pub fn runFunctionForValue(self: *HybridRuntime, function_id: u32, writer: anytype) !runtime_abi.Value {
        const Context = RuntimeContext(@TypeOf(writer));
        var runtime_context = Context{
            .runtime = self,
            .writer = writer,
        };
        native_bridge.installRuntimeInvoker(&runtime_context, runtimeInvoke(Context));
        defer native_bridge.clearRuntimeInvoker();
        const function_decl = self.module.findFunctionById(function_id) orelse return error.UnknownFunction;
        return self.vm.runFunctionById(&self.module, function_decl.id, &.{}, writer, .{
            .context = @as(?*anyopaque, @ptrCast(&runtime_context)),
            .call_native = nativeCallHook(Context),
            .resolve_function = resolveFunctionHook(Context),
            .copy_struct_args_by_value = false,
        });
    }

    pub fn flushPendingCallbackReturns(self: *HybridRuntime) void {
        self.cleanupPendingCallbackReturns();
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
        var materialized_args_cleaned = false;
        errdefer if (!materialized_args_cleaned) {
            for (materialized_args, 0..) |materialized, index| {
                if (materialized) self.vm.dropManagedValue(runtime_args[index]);
            }
        };
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
            // An enum argument arrives as a native-layout `[u64 tag, u64 payload]` block, not a
            // managed VM enum. Without materializing it the callee operates on raw native words
            // reinterpreted as VM `Value` slots (tag 0 reads back as `.void`), and returning the
            // argument hands `lowerEnumToNativeOwned` a bogus enum — the basic-kira-ui-app
            // `effectiveGraphicsPlatform(requested) -> requested` "enum native lowering requires an
            // integer tag slot" crash. Materialize it like an `ffi_struct` arg so the managed copy
            // is dropped after the call (or survives as the result via the alias check below).
            if (local_ty.kind == .enum_instance and runtime_args[index] == .raw_ptr and runtime_args[index].raw_ptr != 0 and !self.vm.isManagedStructPointer(runtime_args[index].raw_ptr)) {
                native_arg_ptrs[index] = runtime_args[index].raw_ptr;
                runtime_args[index] = .{ .raw_ptr = try self.vm.copyEnumFromNativeLayout(
                    &self.module,
                    local_ty.name orelse return error.RuntimeFailure,
                    runtime_args[index].raw_ptr,
                ) };
                materialized_args[index] = true;
                continue;
            }
            if (local_ty.kind == .array and runtime_args[index] == .raw_ptr and runtime_args[index].raw_ptr != 0 and self.vm.heap.getArray(runtime_args[index].raw_ptr) == null) {
                native_arg_ptrs[index] = runtime_args[index].raw_ptr;
                runtime_args[index] = .{ .raw_ptr = try self.vm.copyArrayFromNativeLayout(
                    &self.module,
                    local_ty,
                    runtime_args[index].raw_ptr,
                ) };
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
        if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG invoke fn={s}({d}) args={d} -> runFunctionById START\n", .{ function_decl.name, function_id, args.len });
        const result = try self.vm.runFunctionById(&self.module, function_decl.id, runtime_args, context.writer, .{
            .context = @as(?*anyopaque, @ptrCast(context)),
            .call_native = nativeCallHook(@TypeOf(context.*)),
            .resolve_function = resolveFunctionHook(@TypeOf(context.*)),
            .copy_struct_args_by_value = false,
        });
        if (std.c.getenv("KIRA_DBG") != null) std.debug.print("DBG invoke fn={d} -> runFunctionById DONE result_tag={s}\n", .{ function_id, @tagName(result) });
        // Armed before the fallible borrow-mut writeback below: a writeback error must release
        // `result`, or the managed return value leaks. Cleared once ownership transfers to a
        // pending-callback list further down.
        var result_owned_by_pending = false;
        errdefer if (!result_owned_by_pending) self.vm.dropManagedValue(result);
        for (native_arg_ptrs, 0..) |native_ptr, index| {
            if (native_ptr == 0) continue;
            // Only a `borrow mut` param can be mutated by the callee, so only it needs to
            // sync back into the caller's native struct. Writing back a read-only/owned
            // param is not just wasted work: writeNativeFieldValue *reallocates* pointer
            // fields (enum/array/nested struct) with the VM allocator and orphans the
            // originals, leaving the caller's native field pointing at a VM-allocator block
            // that the caller then frees with libc `free` — the basic-foundation-app event
            // handler's "pointer being freed was not allocated" double-free (a `borrow`
            // GraphicsEvent whose enum fields got rewritten under it).
            const writeback_mode = if (index < function_decl.param_ownership.len) function_decl.param_ownership[index] else .owned;
            if (writeback_mode != .borrow_mut) continue;
            const local_ty = function_decl.local_types[index];
            switch (local_ty.kind) {
                .ffi_struct => try self.vm.writeStructToNativeLayout(
                    &self.module,
                    local_ty.name orelse return error.RuntimeFailure,
                    runtime_args[index].raw_ptr,
                    native_ptr,
                ),
                .array => try self.vm.writeArrayToNativeLayout(
                    &self.module,
                    local_ty,
                    runtime_args[index].raw_ptr,
                    native_ptr,
                ),
                else => {},
            }
        }
        for (materialized_args, 0..) |materialized, index| {
            if (!materialized) continue;
            if (result == .raw_ptr and result.raw_ptr == runtime_args[index].raw_ptr) continue;
            self.vm.dropManagedValue(runtime_args[index]);
        }
        materialized_args_cleaned = true;

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
            if (self.vm.nativeReturnIsSelfContained(&self.module, function_decl.return_type, result.raw_ptr)) {
                // The native copy owns all of its data, so this is a Rust-style
                // move into native ownership: drop the managed VM value immediately
                // (via the trailing drop below) instead of retaining it for the
                // whole runtime lifetime, which otherwise grows without bound (F6).
            } else {
                try self.pending_callback_return_values.append(self.allocator, result);
                result_owned_by_pending = true;
            }
            bridge_result = runtime_abi.bridgeValueFromValue(.{ .raw_ptr = native_result });
        } else if (function_decl.return_type.kind == .array and result == .raw_ptr and result.raw_ptr != 0) {
            const array_ty = function_decl.return_type;
            const native_array = try self.vm.copyArrayToNativeLayout(&self.module, array_ty, result.raw_ptr);
            errdefer self.vm.destroyArrayNativeLayout(&self.module, array_ty, native_array);
            try self.pending_callback_native_arrays.append(self.allocator, .{
                .ty = array_ty,
                .ptr = native_array,
            });
            if (self.vm.nativeReturnIsSelfContained(&self.module, function_decl.return_type, result.raw_ptr)) {
                // The native copy owns all of its data, so this is a Rust-style
                // move into native ownership: drop the managed VM value immediately
                // (via the trailing drop below) instead of retaining it for the
                // whole runtime lifetime, which otherwise grows without bound (F6).
            } else {
                try self.pending_callback_return_values.append(self.allocator, result);
                result_owned_by_pending = true;
            }
            bridge_result = runtime_abi.bridgeValueFromValue(.{ .raw_ptr = native_array });
        } else if (function_decl.return_type.kind == .enum_instance and result == .raw_ptr and result.raw_ptr != 0) {
            // An enum returned to native is moved into a native struct field as a raw pointer
            // and later freed by that struct's `release_contents` (libc `free`). The VM enum
            // block is allocated with the runner's `smp_allocator`, so handing it to native
            // verbatim makes native `free` a non-libc pointer ("pointer being freed was not
            // allocated"). Lower it to a libc-`malloc`'d native enum block, and retain the
            // original managed enum so any borrowed payload (for example string bytes boxed
            // into the native enum) stays alive until runtime teardown.
            const type_name = function_decl.return_type.name orelse return error.RuntimeFailure;
            const native_enum = try self.vm.lowerEnumToNativeOwned(&self.module, type_name, result.raw_ptr);
            try self.pending_callback_native_enums.append(self.allocator, .{
                .type_name = type_name,
                .ptr = native_enum,
            });
            if (self.vm.nativeReturnIsSelfContained(&self.module, function_decl.return_type, result.raw_ptr)) {
                // The native copy owns all of its data, so this is a Rust-style
                // move into native ownership: drop the managed VM value immediately
                // (via the trailing drop below) instead of retaining it for the
                // whole runtime lifetime, which otherwise grows without bound (F6).
            } else {
                try self.pending_callback_return_values.append(self.allocator, result);
                result_owned_by_pending = true;
            }
            bridge_result = runtime_abi.bridgeValueFromValue(.{ .raw_ptr = native_enum });
        } else switch (result) {
            // A scalar/void return owns no heap and is handed to native BY VALUE
            // (the bridge value carries the scalar itself, not a pointer into the
            // managed value), so native cannot borrow into it and there is nothing
            // to keep alive. Leave it for the trailing drop below instead of
            // retaining it for the whole runtime lifetime — otherwise a callback
            // invoked every frame grows `pending_callback_return_values` without
            // bound (F6). String/raw_ptr returns are still retained because the
            // bridge value borrows their bytes until runtime teardown; bounding
            // those needs the native lowering to deep-copy the borrowed payload
            // (the affine ownership rework that F3 touches).
            .void, .integer, .float, .boolean => {},
            else => {
                try self.pending_callback_return_values.append(self.allocator, result);
                result_owned_by_pending = true;
            },
        }
        self.trimPendingCallbackReturns();
        if (out_result) |ptr| ptr.* = bridge_result;
        if (!result_owned_by_pending) self.vm.dropManagedValue(result);
        runtime_abi.emitExecutionTrace("CALLBACK", "RETURN", "runtime->native fn={s}({d}) tag={s}", .{
            function_decl.name,
            function_id,
            @tagName(bridge_result.tag),
        });
    }

    fn cleanupPendingCallbackReturns(self: *HybridRuntime) void {
        for (self.pending_callback_native_arrays.items) |item| {
            self.vm.destroyArrayNativeLayout(&self.module, item.ty, item.ptr);
        }
        self.pending_callback_native_arrays.clearRetainingCapacity();
        // The native enum block returned to native (lowerEnumToNativeOwned, libc-
        // allocated) is OWNED by the native caller: native drops it once — via its
        // own scope-exit drop when transient, or via the containing struct's
        // `release_contents` when moved into a field. Freeing it here too is a
        // double free (the basic enum-bridge `Swatch.shade` case). We still retain
        // the original managed VM enum in `pending_callback_return_values` (dropped
        // below) so any payload the native block borrows stays alive until teardown.
        self.pending_callback_native_enums.clearRetainingCapacity();
        for (self.pending_callback_native_structs.items) |item| {
            self.vm.destroyStructNativeLayout(&self.module, item.type_name, item.ptr);
        }
        self.pending_callback_native_structs.clearRetainingCapacity();
        for (self.pending_callback_return_values.items) |value| {
            self.vm.dropManagedValue(value);
        }
        self.pending_callback_return_values.clearRetainingCapacity();
    }

    fn trimPendingCallbackReturns(self: *HybridRuntime) void {
        // Native callbacks may keep returned bridge arrays/structs across frames.
        // Keep them alive for the runtime lifetime; cleanupPendingCallbackReturns drops them on deinit.
        _ = self;
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

const NativeArrayReturn = struct {
    ty: bytecode.TypeRef,
    ptr: usize,
};

const NativeEnumReturn = struct {
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
            return native_calls.callNative(runtime_context.runtime, function_id, args);
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
