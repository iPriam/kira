// Closure construction and closure-call lowering for the LLVM C-API backend. Split out
// of backend_capi_codegen.zig (Core Law #5). Free functions over *FunctionCodegen,
// matching backend_capi_aggregate.zig's style.
const std = @import("std");
const ir = @import("kira_ir");
const llvm = @import("llvm_c.zig");
const drop = @import("backend_capi_drop.zig");
const dispatch = @import("backend_capi_dispatch.zig");
const FunctionCodegen = @import("backend_capi_codegen.zig").FunctionCodegen;

// A closure is a heap block { i64 fn_id; i64 count; [count x bridge] captures } whose
// i64 register carries the high bit set as a closure tag. The UNTAGGED heap pointer is
// recorded for drop (freeing the tagged value would corrupt the heap).
pub fn lowerConstClosure(fc: *FunctionCodegen, v: ir.ConstClosure) !void {
    const api = fc.api;
    const b = fc.builder;
    const n = v.captures.len;
    const captures_arr_ty = api.LLVMArrayType2(fc.types.bridge_ty, n);
    const captures_size = api.LLVMSizeOf(captures_arr_ty);
    const total = api.LLVMBuildAdd(b, api.LLVMConstInt(fc.types.i64, 16, 0), captures_size, "closure.size");
    var margs = [_]llvm.c.LLVMValueRef{total};
    const ptr = api.LLVMBuildCall2(b, fc.runtime_decls.malloc.ty, fc.runtime_decls.malloc.fn_value, &margs, margs.len, "closure.alloc");
    var id_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 0, 0)};
    const id_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, ptr, &id_idx, id_idx.len, "closure.id.slot");
    _ = api.LLVMBuildStore(b, api.LLVMConstInt(fc.types.i64, v.function_id, 0), id_slot);
    var count_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 1, 0)};
    const count_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, ptr, &count_idx, count_idx.len, "closure.count.slot");
    _ = api.LLVMBuildStore(b, api.LLVMConstInt(fc.types.i64, n, 0), count_slot);
    var slots_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 16, 0)};
    const slots = api.LLVMBuildInBoundsGEP2(b, fc.types.i8, ptr, &slots_idx, slots_idx.len, "closure.slots");
    for (v.captures, 0..) |capture_reg, index| {
        var slot_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, index, 0)};
        const slot = api.LLVMBuildInBoundsGEP2(b, fc.types.bridge_ty, slots, &slot_idx, slot_idx.len, "closure.slot");
        const bv = try fc.packBridgeBoxed(fc.register_types[capture_reg], fc.registers[capture_reg], false);
        _ = api.LLVMBuildStore(b, bv, slot);
    }
    const raw = api.LLVMBuildPtrToInt(b, ptr, fc.types.i64, "closure.raw");
    // Set the high bit (0x8000000000000000) to tag this i64 as a closure pointer.
    fc.registers[v.dst] = api.LLVMBuildOr(b, raw, api.LLVMConstInt(fc.types.i64, 0x8000000000000000, 0), "closure.tagged");
    // Record the UNTAGGED heap pointer for drop (freeing the tagged value would
    // corrupt the heap).
    drop.onAllocPointer(fc, v.dst, ptr);
}

pub fn lowerCallValue(fc: *FunctionCodegen, v: ir.CallValue) !void {
    const api = fc.api;
    const b = fc.builder;
    const hash = dispatch.hashCallValueSignature(v.param_types, v.return_type);
    const decl = fc.dispatchers.get(hash) orelse return error.MissingFunctionDeclaration;
    const args = try fc.allocator.alloc(llvm.c.LLVMValueRef, v.args.len + 1);
    defer fc.allocator.free(args);
    args[0] = fc.registers[v.callee];
    for (v.args, 0..) |arg, index| args[index + 1] = fc.registers[arg];
    const result = api.LLVMBuildCall2(b, decl.fn_ty, decl.fn_value, args.ptr, @intCast(args.len), "");
    // NOTE: arguments are intentionally NOT drop-escaped here. The call_value dispatcher
    // does not run a callee owned-param drop the way a direct Call does, so escaping an
    // owned arg (e.g. a closure passed by value) would leak it — nothing would free it.
    // The caller keeps ownership (borrow-pass); conservative — never a double-free.
    // (ir.CallValue.param_ownership is plumbed through for when the dispatcher learns to
    // take ownership of owned/move args and free them.)
    if (v.dst) |dst| {
        fc.registers[dst] = result;
        // A native callback's owned-aggregate result is fresh caller-stable heap the caller
        // drops. In HYBRID, a call through a runtime callback returns a VM-OWNED value (the
        // VM allocated and still tracks it); tracking it for the native drop would free it
        // twice (native + VM). Skip tracking in hybrid — conservative: may leak, never
        // double-frees.
        if (fc.request.mode != .hybrid) {
            switch (v.return_type.kind) {
                .ffi_struct, .array => drop.onAlloc(fc, dst),
                else => {},
            }
        }
    }
}
