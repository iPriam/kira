const std = @import("std");
const ir = @import("ir.zig");
const model = @import("kira_semantics_model");
const type_impl = @import("lower_from_hir_types.zig");

const lowerResolvedType = type_impl.lowerResolvedType;

/// A deferred array-element write-back.
///
/// When a mutation targets a place rooted at an array index, the VM materialises the
/// element by value (`array_get` deep-copies; the VM has no element-pointer op), so an
/// in-place store/append into that element only mutates a transient copy. The copy must
/// be persisted back into its array with `array_set`. Write-backs are emitted
/// deepest-first so a nested array element is persisted into its parent element copy
/// before the parent copy is persisted into its own array.
pub const Writeback = struct {
    array: u32,
    index: u32,
    elem: u32,
};

pub const WritebackList = std.array_list.Managed(Writeback);

/// Lower `expr` as the *object* of a mutation (an assignment target's base, an
/// `array.append` receiver, or a `borrow mut` argument) and return a register that
/// refers to the object's storage so that a subsequent `field_ptr` / `array_set` /
/// `array_append` mutates it.
///
/// This mirrors the read lowering of the same place (`Lowerer.lowerExpr`) so a deep
/// store resolves through the identical projection chain, but additionally records the
/// element write-backs the VM needs: for every array-index projection in the chain the
/// element is read by value (`array_get`) and a matching `array_set` write-back is
/// appended to `writebacks`. Callers must emit those after performing the mutation (see
/// `emitWritebacks`).
///
/// Backend-agnostic: LLVM/native round-trip `array_get`/`array_set` the same way (the
/// single-projection `arr[i].field = v` case has always relied on it), so generalising
/// it to deeper chains keeps VM/LLVM/hybrid parity.
pub fn lowerMutableObject(
    lowerer: anytype,
    instructions: *std.array_list.Managed(ir.Instruction),
    expr: *model.Expr,
    writebacks: *WritebackList,
) anyerror!u32 {
    switch (expr.*) {
        .index => |node| {
            // The place passes through an array index: materialise the element copy and
            // schedule its write-back so mutations through deeper projections survive.
            const array_reg = try lowerMutableObject(lowerer, instructions, node.object, writebacks);
            const index_reg = try lowerer.lowerExpr(instructions, node.index);
            const elem_ty = try lowerResolvedType(lowerer.program, node.ty);
            const elem_reg = lowerer.freshRegister();
            try instructions.append(.{ .array_get = .{
                .dst = elem_reg,
                .array = array_reg,
                .index = index_reg,
                .ty = elem_ty,
            } });
            try writebacks.append(.{ .array = array_reg, .index = index_reg, .elem = elem_reg });
            return elem_reg;
        },
        .field => |node| {
            // A `native_state_view` field is backed by its own storage and has dedicated
            // get/set ops; it never roots a managed-array write-back, so defer to the
            // value lowering (which emits `native_state_field_get`).
            if (model.hir.exprType(node.object.*).kind == .native_state_view) {
                return try lowerer.lowerExpr(instructions, expr);
            }
            const base_reg = try lowerMutableObject(lowerer, instructions, node.object, writebacks);
            const field_ty = try lowerResolvedType(lowerer.program, node.ty);
            const field_ptr_reg = lowerer.freshRegister();
            try instructions.append(.{ .field_ptr = .{
                .dst = field_ptr_reg,
                .base = base_reg,
                .base_type_name = node.container_type_name,
                .field_index = node.field_index,
                .field_ty = field_ty,
            } });
            // A nested struct field's `field_ptr` already points in place, so deeper
            // projections mutate the parent element copy directly. Non-struct fields
            // (e.g. an array reference) are loaded so the handle itself can be mutated
            // in place (`array_set`/`array_append`) and persisted by the parent
            // element's write-back.
            if (field_ty.kind == .ffi_struct) return field_ptr_reg;
            const dst = lowerer.freshRegister();
            try instructions.append(.{ .load_indirect = .{
                .dst = dst,
                .ptr = field_ptr_reg,
                .ty = field_ty,
            } });
            return dst;
        },
        // The place root (a local/array reference) and anything that is not a struct
        // field or array index is lowered by value; mutations through a local struct or
        // array handle already act in place, so no write-back is required.
        else => return try lowerer.lowerExpr(instructions, expr),
    }
}

/// Emit the deferred array-element write-backs (deepest-first) recorded by
/// `lowerMutableObject`, persisting each mutated element copy back into its array.
pub fn emitWritebacks(
    instructions: *std.array_list.Managed(ir.Instruction),
    writebacks: *const WritebackList,
) !void {
    var i = writebacks.items.len;
    while (i > 0) {
        i -= 1;
        const wb = writebacks.items[i];
        try instructions.append(.{ .array_set = .{
            .array = wb.array,
            .index = wb.index,
            .src = wb.elem,
        } });
    }
}

/// Ownership of parameter `arg_index` of the directly-called function `function_id`, or
/// `.owned` when the callee/parameter cannot be resolved (e.g. a generated function).
/// Used to decide whether an array-element argument passed to a `borrow mut` parameter
/// must be read-modified-written back after the call.
pub fn paramOwnership(program: model.Program, function_id: u32, arg_index: usize) model.OwnershipMode {
    for (program.functions) |function_decl| {
        if (function_decl.id != function_id) continue;
        if (arg_index >= function_decl.params.len) return .owned;
        return function_decl.params[arg_index].ownership;
    }
    return .owned;
}

/// Lower the arguments of a direct call, materialising any array-element argument that
/// is passed to a `borrow mut` parameter as a write-back-tracked element copy so the
/// callee's mutations are persisted back into the array after the call. Other arguments
/// are lowered by value exactly as before. Records the required write-backs in
/// `writebacks`; the caller emits them after the call instruction (see `emitWritebacks`).
pub fn lowerDirectCallArgs(
    lowerer: anytype,
    instructions: *std.array_list.Managed(ir.Instruction),
    args: []const *model.Expr,
    function_id: u32,
    writebacks: *WritebackList,
) ![]u32 {
    const regs = try lowerer.allocator.alloc(u32, args.len);
    errdefer lowerer.allocator.free(regs);
    for (args, 0..) |arg, k| {
        if (paramOwnership(lowerer.program, function_id, k) == .borrow_mut) {
            regs[k] = try lowerMutableObject(lowerer, instructions, arg, writebacks);
        } else {
            regs[k] = try lowerer.lowerExpr(instructions, arg);
        }
    }
    return regs;
}
