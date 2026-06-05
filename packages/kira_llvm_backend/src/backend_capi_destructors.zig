// Per-type ownership helper generation for the LLVM C-API backend: emits
// kira_destroy_<T> / kira_release_contents_<T> and the deep-clone helpers
// kira_clone_<T> / kira_clone_contents_<T> (plus the shared kira_destroy_raw_ptr
// declaration and the kira_destroy_closure declaration), mirroring
// backend_text_ir_core's appendReleaseDefinitions / appendCloneDefinitions.
// Split out of backend_capi_drop.zig (Core Law #5); the runtime cleanup-slot drop
// driver that consumes these helpers stays in backend_capi_drop.zig.
const std = @import("std");
const ir = @import("kira_ir");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const capi = @import("backend_capi.zig");

const findTypeDecl = utils.findTypeDecl;
const allocPrintZ = utils.allocPrintZ;
pub const TypeHelpers = struct {
    release_contents: capi.RuntimeDecls.Decl,
    destroy: capi.RuntimeDecls.Decl,
    clone_contents: capi.RuntimeDecls.Decl,
    clone: capi.RuntimeDecls.Decl,
};

pub const Destructors = struct {
    map: std.StringHashMapUnmanaged(TypeHelpers) = .{},
    destroy_raw_ptr: capi.RuntimeDecls.Decl,
    // Tag-safe owned-closure drop (kira_destroy_closure(i64)): frees a heap closure
    // block, no-ops a callable-value function id. Used for owned closure parameters.
    destroy_closure: capi.RuntimeDecls.Decl,

    pub fn deinit(self: *Destructors, allocator: std.mem.Allocator) void {
        self.map.deinit(allocator);
    }

    // The per-element destroy function for an array type, or null for primitive
    // elements (whose buffer is freed without a per-element callback).
    pub fn elementDestroy(self: Destructors, program: *const ir.Program, array_ty: ir.ValueType) ?llvm.c.LLVMValueRef {
        const name = array_ty.name orelse return null;
        const type_decl = findTypeDecl(program, name) orelse return null;
        if (type_decl.ffi) |ffi_info| {
            switch (ffi_info) {
                .ffi_struct => {},
                else => return null,
            }
        }
        const helpers = self.map.get(type_decl.name) orelse return null;
        return helpers.destroy.fn_value;
    }

    pub fn elementClone(self: Destructors, program: *const ir.Program, array_ty: ir.ValueType) ?llvm.c.LLVMValueRef {
        const name = array_ty.name orelse return null;
        const type_decl = findTypeDecl(program, name) orelse return null;
        if (type_decl.ffi) |ffi_info| {
            switch (ffi_info) {
                .ffi_struct => {},
                else => return null,
            }
        }
        const helpers = self.map.get(type_decl.name) orelse return null;
        return helpers.clone.fn_value;
    }
};

pub fn build(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    module_ref: llvm.c.LLVMModuleRef,
    types: capi.Types,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    program: *const ir.Program,
    runtime: capi.RuntimeDecls,
) !Destructors {
    var ptr_param = [_]llvm.c.LLVMTypeRef{types.ptr_ty};
    const void_ptr_ty = api.LLVMFunctionType(types.void_ty, &ptr_param, ptr_param.len, 0);

    const destroy_raw = api.LLVMAddFunction(module_ref, "kira_destroy_raw_ptr", void_ptr_ty);
    var closure_param = [_]llvm.c.LLVMTypeRef{types.i64};
    const void_i64_ty = api.LLVMFunctionType(types.void_ty, &closure_param, closure_param.len, 0);
    const destroy_closure = api.LLVMAddFunction(module_ref, "kira_destroy_closure", void_i64_ty);
    var result = Destructors{
        .destroy_raw_ptr = .{ .ty = void_ptr_ty, .fn_value = destroy_raw },
        .destroy_closure = .{ .ty = void_i64_ty, .fn_value = destroy_closure },
    };

    const builder = api.LLVMCreateBuilderInContext(types.context);
    defer api.LLVMDisposeBuilder(builder);

    {
        const entry = api.LLVMAppendBasicBlockInContext(types.context, destroy_raw, "entry");
        api.LLVMPositionBuilderAtEnd(builder, entry);
        var args = [_]llvm.c.LLVMValueRef{api.LLVMGetParam(destroy_raw, 0)};
        _ = api.LLVMBuildCall2(builder, runtime.free.ty, runtime.free.fn_value, &args, args.len, "");
        _ = api.LLVMBuildRetVoid(builder);
    }

    // Pass 1: declare all type helpers so bodies can reference each other.
    for (program.types) |type_decl| {
        if (type_decl.ffi) |ffi_info| {
            if (ffi_info != .ffi_struct) continue;
        }
        const rc_name = try allocPrintZ(allocator, "kira_release_contents_{s}", .{type_decl.name});
        defer allocator.free(rc_name);
        const d_name = try allocPrintZ(allocator, "kira_destroy_{s}", .{type_decl.name});
        defer allocator.free(d_name);
        const cc_name = try allocPrintZ(allocator, "kira_clone_contents_{s}", .{type_decl.name});
        defer allocator.free(cc_name);
        const c_name = try allocPrintZ(allocator, "kira_clone_{s}", .{type_decl.name});
        defer allocator.free(c_name);
        var i64_param = [_]llvm.c.LLVMTypeRef{types.i64};
        const clone_ty = api.LLVMFunctionType(types.i64, &i64_param, i64_param.len, 0);
        try result.map.put(allocator, type_decl.name, .{
            .release_contents = .{ .ty = void_ptr_ty, .fn_value = api.LLVMAddFunction(module_ref, rc_name.ptr, void_ptr_ty) },
            .destroy = .{ .ty = void_ptr_ty, .fn_value = api.LLVMAddFunction(module_ref, d_name.ptr, void_ptr_ty) },
            .clone_contents = .{ .ty = void_ptr_ty, .fn_value = api.LLVMAddFunction(module_ref, cc_name.ptr, void_ptr_ty) },
            .clone = .{ .ty = clone_ty, .fn_value = api.LLVMAddFunction(module_ref, c_name.ptr, clone_ty) },
        });
    }

    // Pass 2: build bodies.
    for (program.types) |type_decl| {
        if (type_decl.ffi) |ffi_info| {
            if (ffi_info != .ffi_struct) continue;
        }
        const struct_ty = struct_types.get(type_decl.name) orelse continue;
        const helpers = result.map.get(type_decl.name).?;
        try buildReleaseContents(api, builder, types, runtime, result, program, struct_ty, type_decl, helpers.release_contents.fn_value);
        try buildDestroy(api, builder, types, runtime, struct_ty, helpers.release_contents, helpers.destroy.fn_value);
        try buildCloneContents(api, builder, types, runtime, result, program, struct_ty, type_decl, helpers.clone_contents.fn_value);
        try buildClone(api, builder, types, runtime, struct_ty, helpers.clone_contents, helpers.clone.fn_value);
    }

    return result;
}

fn buildReleaseContents(api: *const llvm.Api, b: llvm.c.LLVMBuilderRef, types: capi.Types, runtime: capi.RuntimeDecls, dtors: Destructors, program: *const ir.Program, struct_ty: llvm.c.LLVMTypeRef, type_decl: ir.TypeDecl, fn_value: llvm.c.LLVMValueRef) !void {
    const entry = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "entry");
    const body = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "body");
    const done = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "done");
    api.LLVMPositionBuilderAtEnd(b, entry);
    const value = api.LLVMGetParam(fn_value, 0);
    // Null guard: a moved-out / escaped backing is passed as null and must be a no-op
    // (callers release a struct's contents only when it still owns them).
    const is_null = api.LLVMBuildICmp(b, llvm.c.LLVMIntEQ, value, api.LLVMConstNull(types.ptr_ty), "rc.isnull");
    _ = api.LLVMBuildCondBr(b, is_null, done, body);
    api.LLVMPositionBuilderAtEnd(b, body);
    for (type_decl.fields, 0..) |field, index| {
        var idx = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(types.i32, 0, 0), api.LLVMConstInt(types.i32, @intCast(index), 0) };
        switch (field.ty.kind) {
            .ffi_struct => {
                const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, value, &idx, idx.len, "rc.field");
                const fh = dtors.map.get(field.ty.name orelse continue) orelse continue;
                var args = [_]llvm.c.LLVMValueRef{field_ptr};
                _ = api.LLVMBuildCall2(b, fh.release_contents.ty, fh.release_contents.fn_value, &args, args.len, "");
            },
            .array => {
                const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, value, &idx, idx.len, "rc.arrfield");
                const arr = api.LLVMBuildLoad2(b, types.ptr_ty, field_ptr, "rc.arr");
                const elem = dtors.elementDestroy(program, field.ty);
                var args = [_]llvm.c.LLVMValueRef{ arr, elem orelse api.LLVMConstNull(types.ptr_ty) };
                _ = api.LLVMBuildCall2(b, runtime.array_release.ty, runtime.array_release.fn_value, &args, args.len, "");
            },
            else => {},
        }
    }
    _ = api.LLVMBuildBr(b, done);
    api.LLVMPositionBuilderAtEnd(b, done);
    _ = api.LLVMBuildRetVoid(b);
}

fn buildDestroy(api: *const llvm.Api, b: llvm.c.LLVMBuilderRef, types: capi.Types, runtime: capi.RuntimeDecls, struct_ty: llvm.c.LLVMTypeRef, rc: capi.RuntimeDecls.Decl, fn_value: llvm.c.LLVMValueRef) !void {
    _ = struct_ty;
    const entry = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "entry");
    const body = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "body");
    const done = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "done");
    api.LLVMPositionBuilderAtEnd(b, entry);
    const value = api.LLVMGetParam(fn_value, 0);
    const is_null = api.LLVMBuildICmp(b, llvm.c.LLVMIntEQ, value, api.LLVMConstNull(types.ptr_ty), "isnull");
    _ = api.LLVMBuildCondBr(b, is_null, done, body);
    api.LLVMPositionBuilderAtEnd(b, body);
    var rc_args = [_]llvm.c.LLVMValueRef{value};
    _ = api.LLVMBuildCall2(b, rc.ty, rc.fn_value, &rc_args, rc_args.len, "");
    var free_args = [_]llvm.c.LLVMValueRef{value};
    _ = api.LLVMBuildCall2(b, runtime.free.ty, runtime.free.fn_value, &free_args, free_args.len, "");
    _ = api.LLVMBuildBr(b, done);
    api.LLVMPositionBuilderAtEnd(b, done);
    _ = api.LLVMBuildRetVoid(b);
}

fn buildCloneContents(api: *const llvm.Api, b: llvm.c.LLVMBuilderRef, types: capi.Types, runtime: capi.RuntimeDecls, dtors: Destructors, program: *const ir.Program, struct_ty: llvm.c.LLVMTypeRef, type_decl: ir.TypeDecl, fn_value: llvm.c.LLVMValueRef) !void {
    const entry = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "entry");
    api.LLVMPositionBuilderAtEnd(b, entry);
    const value = api.LLVMGetParam(fn_value, 0);
    for (type_decl.fields, 0..) |field, index| {
        var idx = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(types.i32, 0, 0), api.LLVMConstInt(types.i32, @intCast(index), 0) };
        switch (field.ty.kind) {
            .ffi_struct => {
                const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, value, &idx, idx.len, "cc.field");
                const fh = dtors.map.get(field.ty.name orelse continue) orelse continue;
                var args = [_]llvm.c.LLVMValueRef{field_ptr};
                _ = api.LLVMBuildCall2(b, fh.clone_contents.ty, fh.clone_contents.fn_value, &args, args.len, "");
            },
            .array => {
                const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, value, &idx, idx.len, "cc.arrfield");
                const old = api.LLVMBuildLoad2(b, types.ptr_ty, field_ptr, "cc.old");
                const elem = dtors.elementClone(program, field.ty);
                var args = [_]llvm.c.LLVMValueRef{ old, elem orelse api.LLVMConstNull(types.ptr_ty) };
                const new = api.LLVMBuildCall2(b, runtime.array_clone.ty, runtime.array_clone.fn_value, &args, args.len, "cc.new");
                _ = api.LLVMBuildStore(b, new, field_ptr);
            },
            else => {},
        }
    }
    _ = api.LLVMBuildRetVoid(b);
}

fn buildClone(api: *const llvm.Api, b: llvm.c.LLVMBuilderRef, types: capi.Types, runtime: capi.RuntimeDecls, struct_ty: llvm.c.LLVMTypeRef, cc: capi.RuntimeDecls.Decl, fn_value: llvm.c.LLVMValueRef) !void {
    const entry = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "entry");
    const nullret = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "nullret");
    const body = api.LLVMAppendBasicBlockInContext(types.context, fn_value, "body");
    api.LLVMPositionBuilderAtEnd(b, entry);
    const srcint = api.LLVMGetParam(fn_value, 0);
    const src = api.LLVMBuildIntToPtr(b, srcint, types.ptr_ty, "clone.src");
    const is_null = api.LLVMBuildICmp(b, llvm.c.LLVMIntEQ, src, api.LLVMConstNull(types.ptr_ty), "clone.isnull");
    _ = api.LLVMBuildCondBr(b, is_null, nullret, body);
    api.LLVMPositionBuilderAtEnd(b, nullret);
    _ = api.LLVMBuildRet(b, api.LLVMConstInt(types.i64, 0, 0));
    api.LLVMPositionBuilderAtEnd(b, body);
    var malloc_args = [_]llvm.c.LLVMValueRef{api.LLVMSizeOf(struct_ty)};
    const dst = api.LLVMBuildCall2(b, runtime.malloc.ty, runtime.malloc.fn_value, &malloc_args, malloc_args.len, "clone.dst");
    const val = api.LLVMBuildLoad2(b, struct_ty, src, "clone.val");
    _ = api.LLVMBuildStore(b, val, dst);
    var cc_args = [_]llvm.c.LLVMValueRef{dst};
    _ = api.LLVMBuildCall2(b, cc.ty, cc.fn_value, &cc_args, cc_args.len, "");
    _ = api.LLVMBuildRet(b, api.LLVMBuildPtrToInt(b, dst, types.i64, "clone.dstint"));
}
