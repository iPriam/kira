const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");

pub fn materializeFromNativeIfNeeded(
    vm: anytype,
    module: *const bytecode.Module,
    value_type: bytecode.TypeRef,
    raw_ptr: usize,
) anyerror!runtime_abi.Value {
    if (raw_ptr == 0) return .{ .raw_ptr = 0 };
    if (vm.isManagedStructPointer(raw_ptr)) return .{ .raw_ptr = raw_ptr };
    const type_name = resolveNativeTypeName(module, value_type, raw_ptr) orelse return .{ .raw_ptr = raw_ptr };
    return .{ .raw_ptr = try vm.copyStructFromNativeLayout(module, type_name, raw_ptr) };
}

fn resolveNativeTypeName(module: *const bytecode.Module, value_type: bytecode.TypeRef, raw_ptr: usize) ?[]const u8 {
    if (raw_ptr <= @sizeOf(u64) or runtime_abi.isTaggedNativeClosurePointer(raw_ptr)) return null;
    const type_id_ptr: *const u64 = @ptrFromInt(raw_ptr - @sizeOf(u64));
    const type_id = type_id_ptr.*;

    if (value_type.construct_constraint) |constraint| {
        for (module.construct_implementations) |implementation| {
            if (!std.mem.eql(u8, implementation.construct_constraint.construct_name, constraint.construct_name)) continue;
            if (nativeStateTypeId(implementation.type_name) == type_id) return implementation.type_name;
        }
        return null;
    }

    for (module.types) |type_decl| {
        if (nativeStateTypeId(type_decl.name) == type_id) return type_decl.name;
    }
    return null;
}

fn nativeStateTypeId(type_name: []const u8) u64 {
    var hash: u64 = 14695981039346656037;
    for (type_name) |byte| {
        hash ^= @as(u64, byte);
        hash *%= 1099511628211;
    }
    return hash & 0x7fff_ffff_ffff_ffff;
}
