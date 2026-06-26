const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const builtins = @import("builtins.zig");
const ownership = @import("ownership.zig");
const native_layout = @import("native_layout.zig");
const helper_impl = @import("vm_helpers.zig");
const clone_impl = @import("vm_value_clone.zig");
const value_impl = @import("vm_values.zig");
const vm_prepare = @import("vm_prepare.zig");
const interpreter = @import("vm_interpreter.zig");
const native_bridge = @import("vm_native_bridge.zig");

const ArrayObject = ownership.ArrayObject;
const ClosureObject = ownership.ClosureObject;

/// VM-side native-state token.
///
/// The leading three fields (`type_id`, `payload`, `runtime_payload`) are the C-ABI prefix
/// shared with the native backend's `KiraNativeState` in
/// `packages/kira_native_bridge/src/runtime_helpers.c`. Everything after that prefix is
/// VM-internal metadata used to clean up Zig-allocated payloads at shutdown
/// (see `deinitTrackedNativeStates`); the native backend never reads those fields.
///
/// Tokens are NOT cast across backends: VM tokens are always allocated and read here
/// (`allocateNativeState`/`recoverNativeState`), and their `payload`/`runtime_payload` hold
/// Zig `BridgeValue`/`Value` arrays, whereas the C path's payload is a raw byte buffer with
/// incompatible semantics. The `comptime` block below enforces the shared prefix layout so the
/// two structs cannot silently drift apart at the C-visible boundary.
pub const NativeStateBox = extern struct {
    type_id: u64,
    payload: usize,
    runtime_payload: usize,
    module: *const bytecode.Module,
    type_name_ptr: [*]const u8,
    type_name_len: usize,
    field_count: usize,

    comptime {
        // Must match the 3-field `KiraNativeState` C struct prefix exactly.
        std.debug.assert(@offsetOf(NativeStateBox, "type_id") == 0);
        std.debug.assert(@offsetOf(NativeStateBox, "payload") == @sizeOf(u64));
        std.debug.assert(@offsetOf(NativeStateBox, "runtime_payload") == @sizeOf(u64) + @sizeOf(usize));
    }

    pub fn init(module: *const bytecode.Module, type_name: []const u8, type_id: u64, field_count: usize, payload: usize) NativeStateBox {
        return .{
            .type_id = type_id,
            .payload = payload,
            .runtime_payload = 0,
            .module = module,
            .type_name_ptr = type_name.ptr,
            .type_name_len = type_name.len,
            .field_count = field_count,
        };
    }

    pub fn typeName(self: *const NativeStateBox) []const u8 {
        return self.type_name_ptr[0..self.type_name_len];
    }
};

pub const ExportedNativeClosure = struct {
    native_ptr: usize,
    captures: []runtime_abi.Value,
};

const NativeLayoutStats = struct {
    arrays_current: usize = 0,
    arrays_peak: usize = 0,
    arrays_allocated: usize = 0,
    arrays_freed: usize = 0,
    structs_current: usize = 0,
    structs_peak: usize = 0,
    structs_allocated: usize = 0,
    structs_freed: usize = 0,
    native_state_recovers: usize = 0,
    native_state_materializations: usize = 0,
};

pub const NativeCallHook = *const fn (?*anyopaque, u32, []const runtime_abi.Value) anyerror!runtime_abi.Value;
pub const ResolveFunctionHook = *const fn (?*anyopaque, u32) anyerror!usize;

pub const Hooks = struct { context: ?*anyopaque = null, call_native: ?NativeCallHook = null, resolve_function: ?ResolveFunctionHook = null, copy_struct_args_by_value: bool = true };

pub const Vm = struct {
    allocator: std.mem.Allocator,
    heap: ownership.Heap,
    native_layout_stats: NativeLayoutStats = .{},
    native_state_materialized_types: std.StringHashMap(usize),
    native_state_boxes: std.AutoHashMap(usize, void),
    exported_native_closures: std.AutoHashMap(usize, ExportedNativeClosure),
    last_error_buffer: [256]u8 = [_]u8{0} ** 256,
    last_error_len: usize = 0,
    // Decoded form of the current module (resolved branch targets, function
    // indices, type indices; see vm_prepare.zig). Rebuilt when a different
    // module pointer is executed; fresh Vms are created per run/reload, so
    // stale-address reuse within one Vm lifetime does not occur in practice.
    prepared_cache: ?*vm_prepare.PreparedModule = null,
    // LIFO pool of frame storage buffers (combined register+local value/owned
    // arrays). The interpreter reuses these instead of malloc/free-ing per call.
    // Pooled buffers are kept alive (only their *capacity* slice is stored), so any
    // pointer taken into a live frame stays valid, and a nested call always draws a
    // different buffer — no aliasing between caller and callee storage.
    frame_pool: std.ArrayListUnmanaged(FrameBuf) = .empty,
    // Type lookups by name are hot: every struct/enum clone, instantiate, and drop in
    // the per-frame UI path resolves its TypeDecl, and the linear name scan in
    // vm_helpers.findType made string compares the single biggest CPU cost of a hybrid
    // UI frame (the live-app beachball). The module's type list is immutable, so cache
    // a name -> TypeDecl map keyed by the module pointer; a different module pointer
    // rebuilds the map (fresh Vms are created per run/reload, so stale-address reuse
    // within one Vm lifetime does not occur in practice).
    type_cache: std.StringHashMapUnmanaged(bytecode.TypeDecl) = .empty,
    type_cache_module: ?*const bytecode.Module = null,
    // Enum declarations by name: enum bridge operations (payload typing,
    // native lowering, clone-by-name) used to scan module.enums linearly with
    // a string compare per entry, which profiling showed hot in hybrid UI
    // frames. Built together with type_cache.
    enum_cache: std.StringHashMapUnmanaged(bytecode.EnumTypeDecl) = .empty,
    // Pointer-keyed fast path in front of type_cache: type-name slices come out
    // of stable module memory, so after one string-hash miss every later lookup
    // for the same name slice is a single cheap pointer-key probe. Caches
    // negative results too (names that are not struct types, e.g. enums).
    type_ptr_cache: std.HashMapUnmanaged(SliceKey, ?bytecode.TypeDecl, SliceKeyContext, std.hash_map.default_max_load_percentage) = .empty,
    // Same idea for array element types: resolveTypeText walks string compares
    // and a linear module.types scan, which profiling showed hot in array
    // clone/copy paths.
    element_type_cache: std.HashMapUnmanaged(SliceKey, bytecode.TypeRef, SliceKeyContext, std.hash_map.default_max_load_percentage) = .empty,

    pub const FrameBuf = struct { values: []runtime_abi.Value, owned: []bool };

    /// Identity of a string slice (address + length) used as a cache key for
    /// names living in stable module memory. Two distinct simultaneous slices
    /// can never share both pointer and length, so the key is exact.
    const SliceKey = struct { ptr: usize, len: usize };

    const SliceKeyContext = struct {
        pub fn hash(_: SliceKeyContext, key: SliceKey) u64 {
            var mixed: u64 = @intCast(key.ptr ^ (key.len *% 0x9E3779B97F4A7C15));
            mixed ^= mixed >> 33;
            mixed *%= 0xff51afd7ed558ccd;
            mixed ^= mixed >> 33;
            return mixed;
        }

        pub fn eql(_: SliceKeyContext, a: SliceKey, b: SliceKey) bool {
            return a.ptr == b.ptr and a.len == b.len;
        }
    };

    fn sliceKey(name: []const u8) SliceKey {
        return .{ .ptr = @intFromPtr(name.ptr), .len = name.len };
    }

    pub fn init(allocator: std.mem.Allocator) Vm {
        return .{
            .allocator = allocator,
            .heap = ownership.Heap.init(allocator),
            .native_state_materialized_types = std.StringHashMap(usize).init(allocator),
            .native_state_boxes = std.AutoHashMap(usize, void).init(allocator),
            .exported_native_closures = std.AutoHashMap(usize, ExportedNativeClosure).init(allocator),
        };
    }

    /// Returns the decoded form of `module`, building it on first use. Keyed by
    /// module pointer like type_cache; a different module pointer rebuilds.
    pub fn preparedFor(self: *Vm, module: *const bytecode.Module) !*const vm_prepare.PreparedModule {
        if (self.prepared_cache) |prepared| {
            if (prepared.module == module) return prepared;
            prepared.deinit(self.allocator);
            self.allocator.destroy(prepared);
            self.prepared_cache = null;
        }
        const prepared = try vm_prepare.prepare(self.allocator, module);
        self.prepared_cache = prepared;
        return prepared;
    }

    /// (Re)builds the per-module type caches when a different module pointer is
    /// seen. All type caches share this module identity, so any of them may be
    /// consulted after this returns true; false means the rebuild failed (OOM)
    /// and callers must use the uncached path.
    fn ensureTypeCaches(self: *Vm, module: *const bytecode.Module) bool {
        if (self.type_cache_module == module) return true;
        self.type_cache.clearRetainingCapacity();
        self.type_ptr_cache.clearRetainingCapacity();
        self.element_type_cache.clearRetainingCapacity();
        self.enum_cache.clearRetainingCapacity();
        self.type_cache.ensureTotalCapacity(self.allocator, @intCast(module.types.len)) catch return false;
        self.enum_cache.ensureTotalCapacity(self.allocator, @intCast(module.enums.len)) catch return false;
        for (module.types) |type_decl| self.type_cache.putAssumeCapacity(type_decl.name, type_decl);
        for (module.enums) |enum_decl| self.enum_cache.putAssumeCapacity(enum_decl.name, enum_decl);
        self.type_cache_module = module;
        return true;
    }

    pub fn findTypeCached(self: *Vm, module: *const bytecode.Module, name: []const u8) ?bytecode.TypeDecl {
        if (!self.ensureTypeCaches(module)) return helper_impl.findType(module, name);
        if (self.type_ptr_cache.get(sliceKey(name))) |cached| return cached;
        const result = self.type_cache.get(name);
        self.type_ptr_cache.put(self.allocator, sliceKey(name), result) catch {};
        return result;
    }

    pub fn findEnumCached(self: *Vm, module: *const bytecode.Module, name: []const u8) ?bytecode.EnumTypeDecl {
        if (!self.ensureTypeCaches(module)) {
            for (module.enums) |enum_decl| {
                if (std.mem.eql(u8, enum_decl.name, name)) return enum_decl;
            }
            return null;
        }
        return self.enum_cache.get(name);
    }

    pub fn acquireFrame(self: *Vm, count: usize) !FrameBuf {
        if (self.frame_pool.pop()) |buf| {
            if (buf.values.len >= count) return buf;
            self.allocator.free(buf.values);
            self.allocator.free(buf.owned);
        }
        return .{
            .values = try self.allocator.alloc(runtime_abi.Value, count),
            .owned = try self.allocator.alloc(bool, count),
        };
    }

    pub fn releaseFrame(self: *Vm, buf: FrameBuf) void {
        self.frame_pool.append(self.allocator, buf) catch {
            self.allocator.free(buf.values);
            self.allocator.free(buf.owned);
        };
    }

    pub fn deinit(self: *Vm) void {
        var exported_iterator = self.exported_native_closures.iterator();
        while (exported_iterator.next()) |entry| {
            const exported = entry.value_ptr.*;
            for (exported.captures) |capture| self.heap.dropValue(capture);
            self.allocator.free(exported.captures);
            const byte_len = 16 + exported.captures.len * @sizeOf(runtime_abi.BridgeValue);
            const word_count = @max(1, std.math.divCeil(usize, byte_len, @sizeOf(u64)) catch unreachable);
            const words: [*]u64 = @ptrFromInt(exported.native_ptr);
            self.allocator.free(words[0..word_count]);
        }
        self.exported_native_closures.deinit();
        native_bridge.deinitTrackedNativeStates(self);
        self.heap.deinit();
        self.native_state_materialized_types.deinit();
        if (self.prepared_cache) |prepared| {
            prepared.deinit(self.allocator);
            self.allocator.destroy(prepared);
        }
        for (self.frame_pool.items) |buf| {
            self.allocator.free(buf.values);
            self.allocator.free(buf.owned);
        }
        self.frame_pool.deinit(self.allocator);
        self.type_cache.deinit(self.allocator);
        self.type_ptr_cache.deinit(self.allocator);
        self.element_type_cache.deinit(self.allocator);
        self.enum_cache.deinit(self.allocator);
    }

    pub fn managedObjectCount(self: *const Vm) usize {
        return self.heap.count();
    }

    pub fn emitMemoryReport(self: *const Vm, label: []const u8) void {
        const heap_stats = self.heap.stats;
        const native_stats = self.native_layout_stats;
        std.debug.print(
            "Kira runtime memory report ({s}): heap arrays current={d} peak={d} allocated={d} freed={d} structs current={d} peak={d} allocated={d} freed={d} closures current={d} peak={d} allocated={d} freed={d} strings current={d} peak={d} allocated={d} freed={d} nativeArrays current={d} peak={d} allocated={d} freed={d} nativeStructs current={d} peak={d} allocated={d} freed={d} nativeStateRecovers={d} nativeStateMaterializations={d}\n",
            .{
                label,
                heap_stats.arrays_current,
                heap_stats.arrays_peak,
                heap_stats.arrays_allocated,
                heap_stats.arrays_freed,
                heap_stats.structs_current,
                heap_stats.structs_peak,
                heap_stats.structs_allocated,
                heap_stats.structs_freed,
                heap_stats.closures_current,
                heap_stats.closures_peak,
                heap_stats.closures_allocated,
                heap_stats.closures_freed,
                heap_stats.strings_current,
                heap_stats.strings_peak,
                heap_stats.strings_allocated,
                heap_stats.strings_freed,
                native_stats.arrays_current,
                native_stats.arrays_peak,
                native_stats.arrays_allocated,
                native_stats.arrays_freed,
                native_stats.structs_current,
                native_stats.structs_peak,
                native_stats.structs_allocated,
                native_stats.structs_freed,
                native_stats.native_state_recovers,
                native_stats.native_state_materializations,
            },
        );
    }

    pub fn emitMemoryDetail(self: *const Vm) void {
        self.heap.emitCurrentTypeReport();
        var iterator = self.native_state_materialized_types.iterator();
        while (iterator.next()) |entry| {
            std.debug.print("Kira runtime memory detail: nativeStateType={s} materialized={d}\n", .{ entry.key_ptr.*, entry.value_ptr.* });
        }
    }

    pub fn dropManagedValue(self: *Vm, value: runtime_abi.Value) void {
        self.heap.dropValue(value);
    }

    pub fn retainManagedValue(self: *Vm, value: runtime_abi.Value) void {
        _ = self;
        _ = value;
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
        return self.runMainWithHooks(module, writer, .{});
    }

    /// Runs the module entrypoint with caller-provided hooks. `kira run` uses
    /// this to install the LibFFI dispatcher (vm_ffi.zig) so direct FFI calls
    /// execute in the pure VM without LLVM-compiled trampolines.
    pub fn runMainWithHooks(self: *Vm, module: *const bytecode.Module, writer: anytype, hooks: Hooks) anyerror!void {
        const entry_function_id = module.entry_function_id orelse {
            self.rememberError("bytecode module has no runtime entrypoint");
            return error.RuntimeFailure;
        };
        const result = try self.runFunctionById(module, entry_function_id, &.{}, writer, hooks);
        self.heap.dropValue(result);
    }

    pub fn runFunctionById(
        self: *Vm,
        module: *const bytecode.Module,
        function_id: u32,
        args: []const runtime_abi.Value,
        writer: anytype,
        hooks: Hooks,
    ) anyerror!runtime_abi.Value {
        const prepared = try self.preparedFor(module);
        const function_index = prepared.indexOfId(function_id) orelse {
            self.rememberError("bytecode function id is out of range");
            return error.RuntimeFailure;
        };
        return interpreter.runPrepared(self, prepared, &prepared.functions[function_index], args, writer, hooks);
    }

    pub fn lastError(self: *const Vm) ?[]const u8 {
        if (self.last_error_len == 0) return null;
        return self.last_error_buffer[0..self.last_error_len];
    }

    // --- Native bridge facade -----------------------------------------------
    // Implementations live in vm_native_bridge.zig; these thin wrappers keep
    // the public method surface used by the hybrid runtime, the interpreter,
    // vm_helpers, and tests.

    pub fn materializeNativeStruct(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) !usize {
        return native_bridge.materializeNativeStruct(self, module, type_name, native_ptr);
    }

    pub fn materializeNativeClosure(self: *Vm, module: *const bytecode.Module, native_ptr: usize, external_capture_types: ?[]const bytecode.TypeRef) !usize {
        return native_bridge.materializeNativeClosure(self, module, native_ptr, external_capture_types);
    }

    pub fn lowerStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) !usize {
        return native_bridge.lowerStructToNativeLayout(self, module, type_name, runtime_ptr);
    }

    pub fn writeStructToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) !void {
        return native_bridge.writeStructToNativeLayout(self, module, type_name, runtime_ptr, native_ptr);
    }

    pub fn copyArrayToNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize) anyerror!usize {
        return native_bridge.copyArrayToNativeLayout(self, module, array_ty, runtime_array_ptr);
    }

    pub fn copyArrayFromNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize) anyerror!usize {
        return native_bridge.copyArrayFromNativeLayout(self, module, array_ty, native_array_ptr);
    }

    pub fn syncArrayFromNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, runtime_array_ptr: usize, native_array_ptr: usize) anyerror!void {
        return native_bridge.syncArrayFromNativeLayout(self, module, array_ty, runtime_array_ptr, native_array_ptr);
    }

    pub fn syncStructFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) !void {
        return native_bridge.syncStructFromNativeLayout(self, module, type_name, runtime_ptr, native_ptr);
    }

    pub fn destroyArrayNativeLayout(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef, native_array_ptr: usize) void {
        return native_bridge.destroyArrayNativeLayout(self, module, array_ty, native_array_ptr);
    }

    pub fn destroyStructNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
        return native_bridge.destroyStructNativeLayout(self, module, type_name, native_ptr);
    }

    pub fn destroyNativeLayoutValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) void {
        return native_bridge.destroyNativeLayoutValue(self, module, ty, value);
    }

    pub fn allocateNativeState(self: *Vm, module: *const bytecode.Module, type_name: []const u8, type_id: u64, src_payload: usize) !usize {
        return native_bridge.allocateNativeState(self, module, type_name, type_id, src_payload);
    }

    pub fn recoverNativeState(self: *Vm, module: *const bytecode.Module, type_name: []const u8, state_token: usize, expected_type_id: u64) !usize {
        return native_bridge.recoverNativeState(self, module, type_name, state_token, expected_type_id);
    }

    pub fn materializeCallbackValueFromNative(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        return native_bridge.materializeCallbackValueFromNative(self, module, ty, value);
    }

    pub fn exportRuntimeClosureToNative(self: *Vm, module: *const bytecode.Module, closure_ptr: usize) !usize {
        return native_bridge.exportRuntimeClosureToNative(self, module, closure_ptr);
    }

    pub fn copyEnumToNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) anyerror!usize {
        return native_bridge.copyEnumToNativeLayout(self, module, type_name, runtime_ptr);
    }

    pub fn lowerEnumToNativeOwned(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize) anyerror!usize {
        return native_bridge.lowerEnumToNativeOwned(self, module, type_name, runtime_ptr);
    }

    pub fn copyEnumFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) anyerror!usize {
        return native_bridge.copyEnumFromNativeLayout(self, module, type_name, native_ptr);
    }

    pub fn destroyEnumNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
        return native_bridge.destroyEnumNativeLayout(self, module, type_name, native_ptr);
    }

    pub fn destroyOwnedEnumNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) void {
        return native_bridge.destroyOwnedEnumNativeLayout(self, module, type_name, native_ptr);
    }

    pub fn enumPayloadFromNativeWord(self: *Vm, module: *const bytecode.Module, payload_ty: bytecode.TypeRef, word: u64) anyerror!runtime_abi.Value {
        return native_bridge.enumPayloadFromNativeWord(self, module, payload_ty, word);
    }

    pub fn materializeNativeResult(self: *Vm, module: *const bytecode.Module, return_ty: bytecode.TypeRef, result: runtime_abi.Value) anyerror!runtime_abi.Value {
        return native_bridge.materializeNativeResult(self, module, return_ty, result);
    }

    pub fn materializeNativeResultFromC(self: *Vm, module: *const bytecode.Module, return_ty: bytecode.TypeRef, result: runtime_abi.Value) anyerror!runtime_abi.Value {
        return native_bridge.materializeNativeResultFromC(self, module, return_ty, result);
    }

    pub fn materializeNativeStateValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        return native_bridge.materializeNativeStateValue(self, module, ty, value);
    }

    pub fn preserveNativeStateValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        return native_bridge.preserveNativeStateValue(self, module, ty, value);
    }

    pub fn destroyPreservedNativeStateValue(self: *Vm, module: *const bytecode.Module, ty: bytecode.TypeRef, value: runtime_abi.Value) void {
        native_bridge.destroyPreservedNativeStateValue(self, module, ty, value);
    }

    pub fn copyStructFromNativeLayout(self: *Vm, module: *const bytecode.Module, type_name: []const u8, native_ptr: usize) anyerror!usize {
        return native_bridge.copyStructFromNativeLayout(self, module, type_name, native_ptr);
    }

    pub fn copyStructFromNativeLayoutInto(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) anyerror!void {
        return native_bridge.copyStructFromNativeLayoutInto(self, module, type_name, runtime_ptr, native_ptr);
    }

    pub fn copyStructToNativeLayoutInto(self: *Vm, module: *const bytecode.Module, type_name: []const u8, runtime_ptr: usize, native_ptr: usize) anyerror!void {
        return native_bridge.copyStructToNativeLayoutInto(self, module, type_name, runtime_ptr, native_ptr);
    }

    pub fn rememberError(self: *Vm, message: []const u8) void {
        const length = @min(message.len, self.last_error_buffer.len);
        @memcpy(self.last_error_buffer[0..length], message[0..length]);
        self.last_error_len = length;
    }

    pub fn rememberFmt(self: *Vm, comptime fmt: []const u8, args: anytype) void {
        const message = std.fmt.bufPrint(&self.last_error_buffer, fmt, args) catch {
            self.last_error_len = 0;
            return;
        };
        self.last_error_len = message.len;
    }

    pub fn copyCString(self: *Vm, value: runtime_abi.Value) !runtime_abi.Value {
        if (value != .raw_ptr or value.raw_ptr == 0) return .{ .string = "" };
        const source: [*:0]const u8 = @ptrFromInt(value.raw_ptr);
        const bytes = std.mem.span(source);
        if (bytes.len == 0) return .{ .string = "" };
        const owned = try self.allocator.dupe(u8, bytes);
        errdefer self.allocator.free(owned);
        try self.heap.registerString(owned);
        return .{ .string = owned };
    }

    pub fn allocateClosure(
        self: *Vm,
        registers: []const runtime_abi.Value,
        function_id: u32,
        capture_registers: []const u32,
        capture_ownership: []const bytecode.OwnershipMode,
    ) !usize {
        const closure = try self.heap.allocClosureObject();
        const captures = try self.heap.allocValueSlice(capture_registers.len);
        for (captures) |*capture| capture.* = .{ .void = {} };
        for (capture_registers, 0..) |reg, index| {
            switch (captureOwnershipAt(capture_ownership, index)) {
                .owned, .move, .copy => self.heap.assignTransferred(&captures[index], registers[reg]),
                .borrow_read, .borrow_mut => self.heap.assignBorrowed(&captures[index], registers[reg]),
            }
        }
        closure.* = .{ .function_id = function_id, .captures = captures };
        return self.heap.registerClosure(closure);
    }

    fn captureOwnershipAt(capture_ownership: []const bytecode.OwnershipMode, index: usize) bytecode.OwnershipMode {
        if (index < capture_ownership.len) return capture_ownership[index];
        return .borrow_read;
    }

    pub fn allocateStruct(self: *Vm, module: *const bytecode.Module, type_name: []const u8) !usize {
        const type_decl = self.findTypeCached(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        return self.allocateStructByDecl(module, type_decl);
    }

    /// Allocation fast path for callers that already resolved the TypeDecl
    /// (the decode pass pre-resolves alloc_struct and struct-local types).
    pub fn allocateStructByDecl(self: *Vm, module: *const bytecode.Module, type_decl: bytecode.TypeDecl) !usize {
        const fields = try self.heap.allocValueSlice(type_decl.fields.len);
        for (type_decl.fields, 0..) |field_decl, index| {
            fields[index] = try self.zeroValueForType(module, field_decl.ty);
        }
        return self.heap.registerStruct(type_decl.name, fields);
    }

    pub fn zeroValueForType(self: *Vm, module: *const bytecode.Module, value_type: bytecode.TypeRef) anyerror!runtime_abi.Value {
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

    pub fn allocateArray(self: *Vm, len: usize) !usize {
        const object = try self.heap.allocArrayObject();
        const items = try self.heap.allocBridgeSlice(if (len == 0) 1 else len);
        for (items) |*item| item.* = runtime_abi.bridgeValueFromValue(.{ .void = {} });
        object.* = .{
            .len = len,
            .items = items.ptr,
        };
        return self.heap.registerArray(object);
    }

    pub fn arrayElementType(self: *Vm, module: *const bytecode.Module, array_ty: bytecode.TypeRef) !bytecode.TypeRef {
        const name = array_ty.name orelse return .{ .kind = .raw_ptr };
        // resolveTypeText does a pile of string compares plus a linear type
        // scan; element type names come from stable module memory, so cache by
        // slice identity (invalidated with the other type caches on module change).
        if (!self.ensureTypeCaches(module)) return resolveTypeText(module, name);
        if (self.element_type_cache.get(sliceKey(name))) |cached| return cached;
        const resolved = resolveTypeText(module, name);
        self.element_type_cache.put(self.allocator, sliceKey(name), resolved) catch {};
        return resolved;
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
        if (text.len > 4 and std.mem.startsWith(u8, text, "any ")) {
            return .{
                .kind = .construct_any,
                .name = text,
                .construct_constraint = .{ .construct_name = text[4..] },
            };
        }
        if (std.mem.eql(u8, text, "RawPtr") or std.mem.endsWith(u8, text, "_ptr")) return .{ .kind = .raw_ptr, .name = text };
        if (text.len >= 2 and text[0] == '[' and text[text.len - 1] == ']') return .{ .kind = .array, .name = text[1 .. text.len - 1] };
        if (helper_impl.findType(module, text) != null) return .{ .kind = .ffi_struct, .name = text };
        for (module.enums) |enum_decl| {
            if (std.mem.eql(u8, enum_decl.name, text)) return .{ .kind = .enum_instance, .name = text };
        }
        return .{ .kind = .raw_ptr, .name = text };
    }

    /// Build a managed enum value `[tag, payload]`. The caller resolves `payload`
    /// with the correct ownership (a moved-in owned value, a clone of a borrowed
    /// value, or `.void` for a payload-less variant); ownership of `payload`
    /// transfers into the enum's slot so the payload outlives the constructing
    /// frame when the enum escapes (e.g. via `return`).
    pub fn allocateEnum(self: *Vm, enum_type_name: []const u8, discriminant: u32, payload: runtime_abi.Value) !usize {
        const slots = try self.heap.allocValueSlice(2);
        slots[0] = .{ .integer = @as(i64, @intCast(discriminant)) };
        slots[1] = payload;
        return self.heap.registerStruct(enum_type_name, slots);
    }

    /// Resolve the payload type of `enum_type_name`'s variant `discriminant`, or
    /// `void` when the variant carries no payload / the enum is unknown.
    pub fn enumPayloadTypeOf(self: *Vm, module: *const bytecode.Module, enum_type_name: []const u8, discriminant: u32) bytecode.TypeRef {
        return native_bridge.enumPayloadType(self, module, enum_type_name, discriminant) orelse bytecode.TypeRef{ .kind = .void };
    }

    pub fn typeFieldCount(self: *Vm, module: *const bytecode.Module, type_name: []const u8) ?usize {
        const type_decl = self.findTypeCached(module, type_name) orelse return null;
        return type_decl.fields.len;
    }

    pub fn managedStructTypeName(self: *Vm, ptr: usize) ?[]const u8 {
        return self.heap.getStructTypeName(ptr);
    }

    pub fn isManagedStructPointer(self: *Vm, ptr: usize) bool {
        return self.managedStructTypeName(ptr) != null;
    }

    pub fn isCallbackTypeName(name: []const u8) bool {
        return std.mem.indexOf(u8, name, "->") != null;
    }

    pub fn resolveStructValuePointer(self: *Vm, expected_type_name: []const u8, ptr: usize) !usize {
        if (ptr == 0) {
            self.rememberError("struct value pointer is null");
            return error.RuntimeFailure;
        }
        if (self.isManagedStructPointer(ptr)) return ptr;
        _ = expected_type_name;

        const slot_ptr: *const runtime_abi.Value = @ptrFromInt(ptr);
        const value = slot_ptr.*;
        if (value != .raw_ptr) {
            // Some lowered paths hand us a direct pointer to inline struct field storage
            // instead of a slot containing a managed struct pointer. Treat that as an
            // already-resolved struct pointer and let downstream field access validate it.
            return ptr;
        }
        if (value.raw_ptr == 0) return 0;
        if (!self.isManagedStructPointer(value.raw_ptr)) {
            self.rememberError("struct pointer slot does not contain a managed struct value");
            return error.RuntimeFailure;
        }
        return value.raw_ptr;
    }

    pub fn ensureStructDestinationPointer(self: *Vm, module: *const bytecode.Module, expected_type_name: []const u8, ptr: usize) !usize {
        if (ptr == 0) {
            self.rememberError("struct destination pointer is null");
            return error.RuntimeFailure;
        }
        if (self.isManagedStructPointer(ptr)) return ptr;

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
        self.heap.dropValue(old);
        return slot_ptr.raw_ptr;
    }

    pub fn copyStructValueInto(
        self: *Vm,
        module: *const bytecode.Module,
        type_name: []const u8,
        dst_raw_ptr: usize,
        src_value: runtime_abi.Value,
    ) !void {
        const type_decl = self.findTypeCached(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        const dst_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(dst_raw_ptr);
        if (src_value == .raw_ptr and src_value.raw_ptr != 0) {
            if (self.isManagedStructPointer(src_value.raw_ptr)) {
                const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(src_value.raw_ptr);
                try self.copyStruct(module, type_decl, dst_ptr, src_ptr);
            } else {
                try self.copyStructFromNativeLayoutInto(module, type_name, dst_raw_ptr, src_value.raw_ptr);
            }
            return;
        }
        if (src_value == .raw_ptr and src_value.raw_ptr == 0) {
            const default_ptr = try self.allocateStruct(module, type_name);
            defer self.heap.dropValue(.{ .raw_ptr = default_ptr });
            const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(default_ptr);
            try self.copyStruct(module, type_decl, dst_ptr, src_ptr);
            return;
        }
        self.rememberError("struct copy source must be a struct value");
        return error.RuntimeFailure;
    }

    pub fn cloneStructValue(self: *Vm, module: *const bytecode.Module, type_name: []const u8, src_raw_ptr: usize) !usize {
        const type_decl = self.findTypeCached(module, type_name) orelse {
            self.rememberError("struct type could not be resolved");
            return error.RuntimeFailure;
        };
        const fresh = try self.allocateStruct(module, type_name);
        errdefer self.heap.dropValue(.{ .raw_ptr = fresh });
        const dst_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(fresh);
        const src_ptr: [*]align(1) runtime_abi.Value = @ptrFromInt(src_raw_ptr);
        try self.copyStruct(module, type_decl, dst_ptr, src_ptr);
        return fresh;
    }

    pub fn cloneBorrowedValueForStore(
        self: *Vm,
        module: *const bytecode.Module,
        value_type: bytecode.TypeRef,
        value: runtime_abi.Value,
    ) anyerror!runtime_abi.Value {
        return clone_impl.cloneBorrowedValueForStore(self, module, value_type, value);
    }

    fn cloneClosureValue(self: *Vm, module: *const bytecode.Module, closure_ptr: usize) anyerror!usize {
        return clone_impl.cloneClosureValue(self, module, closure_ptr);
    }

    pub fn cloneBorrowedManagedValueDynamic(self: *Vm, module: *const bytecode.Module, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        return clone_impl.cloneBorrowedManagedValueDynamic(self, module, value);
    }

    pub fn cloneBorrowedLocalValue(
        self: *Vm,
        module: *const bytecode.Module,
        value_type: bytecode.TypeRef,
        value: runtime_abi.Value,
    ) !runtime_abi.Value {
        return clone_impl.cloneBorrowedLocalValue(self, module, value_type, value);
    }

    pub fn resolveVirtualMethod(
        self: *Vm,
        module: *const bytecode.Module,
        actual_type_name: []const u8,
        method_name: []const u8,
    ) ?bytecode.MethodMember {
        const type_decl = self.findTypeCached(module, actual_type_name) orelse return null;
        for (type_decl.methods) |method_decl| {
            if (std.mem.eql(u8, method_decl.name, method_name)) return method_decl;
        }
        return null;
    }

    pub fn copyStruct(
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
                const nested_type = self.findTypeCached(module, nested_name) orelse {
                    self.rememberError("struct type could not be resolved");
                    return error.RuntimeFailure;
                };
                if (src_ptr[index] != .raw_ptr) {
                    self.rememberFmt(
                        "nested struct copy source must be a pointer: {s}.{s}",
                        .{ type_decl.name, field_decl.name },
                    );
                    return error.RuntimeFailure;
                }
                if (dst_ptr[index] != .raw_ptr or dst_ptr[index].raw_ptr == 0) {
                    const old = dst_ptr[index];
                    dst_ptr[index] = .{ .raw_ptr = try self.allocateStruct(module, nested_name) };
                    self.heap.dropValue(old);
                }
                if (src_ptr[index].raw_ptr == 0) {
                    // Treat null nested pointers as zero/default nested structs.
                    const old = dst_ptr[index];
                    dst_ptr[index] = .{ .raw_ptr = try self.allocateStruct(module, nested_name) };
                    self.heap.dropValue(old);
                    continue;
                }
                const nested_dst: [*]align(1) runtime_abi.Value = @ptrFromInt(dst_ptr[index].raw_ptr);
                const nested_src: [*]align(1) runtime_abi.Value = @ptrFromInt(src_ptr[index].raw_ptr);
                try self.copyStruct(module, nested_type, nested_dst, nested_src);
            } else {
                const old = dst_ptr[index];
                dst_ptr[index] = try self.cloneBorrowedValueForStore(module, field_decl.ty, src_ptr[index]);
                self.heap.dropValue(old);
            }
        }
    }

    /// Deep-clone a managed array value so the result shares no backing storage
    /// with the source. Struct and nested-array elements are cloned recursively;
    /// primitive/string elements are retained. Implements affine copy semantics
    /// for array-typed struct fields (see copyStruct).
    pub fn cloneArrayValueDeep(
        self: *Vm,
        module: *const bytecode.Module,
        element_ty: bytecode.TypeRef,
        src_value: runtime_abi.Value,
    ) anyerror!runtime_abi.Value {
        if (src_value != .raw_ptr or src_value.raw_ptr == 0) return src_value;
        const src_array: *const ArrayObject = @ptrFromInt(src_value.raw_ptr);
        const len = src_array.len;
        const dst_ptr = try self.allocateArray(len);
        const dst_array: *ArrayObject = @ptrFromInt(dst_ptr);
        var index: usize = 0;
        while (index < len) : (index += 1) {
            const element = runtime_abi.bridgeValueToValue(src_array.items[index]);
            const cloned = switch (element_ty.kind) {
                .ffi_struct => blk: {
                    if (element != .raw_ptr or element.raw_ptr == 0) break :blk element;
                    const nested_name = element_ty.name orelse break :blk element;
                    if (!self.isManagedStructPointer(element.raw_ptr)) {
                        break :blk runtime_abi.Value{ .raw_ptr = try self.copyStructFromNativeLayout(module, nested_name, element.raw_ptr) };
                    }
                    const fresh = try self.allocateStruct(module, nested_name);
                    const nested_type = self.findTypeCached(module, nested_name) orelse {
                        self.heap.dropValue(.{ .raw_ptr = fresh });
                        self.rememberError("array element struct type could not be resolved");
                        return error.RuntimeFailure;
                    };
                    const fresh_fields: [*]align(1) runtime_abi.Value = @ptrFromInt(fresh);
                    const src_fields: [*]align(1) runtime_abi.Value = @ptrFromInt(element.raw_ptr);
                    try self.copyStruct(module, nested_type, fresh_fields, src_fields);
                    break :blk runtime_abi.Value{ .raw_ptr = fresh };
                },
                .array => try self.cloneArrayValueDeep(module, try self.arrayElementType(module, element_ty), element),
                .enum_instance => blk: {
                    if (element != .raw_ptr or element.raw_ptr == 0) break :blk element;
                    const enum_name = element_ty.name orelse break :blk element;
                    break :blk try self.cloneEnumValue(module, enum_name, element);
                },
                .construct_any => try self.cloneBorrowedValueForStore(module, element_ty, element),
                else => element,
            };
            dst_array.items[index] = runtime_abi.bridgeValueFromValue(cloned);
        }
        return .{ .raw_ptr = dst_ptr };
    }

    pub fn cloneEnumValue(self: *Vm, module: *const bytecode.Module, type_name: []const u8, value: runtime_abi.Value) anyerror!runtime_abi.Value {
        if (value != .raw_ptr or value.raw_ptr == 0) return value;
        if (!self.isManagedStructPointer(value.raw_ptr)) {
            var native_candidate = value.raw_ptr;
            var depth: usize = 0;
            while (depth < 8) : (depth += 1) {
                const native_words: [*]const u64 = @ptrFromInt(native_candidate);
                if (native_bridge.enumNativeVariant(self, module, type_name, native_words[0])) |_| {
                    return .{ .raw_ptr = try self.copyEnumFromNativeLayout(module, type_name, native_candidate) };
                }
                const next_candidate: usize = @intCast(native_words[0]);
                if (next_candidate == 0 or next_candidate == native_candidate or next_candidate % @alignOf(u64) != 0) break;
                native_candidate = next_candidate;
            }
        }
        const src: [*]align(1) const runtime_abi.Value = @ptrFromInt(value.raw_ptr);
        if (src[0] == .raw_ptr and src[0].raw_ptr != 0 and src[0].raw_ptr != value.raw_ptr) {
            return self.cloneEnumValue(module, type_name, src[0]);
        }
        if (src[0] != .integer) {
            const native_words: [*]const u64 = @ptrFromInt(value.raw_ptr);
            var chain_candidate: usize = value.raw_ptr;
            var chain_words = [_]u64{0} ** 4;
            var chain_index: usize = 0;
            while (chain_index < chain_words.len) : (chain_index += 1) {
                const chain_ptr: [*]const u64 = @ptrFromInt(chain_candidate);
                chain_words[chain_index] = chain_ptr[0];
                const next_candidate: usize = @intCast(chain_ptr[0]);
                if (next_candidate == 0 or next_candidate == chain_candidate or next_candidate % @alignOf(u64) != 0) break;
                chain_candidate = next_candidate;
            }
            self.rememberFmt(
                "enum clone requires an integer tag slot: type={s} ptr=0x{x} first_word=0x{x} chain=0x{x},0x{x},0x{x},0x{x}",
                .{ type_name, value.raw_ptr, native_words[0], chain_words[0], chain_words[1], chain_words[2], chain_words[3] },
            );
            return error.RuntimeFailure;
        }
        const payload_ty = native_bridge.enumPayloadType(self, module, type_name, @intCast(src[0].integer)) orelse bytecode.TypeRef{ .kind = .void };
        const slots = try self.allocator.alloc(runtime_abi.Value, 2);
        errdefer self.allocator.free(slots);
        slots[0] = src[0];
        slots[1] = switch (payload_ty.kind) {
            .ffi_struct => blk: {
                if (src[1] != .raw_ptr or src[1].raw_ptr == 0) break :blk src[1];
                break :blk .{ .raw_ptr = try self.cloneStructValue(module, payload_ty.name orelse type_name, src[1].raw_ptr) };
            },
            .array => try self.cloneArrayValueDeep(module, try self.arrayElementType(module, payload_ty), src[1]),
            .enum_instance => try self.cloneEnumValue(module, payload_ty.name orelse type_name, src[1]),
            else => src[1],
        };
        return .{ .raw_ptr = try self.heap.registerStruct(type_name, slots) };
    }
};

test {
    _ = @import("vm_execution_tests.zig");
    _ = @import("vm_native_bridge_tests.zig");
}
