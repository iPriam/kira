//! Bytecode decode pass for the VM interpreter.
//!
//! `prepare` turns a `bytecode.Module` into a `PreparedModule`: a per-function
//! rewritten copy of the instruction stream where every per-dispatch lookup the
//! interpreter used to perform has been resolved once, up front:
//!
//!   - `branch`/`jump` label ids become direct pc offsets (no label table or
//!     bounds checks on the hot path),
//!   - `label` instructions are removed entirely (they were per-iteration
//!     no-op dispatches in every loop),
//!   - `call_runtime` function ids become indices into `PreparedModule.functions`
//!     (no linear `findFunctionById` scan and no by-value `Function` copy per call),
//!   - `alloc_struct` type names get a parallel `module.types` index (no string
//!     hash per allocation),
//!   - struct locals that need pre-allocated backing storage are collected once
//!     per function instead of scanning `local_types` on every call,
//!   - hot multi-instruction patterns are fused into VM-internal
//!     superinstructions (see `Fusion` below),
//!   - a trailing implicit `ret` guarantees termination so the dispatch loop
//!     needs no pc bounds check.
//!
//! ## Fusion
//!
//! The compiler emits load/const/arith/compare/store sequences through
//! single-use temporary registers. When a temporary register is read exactly
//! once in the whole function and that read happens inside the pattern, the
//! pattern is collapsed into one fused instruction and the dead intermediate
//! register writes are dropped. Skipping a dead register write only delays the
//! *deallocation point* of whatever the register previously held until the
//! next write or frame exit — drops free memory and have no user-visible
//! side effects, so ownership semantics are unchanged.
//!
//! This is a pure decode/resolution pass: no semantics change, and failure
//! cases that used to surface at run time still surface at run time through
//! sentinel indices (`no_function_index`, `trap_label_index`, `no_type_index`).

const std = @import("std");
const bytecode = @import("kira_bytecode");

/// Sentinel for "type name could not be resolved statically"; the interpreter
/// falls back to the dynamic lookup (and its runtime error) in that case.
pub const no_type_index: u32 = std.math.maxInt(u32);

/// Sentinel for a `call_runtime` whose function id does not exist in the
/// module. Executing it reports the original "bytecode function id is out of
/// range" failure.
pub const no_function_index: u32 = std.math.maxInt(u32);

/// Sentinel for a branch/jump whose label could not be resolved at decode
/// time. The target is rewritten to a trap instruction carrying this index, so
/// executing the bad branch still fails at run time (and only then), matching
/// the previous interpreter behavior.
pub const trap_label_index: u32 = std.math.maxInt(u32) - 1;

pub const StructLocal = struct {
    local: u32,
    /// Index into `module.types`, or `no_type_index` when unresolved (the
    /// interpreter then reports the original runtime error).
    type_index: u32,
    type_name: ?[]const u8,
    /// Parameter slots only get a private pre-allocated copy destination when
    /// the VM copies struct args by value (pure-VM mode); hybrid mode aliases.
    param_only_when_copied: bool,
};

pub const PreparedFunction = struct {
    decl: *const bytecode.Function,
    /// Rewritten instruction copy: fused/compacted body + implicit `ret` + label trap.
    code: []bytecode.Instruction,
    /// Parallel to `code`: resolved `module.types` index for `alloc_struct`.
    alloc_type_index: []u32,
    struct_locals: []StructLocal,
    frame_size: usize,
};

pub const PreparedModule = struct {
    module: *const bytecode.Module,
    functions: []PreparedFunction,
    /// Function id -> index in `functions`. Dense array when ids are compact
    /// (the compiler assigns sequential ids); sparse map otherwise.
    index_by_id_dense: []u32,
    index_by_id_sparse: std.AutoHashMapUnmanaged(u32, u32) = .{},

    pub fn indexOfId(self: *const PreparedModule, id: u32) ?u32 {
        if (self.index_by_id_dense.len != 0) {
            if (id >= self.index_by_id_dense.len) return null;
            const index = self.index_by_id_dense[id];
            return if (index == no_function_index) null else index;
        }
        return self.index_by_id_sparse.get(id);
    }

    pub fn deinit(self: *PreparedModule, allocator: std.mem.Allocator) void {
        for (self.functions) |function| {
            allocator.free(function.code);
            allocator.free(function.alloc_type_index);
            allocator.free(function.struct_locals);
        }
        allocator.free(self.functions);
        allocator.free(self.index_by_id_dense);
        self.index_by_id_sparse.deinit(allocator);
    }
};

pub fn prepare(allocator: std.mem.Allocator, module: *const bytecode.Module) !*PreparedModule {
    const prepared = try allocator.create(PreparedModule);
    errdefer allocator.destroy(prepared);
    prepared.* = .{
        .module = module,
        .functions = &.{},
        .index_by_id_dense = &.{},
    };
    errdefer prepared.deinit(allocator);

    try buildIdIndex(allocator, prepared, module);

    var type_index_by_name: std.StringHashMapUnmanaged(u32) = .{};
    defer type_index_by_name.deinit(allocator);
    try type_index_by_name.ensureTotalCapacity(allocator, @intCast(module.types.len));
    for (module.types, 0..) |type_decl, index| {
        type_index_by_name.putAssumeCapacity(type_decl.name, @intCast(index));
    }

    const functions = try allocator.alloc(PreparedFunction, module.functions.len);
    var prepared_count: usize = 0;
    errdefer {
        for (functions[0..prepared_count]) |function| {
            allocator.free(function.code);
            allocator.free(function.alloc_type_index);
            allocator.free(function.struct_locals);
        }
        allocator.free(functions);
    }
    for (module.functions, 0..) |*decl, index| {
        functions[index] = try prepareFunction(allocator, prepared, &type_index_by_name, decl);
        prepared_count += 1;
    }
    prepared.functions = functions;
    if (std.c.getenv("KIRA_VM_DUMP_BYTECODE") != null) dump(prepared);
    return prepared;
}

/// Debug aid (KIRA_VM_DUMP_BYTECODE=1): print the decoded instruction stream
/// of every function so fusion/elision decisions can be inspected.
fn dump(prepared: *const PreparedModule) void {
    for (prepared.functions) |function| {
        std.debug.print("fn {s} (id={d} params={d} regs={d} locals={d})\n", .{
            function.decl.name,
            function.decl.id,
            function.decl.param_count,
            function.decl.register_count,
            function.decl.local_count,
        });
        for (function.code, 0..) |inst, pc| {
            switch (inst) {
                .branch => |value| std.debug.print("  {d:>4}: branch r{d} -> {d} / {d}\n", .{ pc, value.condition, value.true_label, value.false_label }),
                .jump => |value| std.debug.print("  {d:>4}: jump -> {d}\n", .{ pc, value.label }),
                .load_local => |value| std.debug.print("  {d:>4}: load_local r{d} <- l{d} ({s})\n", .{ pc, value.dst, value.local, @tagName(value.ownership) }),
                .store_local => |value| std.debug.print("  {d:>4}: store_local l{d} <- r{d}\n", .{ pc, value.local, value.src }),
                .array_get => |value| std.debug.print("  {d:>4}: array_get r{d} <- r{d}[r{d}] ({s})\n", .{ pc, value.dst, value.array, value.index, @tagName(value.ty.kind) }),
                .field_ptr => |value| std.debug.print("  {d:>4}: field_ptr r{d} <- r{d}.{d} ({s})\n", .{ pc, value.dst, value.base, value.field_index, @tagName(value.field_ty.kind) }),
                .load_indirect => |value| std.debug.print("  {d:>4}: load_indirect r{d} <- [r{d}] ({s})\n", .{ pc, value.dst, value.ptr, @tagName(value.ty.kind) }),
                .const_int => |value| std.debug.print("  {d:>4}: const_int r{d} <- {d}\n", .{ pc, value.dst, value.value }),
                .add => |value| std.debug.print("  {d:>4}: add r{d} <- r{d}, r{d}\n", .{ pc, value.dst, value.lhs, value.rhs }),
                .compare => |value| std.debug.print("  {d:>4}: compare r{d} <- r{d} {s} r{d}\n", .{ pc, value.dst, value.lhs, @tagName(value.op), value.rhs }),
                .call_runtime => |value| std.debug.print("  {d:>4}: call_runtime fi={d} args={d} dst={?d}\n", .{ pc, value.function_id, value.args.len, value.dst }),
                .array_len => |value| std.debug.print("  {d:>4}: array_len r{d} <- r{d}\n", .{ pc, value.dst, value.array }),
                .array_append => |value| std.debug.print("  {d:>4}: array_append r{d} <- r{d}\n", .{ pc, value.array, value.src }),
                .fused_cmp_local_const_branch => |value| std.debug.print("  {d:>4}: fused_cmp_local_const_branch l{d} {s} {d} -> {d} / {d}\n", .{ pc, value.local, @tagName(value.op), value.imm, value.true_target, value.false_target }),
                .fused_compare_branch => |value| std.debug.print("  {d:>4}: fused_compare_branch r{d} {s} r{d} -> {d} / {d}\n", .{ pc, value.lhs, @tagName(value.op), value.rhs, value.true_target, value.false_target }),
                .fused_compare_const_branch => |value| std.debug.print("  {d:>4}: fused_compare_const_branch r{d} {s} {d} -> {d} / {d}\n", .{ pc, value.lhs, @tagName(value.op), value.imm, value.true_target, value.false_target }),
                .fused_arith_locals_store => |value| std.debug.print("  {d:>4}: fused_arith_locals_store l{d} <- l{d} {s} l{d}\n", .{ pc, value.dst_local, value.lhs_local, @tagName(value.kind), value.rhs_local }),
                .fused_arith_local_const_store => |value| std.debug.print("  {d:>4}: fused_arith_local_const_store l{d} <- l{d} {s} {d}\n", .{ pc, value.dst_local, value.lhs_local, @tagName(value.kind), value.imm }),
                .fused_arith_locals_ret => |value| std.debug.print("  {d:>4}: fused_arith_locals_ret l{d} {s} l{d}\n", .{ pc, value.lhs_local, @tagName(value.kind), value.rhs_local }),
                else => std.debug.print("  {d:>4}: {s}\n", .{ pc, @tagName(inst) }),
            }
        }
    }
}

fn buildIdIndex(allocator: std.mem.Allocator, prepared: *PreparedModule, module: *const bytecode.Module) !void {
    var max_id: u32 = 0;
    for (module.functions) |function_decl| max_id = @max(max_id, function_decl.id);
    const dense_limit = module.functions.len * 4 + 1024;
    if (module.functions.len != 0 and @as(usize, max_id) < dense_limit) {
        const dense = try allocator.alloc(u32, @as(usize, max_id) + 1);
        @memset(dense, no_function_index);
        for (module.functions, 0..) |function_decl, index| dense[function_decl.id] = @intCast(index);
        prepared.index_by_id_dense = dense;
        return;
    }
    try prepared.index_by_id_sparse.ensureTotalCapacity(allocator, @intCast(module.functions.len));
    for (module.functions, 0..) |function_decl, index| {
        prepared.index_by_id_sparse.putAssumeCapacity(function_decl.id, @intCast(index));
    }
}

fn prepareFunction(
    allocator: std.mem.Allocator,
    prepared: *const PreparedModule,
    type_index_by_name: *const std.StringHashMapUnmanaged(u32),
    decl: *const bytecode.Function,
) !PreparedFunction {
    const instructions = decl.instructions;
    const body_len = instructions.len;

    const label_offsets = try buildLabelOffsets(allocator, instructions);
    defer allocator.free(label_offsets);
    const reads = try countRegisterReads(allocator, instructions, decl.register_count);
    defer allocator.free(reads);
    var analysis = try FunctionAnalysis.init(allocator, instructions, decl, reads);
    defer analysis.deinit(allocator);

    // Emit pass: copy instructions, dropping labels and fusing patterns.
    // old_to_new maps every original pc (plus the one-past-the-end pc) to the
    // emitted index where execution of that point begins; branch targets can
    // only land on label pcs, which map to the next emitted instruction.
    var code_list: std.ArrayListUnmanaged(bytecode.Instruction) = .empty;
    errdefer code_list.deinit(allocator);
    try code_list.ensureTotalCapacity(allocator, body_len + 2);
    const old_to_new = try allocator.alloc(u32, body_len + 1);
    defer allocator.free(old_to_new);

    var index: usize = 0;
    while (index < body_len) {
        old_to_new[index] = @intCast(code_list.items.len);
        if (analysis.bind_sites.contains(@intCast(index))) {
            const get = instructions[index].array_get;
            const binding = instructions[index + 1].load_local;
            for (index..index + 3) |consumed_pc| old_to_new[consumed_pc] = @intCast(code_list.items.len);
            try code_list.append(allocator, .{ .fused_array_bind_local = .{
                .array = get.array,
                .index = get.index,
                .dst_local = binding.local,
                .type_name = get.ty.name.?,
            } });
            index += 3;
            continue;
        }
        if (tryFuse(instructions, index, reads, decl)) |fused| {
            for (index..index + fused.consumed) |consumed_pc| {
                old_to_new[consumed_pc] = @intCast(code_list.items.len);
            }
            try code_list.append(allocator, fused.inst);
            index += fused.consumed;
            continue;
        }
        if (instructions[index] == .label) {
            index += 1;
            continue;
        }
        try code_list.append(allocator, instructions[index]);
        index += 1;
    }
    old_to_new[body_len] = @intCast(code_list.items.len);

    // Implicit return: falling off the end of a function returns void, exactly
    // like the old `while (pc < len)` loop exit.
    try code_list.append(allocator, .{ .ret = .{ .src = null } });
    // Label trap: unresolved branch targets land here and fail at run time.
    const trap_pc: u32 = @intCast(code_list.items.len);
    try code_list.append(allocator, .{ .call_runtime = .{ .function_id = trap_label_index, .args = &.{}, .dst = null } });

    const code = try code_list.toOwnedSlice(allocator);
    errdefer allocator.free(code);

    const alloc_type_index = try allocator.alloc(u32, code.len);
    errdefer allocator.free(alloc_type_index);
    @memset(alloc_type_index, no_type_index);

    // Fixup pass: resolve label ids to emitted pc offsets, function ids to
    // function indices, and alloc_struct type names to type indices.
    for (code, 0..) |*inst, pc| {
        switch (inst.*) {
            .branch => |*value| {
                value.true_label = mapLabel(label_offsets, old_to_new, trap_pc, value.true_label);
                value.false_label = mapLabel(label_offsets, old_to_new, trap_pc, value.false_label);
            },
            .jump => |*value| {
                value.label = mapLabel(label_offsets, old_to_new, trap_pc, value.label);
            },
            .fused_compare_branch => |*value| {
                value.true_target = mapLabel(label_offsets, old_to_new, trap_pc, value.true_target);
                value.false_target = mapLabel(label_offsets, old_to_new, trap_pc, value.false_target);
            },
            .fused_compare_const_branch => |*value| {
                value.true_target = mapLabel(label_offsets, old_to_new, trap_pc, value.true_target);
                value.false_target = mapLabel(label_offsets, old_to_new, trap_pc, value.false_target);
            },
            .fused_cmp_local_const_branch => |*value| {
                value.true_target = mapLabel(label_offsets, old_to_new, trap_pc, value.true_target);
                value.false_target = mapLabel(label_offsets, old_to_new, trap_pc, value.false_target);
            },
            .call_runtime => |*value| {
                // The trap instruction keeps its sentinel id.
                if (value.function_id != trap_label_index) {
                    value.function_id = prepared.indexOfId(value.function_id) orelse no_function_index;
                }
            },
            .alloc_struct => |value| {
                alloc_type_index[pc] = type_index_by_name.get(value.type_name) orelse no_type_index;
            },
            else => {},
        }
    }

    return .{
        .decl = decl,
        .code = code,
        .alloc_type_index = alloc_type_index,
        .struct_locals = try collectStructLocals(allocator, type_index_by_name, decl),
        .frame_size = @as(usize, decl.register_count) + @as(usize, decl.local_count),
    };
}

fn mapLabel(label_offsets: []const u32, old_to_new: []const u32, trap_pc: u32, label: u32) u32 {
    const old_pc = resolveLabel(label_offsets, label) orelse return trap_pc;
    return old_to_new[old_pc];
}

const FuseResult = struct { inst: bytecode.Instruction, consumed: usize };

fn tryFuse(
    instructions: []const bytecode.Instruction,
    index: usize,
    reads: []const u32,
    decl: *const bytecode.Function,
) ?FuseResult {
    const remaining = instructions.len - index;
    if (remaining >= 4) {
        if (fuseCmpLocalConstBranch(instructions, index, reads)) |result| return result;
        if (fuseArithLocalsStore(instructions, index, reads, decl)) |result| return result;
        if (fuseArithLocalConstStore(instructions, index, reads, decl)) |result| return result;
        if (fuseArithLocalsRet(instructions, index, reads)) |result| return result;
    }
    if (remaining >= 3) {
        if (fuseCompareConstBranch(instructions, index, reads)) |result| return result;
    }
    if (remaining >= 2) {
        if (fuseCompareBranch(instructions, index, reads)) |result| return result;
    }
    return null;
}

/// True when the register is read exactly once in the whole function — i.e.
/// the read inside the matched pattern is its only consumer, so the register
/// write can be elided.
fn readsOnce(reads: []const u32, register: u32) bool {
    return register < reads.len and reads[register] == 1;
}

const BorrowLoad = struct { dst: u32, local: u32 };

/// load_local variants that just copy the local into a register (all three
/// non-move ownerships take the same setSlotBorrowed path in the interpreter).
fn asBorrowLoad(inst: bytecode.Instruction) ?BorrowLoad {
    if (inst != .load_local) return null;
    const value = inst.load_local;
    return switch (value.ownership) {
        .borrow_read, .borrow_mut, .copy => .{ .dst = value.dst, .local = value.local },
        .move, .owned => null,
    };
}

const ArithMatch = struct { kind: bytecode.ArithKind, dst: u32, lhs: u32, rhs: u32 };

fn asArith(inst: bytecode.Instruction) ?ArithMatch {
    return switch (inst) {
        .add => |value| .{ .kind = .add, .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs },
        .subtract => |value| .{ .kind = .subtract, .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs },
        .multiply => |value| .{ .kind = .multiply, .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs },
        else => null,
    };
}

/// Stores into struct-typed locals go through the clone path and must not be
/// fused; everything else takes the plain setSlotBorrowed path (arith results
/// are never heap-managed: addValues and friends only produce int/float).
fn storableLocal(decl: *const bytecode.Function, local: u32) bool {
    if (local >= decl.local_types.len) return true;
    return decl.local_types[local].kind != .ffi_struct;
}

// load_local(a, L); const_int(c, imm); compare(d, a, c); branch(d, t, f)
fn fuseCmpLocalConstBranch(instructions: []const bytecode.Instruction, index: usize, reads: []const u32) ?FuseResult {
    const load = asBorrowLoad(instructions[index]) orelse return null;
    if (instructions[index + 1] != .const_int) return null;
    const constant = instructions[index + 1].const_int;
    if (instructions[index + 2] != .compare) return null;
    const compare = instructions[index + 2].compare;
    if (instructions[index + 3] != .branch) return null;
    const branch = instructions[index + 3].branch;
    if (load.dst == constant.dst) return null;
    if (compare.lhs != load.dst or compare.rhs != constant.dst) return null;
    if (branch.condition != compare.dst) return null;
    if (!readsOnce(reads, load.dst) or !readsOnce(reads, constant.dst) or !readsOnce(reads, compare.dst)) return null;
    return .{
        .inst = .{ .fused_cmp_local_const_branch = .{
            .local = load.local,
            .imm = constant.value,
            .op = compare.op,
            .true_target = branch.true_label,
            .false_target = branch.false_label,
        } },
        .consumed = 4,
    };
}

// load_local(a, L1); load_local(b, L2); <arith>(d, a, b); store_local(L3, d)
fn fuseArithLocalsStore(
    instructions: []const bytecode.Instruction,
    index: usize,
    reads: []const u32,
    decl: *const bytecode.Function,
) ?FuseResult {
    const lhs_load = asBorrowLoad(instructions[index]) orelse return null;
    const rhs_load = asBorrowLoad(instructions[index + 1]) orelse return null;
    const arith = asArith(instructions[index + 2]) orelse return null;
    if (instructions[index + 3] != .store_local) return null;
    const store = instructions[index + 3].store_local;
    if (lhs_load.dst == rhs_load.dst) return null;
    if (arith.lhs != lhs_load.dst or arith.rhs != rhs_load.dst) return null;
    if (store.src != arith.dst) return null;
    if (!readsOnce(reads, lhs_load.dst) or !readsOnce(reads, rhs_load.dst) or !readsOnce(reads, arith.dst)) return null;
    if (!storableLocal(decl, store.local)) return null;
    return .{
        .inst = .{ .fused_arith_locals_store = .{
            .kind = arith.kind,
            .lhs_local = lhs_load.local,
            .rhs_local = rhs_load.local,
            .dst_local = store.local,
        } },
        .consumed = 4,
    };
}

// load_local(a, L1); const_int(c, imm); <arith>(d, a, c); store_local(L2, d)
fn fuseArithLocalConstStore(
    instructions: []const bytecode.Instruction,
    index: usize,
    reads: []const u32,
    decl: *const bytecode.Function,
) ?FuseResult {
    const load = asBorrowLoad(instructions[index]) orelse return null;
    if (instructions[index + 1] != .const_int) return null;
    const constant = instructions[index + 1].const_int;
    const arith = asArith(instructions[index + 2]) orelse return null;
    if (instructions[index + 3] != .store_local) return null;
    const store = instructions[index + 3].store_local;
    if (load.dst == constant.dst) return null;
    if (arith.lhs != load.dst or arith.rhs != constant.dst) return null;
    if (store.src != arith.dst) return null;
    if (!readsOnce(reads, load.dst) or !readsOnce(reads, constant.dst) or !readsOnce(reads, arith.dst)) return null;
    if (!storableLocal(decl, store.local)) return null;
    return .{
        .inst = .{ .fused_arith_local_const_store = .{
            .kind = arith.kind,
            .lhs_local = load.local,
            .imm = constant.value,
            .dst_local = store.local,
        } },
        .consumed = 4,
    };
}

// load_local(a, L1); load_local(b, L2); <arith>(d, a, b); ret(d)
fn fuseArithLocalsRet(instructions: []const bytecode.Instruction, index: usize, reads: []const u32) ?FuseResult {
    const lhs_load = asBorrowLoad(instructions[index]) orelse return null;
    const rhs_load = asBorrowLoad(instructions[index + 1]) orelse return null;
    const arith = asArith(instructions[index + 2]) orelse return null;
    if (instructions[index + 3] != .ret) return null;
    const ret = instructions[index + 3].ret;
    if (lhs_load.dst == rhs_load.dst) return null;
    if (arith.lhs != lhs_load.dst or arith.rhs != rhs_load.dst) return null;
    if (ret.src == null or ret.src.? != arith.dst) return null;
    if (!readsOnce(reads, lhs_load.dst) or !readsOnce(reads, rhs_load.dst) or !readsOnce(reads, arith.dst)) return null;
    return .{
        .inst = .{ .fused_arith_locals_ret = .{
            .kind = arith.kind,
            .lhs_local = lhs_load.local,
            .rhs_local = rhs_load.local,
        } },
        .consumed = 4,
    };
}

// const_int(c, imm); compare(d, lhs, c); branch(d, t, f)
fn fuseCompareConstBranch(instructions: []const bytecode.Instruction, index: usize, reads: []const u32) ?FuseResult {
    if (instructions[index] != .const_int) return null;
    const constant = instructions[index].const_int;
    if (instructions[index + 1] != .compare) return null;
    const compare = instructions[index + 1].compare;
    if (instructions[index + 2] != .branch) return null;
    const branch = instructions[index + 2].branch;
    if (compare.rhs != constant.dst or compare.lhs == constant.dst) return null;
    if (branch.condition != compare.dst) return null;
    if (!readsOnce(reads, constant.dst) or !readsOnce(reads, compare.dst)) return null;
    return .{
        .inst = .{ .fused_compare_const_branch = .{
            .lhs = compare.lhs,
            .imm = constant.value,
            .op = compare.op,
            .true_target = branch.true_label,
            .false_target = branch.false_label,
        } },
        .consumed = 3,
    };
}

// compare(d, lhs, rhs); branch(d, t, f)
fn fuseCompareBranch(instructions: []const bytecode.Instruction, index: usize, reads: []const u32) ?FuseResult {
    if (instructions[index] != .compare) return null;
    const compare = instructions[index].compare;
    if (instructions[index + 1] != .branch) return null;
    const branch = instructions[index + 1].branch;
    if (branch.condition != compare.dst) return null;
    if (!readsOnce(reads, compare.dst)) return null;
    return .{
        .inst = .{ .fused_compare_branch = .{
            .lhs = compare.lhs,
            .rhs = compare.rhs,
            .op = compare.op,
            .true_target = branch.true_label,
            .false_target = branch.false_label,
        } },
        .consumed = 2,
    };
}

/// Decode-time proof machinery for `fused_array_bind_local` (loop-element
/// borrow elision). The compiler lowers `for x in array { ...reads of x... }`
/// to `array_get (deep clone); load_local x; copy_indirect (second deep
/// copy)`. The native backend never copies borrowed loop elements, so the VM
/// may alias the element instead — but only when it can PROVE, from the
/// bytecode alone, that
///
///   1. the array is stable for the binding's lifetime: it is rooted in a
///      local/param that is never reassigned, moved, or address-taken
///      (possibly through a chain of borrowed struct fields), so nothing in
///      this frame can drop it while the binding lives, and
///   2. the binding local is read-only: every access is a borrow load whose
///      register only feeds field reads (`field_ptr` + `load_indirect`),
///      copies FROM it, or `print` — never a mutation through it, a move, an
///      address-taking, a call argument, or a return, and
///   3. every write to the binding local is one of these elided bind sites
///      (a non-elided deep-copy write into a local that currently aliases an
///      array element would mutate the array itself).
///
/// Anything unproven keeps the original clone semantics.
const FunctionAnalysis = struct {
    bind_sites: std.AutoHashMapUnmanaged(u32, void) = .{},
    register_writes: []u32,
    register_def_pc: []u32,
    local_reassigned: []bool,

    fn deinit(self: *FunctionAnalysis, allocator: std.mem.Allocator) void {
        self.bind_sites.deinit(allocator);
        allocator.free(self.register_writes);
        allocator.free(self.register_def_pc);
        allocator.free(self.local_reassigned);
    }

    fn init(
        allocator: std.mem.Allocator,
        instructions: []const bytecode.Instruction,
        decl: *const bytecode.Function,
        reads: []const u32,
    ) !FunctionAnalysis {
        const register_writes = try allocator.alloc(u32, decl.register_count);
        errdefer allocator.free(register_writes);
        const register_def_pc = try allocator.alloc(u32, decl.register_count);
        errdefer allocator.free(register_def_pc);
        const local_reassigned = try allocator.alloc(bool, decl.local_count);
        errdefer allocator.free(local_reassigned);
        var self = FunctionAnalysis{
            .register_writes = register_writes,
            .register_def_pc = register_def_pc,
            .local_reassigned = local_reassigned,
        };
        errdefer self.bind_sites.deinit(allocator);
        @memset(self.register_writes, 0);
        @memset(self.register_def_pc, 0);
        @memset(self.local_reassigned, false);

        for (instructions, 0..) |inst, pc| {
            if (registerWriteOf(inst)) |register| {
                if (register < self.register_writes.len) {
                    self.register_writes[register] +|= 1;
                    self.register_def_pc[register] = @intCast(pc);
                }
            }
            switch (inst) {
                .store_local => |value| self.markLocalReassigned(value.local),
                .local_ptr => |value| self.markLocalReassigned(value.local),
                .load_local => |value| switch (value.ownership) {
                    .move, .owned => self.markLocalReassigned(value.local),
                    .borrow_read, .borrow_mut, .copy => {},
                },
                else => {},
            }
        }

        // Collect syntactic candidates, grouped by binding local. A local is
        // accepted only if ALL its candidate sites and ALL its reads validate
        // together (a single unproven access poisons the local).
        var candidate_sites: std.ArrayListUnmanaged(u32) = .empty;
        defer candidate_sites.deinit(allocator);
        var candidate_locals: std.AutoHashMapUnmanaged(u32, void) = .{};
        defer candidate_locals.deinit(allocator);
        if (std.c.getenv("KIRA_VM_NO_BIND_ELIDE") == null) {
            var index: usize = 0;
            while (index + 2 < instructions.len) : (index += 1) {
                const site_local = self.matchBindSite(instructions, decl, reads, index) orelse continue;
                try candidate_sites.append(allocator, @intCast(index));
                try candidate_locals.put(allocator, site_local, {});
            }
        }
        var locals_iterator = candidate_locals.keyIterator();
        while (locals_iterator.next()) |local_ptr| {
            const local = local_ptr.*;
            if (!self.validateBindingLocal(instructions, local, candidate_sites.items)) continue;
            for (candidate_sites.items) |site_pc| {
                if (instructions[site_pc + 1].load_local.local == local) {
                    try self.bind_sites.put(allocator, site_pc, {});
                }
            }
        }
        // Final pass: any copy_indirect destination that cannot be proven to
        // be either an accepted bind copy or a write to an unrelated,
        // uniquely-attributed local poisons everything — an unattributed
        // deep-copy could be writing into a binding local, and a non-elided
        // copy into an aliasing binding would mutate the array element itself.
        if (self.bind_sites.count() != 0) {
            for (instructions, 0..) |inst, pc| {
                if (inst != .copy_indirect) continue;
                if (pc >= 2 and self.bind_sites.contains(@intCast(pc - 2))) continue;
                const dst_reg = inst.copy_indirect.dst_ptr;
                const dst_local = self.uniqueBorrowLoadLocal(instructions, dst_reg) orelse {
                    // Unattributable destination: drop all elisions.
                    self.bind_sites.clearRetainingCapacity();
                    break;
                };
                if (self.localHasBindSites(instructions, dst_local)) {
                    self.removeBindSitesOf(instructions, dst_local);
                }
            }
        }
        return self;
    }

    /// Instructions that end a borrow-elision span: anything that can mutate
    /// an array element, drop a container that (transitively) owns the array,
    /// or transfer control somewhere this linear analysis cannot follow.
    /// `label` is included because labels are the only branch entry points —
    /// a label inside the span would let control reach the binding's reads
    /// without re-executing the bind.
    fn isBindBarrier(inst: bytecode.Instruction) bool {
        return switch (inst) {
            .label,
            .call_runtime,
            .call_native,
            .call_virtual,
            .call_value,
            .array_set,
            .array_append,
            .store_indirect,
            .copy_indirect,
            .native_state_field_set,
            .recover_native_state,
            .alloc_native_state,
            .const_closure,
            => true,
            else => false,
        };
    }

    /// First pc after the bind site's three instructions at which the elided
    /// binding stops being provably valid.
    fn bindSpanEnd(instructions: []const bytecode.Instruction, site_pc: u32) u32 {
        var pc: usize = site_pc + 3;
        while (pc < instructions.len) : (pc += 1) {
            if (isBindBarrier(instructions[pc])) return @intCast(pc);
        }
        return @intCast(instructions.len);
    }

    /// Validates every access to a binding local against the candidate sites:
    /// each non-bind borrow load (and everything derived from it) must sit
    /// inside the barrier-free span of the nearest preceding bind site.
    fn validateBindingLocal(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        local: u32,
        candidate_sites: []const u32,
    ) bool {
        if (!self.localStable(local)) return false;
        for (instructions, 0..) |inst, pc| {
            if (inst != .load_local) continue;
            const load = inst.load_local;
            if (load.local != local) continue;
            switch (load.ownership) {
                .borrow_read, .copy => {},
                .borrow_mut, .move, .owned => return false,
            }
            if (isBindSiteLoad(instructions, candidate_sites, local, pc)) continue;
            // Find the nearest preceding candidate site for this local.
            var covering_site: ?u32 = null;
            for (candidate_sites) |site_pc| {
                if (instructions[site_pc + 1].load_local.local != local) continue;
                if (site_pc + 2 < pc and (covering_site == null or site_pc > covering_site.?)) covering_site = site_pc;
            }
            const site_pc = covering_site orelse return false;
            const span_end = bindSpanEnd(instructions, site_pc);
            if (pc >= span_end) return false;
            if (!self.registerOnlyReadThrough(instructions, load.dst, @intCast(pc), span_end)) return false;
        }
        return true;
    }

    fn isBindSiteLoad(
        instructions: []const bytecode.Instruction,
        candidate_sites: []const u32,
        local: u32,
        pc: usize,
    ) bool {
        for (candidate_sites) |site_pc| {
            if (site_pc + 1 == pc and instructions[site_pc + 1].load_local.local == local) return true;
        }
        return false;
    }

    fn markLocalReassigned(self: *FunctionAnalysis, local: u32) void {
        if (local < self.local_reassigned.len) self.local_reassigned[local] = true;
    }

    fn localStable(self: *const FunctionAnalysis, local: u32) bool {
        return local < self.local_reassigned.len and !self.local_reassigned[local];
    }

    fn uniqueDef(self: *const FunctionAnalysis, instructions: []const bytecode.Instruction, register: u32) ?bytecode.Instruction {
        if (register >= self.register_writes.len or self.register_writes[register] != 1) return null;
        return instructions[self.register_def_pc[register]];
    }

    fn uniqueBorrowLoadLocal(self: *const FunctionAnalysis, instructions: []const bytecode.Instruction, register: u32) ?u32 {
        const def = self.uniqueDef(instructions, register) orelse return null;
        if (def != .load_local) return null;
        return def.load_local.local;
    }

    fn matchBindSite(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        decl: *const bytecode.Function,
        reads: []const u32,
        pc: usize,
    ) ?u32 {
        if (instructions[pc] != .array_get) return null;
        const get = instructions[pc].array_get;
        if (get.ty.kind != .ffi_struct or get.ty.name == null) return null;
        if (instructions[pc + 1] != .load_local) return null;
        const binding = instructions[pc + 1].load_local;
        switch (binding.ownership) {
            .borrow_read, .copy => {},
            .borrow_mut, .move, .owned => return null,
        }
        if (instructions[pc + 2] != .copy_indirect) return null;
        const copy = instructions[pc + 2].copy_indirect;
        if (copy.dst_ptr != binding.dst or copy.src_ptr != get.dst) return null;
        if (copy.dst_ptr == copy.src_ptr) return null;
        if (!readsOnce(reads, get.dst) or !readsOnce(reads, binding.dst)) return null;
        if (binding.local >= decl.local_types.len or decl.local_types[binding.local].kind != .ffi_struct) return null;
        if (!self.stableArraySource(instructions, decl, get.array, 4)) return null;
        return binding.local;
    }

    /// The array register must be rooted, through borrowed loads only, in a
    /// local that is never reassigned/moved/address-taken in this function —
    /// so nothing this frame does can drop the array while a binding lives.
    fn stableArraySource(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        decl: *const bytecode.Function,
        register: u32,
        depth: usize,
    ) bool {
        if (depth == 0) return false;
        const def = self.uniqueDef(instructions, register) orelse return false;
        return switch (def) {
            .load_local => |value| switch (value.ownership) {
                .borrow_read, .copy => self.localStable(value.local),
                .borrow_mut, .move, .owned => false,
            },
            .load_indirect => |value| value.ty.kind == .array and self.stableSlotPointer(instructions, decl, value.ptr, depth - 1),
            else => false,
        };
    }

    fn stableSlotPointer(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        decl: *const bytecode.Function,
        register: u32,
        depth: usize,
    ) bool {
        if (depth == 0) return false;
        const def = self.uniqueDef(instructions, register) orelse return false;
        return switch (def) {
            .field_ptr => |value| self.stableStructSource(instructions, decl, value.base, depth - 1),
            else => false,
        };
    }

    fn stableStructSource(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        decl: *const bytecode.Function,
        register: u32,
        depth: usize,
    ) bool {
        if (depth == 0) return false;
        const def = self.uniqueDef(instructions, register) orelse return false;
        return switch (def) {
            .load_local => |value| switch (value.ownership) {
                .borrow_read, .copy => self.localStable(value.local),
                .borrow_mut, .move, .owned => false,
            },
            .field_ptr => |value| value.field_ty.kind == .ffi_struct and self.stableStructSource(instructions, decl, value.base, depth - 1),
            else => false,
        };
    }

    /// True when every read of `register` (the borrow-load result) is a
    /// read-only projection that happens inside the bind span: field_ptr whose
    /// result only feeds load_indirect within the span, or print within the
    /// span. Any read outside (load_pc, span_end) fails — even reads belonging
    /// to a different definition of the same register, which keeps multi-def
    /// register reuse conservatively rejected.
    fn registerOnlyReadThrough(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        register: u32,
        load_pc: u32,
        span_end: u32,
    ) bool {
        for (instructions, 0..) |inst, pc| {
            switch (inst) {
                .field_ptr => |value| {
                    if (value.base != register) continue;
                    if (pc <= load_pc or pc >= span_end) return false;
                    if (!self.fieldPointerOnlyRead(instructions, value.dst, @intCast(pc), span_end)) return false;
                },
                .print => |value| {
                    if (value.src == register and (pc <= load_pc or pc >= span_end)) return false;
                },
                else => {
                    if (instructionReadsRegister(inst, register)) return false;
                },
            }
        }
        return true;
    }

    fn fieldPointerOnlyRead(
        self: *const FunctionAnalysis,
        instructions: []const bytecode.Instruction,
        register: u32,
        field_pc: u32,
        span_end: u32,
    ) bool {
        _ = self;
        for (instructions, 0..) |inst, pc| {
            switch (inst) {
                .load_indirect => |value| {
                    if (value.ptr == register and (pc <= field_pc or pc >= span_end)) return false;
                },
                else => {
                    if (instructionReadsRegister(inst, register)) return false;
                },
            }
        }
        return true;
    }

    fn localHasBindSites(self: *const FunctionAnalysis, instructions: []const bytecode.Instruction, local: u32) bool {
        var iterator = self.bind_sites.keyIterator();
        while (iterator.next()) |site_pc| {
            if (instructions[site_pc.* + 1].load_local.local == local) return true;
        }
        return false;
    }

    fn removeBindSitesOf(self: *FunctionAnalysis, instructions: []const bytecode.Instruction, local: u32) void {
        while (true) {
            var found: ?u32 = null;
            var iterator = self.bind_sites.keyIterator();
            while (iterator.next()) |site_pc| {
                if (instructions[site_pc.* + 1].load_local.local == local) {
                    found = site_pc.*;
                    break;
                }
            }
            const doomed = found orelse return;
            _ = self.bind_sites.remove(doomed);
        }
    }
};

/// True when `inst` reads `register` in ANY operand position. Used as the
/// conservative rejection test of the borrow-elision proofs: anything not in
/// the explicit allow-list that touches the register disqualifies it.
fn instructionReadsRegister(inst: bytecode.Instruction, register: u32) bool {
    switch (inst) {
        .const_int, .const_float, .const_string, .const_bool, .const_null_ptr, .const_function, .alloc_struct, .load_local, .local_ptr, .jump, .label => return false,
        .const_closure => |value| {
            for (value.captures) |capture| if (capture == register) return true;
            return false;
        },
        .alloc_enum => |value| return value.payload_src == register,
        .alloc_native_state => |value| return value.src == register,
        .alloc_array => |value| return value.len == register,
        .add => |value| return value.lhs == register or value.rhs == register,
        .subtract => |value| return value.lhs == register or value.rhs == register,
        .multiply => |value| return value.lhs == register or value.rhs == register,
        .divide => |value| return value.lhs == register or value.rhs == register,
        .modulo => |value| return value.lhs == register or value.rhs == register,
        .convert => |value| return value.src == register,
        .compare => |value| return value.lhs == register or value.rhs == register,
        .unary => |value| return value.src == register,
        .store_local => |value| return value.src == register,
        .subobject_ptr => |value| return value.base == register,
        .field_ptr => |value| return value.base == register,
        .recover_native_state => |value| return value.state == register,
        .native_state_field_get => |value| return value.state == register,
        .native_state_field_set => |value| return value.state == register or value.src == register,
        .c_string_to_string => |value| return value.src == register,
        .array_len => |value| return value.array == register,
        .string_len => |value| return value.string == register,
        .array_get => |value| return value.array == register or value.index == register,
        .array_set => |value| return value.array == register or value.index == register or value.src == register,
        .array_append => |value| return value.array == register or value.src == register,
        .enum_tag => |value| return value.src == register,
        .enum_payload => |value| return value.src == register,
        .load_indirect => |value| return value.ptr == register,
        .store_indirect => |value| return value.ptr == register or value.src == register,
        .copy_indirect => |value| return value.dst_ptr == register or value.src_ptr == register,
        .branch => |value| return value.condition == register,
        .print => |value| return value.src == register,
        .call_runtime => |value| {
            for (value.args) |arg| if (arg == register) return true;
            return false;
        },
        .call_native => |value| {
            for (value.args) |arg| if (arg == register) return true;
            return false;
        },
        .call_virtual => |value| {
            if (value.receiver == register) return true;
            for (value.args) |arg| if (arg == register) return true;
            return false;
        },
        .call_value => |value| {
            if (value.callee == register) return true;
            for (value.args) |arg| if (arg == register) return true;
            return false;
        },
        .ret => |value| return value.src == register,
        .fused_compare_branch => |value| return value.lhs == register or value.rhs == register,
        .fused_compare_const_branch => |value| return value.lhs == register,
        .fused_array_bind_local => |value| return value.array == register or value.index == register,
        .fused_cmp_local_const_branch, .fused_arith_locals_store, .fused_arith_local_const_store, .fused_arith_locals_ret => return false,
    }
}

/// The register written by `inst`, if any.
fn registerWriteOf(inst: bytecode.Instruction) ?u32 {
    return switch (inst) {
        .const_int => |value| value.dst,
        .const_float => |value| value.dst,
        .const_string => |value| value.dst,
        .const_bool => |value| value.dst,
        .const_null_ptr => |value| value.dst,
        .const_function => |value| value.dst,
        .const_closure => |value| value.dst,
        .alloc_struct => |value| value.dst,
        .alloc_enum => |value| value.dst,
        .alloc_native_state => |value| value.dst,
        .alloc_array => |value| value.dst,
        .add => |value| value.dst,
        .subtract => |value| value.dst,
        .multiply => |value| value.dst,
        .divide => |value| value.dst,
        .modulo => |value| value.dst,
        .compare => |value| value.dst,
        .unary => |value| value.dst,
        .load_local => |value| value.dst,
        .local_ptr => |value| value.dst,
        .subobject_ptr => |value| value.dst,
        .field_ptr => |value| value.dst,
        .recover_native_state => |value| value.dst,
        .native_state_field_get => |value| value.dst,
        .c_string_to_string => |value| value.dst,
        .array_len => |value| value.dst,
        .string_len => |value| value.dst,
        .array_get => |value| value.dst,
        .enum_tag => |value| value.dst,
        .enum_payload => |value| value.dst,
        .load_indirect => |value| value.dst,
        .call_runtime => |value| value.dst,
        .call_native => |value| value.dst,
        .call_virtual => |value| value.dst,
        .call_value => |value| value.dst,
        else => null,
    };
}

/// Exact per-register read counts across the whole function. Fusion relies on
/// this being complete: a missed read could elide a register write that a
/// later instruction observes. Every register-reading operand of every opcode
/// must be counted here.
fn countRegisterReads(allocator: std.mem.Allocator, instructions: []const bytecode.Instruction, register_count: u32) ![]u32 {
    const reads = try allocator.alloc(u32, register_count);
    @memset(reads, 0);
    for (instructions) |inst| {
        switch (inst) {
            .const_int, .const_float, .const_string, .const_bool, .const_null_ptr, .const_function => {},
            .const_closure => |value| for (value.captures) |register| bumpRead(reads, register),
            .alloc_struct => {},
            .alloc_enum => |value| if (value.payload_src) |register| bumpRead(reads, register),
            .alloc_native_state => |value| bumpRead(reads, value.src),
            .alloc_array => |value| bumpRead(reads, value.len),
            .add => |value| {
                bumpRead(reads, value.lhs);
                bumpRead(reads, value.rhs);
            },
            .subtract => |value| {
                bumpRead(reads, value.lhs);
                bumpRead(reads, value.rhs);
            },
            .multiply => |value| {
                bumpRead(reads, value.lhs);
                bumpRead(reads, value.rhs);
            },
            .divide => |value| {
                bumpRead(reads, value.lhs);
                bumpRead(reads, value.rhs);
            },
            .modulo => |value| {
                bumpRead(reads, value.lhs);
                bumpRead(reads, value.rhs);
            },
            .convert => |value| bumpRead(reads, value.src),
            .compare => |value| {
                bumpRead(reads, value.lhs);
                bumpRead(reads, value.rhs);
            },
            .unary => |value| bumpRead(reads, value.src),
            .store_local => |value| bumpRead(reads, value.src),
            .load_local, .local_ptr => {},
            .subobject_ptr => |value| bumpRead(reads, value.base),
            .field_ptr => |value| bumpRead(reads, value.base),
            .recover_native_state => |value| bumpRead(reads, value.state),
            .native_state_field_get => |value| bumpRead(reads, value.state),
            .native_state_field_set => |value| {
                bumpRead(reads, value.state);
                bumpRead(reads, value.src);
            },
            .c_string_to_string => |value| bumpRead(reads, value.src),
            .array_len => |value| bumpRead(reads, value.array),
            .string_len => |value| bumpRead(reads, value.string),
            .array_get => |value| {
                bumpRead(reads, value.array);
                bumpRead(reads, value.index);
            },
            .array_set => |value| {
                bumpRead(reads, value.array);
                bumpRead(reads, value.index);
                bumpRead(reads, value.src);
            },
            .array_append => |value| {
                bumpRead(reads, value.array);
                bumpRead(reads, value.src);
            },
            .enum_tag => |value| bumpRead(reads, value.src),
            .enum_payload => |value| bumpRead(reads, value.src),
            .load_indirect => |value| bumpRead(reads, value.ptr),
            .store_indirect => |value| {
                bumpRead(reads, value.ptr);
                bumpRead(reads, value.src);
            },
            .copy_indirect => |value| {
                bumpRead(reads, value.dst_ptr);
                bumpRead(reads, value.src_ptr);
            },
            .branch => |value| bumpRead(reads, value.condition),
            .jump, .label => {},
            .print => |value| bumpRead(reads, value.src),
            .call_runtime => |value| for (value.args) |register| bumpRead(reads, register),
            .call_native => |value| for (value.args) |register| bumpRead(reads, register),
            .call_virtual => |value| {
                bumpRead(reads, value.receiver);
                for (value.args) |register| bumpRead(reads, register);
            },
            .call_value => |value| {
                bumpRead(reads, value.callee);
                for (value.args) |register| bumpRead(reads, register);
            },
            .ret => |value| if (value.src) |register| bumpRead(reads, register),
            // Fused instructions never appear in compiler/serializer output,
            // which is the only input this pass sees.
            .fused_compare_branch,
            .fused_compare_const_branch,
            .fused_cmp_local_const_branch,
            .fused_arith_locals_store,
            .fused_arith_local_const_store,
            .fused_arith_locals_ret,
            .fused_array_bind_local,
            => unreachable,
        }
    }
    return reads;
}

fn bumpRead(reads: []u32, register: u32) void {
    // Out-of-range register operands (malformed modules) simply never count as
    // single-read, so no pattern touching them fuses.
    if (register < reads.len) reads[register] +|= 1;
}

fn collectStructLocals(
    allocator: std.mem.Allocator,
    type_index_by_name: *const std.StringHashMapUnmanaged(u32),
    decl: *const bytecode.Function,
) ![]StructLocal {
    var struct_locals: std.ArrayListUnmanaged(StructLocal) = .empty;
    errdefer struct_locals.deinit(allocator);
    for (decl.local_types, 0..) |local_ty, index| {
        if (local_ty.kind != .ffi_struct) continue;
        var param_only_when_copied = false;
        if (index < decl.param_count) {
            // Borrow/`borrow mut` struct parameters always alias the caller's
            // struct and never get a private copy destination.
            const mode = ownershipModeAt(decl.param_ownership, index);
            if (mode == .borrow_read or mode == .borrow_mut) continue;
            param_only_when_copied = true;
        }
        const type_index = if (local_ty.name) |name|
            type_index_by_name.get(name) orelse no_type_index
        else
            no_type_index;
        try struct_locals.append(allocator, .{
            .local = @intCast(index),
            .type_index = type_index,
            .type_name = local_ty.name,
            .param_only_when_copied = param_only_when_copied,
        });
    }
    return struct_locals.toOwnedSlice(allocator);
}

fn ownershipModeAt(values: []const bytecode.OwnershipMode, index: usize) bytecode.OwnershipMode {
    if (index < values.len) return values[index];
    return .owned;
}

fn buildLabelOffsets(allocator: std.mem.Allocator, instructions: []const bytecode.Instruction) ![]u32 {
    var max_label: usize = 0;
    var has_label = false;
    for (instructions) |inst| {
        if (inst != .label) continue;
        has_label = true;
        max_label = @max(max_label, @as(usize, @intCast(inst.label.id)));
    }
    if (!has_label) return allocator.alloc(u32, 0);

    const offsets = try allocator.alloc(u32, max_label + 1);
    @memset(offsets, std.math.maxInt(u32));
    for (instructions, 0..) |inst, index| {
        if (inst != .label) continue;
        offsets[@as(usize, @intCast(inst.label.id))] = @intCast(index);
    }
    return offsets;
}

fn resolveLabel(label_offsets: []const u32, label: u32) ?u32 {
    if (label >= label_offsets.len) return null;
    const offset = label_offsets[label];
    if (offset == std.math.maxInt(u32)) return null;
    return offset;
}
