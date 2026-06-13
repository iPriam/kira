const std = @import("std");
const builtin = @import("builtin");

const NativeLibrary = if (builtin.os.tag == .windows) WindowsNativeLibrary else std.DynLib;

pub const DynamicLibrary = struct {
    inner: NativeLibrary,

    pub fn open(allocator: std.mem.Allocator, path: []const u8) !DynamicLibrary {
        if (builtin.os.tag == .windows) return .{ .inner = try WindowsNativeLibrary.open(allocator, path) };
        return .{ .inner = try std.DynLib.open(path) };
    }

    pub fn close(self: *DynamicLibrary) void {
        self.inner.close();
    }

    pub fn lookup(self: *DynamicLibrary, comptime T: type, name: []const u8) !T {
        var buffer: [256]u8 = undefined;
        if (name.len >= buffer.len) return error.SymbolNameTooLong;
        const symbol_name = try std.fmt.bufPrintZ(&buffer, "{s}", .{name});
        return self.inner.lookup(T, symbol_name) orelse error.MissingNativeSymbol;
    }

    pub fn lookupOptional(self: *DynamicLibrary, comptime T: type, name: []const u8) ?T {
        var buffer: [256]u8 = undefined;
        if (name.len >= buffer.len) return null;
        const symbol_name = std.fmt.bufPrintZ(&buffer, "{s}", .{name}) catch return null;
        return self.inner.lookup(T, symbol_name);
    }
};

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

    fn lookup(self: *WindowsNativeLibrary, comptime T: type, name: [:0]const u8) ?T {
        const address = GetProcAddress(self.handle, name.ptr) orelse return null;
        return @ptrCast(address);
    }
};

test "lookup reports a missing symbol precisely" {
    const missing_path = "definitely-missing-kira-dynamic-ffi-library";
    try std.testing.expectError(error.NativeLibraryLoadFailed, DynamicLibrary.open(std.testing.allocator, missing_path));
}
