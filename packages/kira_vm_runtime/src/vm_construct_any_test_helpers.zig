const std = @import("std");
const bytecode = @import("kira_bytecode");
const native_layout = @import("native_layout.zig");
const native_bridge = @import("vm_native_bridge.zig");
const Vm = @import("vm.zig").Vm;

pub const HeaderedNativeStruct = struct {
    header_ptr: usize,
    payload_ptr: usize,
    word_count: usize,
};

pub fn allocateHeaderedNativeStruct(
    vm: *Vm,
    module: *const bytecode.Module,
    type_name: []const u8,
    runtime_ptr: usize,
) !HeaderedNativeStruct {
    const plain_payload_ptr = try vm.lowerStructToNativeLayout(module, type_name, runtime_ptr);
    errdefer vm.destroyStructNativeLayout(module, type_name, plain_payload_ptr);

    const layout = try native_layout.structLayout(module, type_name);
    const total_bytes = @sizeOf(u64) + layout.size;
    const word_count = @max(1, std.math.divCeil(usize, total_bytes, @sizeOf(u64)) catch unreachable);
    const words = try vm.allocator.alloc(u64, word_count);
    errdefer vm.allocator.free(words);
    @memset(words, 0);
    words[0] = nativeStateTypeId(type_name);

    const dst_bytes: [*]u8 = @ptrCast(words.ptr);
    const src_bytes: [*]const u8 = @ptrFromInt(plain_payload_ptr);
    std.mem.copyForwards(u8, dst_bytes[@sizeOf(u64) .. @sizeOf(u64) + layout.size], src_bytes[0..layout.size]);

    vm.destroyStructNativeLayout(module, type_name, plain_payload_ptr);
    return .{
        .header_ptr = @intFromPtr(words.ptr),
        .payload_ptr = @intFromPtr(words.ptr) + @sizeOf(u64),
        .word_count = word_count,
    };
}

pub fn destroyHeaderedNativeStruct(
    vm: *Vm,
    module: *const bytecode.Module,
    type_name: []const u8,
    native_struct: HeaderedNativeStruct,
) void {
    if (native_struct.payload_ptr == 0 or native_struct.header_ptr == 0) return;
    native_bridge.destroyStructNativeLayoutFields(vm, module, type_name, native_struct.payload_ptr);
    const words: [*]u64 = @ptrFromInt(native_struct.header_ptr);
    vm.allocator.free(words[0..native_struct.word_count]);
}

fn nativeStateTypeId(type_name: []const u8) u64 {
    var hash: u64 = 14695981039346656037;
    for (type_name) |byte| {
        hash ^= @as(u64, byte);
        hash *%= 1099511628211;
    }
    return hash & 0x7fff_ffff_ffff_ffff;
}
