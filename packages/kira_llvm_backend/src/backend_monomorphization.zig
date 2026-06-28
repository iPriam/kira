const std = @import("std");
const ir = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const backend_api = @import("kira_backend_api");
const parent = @import("backend.zig");

const functionById = parent.functionById;
const functionSymbolName = parent.functionSymbolName;
const resolveExecution = parent.resolveExecution;

pub const FunctionVariant = struct {
    function_id: u32,
    param_types: []const ir.ValueType,
    param_ownership: []const ir.OwnershipMode = &.{},
    local_types: []const ir.ValueType,
    return_type: ir.ValueType,
    return_ownership: ir.OwnershipMode = .owned,
    register_types: []const ir.ValueType,
    symbol_name: []const u8,
};

const PendingVariant = struct {
    key: VariantKey,
    variant: FunctionVariant,
    state: enum { pending, resolved },
};

const VariantKey = struct {
    function_id: u32,
    param_types: []const ir.ValueType,
};

pub const Plan = struct {
    allocator: std.mem.Allocator,
    program: *const ir.Program,
    mode: backend_api.BackendMode,
    variants: []FunctionVariant,

    pub fn deinit(self: *Plan) void {
        for (self.variants) |variant| {
            self.allocator.free(variant.param_types);
            self.allocator.free(variant.param_ownership);
            self.allocator.free(variant.local_types);
            self.allocator.free(variant.register_types);
            self.allocator.free(variant.symbol_name);
        }
        self.allocator.free(self.variants);
    }

    pub fn variantForFunction(self: *const Plan, function_id: u32, register_types: []const ir.ValueType, args: []const u32) ?*const FunctionVariant {
        const function_decl = functionById(self.program.*, function_id) orelse return null;
        if (!functionNeedsMonomorphization(function_decl)) {
            for (self.variants) |*variant| {
                if (variant.function_id == function_id and sameTypeSlice(variant.param_types, function_decl.param_types)) return variant;
            }
            return null;
        }

        const specialized = self.allocator.alloc(ir.ValueType, function_decl.param_types.len) catch return null;
        defer self.allocator.free(specialized);
        for (function_decl.param_types, 0..) |param_type, index| {
            const actual_type = if (args[index] < register_types.len) register_types[args[index]] else param_type;
            specialized[index] = concreteParamType(self.program.*, param_type, actual_type) orelse param_type;
        }
        for (self.variants) |*variant| {
            if (variant.function_id == function_id and sameTypeSlice(variant.param_types, specialized)) return variant;
        }
        return null;
    }
};

pub fn buildPlan(allocator: std.mem.Allocator, request: backend_api.CompileRequest) !Plan {
    var pending = std.array_list.Managed(PendingVariant).init(allocator);
    defer pending.deinit();

    var index: usize = 0;
    while (index < pending.items.len) : (index += 1) {
        if (pending.items[index].state == .resolved) continue;
        try resolveVariant(allocator, request.program.programPtr(), request.mode, &pending, index);
    }

    const variants = try allocator.alloc(FunctionVariant, pending.items.len);
    for (pending.items, 0..) |item, i| variants[i] = item.variant;
    return .{ .allocator = allocator, .program = request.program.programPtr(), .mode = request.mode, .variants = variants };
}

fn ensureVariant(
    allocator: std.mem.Allocator,
    program: *const ir.Program,
    mode: backend_api.BackendMode,
    pending: *std.array_list.Managed(PendingVariant),
    function_id: u32,
    param_types: []const ir.ValueType,
) !usize {
    for (pending.items, 0..) |item, index| {
        if (item.key.function_id == function_id and sameTypeSlice(item.key.param_types, param_types)) return index;
    }

    const function_decl = functionById(program.*, function_id) orelse return error.UnknownFunction;
    const owned_params = try dupTypeSlice(allocator, param_types);
    errdefer allocator.free(owned_params);
    const owned_param_ownership = try dupOwnershipSlice(allocator, function_decl.param_ownership);
    errdefer allocator.free(owned_param_ownership);
    const owned_locals = try dupTypeSlice(allocator, function_decl.local_types);
    errdefer allocator.free(owned_locals);
    for (owned_params, 0..) |param_type, index| {
        if (index < owned_locals.len) owned_locals[index] = param_type;
    }
    const symbol_name = try specializationSymbolName(allocator, function_decl, mode, owned_params);
    errdefer allocator.free(symbol_name);
    try pending.append(.{
        .key = .{ .function_id = function_id, .param_types = owned_params },
        .variant = .{
            .function_id = function_id,
            .param_types = owned_params,
            .param_ownership = owned_param_ownership,
            .local_types = owned_locals,
            .return_type = function_decl.return_type,
            .return_ownership = function_decl.return_ownership,
            .register_types = &.{},
            .symbol_name = symbol_name,
        },
        .state = .pending,
    });
    return pending.items.len - 1;
}

fn resolveVariant(
    allocator: std.mem.Allocator,
    program: *const ir.Program,
    mode: backend_api.BackendMode,
    pending: *std.array_list.Managed(PendingVariant),
    variant_index: usize,
) !void {
    var item = &pending.items[variant_index];
    if (item.state == .resolved) return;
    const function_decl = functionById(program.*, item.variant.function_id) orelse return error.UnknownFunction;
    const register_types = try allocator.alloc(ir.ValueType, function_decl.register_count);
    errdefer allocator.free(register_types);

    for (function_decl.instructions) |instruction| {
        switch (instruction) {
            .const_int => |value| register_types[value.dst] = .{ .kind = .integer, .name = "I64" },
            .const_float => |value| register_types[value.dst] = .{ .kind = .float, .name = "F64" },
            .const_string => |value| register_types[value.dst] = .{ .kind = .string },
            .const_bool => |value| register_types[value.dst] = .{ .kind = .boolean },
            .const_null_ptr => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = "RawPtr" },
            .alloc_struct => |value| register_types[value.dst] = .{ .kind = .ffi_struct, .name = value.type_name },
            .alloc_enum => |value| register_types[value.dst] = .{ .kind = .enum_instance, .name = value.enum_type_name },
            .alloc_native_state => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = value.type_name },
            .alloc_array => |value| register_types[value.dst] = value.ty,
            .const_function => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = if (value.representation == .callable_value) "Callable" else "RawPtr" },
            .const_closure => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = "Closure" },
            .add => |value| register_types[value.dst] = register_types[value.lhs],
            .subtract => |value| register_types[value.dst] = register_types[value.lhs],
            .multiply => |value| register_types[value.dst] = register_types[value.lhs],
            .divide => |value| register_types[value.dst] = register_types[value.lhs],
            .modulo => |value| register_types[value.dst] = register_types[value.lhs],
            .convert => |value| register_types[value.dst] = .{ .kind = value.target },
            .compare => |value| register_types[value.dst] = .{ .kind = .boolean },
            .unary => |value| register_types[value.dst] = switch (value.op) {
                .negate => register_types[value.src],
                .not => .{ .kind = .boolean },
            },
            .load_local => |value| register_types[value.dst] = item.variant.local_types[value.local],
            .local_ptr => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = "LocalPtr" },
            .subobject_ptr => |value| register_types[value.dst] = register_types[value.base],
            .field_ptr => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = value.field_ty.name },
            .recover_native_state => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = value.type_name },
            .native_state_field_get => |value| register_types[value.dst] = value.field_ty,
            .c_string_to_string => |value| register_types[value.dst] = .{ .kind = .string },
            .array_len => |value| register_types[value.dst] = .{ .kind = .integer, .name = "I64" },
            .string_len => |value| register_types[value.dst] = .{ .kind = .integer, .name = "I64" },
            .array_get => |value| register_types[value.dst] = value.ty,
            .enum_tag => |value| register_types[value.dst] = .{ .kind = .integer, .name = "I64" },
            .enum_payload => |value| register_types[value.dst] = value.payload_ty,
            .load_indirect => |value| register_types[value.dst] = value.ty,
            .call => |value| if (value.dst) |dst| {
                const callee_decl = functionById(program.*, value.callee) orelse return error.UnknownFunction;
                const callee_execution = parent.functionExecutionById(program.*, value.callee) orelse return error.UnknownFunction;
                if (resolveExecution(callee_execution, mode) == .native) {
                    const specialized_params = try specializeParamTypes(allocator, program.*, callee_decl, register_types, value.args);
                    defer allocator.free(specialized_params);
                    const callee_index = try ensureVariant(allocator, program, mode, pending, value.callee, specialized_params);
                    if (pending.items[callee_index].state != .resolved) try resolveVariant(allocator, program, mode, pending, callee_index);
                    register_types[dst] = pending.items[callee_index].variant.return_type;
                } else {
                    register_types[dst] = callee_decl.return_type;
                }
            },
            .call_value => |value| {
                if (value.dst) |dst| register_types[dst] = value.return_type;
            },
            .call_virtual => |value| {
                if (value.dst) |dst| register_types[dst] = value.return_ty;
            },
            .store_local, .array_set, .array_append, .native_state_field_set, .store_indirect, .copy_indirect, .branch, .jump, .label, .print, .ret, .scope_enter, .scope_exit => {},
        }
    }

    if (function_decl.return_type.kind == .construct_any) {
        var candidate: ?ir.ValueType = null;
        var stable = true;
        for (function_decl.instructions) |instruction| {
            if (instruction != .ret) continue;
            const src = instruction.ret.src orelse continue;
            const ret_ty = register_types[src];
            if (candidate == null) {
                candidate = ret_ty;
            } else if (!sameValueType(candidate.?, ret_ty)) {
                stable = false;
                break;
            }
        }
        if (stable and candidate != null and candidate.?.kind != .construct_any) item.variant.return_type = candidate.?;
    }

    item.variant.register_types = register_types;
    item.state = .resolved;
}

fn specializeParamTypes(allocator: std.mem.Allocator, program: ir.Program, function_decl: ir.Function, register_types: []const ir.ValueType, args: []const u32) ![]ir.ValueType {
    const specialized = try allocator.alloc(ir.ValueType, function_decl.param_types.len);
    for (function_decl.param_types, 0..) |param_type, index| {
        const actual_type = if (args[index] < register_types.len) register_types[args[index]] else param_type;
        specialized[index] = concreteParamType(program, param_type, actual_type) orelse param_type;
    }
    return specialized;
}

fn concreteParamType(program: ir.Program, param_type: ir.ValueType, actual_type: ir.ValueType) ?ir.ValueType {
    if (param_type.kind != .construct_any) return param_type;
    const constraint = param_type.construct_constraint orelse return null;
    if (actual_type.kind != .ffi_struct or actual_type.name == null) return null;
    for (program.construct_implementations) |implementation| {
        if (std.mem.eql(u8, implementation.construct_constraint.construct_name, constraint.construct_name) and std.mem.eql(u8, implementation.type_name, actual_type.name.?)) {
            return actual_type;
        }
    }
    return null;
}

fn functionNeedsMonomorphization(function_decl: ir.Function) bool {
    for (function_decl.param_types) |param_type| {
        if (param_type.kind == .construct_any) return true;
    }
    return function_decl.return_type.kind == .construct_any;
}

fn specializationSymbolName(allocator: std.mem.Allocator, function_decl: ir.Function, mode: backend_api.BackendMode, param_types: []const ir.ValueType) ![]const u8 {
    const base = try functionSymbolName(allocator, function_decl, mode);
    defer allocator.free(base);
    if (!functionNeedsMonomorphization(function_decl)) return allocator.dupe(u8, base);

    var buffer = std.array_list.Managed(u8).init(allocator);
    defer buffer.deinit();
    try buffer.appendSlice(base);
    for (function_decl.param_types, 0..) |param_type, index| {
        if (param_type.kind != .construct_any) continue;
        try buffer.appendSlice("__mono_");
        const index_text = try std.fmt.allocPrint(allocator, "{d}_", .{index});
        defer allocator.free(index_text);
        try buffer.appendSlice(index_text);
        if (param_types[index].name) |name| {
            for (name) |byte| {
                try buffer.append(if (std.ascii.isAlphanumeric(byte)) byte else '_');
            }
        } else {
            try buffer.appendSlice("any");
        }
    }
    return buffer.toOwnedSlice();
}

fn dupTypeSlice(allocator: std.mem.Allocator, src: []const ir.ValueType) ![]ir.ValueType {
    const out = try allocator.alloc(ir.ValueType, src.len);
    @memcpy(out, src);
    return out;
}

fn dupOwnershipSlice(allocator: std.mem.Allocator, src: []const ir.OwnershipMode) ![]ir.OwnershipMode {
    const out = try allocator.alloc(ir.OwnershipMode, src.len);
    @memcpy(out, src);
    return out;
}

fn sameTypeSlice(lhs: []const ir.ValueType, rhs: []const ir.ValueType) bool {
    if (lhs.len != rhs.len) return false;
    for (lhs, rhs) |left, right| {
        if (!sameValueType(left, right)) return false;
    }
    return true;
}

fn sameValueType(lhs: ir.ValueType, rhs: ir.ValueType) bool {
    if (lhs.kind != rhs.kind) return false;
    if (lhs.construct_constraint) |constraint| {
        const rhs_constraint = rhs.construct_constraint orelse return false;
        if (!std.mem.eql(u8, constraint.construct_name, rhs_constraint.construct_name)) return false;
    } else if (rhs.construct_constraint != null) {
        return false;
    }
    if (lhs.name == null and rhs.name == null) return true;
    if (lhs.name == null or rhs.name == null) return false;
    return std.mem.eql(u8, lhs.name.?, rhs.name.?);
}

test "monomorphizes native any construct call chains by concrete implementation" {
    const allocator = std.testing.allocator;
    const widget_any = ir.ValueType{ .kind = .construct_any, .name = "any Widget", .construct_constraint = .{ .construct_name = "Widget" } };
    const button_ty = ir.ValueType{ .kind = .ffi_struct, .name = "Button" };
    var construct_implementations = [_]ir.ConstructImplementation{.{
        .type_name = "Button",
        .construct_constraint = .{ .construct_name = "Widget" },
        .fields = &.{},
        .has_content = false,
        .lifecycle_hooks = &.{},
    }};
    var types = [_]ir.TypeDecl{.{ .name = "Button", .fields = &.{} }};
    var main_instructions = [_]ir.Instruction{
        .{ .alloc_struct = .{ .dst = 0, .type_name = "Button" } },
        .{ .call = .{ .callee = 1, .args = &.{0}, .dst = 1 } },
        .{ .ret = .{ .src = null } },
    };
    var identity_instructions = [_]ir.Instruction{
        .{ .load_local = .{ .dst = 0, .local = 0 } },
        .{ .ret = .{ .src = 0 } },
    };

    var functions = [_]ir.Function{
        .{
            .id = 0,
            .name = "main",
            .execution = .native,
            .param_types = &.{},
            .return_type = .{ .kind = .void },
            .register_count = 2,
            .local_count = 0,
            .local_types = &.{},
            .instructions = main_instructions[0..],
        },
        .{
            .id = 1,
            .name = "identity",
            .execution = .native,
            .param_types = &.{widget_any},
            .return_type = widget_any,
            .register_count = 1,
            .local_count = 1,
            .local_types = &.{widget_any},
            .instructions = identity_instructions[0..],
        },
    };

    const program = ir.Program{
        .construct_implementations = construct_implementations[0..],
        .types = types[0..],
        .functions = functions[0..],
        .entry_index = 0,
    };

    var plan = try buildPlan(allocator, .{
        .module_name = "test",
        .program = &program,
        .mode = .llvm_native,
        .emit = .{ .object_path = "ignored.o" },
        .resolved_native_libraries = &.{},
    });
    defer plan.deinit();

    try std.testing.expectEqual(@as(usize, 1), plan.variants.len);
    try std.testing.expectEqualStrings("Button", plan.variants[0].param_types[0].name.?);
    try std.testing.expectEqualStrings("Button", plan.variants[0].return_type.name.?);
    try std.testing.expectEqual(button_ty.kind, plan.variants[0].return_type.kind);
}
