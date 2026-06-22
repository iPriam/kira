// Aggregate value printing for the LLVM C-API backend. Split out of
// backend_capi_codegen.zig (Core Law #5). Recursively writes a value with the
// no-newline kira_native_write_* helpers (struct `Type(f: v, ...)`, enum
// `Enum.Variant(payload)` via per-variant blocks, array `array(len: N)`), then a
// trailing newline. Free functions over *FunctionCodegen.
const std = @import("std");
const ir = @import("kira_ir");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const capi = @import("backend_capi.zig");
const FunctionCodegen = @import("backend_capi_codegen.zig").FunctionCodegen;
const findEnumDecl = utils.findEnumDecl;

pub fn lowerPrint(fc: *FunctionCodegen, value_type: ir.ValueType, value_ref: llvm.c.LLVMValueRef) !void {
    try writeValue(fc, value_type, value_ref);
    var none = [_]llvm.c.LLVMValueRef{};
    _ = fc.api.LLVMBuildCall2(fc.builder, fc.runtime_decls.write_newline.ty, fc.runtime_decls.write_newline.fn_value, &none, 0, "");
}

pub fn writeCall(fc: *FunctionCodegen, decl: capi.RuntimeDecls.Decl, args: []llvm.c.LLVMValueRef) void {
    _ = fc.api.LLVMBuildCall2(fc.builder, decl.ty, decl.fn_value, args.ptr, @intCast(args.len), "");
}

pub fn writeStringValue(fc: *FunctionCodegen, string_value: llvm.c.LLVMValueRef) void {
    const api = fc.api;
    const b = fc.builder;
    const data_ptr = api.LLVMBuildExtractValue(b, string_value, 0, "str.ptr");
    const length = api.LLVMBuildExtractValue(b, string_value, 1, "str.len");
    var args = [_]llvm.c.LLVMValueRef{ data_ptr, length };
    writeCall(fc, fc.runtime_decls.write_string, &args);
}

// Emit a string literal (no newline) by building a private global and writing it.
pub fn writeLiteral(fc: *FunctionCodegen, text: []const u8) !void {
    const lit = try fc.buildStringConstant(text);
    fc.string_counter += 1;
    writeStringValue(fc, lit);
}

// Recursively write a value's printed form without a trailing newline. Mirrors
// backend_utils.writePrintedValue / writeStructValue / writeEnumValue / writeArraySummary.
pub fn writeValue(fc: *FunctionCodegen, value_type: ir.ValueType, value_ref: llvm.c.LLVMValueRef) anyerror!void {
    const api = fc.api;
    const b = fc.builder;
    switch (value_type.kind) {
        .integer => {
            var args = [_]llvm.c.LLVMValueRef{value_ref};
            writeCall(fc, fc.runtime_decls.write_i64, &args);
        },
        .float => {
            const as_double = if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
                api.LLVMBuildFPExt(b, value_ref, fc.types.double_ty, "fpext")
            else
                value_ref;
            var args = [_]llvm.c.LLVMValueRef{as_double};
            writeCall(fc, fc.runtime_decls.write_f64, &args);
        },
        .boolean => {
            const chosen = api.LLVMBuildSelect(b, value_ref, fc.runtime_decls.bool_true, fc.runtime_decls.bool_false, "bool.str");
            writeStringValue(fc, chosen);
        },
        .string => writeStringValue(fc, value_ref),
        .construct_any, .raw_ptr => {
            var args = [_]llvm.c.LLVMValueRef{value_ref};
            writeCall(fc, fc.runtime_decls.write_ptr, &args);
        },
        .array => {
            try writeLiteral(fc, "array(len: ");
            const arr = api.LLVMBuildIntToPtr(b, value_ref, fc.types.ptr_ty, "print.arr");
            var len_args = [_]llvm.c.LLVMValueRef{arr};
            const len = api.LLVMBuildCall2(b, fc.runtime_decls.array_len.ty, fc.runtime_decls.array_len.fn_value, &len_args, len_args.len, "print.arr.len");
            var w = [_]llvm.c.LLVMValueRef{len};
            writeCall(fc, fc.runtime_decls.write_i64, &w);
            try writeLiteral(fc, ")");
        },
        .ffi_struct => try writeStructValue(fc, value_type, value_ref),
        .enum_instance => try writeEnumValue(fc, value_type, value_ref),
        .void => return error.UnsupportedExecutableFeature,
    }
}

pub fn writeStructValue(fc: *FunctionCodegen, value_type: ir.ValueType, value_ref: llvm.c.LLVMValueRef) !void {
    const api = fc.api;
    const b = fc.builder;
    const type_name = value_type.name orelse {
        var args = [_]llvm.c.LLVMValueRef{value_ref};
        writeCall(fc, fc.runtime_decls.write_ptr, &args);
        return;
    };
    const type_decl = utils.findTypeDecl(fc.request.program.programPtr(), type_name) orelse {
        var args = [_]llvm.c.LLVMValueRef{value_ref};
        writeCall(fc, fc.runtime_decls.write_ptr, &args);
        return;
    };
    const struct_ty = fc.struct_types.get(type_name) orelse return error.UnsupportedExecutableFeature;
    try writeLiteral(fc, type_name);
    try writeLiteral(fc, "(");
    const base = api.LLVMBuildIntToPtr(b, value_ref, fc.types.ptr_ty, "print.struct");
    for (type_decl.fields, 0..) |field_decl, index| {
        if (index != 0) try writeLiteral(fc, ", ");
        try writeLiteral(fc, field_decl.name);
        try writeLiteral(fc, ": ");
        var f_idx = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(fc.types.i32, 0, 0), api.LLVMConstInt(fc.types.i32, @intCast(index), 0) };
        const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, base, &f_idx, f_idx.len, "print.field.ptr");
        const field_value = if (field_decl.ty.kind == .ffi_struct)
            api.LLVMBuildPtrToInt(b, field_ptr, fc.types.i64, "print.field.struct")
        else
            try fc.loadConverted(field_ptr, field_decl.ty);
        try writeValue(fc, field_decl.ty, field_value);
    }
    try writeLiteral(fc, ")");
}

pub fn writeEnumValue(fc: *FunctionCodegen, value_type: ir.ValueType, value_ref: llvm.c.LLVMValueRef) !void {
    const api = fc.api;
    const b = fc.builder;
    const enum_name = value_type.name orelse return error.UnsupportedExecutableFeature;
    const enum_decl = findEnumDecl(fc.request.program.programPtr(), enum_name) orelse {
        var args = [_]llvm.c.LLVMValueRef{value_ref};
        writeCall(fc, fc.runtime_decls.write_ptr, &args);
        return;
    };
    const ctxp = fc.types.context;
    const ptr = api.LLVMBuildIntToPtr(b, value_ref, fc.types.ptr_ty, "print.enum");
    var tag_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 0, 0)};
    const tag_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, ptr, &tag_idx, tag_idx.len, "print.enum.tag.slot");
    const tag = api.LLVMBuildLoad2(b, fc.types.i64, tag_slot, "print.enum.tag");
    var pay_idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(fc.types.i64, 1, 0)};
    const pay_slot = api.LLVMBuildInBoundsGEP2(b, fc.types.i64, ptr, &pay_idx, pay_idx.len, "print.enum.pay.slot");
    const raw = api.LLVMBuildLoad2(b, fc.types.i64, pay_slot, "print.enum.raw");

    const done = api.LLVMAppendBasicBlockInContext(ctxp, fc.function_value, "penum.done");
    const default_block = api.LLVMAppendBasicBlockInContext(ctxp, fc.function_value, "penum.default");
    const sw = api.LLVMBuildSwitch(b, tag, default_block, @intCast(enum_decl.variants.len));
    for (enum_decl.variants) |variant_decl| {
        const case_block = api.LLVMAppendBasicBlockInContext(ctxp, fc.function_value, "penum.case");
        api.LLVMAddCase(sw, api.LLVMConstInt(fc.types.i64, variant_decl.discriminant, 0), case_block);
        api.LLVMPositionBuilderAtEnd(b, case_block);
        const full_name = try std.fmt.allocPrint(fc.allocator, "{s}.{s}", .{ enum_name, variant_decl.name });
        defer fc.allocator.free(full_name);
        try writeLiteral(fc, full_name);
        if (variant_decl.payload_ty) |payload_ty| {
            try writeLiteral(fc, "(");
            const payload = try enumPayloadFromRaw(fc, raw, payload_ty);
            try writeValue(fc, payload_ty, payload);
            try writeLiteral(fc, ")");
        }
        _ = api.LLVMBuildBr(b, done);
    }
    api.LLVMPositionBuilderAtEnd(b, default_block);
    var args = [_]llvm.c.LLVMValueRef{value_ref};
    writeCall(fc, fc.runtime_decls.write_ptr, &args);
    _ = api.LLVMBuildBr(b, done);
    api.LLVMPositionBuilderAtEnd(b, done);
}

// Convert a raw enum payload i64 into a register value of the payload type.
pub fn enumPayloadFromRaw(fc: *FunctionCodegen, raw: llvm.c.LLVMValueRef, payload_ty: ir.ValueType) !llvm.c.LLVMValueRef {
    const api = fc.api;
    const b = fc.builder;
    return switch (payload_ty.kind) {
        .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => raw,
        .boolean => api.LLVMBuildTrunc(b, raw, fc.types.bool_ty, "enum.pay.bool"),
        .float => blk: {
            const d = api.LLVMBuildBitCast(b, raw, fc.types.double_ty, "enum.pay.double");
            break :blk if (payload_ty.name != null and std.mem.eql(u8, payload_ty.name.?, "F32"))
                api.LLVMBuildFPTrunc(b, d, fc.types.float_ty, "enum.pay.f32")
            else
                d;
        },
        .string => blk: {
            const sp = api.LLVMBuildIntToPtr(b, raw, fc.types.ptr_ty, "enum.pay.strptr");
            break :blk api.LLVMBuildLoad2(b, fc.types.string_ty, sp, "enum.pay.str");
        },
        .void => error.UnsupportedExecutableFeature,
    };
}

