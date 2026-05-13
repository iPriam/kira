const std = @import("std");
const builtin = @import("builtin");
const hybrid = @import("kira_hybrid_definition");
const runtime_abi = @import("kira_runtime_abi");
const symbol_resolver = @import("symbol_resolver.zig");
const trampoline = @import("trampoline.zig");

pub const RuntimeInvoker = *const fn (?*anyopaque, u32, []const runtime_abi.BridgeValue, *runtime_abi.BridgeValue) anyerror!void;
const InstallRuntimeInvokerFn = *const fn (*const fn (u32, ?[*]const runtime_abi.BridgeValue, u32, *runtime_abi.BridgeValue) callconv(.c) void) callconv(.c) void;
const InstallArrayAllocatorFn = *const fn (*const fn (usize) callconv(.c) ?*anyopaque, *const fn (?*anyopaque, usize) callconv(.c) void) callconv(.c) void;
const SetTraceEnabledFn = *const fn (u8) callconv(.c) void;

var active_runtime_context: ?*anyopaque = null;
var active_runtime_invoker: ?RuntimeInvoker = null;
var active_array_allocator: ?std.mem.Allocator = null;
const NativeLibrary = if (builtin.os.tag == .windows) WindowsNativeLibrary else std.DynLib;

pub const NativeBridge = struct {
    allocator: std.mem.Allocator,
    library: ?NativeLibrary = null,
    trampolines: std.AutoHashMapUnmanaged(u32, trampoline.Trampoline) = .{},

    pub fn init(allocator: std.mem.Allocator) NativeBridge {
        return .{ .allocator = allocator };
    }

    pub fn deinit(self: *NativeBridge) void {
        self.trampolines.deinit(self.allocator);
        if (self.library) |*library| library.close();
        active_array_allocator = null;
    }

    pub fn bind(self: *NativeBridge, library_path: []const u8, descriptors: []const hybrid.BridgeDescriptor) !void {
        var library = try openNativeLibrary(self.allocator, library_path);
        errdefer library.close();

        for (descriptors) |descriptor| {
            const symbol_name_z = try self.allocator.dupeZ(u8, descriptor.symbol_name);
            const invoke = try symbol_resolver.resolveSymbol(&library, symbol_name_z);
            try self.trampolines.put(self.allocator, descriptor.function_id.value, .{
                .function_id = descriptor.function_id.value,
                .symbol_name = descriptor.symbol_name,
                .invoke = invoke,
            });
        }

        const install_invoker = library.lookup(InstallRuntimeInvokerFn, "kira_hybrid_install_runtime_invoker") orelse return error.MissingRuntimeInvokerInstaller;
        install_invoker(kira_hybrid_host_call_runtime);
        active_array_allocator = self.allocator;
        if (library.lookup(InstallArrayAllocatorFn, "kira_hybrid_install_array_allocator")) |install_array_allocator| {
            install_array_allocator(kira_hybrid_array_alloc, kira_hybrid_array_free);
        }
        if (library.lookup(SetTraceEnabledFn, "kira_set_execution_trace_enabled")) |set_trace_enabled| {
            set_trace_enabled(if (runtime_abi.executionTraceEnabled()) 1 else 0);
        }

        self.library = library;
    }

    pub fn call(self: *NativeBridge, function_id: u32, args: []const runtime_abi.Value) !runtime_abi.Value {
        const tramp = self.trampolines.get(function_id) orelse return error.MissingNativeTrampoline;
        runtime_abi.emitExecutionTrace("BRIDGE", "CALL", "runtime->native fn={d} symbol={s} args={d}", .{
            function_id,
            tramp.symbol_name,
            args.len,
        });
        const lowered_args = try self.allocator.alloc(runtime_abi.BridgeValue, args.len);
        defer self.allocator.free(lowered_args);
        for (args, 0..) |arg, index| lowered_args[index] = runtime_abi.bridgeValueFromValue(arg);

        var result = runtime_abi.BridgeValue{
            .tag = .void,
            .payload = .{ .raw_ptr = 0 },
        };
        tramp.invoke(if (lowered_args.len == 0) null else lowered_args.ptr, @intCast(lowered_args.len), &result);
        const lifted = runtime_abi.bridgeValueToValue(result);
        runtime_abi.emitExecutionTrace("BRIDGE", "RETURN", "native->runtime fn={d} symbol={s} tag={s}", .{
            function_id,
            tramp.symbol_name,
            @tagName(result.tag),
        });
        return lifted;
    }

    pub fn resolveImplementationPointer(self: *NativeBridge, function_id: u32) !usize {
        var library = self.library orelse return error.MissingNativeTrampoline;
        var buffer: [64]u8 = undefined;
        const symbol_name = try std.fmt.bufPrintZ(&buffer, "kira_native_impl_{d}", .{function_id});
        const symbol = library.lookup(*const anyopaque, symbol_name) orelse return error.MissingNativeSymbol;
        return @intFromPtr(symbol);
    }
};

fn openNativeLibrary(allocator: std.mem.Allocator, path: []const u8) !NativeLibrary {
    if (builtin.os.tag == .windows) {
        return WindowsNativeLibrary.open(allocator, path);
    }
    return std.DynLib.open(path);
}

const WindowsNativeLibrary = struct {
    handle: std.os.windows.HMODULE,

    const LOAD_WITH_ALTERED_SEARCH_PATH = 0x00000008;
    extern "kernel32" fn LoadLibraryExW([*:0]const u16, ?std.os.windows.HANDLE, u32) callconv(.winapi) ?std.os.windows.HMODULE;
    extern "kernel32" fn FreeLibrary(std.os.windows.HMODULE) callconv(.winapi) std.os.windows.BOOL;
    extern "kernel32" fn GetProcAddress(std.os.windows.HMODULE, [*:0]const u8) callconv(.winapi) ?*anyopaque;

    fn open(allocator: std.mem.Allocator, path: []const u8) !WindowsNativeLibrary {
        const path_w = try std.unicode.wtf8ToWtf16LeAllocZ(allocator, path);
        defer allocator.free(path_w);
        const handle = LoadLibraryExW(path_w.ptr, null, LOAD_WITH_ALTERED_SEARCH_PATH) orelse return error.NativeLibraryLoadFailed;
        return .{ .handle = handle };
    }

    fn close(self: *WindowsNativeLibrary) void {
        _ = FreeLibrary(self.handle);
    }

    pub fn lookup(self: *WindowsNativeLibrary, comptime T: type, name: [:0]const u8) ?T {
        const address = GetProcAddress(self.handle, name.ptr) orelse return null;
        return @ptrCast(address);
    }
};

pub fn installRuntimeInvoker(context: ?*anyopaque, invoker: RuntimeInvoker) void {
    active_runtime_context = context;
    active_runtime_invoker = invoker;
}

pub fn clearRuntimeInvoker() void {
    active_runtime_context = null;
    active_runtime_invoker = null;
}

pub export fn kira_hybrid_host_call_runtime(
    function_id: u32,
    args: ?[*]const runtime_abi.BridgeValue,
    arg_count: u32,
    out_result: *runtime_abi.BridgeValue,
) callconv(.c) void {
    const invoker = active_runtime_invoker orelse @panic("hybrid runtime invoker not installed");
    const slice = if (args) |ptr| ptr[0..arg_count] else &.{};
    invoker(active_runtime_context, function_id, slice, out_result) catch |err| {
        std.debug.panic("hybrid runtime call failed: {s}", .{@errorName(err)});
    };
}

fn kira_hybrid_array_alloc(size: usize) callconv(.c) ?*anyopaque {
    const allocator = active_array_allocator orelse return null;
    const word_count = std.math.divCeil(usize, @max(size, 1), @sizeOf(u64)) catch return null;
    const words = allocator.alloc(u64, @max(word_count, 1)) catch return null;
    return @ptrCast(words.ptr);
}

fn kira_hybrid_array_free(ptr: ?*anyopaque, size: usize) callconv(.c) void {
    const raw_ptr = ptr orelse return;
    const allocator = active_array_allocator orelse return;
    const word_count = std.math.divCeil(usize, @max(size, 1), @sizeOf(u64)) catch return;
    const words: [*]u64 = @ptrCast(@alignCast(raw_ptr));
    allocator.free(words[0..@max(word_count, 1)]);
}
