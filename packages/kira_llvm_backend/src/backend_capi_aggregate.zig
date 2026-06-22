// Enum and native-state construction for the LLVM C-API backend. Split out of
// backend_capi_codegen.zig (Core Law #5). Free functions over *FunctionCodegen.
const std = @import("std");
const ir = @import("kira_ir");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const drop = @import("backend_capi_drop.zig");
const FunctionCodegen = @import("backend_capi_codegen.zig").FunctionCodegen;

pub fn lowerAllocNativeState(fc: *FunctionCodegen, v: ir.AllocNativeState) !void {
    const api = fc.api;
    const b = fc.builder;
    const type_decl = utils.findTypeDecl(fc.request.program.programPtr(), v.type_name) orelse return error.UnsupportedExecutableFeature;
    const struct_ty = fc.struct_types.get(v.type_name) orelse return error.UnsupportedExecutableFeature;
    const payload_arr_ty = api.LLVMArrayType2(fc.types.bridge_ty, type_decl.fields.len);
    const size = api.LLVMSizeOf(payload_arr_ty);
    var alloc_args = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(fc.types.i64, v.type_id, 0), size };
    const box = api.LLVMBuildCall2(b, fc.runtime_decls.state_alloc.ty, fc.runtime_decls.state_alloc.fn_value, &alloc_args, alloc_args.len, "state.box");
    var pl_args = [_]llvm.c.LLVMValueRef{box};
    const payload = api.LLVMBuildCall2(b, fc.runtime_decls.state_payload.ty, fc.runtime_decls.state_payload.fn_value, &pl_args, pl_args.len, "state.payload");
    const src = api.LLVMBuildIntToPtr(b, fc.registers[v.src], fc.types.ptr_ty, "state.src");
    for (type_decl.fields, 0..) |field_decl, index| {
        var f_idx = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(fc.types.i32, 0, 0), api.LLVMConstInt(fc.types.i32, @intCast(index), 0) };
        const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, src, &f_idx, f_idx.len, "state.field.ptr");
        const field_value = if (field_decl.ty.kind == .ffi_struct)
            api.LLVMBuildPtrToInt(b, field_ptr, fc.types.i64, "state.field.struct")
        else
            try fc.loadConverted(field_ptr, field_decl.ty);
        const bv = try fc.packBridge(field_decl.ty, field_value);
        var s_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, @intCast(index), 0)};
        const slot = api.LLVMBuildInBoundsGEP2(b, fc.types.bridge_ty, payload, &s_idx, s_idx.len, "state.slot");
        _ = api.LLVMBuildStore(b, bv, slot);
    }
    fc.registers[v.dst] = api.LLVMBuildPtrToInt(b, box, fc.types.i64, "state.box.int");
}

pub fn lowerStoreIndirect(fc: *FunctionCodegen, v: ir.StoreIndirect) !void {
    const api = fc.api;
    const b = fc.builder;
    const ptr = api.LLVMBuildIntToPtr(b, fc.registers[v.ptr], fc.types.ptr_ty, "store.ptr");
    const src = fc.registers[v.src];
    sw: switch (v.ty.kind) {
        .integer => {
            const storage = try fc.storageType(v.ty);
            const value = if (storage == fc.types.i64) src else api.LLVMBuildTrunc(b, src, storage, "store.trunc");
            _ = api.LLVMBuildStore(b, value, ptr);
        },
        .float, .string => _ = api.LLVMBuildStore(b, src, ptr),
        .boolean => {
            const value = api.LLVMBuildZExt(b, src, fc.types.i8, "store.bool");
            _ = api.LLVMBuildStore(b, value, ptr);
        },
        .array => {
            if (!fc.drop_enabled) {
                const value = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.rawptr");
                _ = api.LLVMBuildStore(b, value, ptr);
                break :sw;
            }
            // Self-store is a no-op. The `var x = obj.arr; x[i] = ...; obj.arr = x` idiom
            // aliases the field into `x`, mutates in place, then writes the SAME array back.
            // Cloning here (x reads as borrowed) would store a fresh copy and orphan the
            // original the field still pointed at — a per-call leak that is quadratic in a
            // loop. When the source already is the field's current array, leave it untouched.
            const old = api.LLVMBuildLoad2(b, fc.types.ptr_ty, ptr, "store.arr.prev");
            const newp = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.arr.newp");
            const same = api.LLVMBuildICmp(b, llvm.c.LLVMIntEQ, old, newp, "store.arr.same");
            const work_block = api.LLVMAppendBasicBlockInContext(fc.types.context, fc.function_value, "store.arr.work");
            const done_block = api.LLVMAppendBasicBlockInContext(fc.types.context, fc.function_value, "store.arr.done");
            _ = api.LLVMBuildCondBr(b, same, done_block, work_block);
            api.LLVMPositionBuilderAtEnd(b, work_block);
            // Drop-before-overwrite: free the field's (different) prior array. Borrow-checking
            // keeps any borrowed read of the old value from being live across this store.
            const reldtor = fc.dtors.elementDestroy(fc.request.program.programPtr(), v.ty);
            var rargs = [_]llvm.c.LLVMValueRef{ old, reldtor orelse api.LLVMConstNull(fc.types.ptr_ty) };
            _ = api.LLVMBuildCall2(b, fc.runtime_decls.array_release.ty, fc.runtime_decls.array_release.fn_value, &rargs, rargs.len, "");
            if (!drop.isOwned(fc, v.src)) {
                // Borrowed array into an owned field: deep clone so the struct owns
                // independent storage (its destructor frees the clone; the original is untouched).
                const sptr = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.arr.src");
                const elem = fc.dtors.elementClone(fc.request.program.programPtr(), v.ty);
                var cargs = [_]llvm.c.LLVMValueRef{ sptr, elem orelse api.LLVMConstNull(fc.types.ptr_ty) };
                const clone = api.LLVMBuildCall2(b, fc.runtime_decls.array_clone.ty, fc.runtime_decls.array_clone.fn_value, &cargs, cargs.len, "store.arr.clone");
                _ = api.LLVMBuildStore(b, clone, ptr);
            } else {
                // Fresh/owned array moves into the field; the struct destructor frees it.
                const value = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.arr.move");
                _ = api.LLVMBuildStore(b, value, ptr);
                drop.onEscape(fc, v.src);
            }
            _ = api.LLVMBuildBr(b, done_block);
            api.LLVMPositionBuilderAtEnd(b, done_block);
        },
        .enum_instance => {
            // An enum struct field is owned by the struct (its destructor frees it, and
            // copies clone it — see backend_capi_destructors). Match the array-field rule:
            //   owned source  -> MOVE the heap enum pointer in and escape the source slot.
            //   borrowed src   -> CLONE the enum block so the struct owns independent
            //                     storage and the borrowed original is untouched. Storing a
            //                     borrowed field (`Other { mode: src.mode }`) without cloning
            //                     would alias one enum into two owners -> double free.
            if (fc.drop_enabled and fc.request.mode == .llvm_native and !drop.isOwned(fc, v.src)) {
                const sptr = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.enum.src");
                var cargs = [_]llvm.c.LLVMValueRef{sptr};
                const clone = api.LLVMBuildCall2(b, fc.dtors.enum_clone.ty, fc.dtors.enum_clone.fn_value, &cargs, cargs.len, "store.enum.clone");
                _ = api.LLVMBuildStore(b, clone, ptr);
            } else {
                const value = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.enum.move");
                _ = api.LLVMBuildStore(b, value, ptr);
                drop.onEscape(fc, v.src);
            }
        },
        .construct_any, .raw_ptr => {
            // Storing a Kira String into a CString field passes the string's data pointer
            // (the {ptr,len} pair degrades to a char*), matching the text backend. String
            // literals are NUL-terminated globals, so the pointer is a valid C string.
            const src_kind = if (v.src < fc.register_types.len) fc.register_types[v.src].kind else ir.ValueType.Kind.raw_ptr;
            if (v.ty.name != null and std.mem.eql(u8, v.ty.name.?, "CString") and src_kind == .string) {
                const data_ptr = api.LLVMBuildExtractValue(b, src, 0, "store.cstr");
                _ = api.LLVMBuildStore(b, data_ptr, ptr);
            } else {
                const value = api.LLVMBuildIntToPtr(b, src, fc.types.ptr_ty, "store.rawptr");
                _ = api.LLVMBuildStore(b, value, ptr);
                // A closure/enum moved into a struct field is no longer ours to free
                // (the struct has no destructor for it, so it leaks rather than double-frees).
                drop.onEscape(fc, v.src);
            }
        },
        else => return error.UnsupportedExecutableFeature,
    }
}

pub fn lowerAllocArray(fc: *FunctionCodegen, v: ir.AllocArray) !void {
    const api = fc.api;
    const b = fc.builder;
    var args = [_]llvm.c.LLVMValueRef{fc.registers[v.len]};
    const ptr = api.LLVMBuildCall2(b, fc.runtime_decls.array_alloc.ty, fc.runtime_decls.array_alloc.fn_value, &args, args.len, "array.alloc");
    fc.registers[v.dst] = api.LLVMBuildPtrToInt(b, ptr, fc.types.i64, "array.ptr");
    drop.onAlloc(fc, v.dst);
}

pub fn lowerArrayLen(fc: *FunctionCodegen, v: ir.ArrayLen) void {
    const api = fc.api;
    const b = fc.builder;
    const arr = api.LLVMBuildIntToPtr(b, fc.registers[v.array], fc.types.ptr_ty, "array.lenptr");
    var args = [_]llvm.c.LLVMValueRef{arr};
    fc.registers[v.dst] = api.LLVMBuildCall2(b, fc.runtime_decls.array_len.ty, fc.runtime_decls.array_len.fn_value, &args, args.len, "array.len");
}

pub fn lowerArrayGet(fc: *FunctionCodegen, v: ir.ArrayGet) !void {
    const api = fc.api;
    const b = fc.builder;
    const arr = api.LLVMBuildIntToPtr(b, fc.registers[v.array], fc.types.ptr_ty, "array.getptr");
    const slot = fc.entryAlloca(fc.types.bridge_ty, "array.get.slot");
    var args = [_]llvm.c.LLVMValueRef{ arr, fc.registers[v.index], slot };
    _ = api.LLVMBuildCall2(b, fc.runtime_decls.array_load.ty, fc.runtime_decls.array_load.fn_value, &args, args.len, "");
    const bv = api.LLVMBuildLoad2(b, fc.types.bridge_ty, slot, "array.get.bv");
    fc.registers[v.dst] = try fc.unpackBridge(v.ty, bv);
}

// Build the bridge value to store into an array element, applying Rust-style ownership
// for ffi_struct elements (native drop only):
//   owned source  -> MOVE: store its heap pointer directly (no boxed copy, no orphaned
//                    shell); the caller escapes the source slot so the function won't
//                    also free it. The element now owns the struct; array release frees it.
//   borrowed src  -> CLONE: deep-clone (kira_clone_<T>) so the element owns independent
//                    storage and the borrowed original is untouched.
// All other cases (non-struct elements, or the VM-owned hybrid path) keep the existing
// boxed copy. Both move and clone use the box_struct=false packing (tag RAW_PTR + pointer),
// matching the boxed element layout the per-element destructor expects.
fn buildElementBridge(fc: *FunctionCodegen, src_reg: u32) !llvm.c.LLVMValueRef {
    const vt = fc.register_types[src_reg];
    if (fc.drop_enabled and fc.request.mode == .llvm_native and vt.kind == .ffi_struct) {
        if (vt.name) |name| {
            if (fc.dtors.map.get(name) != null) {
                // Move the owned source (or clone a borrow) into a fresh caller-stable
                // heap struct, then store that pointer directly as the element — no boxed
                // shallow copy, no orphaned shell. moveOrCloneToHeap consumes the source.
                const heap = drop.moveOrCloneToHeap(fc, src_reg, name);
                return fc.packBridgeBoxed(vt, heap, false);
            }
        }
    }
    return fc.packBridge(vt, fc.registers[src_reg]);
}

pub fn lowerArraySet(fc: *FunctionCodegen, v: ir.ArraySet) !void {
    const api = fc.api;
    const b = fc.builder;
    const arr = api.LLVMBuildIntToPtr(b, fc.registers[v.array], fc.types.ptr_ty, "array.setptr");
    const bv = try buildElementBridge(fc, v.src);
    const slot = fc.entryAlloca(fc.types.bridge_ty, "array.set.slot");
    _ = api.LLVMBuildStore(b, bv, slot);
    // Drop the element being overwritten when it owns heap contents (a struct element
    // with its own arrays/sub-structs), or each overwrite orphans the prior occupant.
    // kira_array_store_release guards old==new so storing a borrowed element back to its
    // own slot is a no-op, not a use-after-free. Primitive elements (no destructor) keep
    // the plain store.
    const elem_destroy = if (fc.drop_enabled) fc.dtors.elementDestroy(fc.request.program.programPtr(), fc.register_types[v.src]) else null;
    if (elem_destroy) |destroy_fn| {
        var args = [_]llvm.c.LLVMValueRef{ arr, fc.registers[v.index], slot, destroy_fn };
        _ = api.LLVMBuildCall2(b, fc.runtime_decls.array_store_release.ty, fc.runtime_decls.array_store_release.fn_value, &args, args.len, "");
    } else {
        var args = [_]llvm.c.LLVMValueRef{ arr, fc.registers[v.index], slot };
        _ = api.LLVMBuildCall2(b, fc.runtime_decls.array_store.ty, fc.runtime_decls.array_store.fn_value, &args, args.len, "");
    }
    // The element copy now owns (shares) the value's nested storage; stop tracking the
    // source so its slot can't free storage the array still references (conservative:
    // leaks the source shell, never double-frees).
    drop.onEscape(fc, v.src);
}

pub fn lowerArrayAppend(fc: *FunctionCodegen, v: ir.ArrayAppend) !void {
    const api = fc.api;
    const b = fc.builder;
    const arr = api.LLVMBuildIntToPtr(b, fc.registers[v.array], fc.types.ptr_ty, "array.appendptr");
    const bv = try buildElementBridge(fc, v.src);
    const slot = fc.entryAlloca(fc.types.bridge_ty, "array.append.slot");
    _ = api.LLVMBuildStore(b, bv, slot);
    var args = [_]llvm.c.LLVMValueRef{ arr, slot };
    _ = api.LLVMBuildCall2(b, fc.runtime_decls.array_append.ty, fc.runtime_decls.array_append.fn_value, &args, args.len, "");
    // See lowerArraySet: the appended element copy shares the value's nested storage,
    // so the source must stop being tracked for drop.
    drop.onEscape(fc, v.src);
}

// An enum value is a heap 16-byte block: { i64 tag, i64 payload }. The payload
// is the value widened to i64 (strings are heap-boxed). Mirrors
// backend_text_ir_enum_ops.
pub fn lowerAllocEnum(fc: *FunctionCodegen, v: ir.AllocEnum) !void {
    const api = fc.api;
    const b = fc.builder;
    var margs = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 16, 0)};
    const ptr = api.LLVMBuildCall2(b, fc.runtime_decls.malloc.ty, fc.runtime_decls.malloc.fn_value, &margs, margs.len, "enum.alloc");
    var tag_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 0, 0)};
    const tag_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, ptr, &tag_idx, tag_idx.len, "enum.tag.slot");
    _ = api.LLVMBuildStore(b, api.LLVMConstInt(fc.types.i64, v.discriminant, 0), tag_slot);
    var payload_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 1, 0)};
    const payload_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, ptr, &payload_idx, payload_idx.len, "enum.payload.slot");
    const payload: llvm.c.LLVMValueRef = if (v.payload_src) |src| blk: {
        const src_ty = fc.register_types[src];
        // A payload that is itself an owned heap value (an enum/array/struct/closure)
        // moves into this enum block: escape the source so scope-exit teardown does
        // not free it a second time once the enum escapes (return/store). Without
        // this transfer the payload is freed at the constructing frame's exit and a
        // returned enum dangles (use-after-free on re-match). A *borrowed* enum
        // payload is deep-cloned instead, so the new enum owns independent storage
        // and the borrowed original is left intact — mirrors the struct-field store
        // rule in lowerStoreAggregate.
        if (src_ty.kind == .enum_instance and fc.drop_enabled and fc.request.mode == .llvm_native and !drop.isOwned(fc, src)) {
            const sptr = api.LLVMBuildIntToPtr(b, fc.registers[src], fc.types.ptr_ty, "enum.payload.clonesrc");
            var cargs = [_]llvm.c.LLVMValueRef{sptr};
            const clone = api.LLVMBuildCall2(b, fc.dtors.enum_clone.ty, fc.dtors.enum_clone.fn_value, &cargs, cargs.len, "enum.payload.clone");
            break :blk api.LLVMBuildPtrToInt(b, clone, fc.types.i64, "enum.payload.cloneint");
        }
        const widened = try enumPayloadAsI64(fc, src_ty, fc.registers[src]);
        switch (src_ty.kind) {
            // Owned heap payloads transfer ownership into the enum (escape nulls the
            // source's cleanup slot); a borrowed source has no slot, so this is a no-op.
            .enum_instance, .array, .ffi_struct, .raw_ptr, .construct_any => drop.onEscape(fc, src),
            else => {},
        }
        break :blk widened;
    } else api.LLVMConstInt(fc.types.i64, 0, 0);
    _ = api.LLVMBuildStore(b, payload, payload_slot);
    fc.registers[v.dst] = api.LLVMBuildPtrToInt(b, ptr, fc.types.i64, "enum.ptr");
}

pub fn enumPayloadAsI64(fc: *FunctionCodegen, value_type: ir.ValueType, value: llvm.c.LLVMValueRef) !llvm.c.LLVMValueRef {
    const api = fc.api;
    const b = fc.builder;
    return switch (value_type.kind) {
        .integer, .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => value,
        .boolean => api.LLVMBuildZExt(b, value, fc.types.i64, "enum.bool"),
        .float => blk: {
            const as_double = if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
                api.LLVMBuildFPExt(b, value, fc.types.double_ty, "enum.fpext")
            else
                value;
            break :blk api.LLVMBuildBitCast(b, as_double, fc.types.i64, "enum.fbits");
        },
        .string => blk: {
            // Box the %kira.string on the heap and store its address.
            var margs = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 16, 0)};
            const box = api.LLVMBuildCall2(b, fc.runtime_decls.malloc.ty, fc.runtime_decls.malloc.fn_value, &margs, margs.len, "enum.str.box");
            _ = api.LLVMBuildStore(b, value, box);
            break :blk api.LLVMBuildPtrToInt(b, box, fc.types.i64, "enum.str.int");
        },
        .void => api.LLVMConstInt(fc.types.i64, 0, 0),
    };
}

pub fn lowerEnumPayload(fc: *FunctionCodegen, v: ir.EnumPayload) !llvm.c.LLVMValueRef {
    const api = fc.api;
    const b = fc.builder;
    const base = api.LLVMBuildIntToPtr(b, fc.registers[v.src], fc.types.ptr_ty, "enum.payload.base");
    var idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 1, 0)};
    const slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, base, &idx, idx.len, "enum.payload.slot");
    const raw = api.LLVMBuildLoad2(b, fc.types.i64, slot, "enum.payload.raw");
    return switch (v.payload_ty.kind) {
        .integer, .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => raw,
        .boolean => api.LLVMBuildTrunc(b, raw, fc.types.bool_ty, "enum.payload.bool"),
        .float => blk: {
            const d = api.LLVMBuildBitCast(b, raw, fc.types.double_ty, "enum.payload.double");
            break :blk if (v.payload_ty.name != null and std.mem.eql(u8, v.payload_ty.name.?, "F32"))
                api.LLVMBuildFPTrunc(b, d, fc.types.float_ty, "enum.payload.f32")
            else
                d;
        },
        .string => blk: {
            const sp = api.LLVMBuildIntToPtr(b, raw, fc.types.ptr_ty, "enum.payload.strptr");
            break :blk api.LLVMBuildLoad2(b, fc.types.string_ty, sp, "enum.payload.str");
        },
        .void => error.UnsupportedExecutableFeature,
    };
}

// Pack a register value into a %kira.bridge.value (tagged union) for the array
// runtime. Field 0 = type tag, field 2 = i64 payload, field 3 = extra (string
// length). Mirrors backend_text_ir_core array_set/append packing.
// A closure is a heap block: { i64 function_id, i64 capture_count, [N x bridge] }.
// The register value is the pointer with the high bit set, so call_value can tell
// a closure from a plain function id.
