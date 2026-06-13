const std = @import("std");
const builtin = @import("builtin");
const DynamicLibrary = @import("dynamic_library.zig").DynamicLibrary;
const sig = @import("signature.zig");

const FfiStatus = c_uint;
const FfiAbi = c_uint;
const FfiType = anyopaque;
const FfiCifStorage = extern struct {
    words: [32]usize align(@alignOf(usize)),
};

const FfiPrepCifFn = *const fn (*FfiCifStorage, FfiAbi, c_uint, *FfiType, [*]*FfiType) callconv(.c) FfiStatus;
const FfiCallFn = *const fn (*FfiCifStorage, *const anyopaque, ?*anyopaque, [*]?*anyopaque) callconv(.c) void;

pub const ScalarStorage = extern union {
    i8: i8,
    u8: u8,
    i16: i16,
    u16: u16,
    i32: i32,
    u32: u32,
    i64: i64,
    u64: u64,
    f32: f32,
    f64: f64,
    pointer: usize,
};

pub const Value = struct {
    ty: sig.Type,
    storage: ScalarStorage,

    pub fn ptr(self: *Value) ?*anyopaque {
        if (self.ty == .void) return null;
        return @ptrCast(&self.storage);
    }
};

pub const Libffi = struct {
    library: DynamicLibrary,
    prep_cif: FfiPrepCifFn,
    call: FfiCallFn,
    type_void: *FfiType,
    type_uint8: *FfiType,
    type_sint8: *FfiType,
    type_uint16: *FfiType,
    type_sint16: *FfiType,
    type_uint32: *FfiType,
    type_sint32: *FfiType,
    type_uint64: *FfiType,
    type_sint64: *FfiType,
    type_float: *FfiType,
    type_double: *FfiType,
    type_pointer: *FfiType,

    pub fn open(allocator: std.mem.Allocator, path: []const u8) !Libffi {
        var library = try DynamicLibrary.open(allocator, path);
        errdefer library.close();
        return .{
            .library = library,
            .prep_cif = try library.lookup(FfiPrepCifFn, "ffi_prep_cif"),
            .call = try library.lookup(FfiCallFn, "ffi_call"),
            .type_void = try library.lookup(*FfiType, "ffi_type_void"),
            .type_uint8 = try library.lookup(*FfiType, "ffi_type_uint8"),
            .type_sint8 = try library.lookup(*FfiType, "ffi_type_sint8"),
            .type_uint16 = try library.lookup(*FfiType, "ffi_type_uint16"),
            .type_sint16 = try library.lookup(*FfiType, "ffi_type_sint16"),
            .type_uint32 = try library.lookup(*FfiType, "ffi_type_uint32"),
            .type_sint32 = try library.lookup(*FfiType, "ffi_type_sint32"),
            .type_uint64 = try library.lookup(*FfiType, "ffi_type_uint64"),
            .type_sint64 = try library.lookup(*FfiType, "ffi_type_sint64"),
            .type_float = try library.lookup(*FfiType, "ffi_type_float"),
            .type_double = try library.lookup(*FfiType, "ffi_type_double"),
            .type_pointer = try library.lookup(*FfiType, "ffi_type_pointer"),
        };
    }

    pub fn close(self: *Libffi) void {
        self.library.close();
    }

    pub fn openInstall(allocator: std.mem.Allocator, install_home: []const u8) !Libffi {
        const library_path = try findLibraryPath(allocator, install_home);
        defer allocator.free(library_path);
        return open(allocator, library_path);
    }

    pub fn openManagedInstall(allocator: std.mem.Allocator) !Libffi {
        const install_home = try managedInstallHome(allocator);
        defer allocator.free(install_home);
        return openInstall(allocator, install_home);
    }

    pub fn prepare(self: *Libffi, allocator: std.mem.Allocator, signature: sig.Signature) !PreparedCall {
        if (sig.validateSignature(signature)) |_| return error.InvalidFfiSignature;
        var arg_types = try allocator.alloc(*FfiType, signature.parameters.len);
        errdefer allocator.free(arg_types);
        for (signature.parameters, 0..) |param, index| arg_types[index] = try self.mapType(param.ty);
        var cif: FfiCifStorage = undefined;
        @memset(std.mem.asBytes(&cif), 0);
        const status = self.prep_cif(&cif, try abiValue(signature.abi), @intCast(arg_types.len), try self.mapType(signature.result), arg_types.ptr);
        if (status != 0) return error.LibffiPrepareFailed;
        return .{
            .allocator = allocator,
            .cif = cif,
            .arg_types = arg_types,
            .result_type = signature.result,
            .call = self.call,
        };
    }

    fn mapType(self: *Libffi, ty: sig.Type) !*FfiType {
        return switch (ty) {
            .void => self.type_void,
            .bool, .u8 => self.type_uint8,
            .i8 => self.type_sint8,
            .u16 => self.type_uint16,
            .i16 => self.type_sint16,
            .u32, .bitflags => self.type_uint32,
            .i32, .enumeration => self.type_sint32,
            .u64, .handle => self.type_uint64,
            .i64 => self.type_sint64,
            .f32 => self.type_float,
            .f64 => self.type_double,
            .pointer, .callback => self.type_pointer,
            .structure, .union_, .array => error.UnsupportedAggregateByValue,
        };
    }
};

pub fn findLibraryPath(allocator: std.mem.Allocator, install_home: []const u8) ![]u8 {
    return switch (builtin.os.tag) {
        .windows => findLibraryInDir(allocator, install_home, "lib", &.{".dll"}) catch
            findLibraryInDir(allocator, install_home, "bin", &.{".dll"}),
        .linux => findLibraryInDir(allocator, install_home, "lib", &.{".so"}) catch
            findLibraryInDir(allocator, install_home, "lib64", &.{".so"}),
        .macos => findLibraryInDir(allocator, install_home, "lib", &.{".dylib"}),
        else => error.UnsupportedLibffiHost,
    };
}

fn findLibraryInDir(
    allocator: std.mem.Allocator,
    install_home: []const u8,
    relative_dir: []const u8,
    allowed_suffixes: []const []const u8,
) ![]u8 {
    const lib_dir_path = try std.fs.path.join(allocator, &.{ install_home, relative_dir });
    defer allocator.free(lib_dir_path);

    var lib_dir = try std.Io.Dir.openDirAbsolute(std.Options.debug_io, lib_dir_path, .{ .iterate = true });
    defer lib_dir.close(std.Options.debug_io);

    var iterator = lib_dir.iterate();
    while (try iterator.next(std.Options.debug_io)) |entry| {
        if (entry.kind != .file and entry.kind != .sym_link) continue;
        if (!std.mem.startsWith(u8, entry.name, "libffi")) continue;
        for (allowed_suffixes) |suffix| {
            if (std.mem.endsWith(u8, entry.name, suffix)) {
                return std.fs.path.join(allocator, &.{ lib_dir_path, entry.name });
            }
        }
    }
    return error.LibffiLibraryNotFound;
}

pub const PreparedCall = struct {
    allocator: std.mem.Allocator,
    cif: FfiCifStorage,
    arg_types: []*FfiType,
    result_type: sig.Type,
    call: FfiCallFn,

    pub fn deinit(self: *PreparedCall) void {
        self.allocator.free(self.arg_types);
    }

    pub fn invoke(self: *PreparedCall, function: *const anyopaque, result: ?*anyopaque, args: []?*anyopaque) !void {
        if (args.len != self.arg_types.len) return error.FfiArgumentCountMismatch;
        self.call(&self.cif, function, result, args.ptr);
    }
};

fn abiValue(abi: sig.Abi) !FfiAbi {
    if (builtin.target.os.tag == .windows and builtin.target.cpu.arch == .x86_64) {
        return switch (abi) {
            .c, .system, .win64 => 1,
            else => error.UnsupportedAbiForTarget,
        };
    }
    if (builtin.target.cpu.arch == .x86_64) {
        return switch (abi) {
            .c, .system, .unix64, .sysv => 2,
            .win64 => 3,
            else => error.UnsupportedAbiForTarget,
        };
    }
    return switch (abi) {
        .c, .system, .sysv, .aarch64 => 1,
        else => error.UnsupportedAbiForTarget,
    };
}

test "maps supported ABI labels to deterministic libffi ids" {
    if (builtin.target.os.tag == .windows and builtin.target.cpu.arch == .x86_64) {
        try std.testing.expectEqual(@as(FfiAbi, 1), try abiValue(.c));
        try std.testing.expectEqual(@as(FfiAbi, 1), try abiValue(.win64));
        try std.testing.expectError(error.UnsupportedAbiForTarget, abiValue(.unix64));
    } else if (builtin.target.cpu.arch == .x86_64) {
        try std.testing.expectEqual(@as(FfiAbi, 2), try abiValue(.c));
        try std.testing.expectEqual(@as(FfiAbi, 3), try abiValue(.win64));
    } else {
        try std.testing.expectEqual(@as(FfiAbi, 1), try abiValue(.c));
    }
}

test "invokes ffi_get_version through managed LibFFI when available" {
    const install_home = managedInstallHome(std.testing.allocator) catch |err| switch (err) {
        error.EnvironmentVariableNotFound, error.UnsupportedLibffiHost => return error.SkipZigTest,
        else => |other| return other,
    };
    defer std.testing.allocator.free(install_home);
    if (!dirExistsAbsolute(install_home)) return error.SkipZigTest;

    var libffi = Libffi.openInstall(std.testing.allocator, install_home) catch |err| switch (err) {
        error.LibffiLibraryNotFound, error.NativeLibraryLoadFailed => return error.SkipZigTest,
        else => |other| return other,
    };
    defer libffi.close();

    const function = libffi.library.lookup(*const anyopaque, "ffi_get_version") catch |err| switch (err) {
        error.MissingNativeSymbol => return error.SkipZigTest,
        else => |other| return other,
    };
    var prepared = try libffi.prepare(std.testing.allocator, .{
        .symbol = "ffi_get_version",
        .abi = .system,
        .parameters = &.{},
        .result = .{ .pointer = .{} },
    });
    defer prepared.deinit();

    var result: usize = 0;
    try prepared.invoke(function, &result, &.{});
    try std.testing.expect(result != 0);
    const version: [*:0]const u8 = @ptrFromInt(result);
    try std.testing.expect(std.mem.startsWith(u8, std.mem.span(version), "3."));
}

pub fn managedInstallHome(allocator: std.mem.Allocator) anyerror![]u8 {
    const host_key = switch (builtin.target.os.tag) {
        .windows => switch (builtin.target.cpu.arch) {
            .x86_64 => "x86_64-windows-msvc",
            else => return error.UnsupportedLibffiHost,
        },
        .linux => switch (builtin.target.cpu.arch) {
            .x86_64 => "x86_64-linux-gnu",
            else => return error.UnsupportedLibffiHost,
        },
        .macos => switch (builtin.target.cpu.arch) {
            .aarch64 => "aarch64-macos",
            else => return error.UnsupportedLibffiHost,
        },
        else => return error.UnsupportedLibffiHost,
    };
    const home = envVarOwned(allocator, if (builtin.os.tag == .windows) "USERPROFILE" else "HOME") catch
        return error.EnvironmentVariableNotFound;
    defer allocator.free(home);
    return std.fs.path.join(allocator, &.{ home, ".kira", "toolchains", "libffi", "3.5.2", host_key });
}

fn envVarOwned(allocator: std.mem.Allocator, name: []const u8) ![]u8 {
    if (builtin.os.tag == .windows or (builtin.os.tag == .wasi and !builtin.link_libc)) {
        var environ = try std.process.Environ.createMap(.{ .block = .global }, allocator);
        defer environ.deinit();
        const value = environ.get(name) orelse return error.EnvironmentVariableNotFound;
        return allocator.dupe(u8, value);
    }

    if (builtin.link_libc) {
        const name_z = try allocator.dupeZ(u8, name);
        defer allocator.free(name_z);
        const value = std.c.getenv(name_z.ptr) orelse return error.EnvironmentVariableNotFound;
        return allocator.dupe(u8, std.mem.span(value));
    }

    return error.EnvironmentVariableNotFound;
}

fn dirExistsAbsolute(path: []const u8) bool {
    var dir = std.Io.Dir.openDirAbsolute(std.Options.debug_io, path, .{}) catch return false;
    dir.close(std.Options.debug_io);
    return true;
}
