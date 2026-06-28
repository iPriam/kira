// Per-function instruction lowering for the LLVM C-API backend. Split out of
// backend_capi.zig (Core Law #5): the FunctionCodegen state machine that walks
// a function's IR instructions and builds LLVM basic blocks and values.
const std = @import("std");
const ir = @import("kira_ir");
const backend_api = @import("kira_backend_api");
const llvm = @import("llvm_c.zig");
const utils = @import("backend_utils.zig");
const runtime_symbols = @import("runtime_symbols.zig");
const capi = @import("backend_capi.zig");
const dispatch = @import("backend_capi_dispatch.zig");
const DispatcherDecl = dispatch.DispatcherDecl;
const hashCallValueSignature = dispatch.hashCallValueSignature;
const unpackBridgeValue = dispatch.unpackBridgeValue;

const functionExecutionById = utils.functionExecutionById;
const functionById = utils.functionById;
const resolveExecution = utils.resolveExecution;
const inferRegisterTypes = utils.inferRegisterTypes;
const allocPrintZ = utils.allocPrintZ;
const findEnumDecl = utils.findEnumDecl;
const print = @import("backend_capi_print.zig");
const drop = @import("backend_capi_drop.zig");
const aggregate = @import("backend_capi_aggregate.zig");
const closures = @import("backend_capi_closures.zig");
const ffi = @import("backend_capi_ffi.zig");
const calls = @import("backend_capi_calls.zig");

pub const FunctionCodegen = struct {
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    builder: llvm.c.LLVMBuilderRef,
    module_ref: llvm.c.LLVMModuleRef,
    types: capi.Types,
    runtime_decls: capi.RuntimeDecls,
    struct_types: *const std.StringHashMapUnmanaged(llvm.c.LLVMTypeRef),
    dispatchers: *const std.AutoHashMapUnmanaged(u64, DispatcherDecl),
    dtors: *const drop.Destructors,
    drop_enabled: bool,
    request: backend_api.CompileRequest,
    functions: *const std.AutoHashMapUnmanaged(u32, llvm.c.LLVMValueRef),
    function_decl: ir.Function,
    function_value: llvm.c.LLVMValueRef,

    registers: []llvm.c.LLVMValueRef = &.{},
    register_types: []ir.ValueType = &.{},
    locals: []llvm.c.LLVMValueRef = &.{},
    blocks: std.AutoHashMapUnmanaged(u32, llvm.c.LLVMBasicBlockRef) = .{},
    string_counter: usize = 0,
    terminated: bool = false,
    // Owned-value drop state (active only when drop_enabled). One cleanup slot
    // (entry-block alloca ptr, init null) per heap-allocating instruction; the
    // register/local holding the value points at its slot index.
    drop_slots: std.ArrayListUnmanaged(drop.OwnedSlot) = .empty,
    register_slot: []?u32 = &.{},
    local_slot: []?u32 = &.{},
    // register -> backing local (load_local/local_ptr) so copy_indirect can resolve
    // which local owns its destination contents.
    reg_local: []?u32 = &.{},
    // local -> struct_contents cleanup slot index for copy_indirect destinations.
    copy_dest_slot: []?u32 = &.{},
    // local -> per-local cleanup slot index for owned enum locals (`var`/`let` of enum
    // type). Mirrors copy_dest_slot for structs: one slot per enum local holding the
    // local's current live heap enum block, reused across reassignments (drop-before-
    // overwrite), freed once at exit, escaped on return. Without it each alloc_enum gets
    // its own slot and a branch-reassigned enum var's return frees the wrong (last-
    // lowered) branch's slot, freeing the live returned enum (use-after-free).
    enum_local_slot: []?u32 = &.{},

    // Build a scratch `alloca` in the function entry block regardless of where the
    // builder is currently positioned. LLVM only reclaims (and SROA/mem2reg only
    // promotes) allocas placed in the entry block; an alloca emitted at an arbitrary
    // insertion point inside a loop body becomes a *dynamic* stack allocation whose
    // space is not released until the function returns — so a per-iteration scratch
    // slot (array op / runtime-call / FFI bridge buffer) grows the stack every
    // iteration and overflows it. These scratch slots are written and consumed
    // immediately, so hoisting the alloca to the entry block (allocated once, reused
    // each iteration) is both correct and the standard LLVM idiom. The store/use of
    // the slot stays at the caller's current position.
    pub fn entryAlloca(self: *FunctionCodegen, ty: llvm.c.LLVMTypeRef, name: [:0]const u8) llvm.c.LLVMValueRef {
        const api = self.api;
        const restore = api.LLVMGetInsertBlock(self.builder);
        const entry = api.LLVMGetEntryBasicBlock(self.function_value);
        const terminator = api.LLVMGetBasicBlockTerminator(entry);
        if (terminator != null) {
            api.LLVMPositionBuilderBefore(self.builder, terminator);
        } else {
            api.LLVMPositionBuilderAtEnd(self.builder, entry);
        }
        const slot = api.LLVMBuildAlloca(self.builder, ty, name.ptr);
        api.LLVMPositionBuilderAtEnd(self.builder, restore);
        return slot;
    }

    pub fn lower(self: *FunctionCodegen) !void {
        const api = self.api;
        const entry_block = api.LLVMAppendBasicBlockInContext(self.types.context, self.function_value, "entry");
        api.LLVMPositionBuilderAtEnd(self.builder, entry_block);

        try drop.setup(self);
        defer drop.teardown(self);

        self.register_types = try inferRegisterTypes(self.allocator, self.request.program.programPtr().*, self.function_decl);
        defer self.allocator.free(self.register_types);
        self.registers = try self.allocator.alloc(llvm.c.LLVMValueRef, self.function_decl.register_count);
        defer self.allocator.free(self.registers);
        @memset(self.registers, null);
        self.locals = try self.allocator.alloc(llvm.c.LLVMValueRef, self.function_decl.local_count);
        defer self.allocator.free(self.locals);
        @memset(self.locals, null);
        defer self.blocks.deinit(self.allocator);

        // Allocate storage for every local in the entry block. Each local slot is an
        // i64 in the register ABI. A struct (ffi_struct) local additionally needs its
        // own zero-initialized backing storage, with the slot holding a pointer to it:
        // the IR addresses such a local by value (`copy_indirect(dst = load_local)`),
        // so the slot must point at real struct-sized storage, not be left undefined.
        for (self.function_decl.local_types, 0..) |local_type, index| {
            self.locals[index] = api.LLVMBuildAlloca(self.builder, self.types.llvmType(local_type), "local");
            if (local_type.kind == .ffi_struct) {
                if (local_type.name) |name| {
                    if (self.struct_types.get(name)) |struct_ty| {
                        const storage = api.LLVMBuildAlloca(self.builder, struct_ty, "local.storage");
                        _ = api.LLVMBuildStore(self.builder, api.LLVMConstNull(struct_ty), storage);
                        const storage_int = api.LLVMBuildPtrToInt(self.builder, storage, self.types.i64, "local.storage.int");
                        _ = api.LLVMBuildStore(self.builder, storage_int, self.locals[index]);
                    }
                }
            }
        }
        // The first param_count locals are the function parameters; bind them. A struct
        // parameter's pointer overwrites the backing-storage pointer above (the callee
        // borrows the caller's struct), which is correct.
        for (self.function_decl.param_types, 0..) |_, index| {
            const param = api.LLVMGetParam(self.function_value, @intCast(index));
            _ = api.LLVMBuildStore(self.builder, param, self.locals[index]);
        }
        // Seed owned-aggregate param cleanup slots now that the params are bound.
        drop.seedOwnedParams(self);

        // Pre-create a basic block for every label target.
        for (self.function_decl.instructions) |instruction| {
            if (instruction == .label) {
                const block = api.LLVMAppendBasicBlockInContext(self.types.context, self.function_value, "blk");
                try self.blocks.put(self.allocator, instruction.label.id, block);
            }
        }

        self.terminated = false;
        for (self.function_decl.instructions) |instruction| {
            // Skip dead instructions that follow a terminator until the next label.
            if (self.terminated and instruction != .label) continue;
            try self.lowerInstruction(instruction);
        }

        if (!self.terminated) {
            drop.emitExitCleanup(self, null);
            if (self.function_decl.return_type.kind == .void) {
                _ = api.LLVMBuildRetVoid(self.builder);
            } else {
                // A non-void function that falls off the end is ill-formed Kira;
                // emit a defined zero so the module still verifies.
                _ = api.LLVMBuildRet(self.builder, self.zeroValue(self.function_decl.return_type));
            }
        }
    }

    fn zeroValue(self: *FunctionCodegen, value_type: ir.ValueType) llvm.c.LLVMValueRef {
        const api = self.api;
        return switch (value_type.kind) {
            .float => api.LLVMConstReal(self.types.llvmType(value_type), 0.0),
            .string => api.LLVMGetUndef(self.types.string_ty),
            .boolean => api.LLVMConstInt(self.types.bool_ty, 0, 0),
            else => api.LLVMConstInt(self.types.i64, 0, 0),
        };
    }

    fn isFloat(self: *FunctionCodegen, reg: u32) bool {
        return reg < self.register_types.len and self.register_types[reg].kind == .float;
    }

    /// Float -> Int conversion matching the VM's `convertValue`: truncate toward
    /// zero, but saturate out-of-range magnitudes to i64 min/max and map NaN to
    /// 0. A bare `fptosi` is poison for those inputs, so VM and LLVM would
    /// otherwise diverge (Core Law #1). `raw` is only selected when the source
    /// is in range and non-NaN, so its poison value never reaches the result.
    fn lowerFloatToIntSaturating(self: *FunctionCodegen, src: llvm.c.LLVMValueRef) llvm.c.LLVMValueRef {
        const api = self.api;
        const b = self.builder;
        const i64_ty = self.types.i64;
        const f64_ty = self.types.double_ty;
        const raw = api.LLVMBuildFPToSI(b, src, i64_ty, "fptosi");
        // i64 bounds as doubles (these round to +/-2^63, matching the VM's
        // `@floatFromInt(maxInt/minInt)` comparison thresholds).
        const max_f = api.LLVMConstReal(f64_ty, @as(f64, @floatFromInt(std.math.maxInt(i64))));
        const min_f = api.LLVMConstReal(f64_ty, @as(f64, @floatFromInt(std.math.minInt(i64))));
        const max_i = api.LLVMConstInt(i64_ty, @bitCast(@as(i64, std.math.maxInt(i64))), 1);
        const min_i = api.LLVMConstInt(i64_ty, @bitCast(@as(i64, std.math.minInt(i64))), 1);
        const zero_i = api.LLVMConstInt(i64_ty, 0, 1);
        const ge_max = api.LLVMBuildFCmp(b, llvm.c.LLVMRealOGE, src, max_f, "sat.ge");
        const le_min = api.LLVMBuildFCmp(b, llvm.c.LLVMRealOLE, src, min_f, "sat.le");
        const is_nan = api.LLVMBuildFCmp(b, llvm.c.LLVMRealUNO, src, src, "sat.nan");
        var result = api.LLVMBuildSelect(b, ge_max, max_i, raw, "sat.hi");
        result = api.LLVMBuildSelect(b, le_min, min_i, result, "sat.lo");
        result = api.LLVMBuildSelect(b, is_nan, zero_i, result, "sat.nan.sel");
        return result;
    }

    fn lowerInstruction(self: *FunctionCodegen, instruction: ir.Instruction) !void {
        const api = self.api;
        const b = self.builder;
        switch (instruction) {
            .const_int => |v| self.registers[v.dst] = api.LLVMConstInt(self.types.i64, @bitCast(v.value), 1),
            .const_float => |v| self.registers[v.dst] = api.LLVMConstReal(self.types.llvmType(self.register_types[v.dst]), v.value),
            .const_bool => |v| self.registers[v.dst] = api.LLVMConstInt(self.types.bool_ty, if (v.value) 1 else 0, 0),
            .const_null_ptr => |v| self.registers[v.dst] = api.LLVMConstInt(self.types.i64, 0, 0),
            .const_string => |v| {
                self.registers[v.dst] = try self.buildStringConstant(v.value);
                self.string_counter += 1;
            },
            .add => |v| self.registers[v.dst] = if (self.isFloat(v.lhs)) api.LLVMBuildFAdd(b, self.registers[v.lhs], self.registers[v.rhs], "fadd") else api.LLVMBuildAdd(b, self.registers[v.lhs], self.registers[v.rhs], "add"),
            .subtract => |v| self.registers[v.dst] = if (self.isFloat(v.lhs)) api.LLVMBuildFSub(b, self.registers[v.lhs], self.registers[v.rhs], "fsub") else api.LLVMBuildSub(b, self.registers[v.lhs], self.registers[v.rhs], "sub"),
            .multiply => |v| self.registers[v.dst] = if (self.isFloat(v.lhs)) api.LLVMBuildFMul(b, self.registers[v.lhs], self.registers[v.rhs], "fmul") else api.LLVMBuildMul(b, self.registers[v.lhs], self.registers[v.rhs], "mul"),
            .divide => |v| self.registers[v.dst] = if (self.isFloat(v.lhs)) api.LLVMBuildFDiv(b, self.registers[v.lhs], self.registers[v.rhs], "fdiv") else api.LLVMBuildSDiv(b, self.registers[v.lhs], self.registers[v.rhs], "sdiv"),
            .modulo => |v| self.registers[v.dst] = if (self.isFloat(v.lhs)) api.LLVMBuildFRem(b, self.registers[v.lhs], self.registers[v.rhs], "frem") else api.LLVMBuildSRem(b, self.registers[v.lhs], self.registers[v.rhs], "srem"),
            .convert => |v| {
                const src_is_float = self.isFloat(v.src);
                if (v.target == .float) {
                    // Int -> Float is sitofp; Float -> Float is identity.
                    self.registers[v.dst] = if (src_is_float) self.registers[v.src] else api.LLVMBuildSIToFP(b, self.registers[v.src], self.types.double_ty, "sitofp");
                } else {
                    // Float -> Int truncates toward zero; Int -> Int is identity.
                    self.registers[v.dst] = if (src_is_float) self.lowerFloatToIntSaturating(self.registers[v.src]) else self.registers[v.src];
                }
            },
            .compare => |v| self.registers[v.dst] = try self.lowerCompare(v),
            .unary => |v| self.registers[v.dst] = switch (v.op) {
                .negate => if (self.isFloat(v.src)) api.LLVMBuildFNeg(b, self.registers[v.src], "fneg") else api.LLVMBuildNeg(b, self.registers[v.src], "neg"),
                .not => api.LLVMBuildNot(b, self.registers[v.src], "not"),
            },
            .store_local => |v| {
                _ = api.LLVMBuildStore(b, self.registers[v.src], self.locals[v.local]);
                drop.onStoreLocal(self, v.local, v.src, v.borrow);
            },
            .load_local => |v| {
                self.registers[v.dst] = api.LLVMBuildLoad2(b, self.types.llvmType(self.function_decl.local_types[v.local]), self.locals[v.local], "load");
                drop.onLoadLocal(self, v.dst, v.local);
                drop.recordRegLocal(self, v.dst, v.local);
            },
            .local_ptr => |v| {
                self.registers[v.dst] = api.LLVMBuildPtrToInt(b, self.locals[v.local], self.types.i64, "local.ptr");
                drop.recordRegLocal(self, v.dst, v.local);
            },
            .branch => |v| {
                const true_block = self.blocks.get(v.true_label) orelse return error.UnknownLabel;
                const false_block = self.blocks.get(v.false_label) orelse return error.UnknownLabel;
                _ = api.LLVMBuildCondBr(b, self.registers[v.condition], true_block, false_block);
                self.terminated = true;
            },
            .jump => |v| {
                const target = self.blocks.get(v.label) orelse return error.UnknownLabel;
                _ = api.LLVMBuildBr(b, target);
                self.terminated = true;
            },
            .label => |v| {
                const block = self.blocks.get(v.id) orelse return error.UnknownLabel;
                if (!self.terminated) _ = api.LLVMBuildBr(b, block);
                api.LLVMPositionBuilderAtEnd(b, block);
                self.terminated = false;
            },
            .print => |v| try print.lowerPrint(self, self.register_types[v.src], self.registers[v.src]),
            .call => |v| try calls.lowerCall(self, v),
            .ret => |v| {
                if (v.src) |src| {
                    if (self.drop_enabled and self.function_decl.return_type.kind == .ffi_struct) {
                        // Lower the struct result into caller-stable heap storage and
                        // escape its source slot BEFORE exit cleanup, so the returned
                        // struct outlives the callee frame and the callee can release all
                        // of its own remaining temporaries. The caller receives an owned
                        // heap struct, tracked as struct_heap at the call site (lowerCall
                        // / setup) and freed there.
                        const ret_val = drop.prepareStructReturn(self, src);
                        drop.emitExitCleanup(self, null);
                        _ = api.LLVMBuildRet(b, ret_val);
                    } else {
                        drop.emitExitCleanup(self, src);
                        _ = api.LLVMBuildRet(b, self.registers[src]);
                    }
                } else {
                    drop.emitExitCleanup(self, null);
                    _ = api.LLVMBuildRetVoid(b);
                }
                self.terminated = true;
            },
            .alloc_struct => |v| {
                const struct_ty = self.struct_types.get(v.type_name) orelse return error.UnsupportedExecutableFeature;
                const size = api.LLVMSizeOf(struct_ty);
                var args = [_]llvm.c.LLVMValueRef{
                    api.LLVMConstInt(self.types.i64, ir.nativeStateTypeId(v.type_name), 0),
                    size,
                };
                const ptr = api.LLVMBuildCall2(b, self.runtime_decls.struct_alloc.ty, self.runtime_decls.struct_alloc.fn_value, &args, args.len, "struct.alloc");
                _ = api.LLVMBuildStore(b, api.LLVMConstNull(struct_ty), ptr);
                self.registers[v.dst] = api.LLVMBuildPtrToInt(b, ptr, self.types.i64, "struct.ptr");
                drop.onAlloc(self, v.dst);
            },
            .field_ptr => |v| {
                const struct_ty = self.struct_types.get(v.base_type_name) orelse return error.UnsupportedExecutableFeature;
                const base = api.LLVMBuildIntToPtr(b, self.registers[v.base], self.types.ptr_ty, "field.base");
                var indices = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(self.types.i32, 0, 0), api.LLVMConstInt(self.types.i32, v.field_index, 0) };
                const field_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, base, &indices, indices.len, "field.ptr");
                self.registers[v.dst] = api.LLVMBuildPtrToInt(b, field_ptr, self.types.i64, "field.ptrint");
            },
            .subobject_ptr => |v| {
                const base_name = self.register_types[v.base].name orelse return error.UnsupportedExecutableFeature;
                const struct_ty = self.struct_types.get(base_name) orelse return error.UnsupportedExecutableFeature;
                const base = api.LLVMBuildIntToPtr(b, self.registers[v.base], self.types.ptr_ty, "sub.base");
                var indices = [_]llvm.c.LLVMValueRef{ api.LLVMConstInt(self.types.i32, 0, 0), api.LLVMConstInt(self.types.i32, v.offset, 0) };
                const sub_ptr = api.LLVMBuildInBoundsGEP2(b, struct_ty, base, &indices, indices.len, "sub.ptr");
                self.registers[v.dst] = api.LLVMBuildPtrToInt(b, sub_ptr, self.types.i64, "sub.ptrint");
            },
            .load_indirect => |v| self.registers[v.dst] = try self.lowerLoadIndirect(v),
            .store_indirect => |v| try aggregate.lowerStoreIndirect(self, v),
            .copy_indirect => |v| {
                const struct_ty = self.struct_types.get(v.type_name) orelse return error.UnsupportedExecutableFeature;
                const src = api.LLVMBuildIntToPtr(b, self.registers[v.src_ptr], self.types.ptr_ty, "copy.src");
                const dst = api.LLVMBuildIntToPtr(b, self.registers[v.dst_ptr], self.types.ptr_ty, "copy.dst");
                // Release any prior occupant of the destination's stack shell before the
                // shallow store discards its array pointers (loop-body reassignment).
                if (self.drop_enabled) drop.releasePriorCopyDest(self, v.dst_ptr, v.type_name);
                const value = api.LLVMBuildLoad2(b, struct_ty, src, "copy.val");
                _ = api.LLVMBuildStore(b, value, dst);
                // Deep-clone the destination's contents so it owns storage independent of
                // the source — affine value semantics (`var b = a` is a copy, not an
                // alias). This is the DEFAULT for pure-Kira value structs, matching the
                // text backend's copy_indirect; FFI/native structs keep the shallow,
                // device-validated copy. With drop on we additionally clone any tracked
                // type and reclaim the clone by tracking dst as struct_contents (aliasing
                // would double-free, since src and dst are separate drop slots).
                const clone_default = blk: {
                    const td = utils.findTypeDecl(self.request.program.programPtr(), v.type_name) orelse break :blk false;
                    break :blk td.ffi == null;
                };
                if ((self.drop_enabled or clone_default)) {
                    if (self.dtors.map.get(v.type_name)) |h| {
                        var cc = [_]llvm.c.LLVMValueRef{dst};
                        _ = api.LLVMBuildCall2(b, h.clone_contents.ty, h.clone_contents.fn_value, &cc, cc.len, "");
                    }
                }
                if (self.drop_enabled) drop.onCopyDest(self, v.dst_ptr, dst, v.type_name);
            },
            .c_string_to_string => |v| self.registers[v.dst] = try calls.lowerCStringToString(self, v),
            .call_virtual => |v| try calls.lowerCallVirtual(self, v),
            .const_function => |v| {
                self.registers[v.dst] = switch (v.representation) {
                    // callable_value: the i64 is just the function id (high bit clear).
                    .callable_value => api.LLVMConstInt(self.types.i64, v.function_id, 0),
                    // native_callback: a raw function pointer.
                    .native_callback => blk: {
                        const fn_value = self.functions.get(v.function_id) orelse return error.MissingFunctionDeclaration;
                        break :blk api.LLVMBuildPtrToInt(b, fn_value, self.types.i64, "fnptr");
                    },
                };
            },
            .const_closure => |v| try closures.lowerConstClosure(self, v),
            .call_value => |v| try closures.lowerCallValue(self, v),
            .string_len => |v| self.registers[v.dst] = api.LLVMBuildExtractValue(b, self.registers[v.string], 1, "string.len"),
            .alloc_enum => |v| {
                try aggregate.lowerAllocEnum(self, v);
                drop.onAlloc(self, v.dst);
            },
            .enum_tag => |v| {
                const base = api.LLVMBuildIntToPtr(b, self.registers[v.src], self.types.ptr_ty, "enum.tag.base");
                var idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(self.types.i64, 0, 0)};
                const slot = api.LLVMBuildInBoundsGEP2(b, self.types.i64, base, &idx, idx.len, "enum.tag.slot");
                self.registers[v.dst] = api.LLVMBuildLoad2(b, self.types.i64, slot, "enum.tag");
            },
            .enum_payload => |v| self.registers[v.dst] = try aggregate.lowerEnumPayload(self, v),
            .alloc_native_state => |v| try aggregate.lowerAllocNativeState(self, v),
            .recover_native_state => |v| {
                const state = api.LLVMBuildIntToPtr(b, self.registers[v.state], self.types.ptr_ty, "state.recover.in");
                var args = [_]llvm.c.LLVMValueRef{ state, api.LLVMConstInt(self.types.i64, v.type_id, 0) };
                const payload = api.LLVMBuildCall2(b, self.runtime_decls.state_recover.ty, self.runtime_decls.state_recover.fn_value, &args, args.len, "state.payload");
                self.registers[v.dst] = api.LLVMBuildPtrToInt(b, payload, self.types.i64, "state.recover.out");
            },
            .native_state_field_get => |v| {
                const payload = api.LLVMBuildIntToPtr(b, self.registers[v.state], self.types.ptr_ty, "state.get.payload");
                var idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(self.types.i64, v.field_index, 0)};
                const slot = api.LLVMBuildInBoundsGEP2(b, self.types.bridge_ty, payload, &idx, idx.len, "state.get.slot");
                const bv = api.LLVMBuildLoad2(b, self.types.bridge_ty, slot, "state.get.bv");
                self.registers[v.dst] = try self.unpackBridge(v.field_ty, bv);
            },
            .native_state_field_set => |v| {
                const payload = api.LLVMBuildIntToPtr(b, self.registers[v.state], self.types.ptr_ty, "state.set.payload");
                var idx = [_]llvm.c.LLVMValueRef{api.LLVMConstInt(self.types.i64, v.field_index, 0)};
                const slot = api.LLVMBuildInBoundsGEP2(b, self.types.bridge_ty, payload, &idx, idx.len, "state.set.slot");
                const bv = try self.packBridge(self.register_types[v.src], self.registers[v.src]);
                _ = api.LLVMBuildStore(b, bv, slot);
            },
            .alloc_array => |v| try aggregate.lowerAllocArray(self, v),
            .array_len => |v| aggregate.lowerArrayLen(self, v),
            .array_get => |v| try aggregate.lowerArrayGet(self, v),
            .array_set => |v| try aggregate.lowerArraySet(self, v),
            .array_append => |v| try aggregate.lowerArrayAppend(self, v),
            // Drop elaboration / ownership scopes are no-ops for the C-API core
            // until aggregate ownership (release/clone) lands here.
            .scope_enter, .scope_exit => {},
            // No `else`: the switch is exhaustive over ir.Instruction. The C-API backend
            // now lowers every IR opcode, so a newly-added instruction must be handled
            // here explicitly rather than silently falling through to an error.
        }
    }

    fn lowerCompare(self: *FunctionCodegen, v: ir.Compare) !llvm.c.LLVMValueRef {
        const api = self.api;
        const operand_kind = if (v.lhs < self.register_types.len) self.register_types[v.lhs].kind else ir.ValueType.Kind.integer;
        if (operand_kind == .string) {
            // String content equality: a `{ptr,len}` value equals another iff the
            // lengths match and the first `min(len)` bytes compare equal. memcmp
            // over min(len) is always in-bounds for both buffers; the length check
            // distinguishes a string from its own prefix.
            const b = self.builder;
            const lhs = self.registers[v.lhs];
            const rhs = self.registers[v.rhs];
            const len_a = api.LLVMBuildExtractValue(b, lhs, 1, "streq.alen");
            const len_b = api.LLVMBuildExtractValue(b, rhs, 1, "streq.blen");
            const ptr_a = api.LLVMBuildExtractValue(b, lhs, 0, "streq.aptr");
            const ptr_b = api.LLVMBuildExtractValue(b, rhs, 0, "streq.bptr");
            const len_eq = api.LLVMBuildICmp(b, llvm.c.LLVMIntEQ, len_a, len_b, "streq.leneq");
            const a_shorter = api.LLVMBuildICmp(b, llvm.c.LLVMIntULT, len_a, len_b, "streq.ashorter");
            const min_len = api.LLVMBuildSelect(b, a_shorter, len_a, len_b, "streq.min");
            var args = [_]llvm.c.LLVMValueRef{ ptr_a, ptr_b, min_len };
            const cmp = api.LLVMBuildCall2(b, self.runtime_decls.memcmp.ty, self.runtime_decls.memcmp.fn_value, &args, args.len, "streq.memcmp");
            const zero = api.LLVMConstInt(self.types.i32, 0, 0);
            const bytes_eq = api.LLVMBuildICmp(b, llvm.c.LLVMIntEQ, cmp, zero, "streq.byteseq");
            const equal = api.LLVMBuildAnd(b, len_eq, bytes_eq, "streq.eq");
            return switch (v.op) {
                .equal => equal,
                .not_equal => api.LLVMBuildNot(b, equal, "streq.ne"),
                // The compare-operand gate rejects ordered string comparisons.
                else => equal,
            };
        }
        if (operand_kind == .float) {
            return api.LLVMBuildFCmp(
                self.builder,
                switch (v.op) {
                    .equal => llvm.c.LLVMRealOEQ,
                    .not_equal => llvm.c.LLVMRealONE,
                    .less => llvm.c.LLVMRealOLT,
                    .less_equal => llvm.c.LLVMRealOLE,
                    .greater => llvm.c.LLVMRealOGT,
                    .greater_equal => llvm.c.LLVMRealOGE,
                },
                self.registers[v.lhs],
                self.registers[v.rhs],
                "fcmp",
            );
        }
        // Integer / boolean / pointer comparison. Equality is valid for all;
        // ordering uses signed predicates (Kira Int is signed).
        return api.LLVMBuildICmp(
            self.builder,
            switch (v.op) {
                .equal => llvm.c.LLVMIntEQ,
                .not_equal => llvm.c.LLVMIntNE,
                .less => llvm.c.LLVMIntSLT,
                .less_equal => llvm.c.LLVMIntSLE,
                .greater => llvm.c.LLVMIntSGT,
                .greater_equal => llvm.c.LLVMIntSGE,
            },
            self.registers[v.lhs],
            self.registers[v.rhs],
            "icmp",
        );
    }

    pub fn storageType(self: *FunctionCodegen, value_type: ir.ValueType) !llvm.c.LLVMTypeRef {
        return capi.fieldStorageType(self.types, self.struct_types.*, self.request.program.programPtr(), value_type);
    }

    // Read a value through a pointer (register i64), converting the in-memory
    // storage representation back to the register representation.
    fn lowerLoadIndirect(self: *FunctionCodegen, v: ir.LoadIndirect) !llvm.c.LLVMValueRef {
        // An ffi_struct field is stored inline; a "load" of it yields the address
        // of that inline struct, which is exactly the field pointer we were given.
        if (v.ty.kind == .ffi_struct) return self.registers[v.ptr];
        const ptr = self.api.LLVMBuildIntToPtr(self.builder, self.registers[v.ptr], self.types.ptr_ty, "load.ptr");
        return self.loadConverted(ptr, v.ty);
    }

    // Load a value from an LLVM pointer and convert storage→register representation
    // (does not handle ffi_struct, whose "value" is the pointer itself).
    pub fn loadConverted(self: *FunctionCodegen, ptr: llvm.c.LLVMValueRef, value_type: ir.ValueType) !llvm.c.LLVMValueRef {
        const api = self.api;
        const b = self.builder;
        // Pointer-like values live in registers as an i64 pointer. Load a pointer-sized
        // word regardless of the field's storage type: an inline fixed FFI array field is
        // laid out as `[N x elem]` in the struct, but reading it as a value yields the
        // pointer in its first word (matching the text backend's degenerate array-field
        // load), so do not load the whole aggregate (ptrtoint of an aggregate is invalid).
        switch (value_type.kind) {
            .array, .construct_any, .raw_ptr, .enum_instance => {
                const raw = api.LLVMBuildLoad2(b, self.types.ptr_ty, ptr, "load");
                return api.LLVMBuildPtrToInt(b, raw, self.types.i64, "load.ptrint");
            },
            else => {},
        }
        const storage = try self.storageType(value_type);
        const raw = api.LLVMBuildLoad2(b, storage, ptr, "load");
        return switch (value_type.kind) {
            .integer => if (storage == self.types.i64) raw else api.LLVMBuildSExt(b, raw, self.types.i64, "load.sext"),
            .float, .string => raw,
            // Boolean fields are stored as i8 but live in registers as i1.
            .boolean => api.LLVMBuildTrunc(b, raw, self.types.bool_ty, "load.bool"),
            else => error.UnsupportedExecutableFeature,
        };
    }

    // Allocate a native-state box and copy a struct's fields into its bridge-value
    // payload (one slot per field). Mirrors backend_text_ir_core alloc_native_state.
    pub fn packBridge(self: *FunctionCodegen, value_type: ir.ValueType, value: llvm.c.LLVMValueRef) !llvm.c.LLVMValueRef {
        return self.packBridgeBoxed(value_type, value, true);
    }

    // box_struct: array elements own an independent heap copy of an ffi_struct;
    // closure captures store the struct pointer directly (matching the text backend).
    pub fn packBridgeBoxed(self: *FunctionCodegen, value_type: ir.ValueType, value: llvm.c.LLVMValueRef, box_struct: bool) !llvm.c.LLVMValueRef {
        const api = self.api;
        const b = self.builder;
        var bv = api.LLVMConstNull(self.types.bridge_ty);
        bv = api.LLVMBuildInsertValue(b, bv, api.LLVMConstInt(self.types.i8, utils.bridgeTagValue(value_type), 0), 0, "bv.tag");
        switch (value_type.kind) {
            .integer, .construct_any, .raw_ptr, .array, .enum_instance => {
                bv = api.LLVMBuildInsertValue(b, bv, value, 2, "bv.payload");
            },
            .ffi_struct => {
                if (!box_struct) {
                    bv = api.LLVMBuildInsertValue(b, bv, value, 2, "bv.payload");
                } else {
                    // Box the inline struct on the heap so the array element owns a copy.
                    const struct_ty = self.struct_types.get(value_type.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
                    const src = api.LLVMBuildIntToPtr(b, value, self.types.ptr_ty, "bv.struct.src");
                    const loaded = api.LLVMBuildLoad2(b, struct_ty, src, "bv.struct.val");
                    const type_name = value_type.name orelse return error.UnsupportedExecutableFeature;
                    var margs = [_]llvm.c.LLVMValueRef{
                        api.LLVMConstInt(self.types.i64, ir.nativeStateTypeId(type_name), 0),
                        api.LLVMSizeOf(struct_ty),
                    };
                    const copy = api.LLVMBuildCall2(b, self.runtime_decls.struct_alloc.ty, self.runtime_decls.struct_alloc.fn_value, &margs, margs.len, "bv.struct.copy");
                    _ = api.LLVMBuildStore(b, loaded, copy);
                    bv = api.LLVMBuildInsertValue(b, bv, api.LLVMBuildPtrToInt(b, copy, self.types.i64, "bv.struct.int"), 2, "bv.payload");
                }
            },
            .float => {
                const as_double = if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
                    api.LLVMBuildFPExt(b, value, self.types.double_ty, "bv.fpext")
                else
                    value;
                const bits = api.LLVMBuildBitCast(b, as_double, self.types.i64, "bv.fbits");
                bv = api.LLVMBuildInsertValue(b, bv, bits, 2, "bv.payload");
            },
            .boolean => {
                const word = api.LLVMBuildZExt(b, value, self.types.i64, "bv.bool");
                bv = api.LLVMBuildInsertValue(b, bv, word, 2, "bv.payload");
            },
            .string => {
                const sp = api.LLVMBuildExtractValue(b, value, 0, "bv.str.ptr");
                const spi = api.LLVMBuildPtrToInt(b, sp, self.types.i64, "bv.str.ptrint");
                const sl = api.LLVMBuildExtractValue(b, value, 1, "bv.str.len");
                bv = api.LLVMBuildInsertValue(b, bv, spi, 2, "bv.payload");
                bv = api.LLVMBuildInsertValue(b, bv, sl, 3, "bv.extra");
            },
            .void => return error.UnsupportedExecutableFeature,
        }
        return bv;
    }

    // Unpack a %kira.bridge.value back into a register value of the requested type.
    pub fn unpackBridge(self: *FunctionCodegen, value_type: ir.ValueType, bv: llvm.c.LLVMValueRef) !llvm.c.LLVMValueRef {
        return unpackBridgeValue(self.api, self.builder, self.types, value_type, bv);
    }

    // print(x) writes the value (no trailing newline pieces) then one newline.
    pub fn buildStringConstant(self: *FunctionCodegen, value: []const u8) !llvm.c.LLVMValueRef {
        const api = self.api;
        const global_name = try allocPrintZ(self.allocator, "kira.capi.str.{d}.{d}", .{ self.function_decl.id, self.string_counter });
        defer self.allocator.free(global_name);
        const array_ty = api.LLVMArrayType2(self.types.i8, value.len + 1);
        const global = api.LLVMAddGlobal(self.module_ref, array_ty, global_name.ptr);
        api.LLVMSetLinkage(global, llvm.c.LLVMPrivateLinkage);
        api.LLVMSetGlobalConstant(global, 1);
        api.LLVMSetInitializer(global, api.LLVMConstStringInContext2(self.types.context, value.ptr, value.len, 0));
        const zero = api.LLVMConstInt(self.types.i32, 0, 0);
        var indices = [_]llvm.c.LLVMValueRef{ zero, zero };
        const data_ptr = api.LLVMConstInBoundsGEP2(array_ty, global, &indices, indices.len);
        var fields = [_]llvm.c.LLVMValueRef{ data_ptr, api.LLVMConstInt(self.types.i64, value.len, 0) };
        return api.LLVMConstNamedStruct(self.types.string_ty, &fields, fields.len);
    }
};
