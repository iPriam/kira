//! VM-side direct FFI execution through LibFFI.
//!
//! This is the runtime that lets `kira run` (the pure VM, no LLVM) call
//! dynamically linked native functions. The bytecode compiler emits foreign
//! (`@FFI.Extern`) declarations as metadata-only stubs carrying the library
//! name, symbol, calling convention, parameter types, and return type. When the
//! interpreter reaches a `call_native` instruction it routes through the
//! `Dispatcher.hook` installed on `Vm.Hooks.call_native`, which:
//!
//!   1. looks up the foreign metadata for the called function id,
//!   2. opens (and caches) the named native library via the host loader,
//!   3. resolves the symbol to a function pointer,
//!   4. builds a LibFFI call signature from the declared FFI primitive types,
//!   5. marshals the VM argument values into LibFFI argument storage,
//!   6. invokes the function through LibFFI, and
//!   7. lifts the native result back into a runtime value.
//!
//! Parity note: the native/LLVM backend reaches the same C functions through
//! compiled trampolines and `dynamic_ffi_helpers.c` (which itself uses LibFFI);
//! the VM uses LibFFI directly. Both honour the same declared signatures.

const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const dynamic_ffi = @import("kira_dynamic_ffi");

/// Maps an FFI primitive type (resolved from `TypeRef.name`) onto the LibFFI
/// type model. Aggregate-by-value, callback, and unknown types are rejected so
/// the VM never silently mis-marshals a call.
fn mapPrimitive(ty: bytecode.TypeRef) ?dynamic_ffi.Type {
    const name = ty.name orelse {
        // Coarse kinds without a precise FFI primitive name.
        return switch (ty.kind) {
            .void => .void,
            .raw_ptr => .{ .pointer = .{} },
            .boolean => .bool,
            else => null,
        };
    };
    if (std.mem.eql(u8, name, "Void")) return .void;
    if (std.mem.eql(u8, name, "Bool")) return .bool;
    if (std.mem.eql(u8, name, "I8")) return .i8;
    if (std.mem.eql(u8, name, "U8")) return .u8;
    if (std.mem.eql(u8, name, "I16")) return .i16;
    if (std.mem.eql(u8, name, "U16")) return .u16;
    if (std.mem.eql(u8, name, "I32")) return .i32;
    if (std.mem.eql(u8, name, "U32")) return .u32;
    if (std.mem.eql(u8, name, "I64")) return .i64;
    if (std.mem.eql(u8, name, "U64")) return .u64;
    if (std.mem.eql(u8, name, "F32")) return .f32;
    if (std.mem.eql(u8, name, "F64")) return .f64;
    if (std.mem.eql(u8, name, "RawPtr")) return .{ .pointer = .{} };
    if (std.mem.eql(u8, name, "CString")) return .{ .pointer = .{} };
    return switch (ty.kind) {
        .void => .void,
        .raw_ptr => .{ .pointer = .{} },
        .boolean => .bool,
        else => null,
    };
}

/// True when the declared type is a NUL-terminated C string parameter, which
/// requires materialising a temporary `[:0]u8` for the duration of the call.
fn isCString(ty: bytecode.TypeRef) bool {
    if (ty.name) |name| return std.mem.eql(u8, name, "CString");
    return false;
}

pub const max_ffi_args = 32;

pub const Dispatcher = struct {
    allocator: std.mem.Allocator,
    module: *const bytecode.Module,
    /// library name -> filesystem path / loadable name (from resolved native libraries).
    library_paths: std.StringHashMapUnmanaged([]const u8) = .{},
    /// open library handles, keyed by library name.
    libraries: std.StringHashMapUnmanaged(dynamic_ffi.DynamicLibrary) = .{},
    /// resolved symbol pointers, keyed by "library\x00symbol".
    symbols: std.StringHashMapUnmanaged(*const anyopaque) = .{},
    libffi: ?dynamic_ffi.Libffi = null,
    error_message: ?[]const u8 = null,

    pub fn init(allocator: std.mem.Allocator, module: *const bytecode.Module) Dispatcher {
        return .{ .allocator = allocator, .module = module };
    }

    pub fn deinit(self: *Dispatcher) void {
        var lib_it = self.libraries.valueIterator();
        while (lib_it.next()) |library| library.close();
        self.libraries.deinit(self.allocator);

        var path_it = self.library_paths.iterator();
        while (path_it.next()) |entry| {
            self.allocator.free(entry.key_ptr.*);
            self.allocator.free(entry.value_ptr.*);
        }
        self.library_paths.deinit(self.allocator);

        var sym_it = self.symbols.keyIterator();
        while (sym_it.next()) |key| self.allocator.free(key.*);
        self.symbols.deinit(self.allocator);

        if (self.libffi) |*libffi| libffi.close();
        if (self.error_message) |message| self.allocator.free(message);
    }

    /// Registers the loadable path for a library name (owned copies are made).
    pub fn registerLibrary(self: *Dispatcher, name: []const u8, path: []const u8) !void {
        if (self.library_paths.contains(name)) return;
        const name_copy = try self.allocator.dupe(u8, name);
        errdefer self.allocator.free(name_copy);
        const path_copy = try self.allocator.dupe(u8, path);
        errdefer self.allocator.free(path_copy);
        try self.library_paths.put(self.allocator, name_copy, path_copy);
    }

    fn rememberError(self: *Dispatcher, comptime fmt: []const u8, args: anytype) void {
        if (self.error_message) |message| self.allocator.free(message);
        self.error_message = std.fmt.allocPrint(self.allocator, fmt, args) catch null;
    }

    pub fn lastError(self: *const Dispatcher) ?[]const u8 {
        return self.error_message;
    }

    fn ensureLibffi(self: *Dispatcher) !*dynamic_ffi.Libffi {
        if (self.libffi) |*libffi| return libffi;
        self.libffi = dynamic_ffi.Libffi.openManagedInstall(self.allocator) catch |err| {
            self.rememberError("LibFFI runtime unavailable ({s}); run `zig build fetch-libffi`", .{@errorName(err)});
            return error.LibffiUnavailable;
        };
        return &self.libffi.?;
    }

    fn openLibrary(self: *Dispatcher, name: []const u8) !*dynamic_ffi.DynamicLibrary {
        if (self.libraries.getPtr(name)) |existing| return existing;
        const load_path = self.library_paths.get(name) orelse name;
        const library = dynamic_ffi.DynamicLibrary.open(self.allocator, load_path) catch |err| {
            self.rememberError("could not load native library '{s}' (path '{s}'): {s}", .{ name, load_path, @errorName(err) });
            return error.NativeLibraryUnavailable;
        };
        const name_copy = try self.allocator.dupe(u8, name);
        errdefer self.allocator.free(name_copy);
        try self.libraries.put(self.allocator, name_copy, library);
        return self.libraries.getPtr(name).?;
    }

    fn resolveSymbol(self: *Dispatcher, library_name: []const u8, symbol_name: []const u8) !*const anyopaque {
        var key_buf: [512]u8 = undefined;
        const key = std.fmt.bufPrint(&key_buf, "{s}\x00{s}", .{ library_name, symbol_name }) catch {
            return error.SymbolNameTooLong;
        };
        if (self.symbols.get(key)) |existing| return existing;
        var library = try self.openLibrary(library_name);
        const symbol = library.lookup(*const anyopaque, symbol_name) catch |err| {
            self.rememberError("native symbol '{s}' missing in '{s}': {s}", .{ symbol_name, library_name, @errorName(err) });
            return error.MissingNativeSymbol;
        };
        const owned_key = try self.allocator.dupe(u8, key);
        errdefer self.allocator.free(owned_key);
        try self.symbols.put(self.allocator, owned_key, symbol);
        return symbol;
    }

    /// `Vm.Hooks.call_native` entry point. `context` must be a `*Dispatcher`.
    pub fn hook(context: ?*anyopaque, function_id: u32, args: []const runtime_abi.Value) anyerror!runtime_abi.Value {
        const self: *Dispatcher = @ptrCast(@alignCast(context orelse return error.FfiDispatcherMissing));
        return self.call(function_id, args);
    }

    fn call(self: *Dispatcher, function_id: u32, args: []const runtime_abi.Value) !runtime_abi.Value {
        const function = self.module.findFunctionById(function_id) orelse {
            self.rememberError("FFI call to unknown function id {d}", .{function_id});
            return error.UnknownFfiFunction;
        };
        const foreign = function.foreign orelse {
            self.rememberError("function '{s}' is not a foreign FFI binding", .{function.name});
            return error.NotForeignFunction;
        };
        if (foreign.calling_convention != .c) {
            self.rememberError("FFI calling convention '{s}' is not supported in the VM", .{@tagName(foreign.calling_convention)});
            return error.UnsupportedFfiCallingConvention;
        }
        if (args.len != function.param_types.len) {
            self.rememberError("FFI call to '{s}' expected {d} args, received {d}", .{ foreign.symbol_name, function.param_types.len, args.len });
            return error.FfiArgumentCountMismatch;
        }
        if (args.len > max_ffi_args) {
            self.rememberError("FFI call to '{s}' exceeds {d} arguments", .{ foreign.symbol_name, max_ffi_args });
            return error.TooManyFfiArguments;
        }

        var parameters: [max_ffi_args]dynamic_ffi.Parameter = undefined;
        for (function.param_types, 0..) |param_ty, index| {
            const mapped = mapPrimitive(param_ty) orelse {
                self.rememberError("FFI parameter {d} of '{s}' has an unsupported type", .{ index, foreign.symbol_name });
                return error.UnsupportedFfiType;
            };
            parameters[index] = .{ .name = "", .ty = mapped };
        }
        const result_type = mapPrimitive(function.return_type) orelse {
            self.rememberError("FFI return type of '{s}' is unsupported in the VM", .{foreign.symbol_name});
            return error.UnsupportedFfiType;
        };

        const libffi = try self.ensureLibffi();
        const function_ptr = try self.resolveSymbol(foreign.library_name, foreign.symbol_name);

        var prepared = libffi.prepare(self.allocator, .{
            .symbol = foreign.symbol_name,
            .abi = .c,
            .parameters = parameters[0..args.len],
            .result = result_type,
        }) catch |err| {
            self.rememberError("LibFFI could not prepare '{s}': {s}", .{ foreign.symbol_name, @errorName(err) });
            return error.FfiPrepareFailed;
        };
        defer prepared.deinit();

        // Stable argument storage for the duration of the call. CString
        // parameters borrow a NUL-terminated copy that is freed afterwards.
        var storage: [max_ffi_args]dynamic_ffi.ScalarStorage = undefined;
        var arg_ptrs: [max_ffi_args]?*anyopaque = undefined;
        var cstrings: [max_ffi_args]?[:0]u8 = .{null} ** max_ffi_args;
        defer for (cstrings) |maybe| if (maybe) |owned| self.allocator.free(owned);

        for (args, 0..) |arg, index| {
            const param_ty = function.param_types[index];
            try self.marshalArg(arg, param_ty, &storage[index], &cstrings[index]);
            arg_ptrs[index] = @ptrCast(&storage[index]);
        }

        var result_storage: dynamic_ffi.ScalarStorage = std.mem.zeroes(dynamic_ffi.ScalarStorage);
        const result_ptr: ?*anyopaque = if (function.return_type.kind == .void) null else @ptrCast(&result_storage);
        prepared.invoke(function_ptr, result_ptr, arg_ptrs[0..args.len]) catch |err| {
            self.rememberError("LibFFI call to '{s}' failed: {s}", .{ foreign.symbol_name, @errorName(err) });
            return error.FfiInvokeFailed;
        };

        return self.liftResult(function.return_type, result_storage);
    }

    fn marshalArg(
        self: *Dispatcher,
        value: runtime_abi.Value,
        param_ty: bytecode.TypeRef,
        storage: *dynamic_ffi.ScalarStorage,
        cstring_slot: *?[:0]u8,
    ) !void {
        storage.* = std.mem.zeroes(dynamic_ffi.ScalarStorage);
        if (isCString(param_ty)) {
            const text: []const u8 = switch (value) {
                .string => |s| s,
                .raw_ptr => |p| {
                    storage.pointer = p;
                    return;
                },
                .void => "",
                else => {
                    self.rememberError("CString FFI argument requires a string value", .{});
                    return error.FfiArgumentTypeMismatch;
                },
            };
            const owned = try self.allocator.dupeZ(u8, text);
            cstring_slot.* = owned;
            storage.pointer = @intFromPtr(owned.ptr);
            return;
        }

        switch (param_ty.kind) {
            .raw_ptr => storage.pointer = switch (value) {
                .raw_ptr => |p| p,
                .integer => |i| @intCast(i),
                .void => 0,
                else => return self.argTypeError(),
            },
            .float => switch (value) {
                .float => |f| {
                    if (param_ty.name) |name| {
                        if (std.mem.eql(u8, name, "F32")) {
                            storage.f32 = @floatCast(f);
                            return;
                        }
                    }
                    storage.f64 = f;
                },
                .integer => |i| storage.f64 = @floatFromInt(i),
                else => return self.argTypeError(),
            },
            .boolean => storage.u8 = switch (value) {
                .boolean => |b| @intFromBool(b),
                .integer => |i| if (i != 0) 1 else 0,
                else => return self.argTypeError(),
            },
            .integer => {
                const int_value: i64 = switch (value) {
                    .integer => |i| i,
                    .boolean => |b| @intFromBool(b),
                    else => return self.argTypeError(),
                };
                storage.u64 = @bitCast(int_value);
            },
            else => return self.argTypeError(),
        }
    }

    fn argTypeError(self: *Dispatcher) error{FfiArgumentTypeMismatch} {
        self.rememberError("FFI argument value does not match the declared parameter type", .{});
        return error.FfiArgumentTypeMismatch;
    }

    fn liftResult(self: *Dispatcher, return_ty: bytecode.TypeRef, storage: dynamic_ffi.ScalarStorage) runtime_abi.Value {
        _ = self;
        if (return_ty.kind == .void) return .{ .void = {} };
        if (return_ty.name) |name| {
            if (std.mem.eql(u8, name, "F32")) return .{ .float = storage.f32 };
            if (std.mem.eql(u8, name, "F64")) return .{ .float = storage.f64 };
            if (std.mem.eql(u8, name, "Bool")) return .{ .boolean = storage.u8 != 0 };
            if (std.mem.eql(u8, name, "RawPtr") or std.mem.eql(u8, name, "CString")) return .{ .raw_ptr = storage.pointer };
            if (std.mem.eql(u8, name, "U64")) return .{ .integer = @bitCast(storage.u64) };
            if (std.mem.eql(u8, name, "U32")) return .{ .integer = @intCast(storage.u32) };
            if (std.mem.eql(u8, name, "U16")) return .{ .integer = @intCast(storage.u16) };
            if (std.mem.eql(u8, name, "U8")) return .{ .integer = @intCast(storage.u8) };
            if (std.mem.eql(u8, name, "I64")) return .{ .integer = storage.i64 };
            if (std.mem.eql(u8, name, "I32")) return .{ .integer = @intCast(storage.i32) };
            if (std.mem.eql(u8, name, "I16")) return .{ .integer = @intCast(storage.i16) };
            if (std.mem.eql(u8, name, "I8")) return .{ .integer = @intCast(storage.i8) };
        }
        return switch (return_ty.kind) {
            .float => .{ .float = storage.f64 },
            .boolean => .{ .boolean = storage.u8 != 0 },
            .raw_ptr => .{ .raw_ptr = storage.pointer },
            else => .{ .integer = @bitCast(storage.u64) },
        };
    }
};

test "dispatcher rejects an unsupported by-value aggregate FFI type" {
    const allocator = std.testing.allocator;
    var functions = [_]bytecode.Function{.{
        .id = 0,
        .name = "takes_struct",
        .param_count = 1,
        .param_types = &.{.{ .kind = .ffi_struct, .name = "VkExtent2D" }},
        .return_type = .{ .kind = .void },
        .is_extern = true,
        .foreign = .{ .library_name = "novalib", .symbol_name = "takes_struct", .calling_convention = .c },
        .register_count = 0,
        .local_count = 0,
        .local_types = &.{},
        .instructions = &.{},
    }};
    const module = bytecode.Module{ .functions = &functions, .entry_function_id = null };

    var dispatcher = Dispatcher.init(allocator, &module);
    defer dispatcher.deinit();

    const args = [_]runtime_abi.Value{.{ .raw_ptr = 0 }};
    // The unsupported parameter type is rejected before any library or LibFFI
    // load is attempted, so the failure is deterministic and host-independent.
    try std.testing.expectError(error.UnsupportedFfiType, dispatcher.call(0, &args));
    try std.testing.expect(dispatcher.lastError() != null);
}

test "dispatcher rejects a non-C calling convention" {
    const allocator = std.testing.allocator;
    var functions = [_]bytecode.Function{.{
        .id = 0,
        .name = "weird_abi",
        .param_count = 0,
        .param_types = &.{},
        .return_type = .{ .kind = .void },
        .is_extern = true,
        .foreign = .{ .library_name = "novalib", .symbol_name = "weird_abi", .calling_convention = .kira_vm },
        .register_count = 0,
        .local_count = 0,
        .local_types = &.{},
        .instructions = &.{},
    }};
    const module = bytecode.Module{ .functions = &functions, .entry_function_id = null };

    var dispatcher = Dispatcher.init(allocator, &module);
    defer dispatcher.deinit();

    try std.testing.expectError(error.UnsupportedFfiCallingConvention, dispatcher.call(0, &.{}));
}

test "mapPrimitive resolves FFI primitive names" {
    try std.testing.expectEqual(dynamic_ffi.Type.i32, mapPrimitive(.{ .kind = .integer, .name = "I32" }).?);
    try std.testing.expectEqual(dynamic_ffi.Type.f32, mapPrimitive(.{ .kind = .float, .name = "F32" }).?);
    try std.testing.expectEqual(dynamic_ffi.Type.void, mapPrimitive(.{ .kind = .void }).?);
    try std.testing.expect(mapPrimitive(.{ .kind = .raw_ptr, .name = "RawPtr" }).? == .pointer);
    try std.testing.expect(mapPrimitive(.{ .kind = .string, .name = "CString" }).? == .pointer);
    try std.testing.expect(mapPrimitive(.{ .kind = .ffi_struct, .name = "VkExtent2D" }) == null);
}
