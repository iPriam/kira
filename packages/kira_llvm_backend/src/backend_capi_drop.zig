// Owned-value cleanup-slot drop driver for the LLVM C-API backend. Consumes the
// per-type destructor/clone helpers generated in backend_capi_destructors.zig and
// implements the runtime ownership model over FunctionCodegen: move/clone into
// caller-stable storage, owned-param drop, call-result tracking, loop-body and
// overwrite drops, and owned-closure-param drop.
//
// Cleanup slots are entry-block allocas, so they dominate every basic block — this
// is what lets the C-API backend free owned values without the "instruction does not
// dominate all uses" problem that defeated the textual writer's clone attempts.
const std = @import("std");
const ir = @import("kira_ir");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const capi = @import("backend_capi.zig");
const runtime_utils = @import("backend_runtime_utils.zig");
const destructors = @import("backend_capi_destructors.zig");

const findTypeDecl = utils.findTypeDecl;

// Per-type destructor/clone generation lives in backend_capi_destructors.zig; re-export
// the public surface so callers (and this driver) keep using `drop.Destructors`/`drop.build`.
pub const TypeHelpers = destructors.TypeHelpers;
pub const Destructors = destructors.Destructors;
pub const build = destructors.build;

// ----- owned-value cleanup-slot drop (gated behind FunctionCodegen.drop_enabled) -----
// A cleanup slot is an entry-block `alloca ptr` (init null) holding a live owned
// heap pointer. At function exit every non-null slot is freed with the right
// destructor. Slots dominate all blocks, so this is free of the "does not dominate"
// problem; moves/escapes null the slot so each value is freed at most once.
const FunctionCodegen = @import("backend_capi_codegen.zig").FunctionCodegen;

pub const OwnedKind = enum { array, struct_heap, struct_ptr, raw, struct_contents, closure };
pub const OwnedSlot = struct {
    alloca: llvm.c.LLVMValueRef,
    kind: OwnedKind,
    ty: ir.ValueType,
};

fn ownedKindFor(value_type: ir.ValueType) ?OwnedKind {
    return switch (value_type.kind) {
        .array => .array,
        .ffi_struct => .struct_heap,
        .construct_any => .struct_ptr,
        // closures/enums are raw heap blocks freed with plain free().
        .raw_ptr, .enum_instance => .raw,
        else => null,
    };
}

fn ownedProducer(instruction: ir.Instruction) ?struct { dst: u32, ty: ir.ValueType } {
    return switch (instruction) {
        .alloc_array => |v| .{ .dst = v.dst, .ty = v.ty },
        .alloc_struct => |v| .{ .dst = v.dst, .ty = .{ .kind = .ffi_struct, .name = v.type_name } },
        .alloc_enum => |v| .{ .dst = v.dst, .ty = .{ .kind = .enum_instance, .name = v.enum_type_name } },
        .const_closure => |v| .{ .dst = v.dst, .ty = .{ .kind = .raw_ptr } },
        else => null,
    };
}

// Pre-scan owned-producing instructions and allocate one cleanup slot per result
// in the entry block (so every slot dominates all exits), seeding register_slot.
// Call with the builder positioned at the entry block.
pub fn setup(fc: *FunctionCodegen) !void {
    fc.register_slot = try fc.allocator.alloc(?u32, fc.function_decl.register_count);
    @memset(fc.register_slot, null);
    fc.local_slot = try fc.allocator.alloc(?u32, fc.function_decl.local_count);
    @memset(fc.local_slot, null);
    fc.reg_local = try fc.allocator.alloc(?u32, fc.function_decl.register_count);
    @memset(fc.reg_local, null);
    fc.copy_dest_slot = try fc.allocator.alloc(?u32, fc.function_decl.local_count);
    @memset(fc.copy_dest_slot, null);
    if (!fc.drop_enabled) return;
    const api = fc.api;

    // Pre-scan: a register->local map (load_local/local_ptr) lets us see, for each
    // copy_indirect, which local backs its destination.
    const scan_reg_local = try fc.allocator.alloc(?u32, fc.function_decl.register_count);
    defer fc.allocator.free(scan_reg_local);
    @memset(scan_reg_local, null);

    for (fc.function_decl.instructions) |instruction| {
        switch (instruction) {
            .load_local => |v| if (v.dst < scan_reg_local.len) {
                scan_reg_local[v.dst] = v.local;
            },
            .local_ptr => |v| if (v.dst < scan_reg_local.len) {
                scan_reg_local[v.dst] = v.local;
            },
            .copy_indirect => |v| {
                // One struct_contents cleanup slot per destination local (reused across
                // reassignments so the same backing is released at most once).
                if (fc.dtors.map.get(v.type_name) == null) continue;
                const local = if (v.dst_ptr < scan_reg_local.len) scan_reg_local[v.dst_ptr] orelse continue else continue;
                if (local >= fc.copy_dest_slot.len or fc.copy_dest_slot[local] != null) continue;
                const slot = api.LLVMBuildAlloca(fc.builder, fc.types.ptr_ty, "drop.contents.slot");
                _ = api.LLVMBuildStore(fc.builder, api.LLVMConstNull(fc.types.ptr_ty), slot);
                const index: u32 = @intCast(fc.drop_slots.items.len);
                try fc.drop_slots.append(fc.allocator, .{ .alloca = slot, .kind = .struct_contents, .ty = .{ .kind = .ffi_struct, .name = v.type_name } });
                fc.copy_dest_slot[local] = index;
            },
            .call => |v| {
                // A native call returning an owned aggregate yields fresh caller-stable
                // heap storage (an ffi_struct the callee moved/cloned out, or a heap
                // array). Track it so the caller frees it at scope exit unless it is
                // consumed/moved first (onEscape nulls the slot; a direct return skips it).
                const dst = v.dst orelse continue;
                const callee = runtime_utils.functionById(fc.request.program.programPtr().*, v.callee) orelse continue;
                const kind: OwnedKind = switch (callee.return_type.kind) {
                    .ffi_struct => .struct_heap,
                    .array => .array,
                    // A returned enum is a fresh heap block the callee allocated and handed
                    // over; the caller owns it and frees it at scope exit unless it is moved
                    // on (a store into a field moves it; a borrow-arg pass keeps it). Native
                    // only — a hybrid runtime call returns a VM-owned enum. This is the
                    // per-frame `graphicsEventKindFromRaw`/`...ButtonFromRaw` leak.
                    .enum_instance => if (fc.request.mode == .llvm_native) .raw else continue,
                    else => continue,
                };
                if (dst >= fc.register_slot.len or fc.register_slot[dst] != null) continue;
                const slot = api.LLVMBuildAlloca(fc.builder, fc.types.ptr_ty, "drop.callret.slot");
                _ = api.LLVMBuildStore(fc.builder, api.LLVMConstNull(fc.types.ptr_ty), slot);
                const index: u32 = @intCast(fc.drop_slots.items.len);
                try fc.drop_slots.append(fc.allocator, .{ .alloca = slot, .kind = kind, .ty = callee.return_type });
                fc.register_slot[dst] = index;
            },
            .call_value => |v| {
                // A closure call returning an owned aggregate yields caller-stable heap
                // storage, same as a direct call; track it for drop.
                const dst = v.dst orelse continue;
                const kind: OwnedKind = switch (v.return_type.kind) {
                    .ffi_struct => .struct_heap,
                    .array => .array,
                    .enum_instance => if (fc.request.mode == .llvm_native) .raw else continue,
                    else => continue,
                };
                if (dst >= fc.register_slot.len or fc.register_slot[dst] != null) continue;
                const slot = api.LLVMBuildAlloca(fc.builder, fc.types.ptr_ty, "drop.cvret.slot");
                _ = api.LLVMBuildStore(fc.builder, api.LLVMConstNull(fc.types.ptr_ty), slot);
                const index: u32 = @intCast(fc.drop_slots.items.len);
                try fc.drop_slots.append(fc.allocator, .{ .alloca = slot, .kind = kind, .ty = v.return_type });
                fc.register_slot[dst] = index;
            },
            .call_virtual => |v| {
                const dst = v.dst orelse continue;
                const kind: OwnedKind = switch (v.return_ty.kind) {
                    .ffi_struct => .struct_heap,
                    .construct_any => .struct_ptr,
                    .array => .array,
                    .enum_instance => if (fc.request.mode == .llvm_native) .raw else continue,
                    else => continue,
                };
                if (dst >= fc.register_slot.len or fc.register_slot[dst] != null) continue;
                const slot = api.LLVMBuildAlloca(fc.builder, fc.types.ptr_ty, "drop.vret.slot");
                _ = api.LLVMBuildStore(fc.builder, api.LLVMConstNull(fc.types.ptr_ty), slot);
                const index: u32 = @intCast(fc.drop_slots.items.len);
                try fc.drop_slots.append(fc.allocator, .{ .alloca = slot, .kind = kind, .ty = v.return_ty });
                fc.register_slot[dst] = index;
            },
            else => {},
        }
        const producer = ownedProducer(instruction) orelse continue;
        const kind = ownedKindFor(producer.ty) orelse continue;
        const slot = api.LLVMBuildAlloca(fc.builder, fc.types.ptr_ty, "drop.slot");
        _ = api.LLVMBuildStore(fc.builder, api.LLVMConstNull(fc.types.ptr_ty), slot);
        const index: u32 = @intCast(fc.drop_slots.items.len);
        try fc.drop_slots.append(fc.allocator, .{ .alloca = slot, .kind = kind, .ty = producer.ty });
        if (producer.dst < fc.register_slot.len) fc.register_slot[producer.dst] = index;
    }

    // Owned aggregate parameters: a moved-in struct/array is owned by the callee, which
    // must drop it at scope exit unless it is moved out (returned/stored — which nulls
    // the slot through the param's local_slot). Allocate a cleanup slot per owned
    // ffi_struct/array param now; seedOwnedParams fills it once the params are bound.
    // A struct param releases only its contents (the shell is the caller's storage).
    for (fc.function_decl.param_types, 0..) |pt, i| {
        if (i >= fc.function_decl.param_ownership.len) break;
        switch (fc.function_decl.param_ownership[i]) {
            .owned, .move => {},
            else => continue,
        }
        const kind: OwnedKind = switch (pt.kind) {
            // A moved-in struct is fully owned by the callee (Rust move semantics): the
            // caller hands over a caller-stable heap shell (lowerCall normalizes any stack
            // source to heap) and relinquishes it, so the callee drops shell + contents at
            // exit (kira_destroy) unless it moves the value onward. This replaces the older
            // split model (callee releases only contents, caller keeps the shell) which
            // leaked every owned struct argument — neither side freed the shell.
            // HYBRID EXCEPTION: a struct value crossing the VM bridge is VM-managed, so the
            // native callee must not free its shell; keep the contents-only model there.
            .ffi_struct => if (pt.name != null and fc.dtors.map.get(pt.name.?) != null)
                (if (fc.request.mode == .hybrid) .struct_contents else .struct_heap)
            else
                continue,
            .array => .array,
            // A moved-in closure / heap value: the callee owns it and frees it at exit
            // (tag-safe so callable-values are a no-op). This reclaims owned closure
            // parameters whose blocks would otherwise leak after the caller escapes them.
            // HYBRID EXCEPTION: a closure/raw_ptr parameter of a native function may be a
            // VM-OWNED value the VM passed across the bridge (a VM closure is tagged for ABI
            // compat but its block is VM-managed). Freeing it here corrupts the VM heap, so
            // do not take ownership of closure/raw_ptr params in hybrid mode.
            .construct_any => if (fc.request.mode == .hybrid) continue else .struct_ptr,
            .raw_ptr => if (fc.request.mode == .hybrid) continue else .closure,
            else => continue,
        };
        const slot = api.LLVMBuildAlloca(fc.builder, fc.types.ptr_ty, "drop.param.slot");
        _ = api.LLVMBuildStore(fc.builder, api.LLVMConstNull(fc.types.ptr_ty), slot);
        const index: u32 = @intCast(fc.drop_slots.items.len);
        try fc.drop_slots.append(fc.allocator, .{ .alloca = slot, .kind = kind, .ty = pt });
        if (i < fc.local_slot.len) fc.local_slot[@intCast(i)] = index;
    }
}

// Seed each owned-aggregate param's cleanup slot with the param's runtime pointer.
// Must run after the params are bound to locals (still in the entry block, so the
// stores dominate every exit). Pairs with the owned-param slots allocated in setup.
pub fn seedOwnedParams(fc: *FunctionCodegen) void {
    if (!fc.drop_enabled) return;
    const api = fc.api;
    for (fc.function_decl.param_types, 0..) |pt, i| {
        if (i >= fc.function_decl.param_ownership.len) break;
        switch (fc.function_decl.param_ownership[i]) {
            .owned, .move => {},
            else => continue,
        }
        switch (pt.kind) {
            .ffi_struct, .array, .construct_any, .raw_ptr => {},
            else => continue,
        }
        if (i >= fc.local_slot.len) continue;
        const idx = fc.local_slot[@intCast(i)] orelse continue;
        const param = api.LLVMGetParam(fc.function_value, @intCast(i));
        const ptr = api.LLVMBuildIntToPtr(fc.builder, param, fc.types.ptr_ty, "param.own.ptr");
        _ = api.LLVMBuildStore(fc.builder, ptr, fc.drop_slots.items[idx].alloca);
    }
}

pub fn teardown(fc: *FunctionCodegen) void {
    fc.drop_slots.deinit(fc.allocator);
    fc.allocator.free(fc.register_slot);
    fc.allocator.free(fc.local_slot);
    fc.allocator.free(fc.reg_local);
    fc.allocator.free(fc.copy_dest_slot);
}

// Record the runtime pointer of a freshly heap-allocated owned value into its
// pre-allocated cleanup slot (slot index was seeded in setup).
pub fn onAlloc(fc: *FunctionCodegen, dst: u32) void {
    if (!fc.drop_enabled) return;
    if (dst >= fc.register_slot.len) return;
    const index = fc.register_slot[dst] orelse return;
    const api = fc.api;
    dropPriorOccupant(fc, index);
    const ptr = api.LLVMBuildIntToPtr(fc.builder, fc.registers[dst], fc.types.ptr_ty, "drop.own");
    _ = api.LLVMBuildStore(fc.builder, ptr, fc.drop_slots.items[index].alloca);
}

// Like onAlloc but records a caller-supplied ptr-typed value instead of the dst
// register. Closures carry a high-bit tag in their register, so the real heap
// pointer (the untagged malloc result) must be recorded — freeing the tagged
// value would corrupt the heap.
pub fn onAllocPointer(fc: *FunctionCodegen, dst: u32, pointer: llvm.c.LLVMValueRef) void {
    if (!fc.drop_enabled) return;
    if (dst >= fc.register_slot.len) return;
    const index = fc.register_slot[dst] orelse return;
    dropPriorOccupant(fc, index);
    _ = fc.api.LLVMBuildStore(fc.builder, pointer, fc.drop_slots.items[index].alloca);
}

fn nullSlot(fc: *FunctionCodegen, index: u32) void {
    _ = fc.api.LLVMBuildStore(fc.builder, fc.api.LLVMConstNull(fc.types.ptr_ty), fc.drop_slots.items[index].alloca);
}

// `reg`'s value escapes the function (returned / consumed): stop tracking it.
pub fn onEscape(fc: *FunctionCodegen, reg: u32) void {
    if (!fc.drop_enabled) return;
    if (reg < fc.register_slot.len) {
        if (fc.register_slot[reg]) |index| nullSlot(fc, index);
    }
}

fn moveStructToHeap(fc: *FunctionCodegen, src_val: llvm.c.LLVMValueRef, name: ?[]const u8) llvm.c.LLVMValueRef {
    const api = fc.api;
    const b = fc.builder;
    const struct_ty = if (name) |n| fc.struct_types.get(n) orelse return src_val else return src_val;
    var margs = [_]llvm.c.LLVMValueRef{
        api.LLVMConstInt(fc.types.i64, ir.nativeStateTypeId(name.?), 0),
        api.LLVMSizeOf(struct_ty),
    };
    const heap = api.LLVMBuildCall2(b, fc.runtime_decls.struct_alloc.ty, fc.runtime_decls.struct_alloc.fn_value, &margs, margs.len, "ret.heap");
    const src_ptr = api.LLVMBuildIntToPtr(b, src_val, fc.types.ptr_ty, "ret.src");
    const val = api.LLVMBuildLoad2(b, struct_ty, src_ptr, "ret.val");
    _ = api.LLVMBuildStore(b, val, heap);
    return api.LLVMBuildPtrToInt(b, heap, fc.types.i64, "ret.heapint");
}

// Lower the value of an ffi_struct `return src`. The caller must receive storage that
// outlives the callee frame and that it solely owns. Three cases by how `src` is
// tracked, cheapest first:
//   struct_heap     — already caller-stable heap (a call result or alloc_struct):
//                     hand the pointer over and escape the slot (no copy).
//   struct_contents — stack-backed local: move the shell into fresh heap storage (a
//                     shallow field copy; the owned arrays move with it) and escape,
//                     so exit cleanup does not release the contents now owned by heap.
//   untracked       — a borrow or a directly-returned owned param: deep-clone into
//                     independent heap storage (the only safe option when the source
//                     is not a tracked owned temporary).
// In every case the returned pointer is an owned heap struct the caller frees.
pub fn prepareStructReturn(fc: *FunctionCodegen, src_reg: u32) llvm.c.LLVMValueRef {
    return moveOrCloneToHeap(fc, src_reg, fc.function_decl.return_type.name);
}

// Produce a caller-stable owned heap pointer for the ffi_struct in `src_reg`, consuming
// the source. Shared by struct returns and Rust-style array-element moves. `type_name`
// is the destination struct type (used only for the borrow->clone fallback). Cases:
//   struct_heap     — already owned heap: hand the pointer over, escape the slot.
//   struct_contents — stack-backed owned local: move the shell into fresh heap storage
//                     (arrays move with it), escape the slot.
//   untracked       — a borrow: deep-clone into independent heap storage (source intact).
pub fn moveOrCloneToHeap(fc: *FunctionCodegen, src_reg: u32, type_name: ?[]const u8) llvm.c.LLVMValueRef {
    const api = fc.api;
    const b = fc.builder;
    const src_val = fc.registers[src_reg];
    if (src_reg < fc.register_slot.len) {
        if (fc.register_slot[src_reg]) |idx| {
            switch (fc.drop_slots.items[idx].kind) {
                .struct_heap => {
                    nullSlot(fc, idx);
                    return src_val;
                },
                .struct_contents => {
                    const heap = moveStructToHeap(fc, src_val, fc.drop_slots.items[idx].ty.name);
                    nullSlot(fc, idx);
                    return heap;
                },
                else => {},
            }
        }
    }
    if (type_name) |name| {
        if (fc.dtors.map.get(name)) |h| {
            var ca = [_]llvm.c.LLVMValueRef{src_val};
            return api.LLVMBuildCall2(b, h.clone.ty, h.clone.fn_value, &ca, ca.len, "heap.clone");
        }
    }
    return src_val;
}

pub fn onStoreLocal(fc: *FunctionCodegen, local: u32, src: u32) void {
    if (!fc.drop_enabled) return;
    if (src < fc.register_slot.len and local < fc.local_slot.len) fc.local_slot[local] = fc.register_slot[src];
}

pub fn onLoadLocal(fc: *FunctionCodegen, dst: u32, local: u32) void {
    if (!fc.drop_enabled) return;
    if (dst < fc.register_slot.len and local < fc.local_slot.len) fc.register_slot[dst] = fc.local_slot[local];
}

// Is `reg` a tracked, owned (freshly allocated, not borrowed) value?
pub fn isOwned(fc: *FunctionCodegen, reg: u32) bool {
    return fc.drop_enabled and reg < fc.register_slot.len and fc.register_slot[reg] != null;
}

// Free every still-live owned value. Emitted at each function return + fallthrough.
// Free the value currently held in cleanup slot `index` with its kind's destructor.
// Every destructor is null-safe, so calling this on an already-null / escaped slot is
// a no-op. Used both at exit and for drop-before-overwrite (loop re-production).
fn freeSlot(fc: *FunctionCodegen, index: u32) void {
    const api = fc.api;
    const b = fc.builder;
    const owned = fc.drop_slots.items[index];
    const ptr = api.LLVMBuildLoad2(b, fc.types.ptr_ty, owned.alloca, "drop.load");
    switch (owned.kind) {
        .array => {
            const elem = fc.dtors.elementDestroy(fc.request.program.programPtr(), owned.ty);
            var args = [_]llvm.c.LLVMValueRef{ ptr, elem orelse api.LLVMConstNull(fc.types.ptr_ty) };
            _ = api.LLVMBuildCall2(b, fc.runtime_decls.array_release.ty, fc.runtime_decls.array_release.fn_value, &args, args.len, "");
        },
        .struct_heap => {
            const destroy = if (owned.ty.name) |n| (if (fc.dtors.map.get(n)) |h| h.destroy else fc.dtors.destroy_struct_ptr) else fc.dtors.destroy_struct_ptr;
            var args = [_]llvm.c.LLVMValueRef{ptr};
            _ = api.LLVMBuildCall2(b, destroy.ty, destroy.fn_value, &args, args.len, "");
        },
        .struct_ptr => {
            var args = [_]llvm.c.LLVMValueRef{ptr};
            _ = api.LLVMBuildCall2(b, fc.dtors.destroy_struct_ptr.ty, fc.dtors.destroy_struct_ptr.fn_value, &args, args.len, "");
        },
        .raw => {
            var args = [_]llvm.c.LLVMValueRef{ptr};
            _ = api.LLVMBuildCall2(b, fc.dtors.destroy_raw_ptr.ty, fc.dtors.destroy_raw_ptr.fn_value, &args, args.len, "");
        },
        .closure => {
            // The slot holds the (possibly tag-bit-set) closure value as a pointer;
            // pass it back as i64 to the tag-safe destructor.
            const as_int = api.LLVMBuildPtrToInt(b, ptr, fc.types.i64, "drop.closure.int");
            var args = [_]llvm.c.LLVMValueRef{as_int};
            _ = api.LLVMBuildCall2(b, fc.dtors.destroy_closure.ty, fc.dtors.destroy_closure.fn_value, &args, args.len, "");
        },
        .struct_contents => {
            // Stack-backed struct copy: release its nested fields (arrays/sub-structs)
            // but do NOT free the shell — the backing storage is an entry-block alloca.
            if (owned.ty.name) |n| {
                if (fc.dtors.map.get(n)) |h| {
                    var args = [_]llvm.c.LLVMValueRef{ptr};
                    _ = api.LLVMBuildCall2(b, h.release_contents.ty, h.release_contents.fn_value, &args, args.len, "");
                }
            }
        },
    }
}

pub fn emitExitCleanup(fc: *FunctionCodegen, returned: ?u32) void {
    if (!fc.drop_enabled) return;
    const returned_slot: ?u32 = if (returned) |r| (if (r < fc.register_slot.len) fc.register_slot[r] else null) else null;
    for (fc.drop_slots.items, 0..) |_, index| {
        if (returned_slot != null and returned_slot.? == @as(u32, @intCast(index))) continue;
        freeSlot(fc, @intCast(index));
    }
}

// Drop-before-overwrite: a value produced into cleanup slot `index` inside a loop
// overwrites the previous iteration's value. Free the prior occupant first so each
// iteration's value is reclaimed (the cleanup-slot model otherwise only frees the
// final occupant at function exit — the loop-body leak). Null-safe on first entry.
fn dropPriorOccupant(fc: *FunctionCodegen, index: u32) void {
    freeSlot(fc, index);
}

// Record a register->local association (from load_local / local_ptr) so a later
// copy_indirect can resolve which local its destination backing belongs to.
pub fn recordRegLocal(fc: *FunctionCodegen, reg: u32, local: u32) void {
    if (!fc.drop_enabled) return;
    if (reg < fc.reg_local.len) fc.reg_local[reg] = local;
}

// A copy_indirect deep-cloned `type_name` into the stack backing addressed by
// `dst_ptr` (the register), `dst_ptr_value` (its ptr value). The destination local
// now owns the cloned contents; track them so they are released at exit unless the
// local later escapes (a move/return/store nulls the slot through register_slot).
pub fn onCopyDest(fc: *FunctionCodegen, dst_ptr_reg: u32, dst_ptr_value: llvm.c.LLVMValueRef, type_name: []const u8) void {
    if (!fc.drop_enabled) return;
    if (fc.dtors.map.get(type_name) == null) return;
    const local = if (dst_ptr_reg < fc.reg_local.len) fc.reg_local[dst_ptr_reg] orelse return else return;
    const index = if (local < fc.copy_dest_slot.len) fc.copy_dest_slot[local] orelse return else return;
    _ = fc.api.LLVMBuildStore(fc.builder, dst_ptr_value, fc.drop_slots.items[index].alloca);
    fc.local_slot[local] = index;
}

// Drop-before-overwrite for a copy_indirect destination. A `copy_indirect` shallow-
// copies the source over the destination's stack shell, discarding whatever array
// pointers that shell already held; in a loop (`var x = ...` reassigned each
// iteration) the prior occupant's cloned contents would leak. Release them BEFORE the
// shallow store overwrites the shell. Null-safe on the first assignment (slot empty).
pub fn releasePriorCopyDest(fc: *FunctionCodegen, dst_ptr_reg: u32, type_name: []const u8) void {
    if (!fc.drop_enabled) return;
    if (fc.dtors.map.get(type_name) == null) return;
    const local = if (dst_ptr_reg < fc.reg_local.len) fc.reg_local[dst_ptr_reg] orelse return else return;
    const index = if (local < fc.copy_dest_slot.len) fc.copy_dest_slot[local] orelse return else return;
    dropPriorOccupant(fc, index);
}
