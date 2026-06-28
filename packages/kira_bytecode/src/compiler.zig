const std = @import("std");
const ir_pkg = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const bytecode = @import("bytecode.zig");
const instruction = @import("instruction.zig");

pub const CompileMode = enum {
    vm,
    hybrid_runtime,
};

// Accepts only a `VerifiedProgram`: the bytecode backend cannot be handed a program that
// has not passed the executable-obligation verifier. The wrapper is unwrapped once here;
// the rest of the compiler operates on the inner `ir.Program` unchanged.
pub fn compileProgram(allocator: std.mem.Allocator, verified: ir_pkg.VerifiedProgram, mode: CompileMode) !bytecode.Module {
    const program = verified.programPtr().*;
    var constructs = std.array_list.Managed(bytecode.Construct).init(allocator);
    for (program.constructs) |construct_decl| {
        try constructs.append(.{ .name = construct_decl.name });
    }

    var construct_implementations = std.array_list.Managed(bytecode.ConstructImplementation).init(allocator);
    for (program.construct_implementations) |implementation| {
        var fields = std.array_list.Managed(bytecode.Field).init(allocator);
        for (implementation.fields) |field_decl| {
            try fields.append(.{ .name = field_decl.name, .ty = lowerTypeRef(field_decl.ty) });
        }
        var lifecycle_hooks = std.array_list.Managed(bytecode.LifecycleHook).init(allocator);
        for (implementation.lifecycle_hooks) |hook| try lifecycle_hooks.append(.{ .name = hook.name });
        try construct_implementations.append(.{
            .type_name = implementation.type_name,
            .construct_constraint = .{ .construct_name = implementation.construct_constraint.construct_name },
            .families = implementation.families,
            .fields = try fields.toOwnedSlice(),
            .has_content = implementation.has_content,
            .lifecycle_hooks = try lifecycle_hooks.toOwnedSlice(),
        });
    }

    var types = std.array_list.Managed(bytecode.TypeDecl).init(allocator);
    for (program.types) |type_decl| {
        var fields = std.array_list.Managed(bytecode.Field).init(allocator);
        for (type_decl.fields) |field_decl| {
            try fields.append(.{
                .name = field_decl.name,
                .ty = lowerTypeRef(field_decl.ty),
            });
        }
        var methods = std.array_list.Managed(bytecode.MethodMember).init(allocator);
        for (type_decl.methods) |method_decl| {
            try methods.append(.{
                .name = method_decl.name,
                .function_id = method_decl.function_id,
                .receiver_offset = method_decl.receiver_offset,
            });
        }
        try types.append(.{
            .name = type_decl.name,
            .kind = @enumFromInt(@intFromEnum(type_decl.kind)),
            .fields = try fields.toOwnedSlice(),
            .methods = try methods.toOwnedSlice(),
        });
    }

    var enums = std.array_list.Managed(bytecode.EnumTypeDecl).init(allocator);
    for (program.enums) |enum_decl| {
        var variants = std.array_list.Managed(bytecode.EnumVariantDecl).init(allocator);
        for (enum_decl.variants) |variant_decl| {
            try variants.append(.{
                .name = variant_decl.name,
                .discriminant = variant_decl.discriminant,
                .payload_ty = if (variant_decl.payload_ty) |payload_ty| lowerTypeRef(payload_ty) else null,
            });
        }
        try enums.append(.{
            .name = enum_decl.name,
            .variants = try variants.toOwnedSlice(),
        });
    }

    var functions = std.array_list.Managed(bytecode.Function).init(allocator);
    var entry_function_id: ?u32 = null;

    for (program.functions, 0..) |function_decl, index| {
        const resolved_execution = resolveExecution(function_decl.execution, mode);
        // Foreign FFI declarations carry no Kira body. In VM mode the
        // interpreter dispatches them through LibFFI (see kira_vm_runtime), so
        // emit a metadata-only stub instead of rejecting the program. The
        // hybrid/native paths resolve them through the native bridge and do not
        // need a bytecode entry.
        if (function_decl.is_extern and resolved_execution == .native) {
            if (mode == .vm) try functions.append(try externStub(allocator, function_decl));
            continue;
        }
        if (mode == .vm and resolved_execution == .native) return error.NativeFunctionInVmBuild;
        if (resolved_execution == .native and mode == .hybrid_runtime) continue;

        var instructions = std.array_list.Managed(instruction.Instruction).init(allocator);
        for (function_decl.instructions) |inst| {
            switch (inst) {
                .const_int => |value| try instructions.append(.{ .const_int = .{ .dst = value.dst, .value = value.value } }),
                .const_float => |value| try instructions.append(.{ .const_float = .{ .dst = value.dst, .value = value.value } }),
                .const_string => |value| try instructions.append(.{ .const_string = .{ .dst = value.dst, .value = value.value } }),
                .const_bool => |value| try instructions.append(.{ .const_bool = .{ .dst = value.dst, .value = value.value } }),
                .const_null_ptr => |value| try instructions.append(.{ .const_null_ptr = .{ .dst = value.dst } }),
                .alloc_struct => |value| try instructions.append(.{ .alloc_struct = .{ .dst = value.dst, .type_name = value.type_name } }),
                .alloc_enum => |value| try instructions.append(.{ .alloc_enum = .{
                    .dst = value.dst,
                    .enum_type_name = value.enum_type_name,
                    .discriminant = value.discriminant,
                    .payload_src = value.payload_src,
                } }),
                .alloc_array => |value| try instructions.append(.{ .alloc_array = .{ .dst = value.dst, .len = value.len } }),
                .const_function => |value| try instructions.append(.{ .const_function = .{
                    .dst = value.dst,
                    .function_id = value.function_id,
                    .representation = @enumFromInt(@intFromEnum(value.representation)),
                } }),
                .const_closure => |value| try instructions.append(.{ .const_closure = .{
                    .dst = value.dst,
                    .function_id = value.function_id,
                    .captures = value.captures,
                    .capture_ownership = try lowerOwnershipModes(allocator, value.capture_ownership),
                } }),
                .add => |value| try instructions.append(.{ .add = .{ .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs } }),
                .alloc_native_state => |value| try instructions.append(.{ .alloc_native_state = .{
                    .dst = value.dst,
                    .src = value.src,
                    .type_name = value.type_name,
                    .type_id = value.type_id,
                } }),
                .subtract => |value| try instructions.append(.{ .subtract = .{ .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs } }),
                .multiply => |value| try instructions.append(.{ .multiply = .{ .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs } }),
                .divide => |value| try instructions.append(.{ .divide = .{ .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs } }),
                .modulo => |value| try instructions.append(.{ .modulo = .{ .dst = value.dst, .lhs = value.lhs, .rhs = value.rhs } }),
                .convert => |value| try instructions.append(.{ .convert = .{ .dst = value.dst, .src = value.src, .to_float = value.target == .float } }),
                .compare => |value| try instructions.append(.{ .compare = .{
                    .dst = value.dst,
                    .lhs = value.lhs,
                    .rhs = value.rhs,
                    .op = @enumFromInt(@intFromEnum(value.op)),
                } }),
                .unary => |value| try instructions.append(.{ .unary = .{
                    .dst = value.dst,
                    .src = value.src,
                    .op = @enumFromInt(@intFromEnum(value.op)),
                } }),
                .store_local => |value| try instructions.append(.{ .store_local = .{ .local = value.local, .src = value.src, .borrow = value.borrow } }),
                .load_local => |value| try instructions.append(.{ .load_local = .{ .dst = value.dst, .local = value.local, .ownership = lowerOwnershipMode(value.ownership) } }),
                .local_ptr => |value| try instructions.append(.{ .local_ptr = .{ .dst = value.dst, .local = value.local } }),
                .subobject_ptr => |value| try instructions.append(.{ .subobject_ptr = .{
                    .dst = value.dst,
                    .base = value.base,
                    .offset = value.offset,
                } }),
                .field_ptr => |value| try instructions.append(.{ .field_ptr = .{
                    .dst = value.dst,
                    .base = value.base,
                    .base_type_name = value.base_type_name,
                    .field_index = value.field_index,
                    .field_ty = lowerTypeRef(value.field_ty),
                } }),
                .recover_native_state => |value| try instructions.append(.{ .recover_native_state = .{
                    .dst = value.dst,
                    .state = value.state,
                    .type_name = value.type_name,
                    .type_id = value.type_id,
                } }),
                .native_state_field_get => |value| try instructions.append(.{ .native_state_field_get = .{
                    .dst = value.dst,
                    .state = value.state,
                    .field_index = value.field_index,
                    .field_ty = lowerTypeRef(value.field_ty),
                } }),
                .native_state_field_set => |value| try instructions.append(.{ .native_state_field_set = .{
                    .state = value.state,
                    .field_index = value.field_index,
                    .src = value.src,
                    .field_ty = lowerTypeRef(value.field_ty),
                } }),
                .c_string_to_string => |value| try instructions.append(.{ .c_string_to_string = .{ .dst = value.dst, .src = value.src } }),
                .array_len => |value| try instructions.append(.{ .array_len = .{ .dst = value.dst, .array = value.array } }),
                .string_len => |value| try instructions.append(.{ .string_len = .{ .dst = value.dst, .string = value.string } }),
                .array_get => |value| try instructions.append(.{ .array_get = .{
                    .dst = value.dst,
                    .array = value.array,
                    .index = value.index,
                    .ty = lowerTypeRef(value.ty),
                } }),
                .array_set => |value| try instructions.append(.{ .array_set = .{
                    .array = value.array,
                    .index = value.index,
                    .src = value.src,
                } }),
                .array_append => |value| try instructions.append(.{ .array_append = .{
                    .array = value.array,
                    .src = value.src,
                } }),
                .enum_tag => |value| try instructions.append(.{ .enum_tag = .{ .dst = value.dst, .src = value.src } }),
                .enum_payload => |value| try instructions.append(.{ .enum_payload = .{
                    .dst = value.dst,
                    .src = value.src,
                    .payload_ty = lowerTypeRef(value.payload_ty),
                } }),
                .load_indirect => |value| try instructions.append(.{ .load_indirect = .{
                    .dst = value.dst,
                    .ptr = value.ptr,
                    .ty = lowerTypeRef(value.ty),
                } }),
                .store_indirect => |value| try instructions.append(.{ .store_indirect = .{
                    .ptr = value.ptr,
                    .src = value.src,
                    .ty = lowerTypeRef(value.ty),
                } }),
                .copy_indirect => |value| try instructions.append(.{ .copy_indirect = .{
                    .dst_ptr = value.dst_ptr,
                    .src_ptr = value.src_ptr,
                    .type_name = value.type_name,
                } }),
                .branch => |value| try instructions.append(.{ .branch = .{
                    .condition = value.condition,
                    .true_label = value.true_label,
                    .false_label = value.false_label,
                } }),
                .jump => |value| try instructions.append(.{ .jump = .{ .label = value.label } }),
                .label => |value| try instructions.append(.{ .label = .{ .id = value.id } }),
                .print => |value| try instructions.append(.{ .print = .{ .src = value.src, .ty = lowerTypeRef(value.ty) } }),
                .call => |value| {
                    const callee_execution = functionExecutionById(program, value.callee) orelse return error.UnknownFunction;
                    const resolved_callee_execution = resolveExecution(callee_execution, mode);
                    try instructions.append(switch (resolved_callee_execution) {
                        .runtime => .{ .call_runtime = .{ .function_id = value.callee, .args = value.args, .dst = value.dst } },
                        .native => .{ .call_native = .{
                            .function_id = value.callee,
                            .args = value.args,
                            .dst = value.dst,
                            .return_ty = lowerTypeRef((functionById(program, value.callee) orelse return error.UnknownFunction).return_type),
                        } },
                        .inherited => unreachable,
                    });
                },
                .call_value => |value| try instructions.append(.{ .call_value = .{
                    .callee = value.callee,
                    .args = value.args,
                    .param_ownership = try lowerOwnershipModes(allocator, value.param_ownership),
                    .dst = value.dst,
                } }),
                .call_virtual => |value| try instructions.append(.{ .call_virtual = .{
                    .receiver = value.receiver,
                    .static_type_name = value.static_type_name,
                    .method_name = value.method_name,
                    .args = value.args,
                    .return_ty = lowerTypeRef(value.return_ty),
                    .dst = value.dst,
                } }),
                .ret => |value| try instructions.append(.{ .ret = .{ .src = value.src } }),
                // Scope markers drive native (LLVM) drop elaboration only; the VM
                // reclaims via its own native-layout destructors, so emit nothing.
                .scope_enter, .scope_exit => {},
            }
        }

        try functions.append(.{
            .id = function_decl.id,
            .name = function_decl.name,
            .param_count = @as(u32, @intCast(function_decl.param_types.len)),
            .param_ownership = try lowerOwnershipModes(allocator, function_decl.param_ownership),
            .param_types = try lowerLocalTypes(allocator, function_decl.param_types),
            .return_type = lowerTypeRef(function_decl.return_type),
            .return_ownership = lowerOwnershipMode(function_decl.return_ownership),
            .register_count = function_decl.register_count,
            .local_count = function_decl.local_count,
            .local_types = try lowerLocalTypes(allocator, function_decl.local_types),
            .instructions = try instructions.toOwnedSlice(),
        });

        if (index == program.entry_index and resolved_execution == .runtime) {
            entry_function_id = function_decl.id;
        }
    }

    return .{
        .constructs = try constructs.toOwnedSlice(),
        .construct_implementations = try construct_implementations.toOwnedSlice(),
        .types = try types.toOwnedSlice(),
        .enums = try enums.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .entry_function_id = entry_function_id,
    };
}

/// Builds a metadata-only bytecode function for a foreign (`@FFI.Extern`)
/// declaration. The function carries no instructions; the VM looks up its
/// `foreign`/`param_types`/`return_type` to drive a LibFFI dispatch.
fn externStub(allocator: std.mem.Allocator, function_decl: ir_pkg.Function) !bytecode.Function {
    return .{
        .id = function_decl.id,
        .name = function_decl.name,
        .param_count = @as(u32, @intCast(function_decl.param_types.len)),
        .param_ownership = try lowerOwnershipModes(allocator, function_decl.param_ownership),
        .param_types = try lowerLocalTypes(allocator, function_decl.param_types),
        .return_type = lowerTypeRef(function_decl.return_type),
        .return_ownership = lowerOwnershipMode(function_decl.return_ownership),
        .is_extern = true,
        .foreign = if (function_decl.foreign) |foreign| .{
            .library_name = foreign.library_name,
            .symbol_name = foreign.symbol_name,
            .calling_convention = foreign.calling_convention,
        } else null,
        .register_count = 0,
        .local_count = 0,
        .local_types = &.{},
        .instructions = &.{},
    };
}

fn lowerLocalTypes(allocator: std.mem.Allocator, local_types: []const ir_pkg.ValueType) ![]instruction.TypeRef {
    const lowered = try allocator.alloc(instruction.TypeRef, local_types.len);
    for (local_types, 0..) |local_ty, index| lowered[index] = lowerTypeRef(local_ty);
    return lowered;
}

fn lowerOwnershipModes(allocator: std.mem.Allocator, values: []const ir_pkg.OwnershipMode) ![]const bytecode.OwnershipMode {
    const lowered = try allocator.alloc(bytecode.OwnershipMode, values.len);
    for (values, 0..) |value, index| lowered[index] = lowerOwnershipMode(value);
    return lowered;
}

fn lowerOwnershipMode(value: ir_pkg.OwnershipMode) bytecode.OwnershipMode {
    return @enumFromInt(@intFromEnum(value));
}

fn lowerTypeRef(value_type: ir_pkg.ValueType) instruction.TypeRef {
    return .{
        .kind = @enumFromInt(@intFromEnum(value_type.kind)),
        .name = value_type.name,
        .construct_constraint = if (value_type.construct_constraint) |constraint| .{ .construct_name = constraint.construct_name } else null,
    };
}

fn functionExecutionById(program: ir_pkg.Program, function_id: u32) ?runtime_abi.FunctionExecution {
    for (program.functions) |function_decl| {
        if (function_decl.id == function_id) return function_decl.execution;
    }
    return null;
}

fn functionById(program: ir_pkg.Program, function_id: u32) ?ir_pkg.Function {
    for (program.functions) |function_decl| {
        if (function_decl.id == function_id) return function_decl;
    }
    return null;
}

fn resolveExecution(execution: runtime_abi.FunctionExecution, mode: CompileMode) runtime_abi.FunctionExecution {
    return switch (execution) {
        .inherited => switch (mode) {
            .vm => .runtime,
            .hybrid_runtime => .runtime,
        },
        else => execution,
    };
}

test "emits hybrid bytecode for runtime and native calls" {
    const program = ir_pkg.Program{
        .types = &.{},
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .execution = .runtime,
                .is_extern = false,
                .foreign = null,
                .param_types = &.{},
                .return_type = .{ .kind = .void },
                .register_count = 0,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .call = .{ .callee = 1, .args = &.{}, .dst = null } },
                    .{ .call = .{ .callee = 2, .args = &.{}, .dst = null } },
                    .{ .ret = .{ .src = null } },
                },
            },
            .{
                .id = 1,
                .name = "runtime_helper",
                .execution = .runtime,
                .is_extern = false,
                .foreign = null,
                .param_types = &.{},
                .return_type = .{ .kind = .void },
                .register_count = 0,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{.{ .ret = .{ .src = null } }},
            },
            .{
                .id = 2,
                .name = "native_helper",
                .execution = .native,
                .is_extern = false,
                .foreign = null,
                .param_types = &.{},
                .return_type = .{ .kind = .void },
                .register_count = 0,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{.{ .ret = .{ .src = null } }},
            },
        },
        .entry_index = 0,
    };

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const module = try compileProgram(arena.allocator(), ir_pkg.VerifiedProgram.assumeVerified(program), .hybrid_runtime);
    try std.testing.expectEqual(@as(usize, 2), module.functions.len);
    try std.testing.expectEqual(@as(?u32, 0), module.entry_function_id);
    try std.testing.expect(module.functions[0].instructions[0] == .call_runtime);
    try std.testing.expect(module.functions[0].instructions[1] == .call_native);
}

test "preserves function constants in bytecode" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const program = ir_pkg.Program{
        .types = &.{},
        .functions = &.{
            .{
                .id = 0,
                .name = "main",
                .execution = .runtime,
                .param_types = &.{},
                .return_type = .{ .kind = .void },
                .register_count = 1,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{
                    .{ .const_function = .{ .dst = 0, .function_id = 1 } },
                    .{ .ret = .{ .src = null } },
                },
            },
            .{
                .id = 1,
                .name = "callback",
                .execution = .runtime,
                .param_types = &.{},
                .return_type = .{ .kind = .void },
                .register_count = 0,
                .local_count = 0,
                .local_types = &.{},
                .instructions = &.{.{ .ret = .{ .src = null } }},
            },
        },
        .entry_index = 0,
    };

    const module = try compileProgram(arena.allocator(), ir_pkg.VerifiedProgram.assumeVerified(program), .vm);
    try std.testing.expect(module.functions[0].instructions[0] == .const_function);
    try std.testing.expectEqual(@as(u32, 1), module.functions[0].instructions[0].const_function.function_id);
}

test "preserves native state instructions in bytecode" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const program = ir_pkg.Program{
        .types = &.{.{
            .name = "CounterState",
            .fields = &.{.{ .name = "count", .ty = .{ .kind = .integer, .name = "I64" } }},
        }},
        .functions = &.{.{
            .id = 0,
            .name = "main",
            .execution = .runtime,
            .param_types = &.{},
            .return_type = .{ .kind = .void },
            .register_count = 3,
            .local_count = 0,
            .local_types = &.{},
            .instructions = &.{
                .{ .alloc_struct = .{ .dst = 0, .type_name = "CounterState" } },
                .{ .alloc_native_state = .{ .dst = 1, .src = 0, .type_name = "CounterState", .type_id = 123 } },
                .{ .recover_native_state = .{ .dst = 2, .state = 1, .type_name = "CounterState", .type_id = 123 } },
                .{ .ret = .{ .src = null } },
            },
        }},
        .entry_index = 0,
    };

    const module = try compileProgram(arena.allocator(), ir_pkg.VerifiedProgram.assumeVerified(program), .vm);
    try std.testing.expect(module.functions[0].instructions[1] == .alloc_native_state);
    try std.testing.expectEqual(@as(u64, 123), module.functions[0].instructions[1].alloc_native_state.type_id);
    try std.testing.expect(module.functions[0].instructions[2] == .recover_native_state);
    try std.testing.expectEqual(@as(u64, 123), module.functions[0].instructions[2].recover_native_state.type_id);
}

test "preserves construct metadata in bytecode" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const program = ir_pkg.Program{
        .constructs = &.{.{ .name = "Widget" }},
        .construct_implementations = &.{.{
            .type_name = "Button",
            .construct_constraint = .{ .construct_name = "Widget" },
            .families = &.{ "Widget", "Renderable" },
            .fields = &.{.{ .name = "title", .ty = .{ .kind = .string } }},
            .has_content = true,
            .lifecycle_hooks = &.{.{ .name = "onAppear" }},
        }},
        .types = &.{},
        .functions = &.{.{
            .id = 0,
            .name = "main",
            .execution = .runtime,
            .param_types = &.{.{ .kind = .construct_any, .name = "any Widget", .construct_constraint = .{ .construct_name = "Widget" } }},
            .return_type = .{ .kind = .void },
            .register_count = 0,
            .local_count = 0,
            .local_types = &.{},
            .instructions = &.{.{ .ret = .{ .src = null } }},
        }},
        .entry_index = 0,
    };

    const module = try compileProgram(arena.allocator(), ir_pkg.VerifiedProgram.assumeVerified(program), .vm);
    try std.testing.expectEqual(@as(usize, 1), module.constructs.len);
    try std.testing.expectEqualStrings("Widget", module.constructs[0].name);
    try std.testing.expectEqual(@as(usize, 1), module.construct_implementations.len);
    try std.testing.expectEqualStrings("Widget", module.construct_implementations[0].construct_constraint.construct_name);
    try std.testing.expectEqual(@as(usize, 2), module.construct_implementations[0].families.len);
    try std.testing.expectEqualStrings("Renderable", module.construct_implementations[0].families[1]);
    try std.testing.expectEqual(@as(usize, 1), module.construct_implementations[0].fields.len);
    try std.testing.expectEqual(instruction.TypeRef.Kind.construct_any, lowerTypeRef(program.functions[0].param_types[0]).kind);
}
