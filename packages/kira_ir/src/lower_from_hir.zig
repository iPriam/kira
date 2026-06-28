const builtin = @import("builtin");
const std = @import("std");
const ir = @import("ir.zig");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");
const program_impl = @import("lower_from_hir_program.zig");
const type_impl = @import("lower_from_hir_types.zig");
const statement_impl = @import("lower_from_hir_statements.zig");
const boxed_impl = @import("lower_from_hir_boxed.zig");
const namespace_ref_impl = @import("lower_from_hir_namespace_refs.zig");
const places_impl = @import("lower_from_hir_places.zig");

pub const lowerTypeDecls = program_impl.lowerTypeDecls;
pub const lowerEnumTypeDecls = program_impl.lowerEnumTypeDecls;
pub const lowerConstructs = program_impl.lowerConstructs;
pub const lowerConstructImplementations = program_impl.lowerConstructImplementations;
pub const markReachableFunction = program_impl.markReachableFunction;
pub const markReachableStatement = program_impl.markReachableStatement;
pub const markReachableExpr = program_impl.markReachableExpr;
pub const markReferencedType = program_impl.markReferencedType;
pub const lowerFieldTypes = program_impl.lowerFieldTypes;
pub const lowerFfiTypeInfo = program_impl.lowerFfiTypeInfo;
pub const lowerAssignmentStatement = program_impl.lowerAssignmentStatement;
pub const findTypeFieldDefaultExpr = program_impl.findTypeFieldDefaultExpr;
pub const fieldDeclIsTypeConstant = program_impl.fieldDeclIsTypeConstant;
pub const functionIdByName = program_impl.functionIdByName;
pub const lowerNamespaceRefExpr = namespace_ref_impl.lowerNamespaceRefExpr;
pub const lowerResolvedType = type_impl.lowerResolvedType;
pub const lowerNamedType = type_impl.lowerNamedType;
pub const lowerExecutableCompareOperandType = type_impl.lowerExecutableCompareOperandType;
pub const lowerExecutableIntegerType = type_impl.lowerExecutableIntegerType;
pub const lowerExecutableNumericType = type_impl.lowerExecutableNumericType;
pub const lowerExecutableBooleanType = type_impl.lowerExecutableBooleanType;
pub const valueTypesEqual = type_impl.valueTypesEqual;
pub const findTypeDeclByName = type_impl.findTypeDeclByName;
pub const resolveConstructFieldIndex = type_impl.resolveConstructFieldIndex;
pub const fieldIndexByName = type_impl.fieldIndexByName;
pub const nativeStateTypeId = type_impl.nativeStateTypeId;

pub fn lowerProgram(allocator: std.mem.Allocator, program: model.Program) !ir.Program {
    return lowerProgramWithOptions(allocator, program, .{});
}

pub const LowerProgramOptions = struct {
    worker_count_override: ?usize = null,
    include_tests: bool = false,
};

pub fn lowerProgramWithOptions(allocator: std.mem.Allocator, program: model.Program, options: LowerProgramOptions) !ir.Program {
    var reachable = std.AutoHashMapUnmanaged(u32, void){};
    defer reachable.deinit(allocator);
    try markReachableFunction(allocator, program, &reachable, program.functions[program.entry_index].id);
    if (options.include_tests) {
        for (program.tests) |test_case| {
            try markReachableFunctionByName(allocator, program, &reachable, test_case.test_function);
            try markReachableFunctionByName(allocator, program, &reachable, test_case.expect_function);
        }
        // The synthesized pure-Kira test driver (when present) is the entry the
        // test runner invokes by name; keep it (and everything it calls) live.
        // No-op when the driver was not synthesized.
        try markReachableFunctionByName(allocator, program, &reachable, "__kira_test_main");
    }

    const constructs = try lowerConstructs(allocator, program);
    const construct_implementations = try lowerConstructImplementations(allocator, program);
    const types = try lowerTypeDecls(allocator, program, reachable);
    const enums = try lowerEnumTypeDecls(allocator, program, reachable);

    const plans = try buildFunctionPlans(allocator, program, reachable);
    const batches = if (shouldParallelLower(options, plans.len))
        try lowerFunctionPlansParallel(allocator, program, plans, options)
    else
        try lowerFunctionPlansSerial(allocator, program, plans);
    defer allocator.free(batches);

    var function_count: usize = 0;
    for (batches) |batch| function_count += 1 + batch.generated_functions.len;
    const functions = try allocator.alloc(ir.Function, function_count);

    var entry_index: ?usize = null;
    var primary_index: usize = 0;
    const entry_function_id = program.functions[program.entry_index].id;
    for (batches) |batch| {
        if (batch.primary.id == entry_function_id) entry_index = primary_index;
        functions[primary_index] = batch.primary;
        primary_index += 1;
    }
    var generated_index = primary_index;
    for (batches) |batch| {
        for (batch.generated_functions) |function_decl| {
            functions[generated_index] = function_decl;
            generated_index += 1;
        }
    }

    return .{
        .constructs = constructs,
        .construct_implementations = construct_implementations,
        .types = types,
        .enums = enums,
        .functions = functions,
        .entry_index = entry_index orelse return error.UnsupportedExecutableFeature,
    };
}

fn markReachableFunctionByName(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    name: []const u8,
) !void {
    for (program.functions) |function_decl| {
        if (std.mem.eql(u8, function_decl.name, name)) {
            try markReachableFunction(allocator, program, reachable, function_decl.id);
            return;
        }
    }
}

const FunctionPlan = struct {
    function_decl: model.Function,
    first_generated_id: u32,
    generated_count: u32,
};

fn buildFunctionPlans(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: std.AutoHashMapUnmanaged(u32, void),
) ![]FunctionPlan {
    var plans = std.array_list.Managed(FunctionPlan).init(allocator);
    var next_generated_id = nextGeneratedFunctionId(program);
    for (program.functions) |function_decl| {
        if (!reachable.contains(function_decl.id)) continue;
        const generated_count = countCallbacksInStatements(function_decl.body);
        try plans.append(.{
            .function_decl = function_decl,
            .first_generated_id = next_generated_id,
            .generated_count = generated_count,
        });
        next_generated_id += generated_count;
    }
    return plans.toOwnedSlice();
}

const FunctionLoweringState = struct {
    next_generated_function_id: u32,
    generated_functions: std.array_list.Managed(ir.Function),
};

const LoweredFunctionBatch = struct {
    primary: ir.Function,
    generated_functions: []ir.Function,
};

fn nextGeneratedFunctionId(program: model.Program) u32 {
    var next_id: u32 = 0;
    for (program.functions) |function_decl| {
        if (function_decl.id >= next_id) next_id = function_decl.id + 1;
    }
    return next_id;
}

fn lowerFunctionPlansSerial(
    allocator: std.mem.Allocator,
    program: model.Program,
    plans: []const FunctionPlan,
) ![]LoweredFunctionBatch {
    const batches = try allocator.alloc(LoweredFunctionBatch, plans.len);
    for (plans, 0..) |plan, index| {
        batches[index] = try lowerFunctionBatch(allocator, program, plan);
    }
    return batches;
}

fn lowerFunctionBatch(
    allocator: std.mem.Allocator,
    program: model.Program,
    plan: FunctionPlan,
) !LoweredFunctionBatch {
    var state = FunctionLoweringState{
        .next_generated_function_id = plan.first_generated_id,
        .generated_functions = std.array_list.Managed(ir.Function).init(allocator),
    };
    errdefer state.generated_functions.deinit();

    const primary = try lowerFunction(allocator, program, plan.function_decl, &state);
    const generated_functions = try state.generated_functions.toOwnedSlice();
    if (state.next_generated_function_id != plan.first_generated_id + plan.generated_count) {
        return error.GeneratedCallbackPlanMismatch;
    }
    return .{
        .primary = primary,
        .generated_functions = generated_functions,
    };
}

fn shouldParallelLower(options: LowerProgramOptions, plan_count: usize) bool {
    _ = options;
    _ = plan_count;
    return false;
}

fn resolveLowerWorkerCount(options: LowerProgramOptions, plan_count: usize) usize {
    if (plan_count < 4) return 1;
    if (options.worker_count_override) |override| {
        if (override == 0) return 1;
        return @min(override, plan_count);
    }

    const cpu_count = std.Thread.getCpuCount() catch 1;
    const cap = switch (builtin.os.tag) {
        .windows => @min(cpu_count, 4),
        else => @min(cpu_count, 8),
    };
    return @max(@min(cap, plan_count), 1);
}

const ParallelLowerResult = struct {
    arena: ?*std.heap.ArenaAllocator = null,
    batch: ?LoweredFunctionBatch = null,
    err: ?anyerror = null,

    fn deinit(self: *ParallelLowerResult) void {
        if (self.arena) |arena_ptr| {
            arena_ptr.deinit();
            std.heap.smp_allocator.destroy(arena_ptr);
        }
        self.* = .{};
    }
};

const ParallelLowerShared = struct {
    program: model.Program,
    plans: []const FunctionPlan,
    results: []ParallelLowerResult,
    next_index: std.atomic.Value(usize) = .init(0),

    fn runUntilDone(self: *ParallelLowerShared) void {
        while (true) {
            const index = self.next_index.fetchAdd(1, .monotonic);
            if (index >= self.plans.len) break;
            self.runJob(index);
        }
    }

    fn runJob(self: *ParallelLowerShared, index: usize) void {
        const result = &self.results[index];
        const arena_ptr = std.heap.smp_allocator.create(std.heap.ArenaAllocator) catch {
            result.err = error.OutOfMemory;
            return;
        };
        arena_ptr.* = std.heap.ArenaAllocator.init(std.heap.smp_allocator);
        const allocator = arena_ptr.allocator();

        const batch = lowerFunctionBatch(allocator, self.program, self.plans[index]) catch |err| {
            result.arena = arena_ptr;
            result.err = err;
            return;
        };

        result.arena = arena_ptr;
        result.batch = batch;
    }
};

fn parallelLowerWorkerMain(shared: *ParallelLowerShared) void {
    shared.runUntilDone();
}

fn lowerFunctionPlansParallel(
    allocator: std.mem.Allocator,
    program: model.Program,
    plans: []const FunctionPlan,
    options: LowerProgramOptions,
) ![]LoweredFunctionBatch {
    const worker_count = resolveLowerWorkerCount(options, plans.len);
    if (worker_count <= 1) return lowerFunctionPlansSerial(allocator, program, plans);

    const results = try allocator.alloc(ParallelLowerResult, plans.len);
    for (results) |*result| result.* = .{};
    defer {
        for (results) |*result| result.deinit();
        allocator.free(results);
    }

    var shared = ParallelLowerShared{
        .program = program,
        .plans = plans,
        .results = results,
    };

    const extra_workers = worker_count - 1;
    const threads = try allocator.alloc(std.Thread, extra_workers);
    defer allocator.free(threads);
    var spawned: usize = 0;
    errdefer {
        shared.runUntilDone();
        for (threads[0..spawned]) |thread| thread.join();
    }
    for (threads) |*thread| {
        thread.* = try std.Thread.spawn(.{}, parallelLowerWorkerMain, .{&shared});
        spawned += 1;
    }
    shared.runUntilDone();
    for (threads) |thread| thread.join();

    const batches = try allocator.alloc(LoweredFunctionBatch, plans.len);
    errdefer allocator.free(batches);
    for (results, 0..) |*result, index| {
        if (result.err) |err| return err;
        batches[index] = try cloneLoweredFunctionBatch(allocator, result.batch.?);
    }
    return batches;
}

fn lowerFunction(
    allocator: std.mem.Allocator,
    program: model.Program,
    function_decl: model.Function,
    state: *FunctionLoweringState,
) !ir.Function {
    if (function_decl.is_extern) {
        return .{
            .id = function_decl.id,
            .name = function_decl.name,
            .execution = function_decl.execution,
            .is_extern = true,
            .foreign = if (function_decl.foreign) |foreign| .{
                .library_name = foreign.library_name,
                .symbol_name = foreign.symbol_name,
                .calling_convention = foreign.calling_convention,
            } else null,
            .param_types = try lowerParamTypes(allocator, program, function_decl.params),
            .param_ownership = try lowerParamOwnership(allocator, function_decl.params),
            .return_type = try lowerResolvedType(program, function_decl.return_type),
            .return_ownership = lowerOwnershipMode(function_decl.return_ownership),
            .register_count = 0,
            .local_count = 0,
            .local_types = &.{},
            .instructions = &.{},
        };
    }

    const boxed_locals = try collectBoxedLocals(allocator, function_decl.locals.len, function_decl.body);
    var lowerer = Lowerer{
        .allocator = allocator,
        .program = program,
        .state = state,
        .execution = function_decl.execution,
        .function_name = function_decl.name,
        .next_register = 0,
        .next_label = 0,
        .next_local = @as(u32, @intCast(function_decl.locals.len)),
        .hidden_local_types = std.array_list.Managed(ir.ValueType).init(allocator),
        .loop_stack = std.array_list.Managed(Lowerer.LoopLabels).init(allocator),
        .boxed_locals = boxed_locals,
    };
    defer allocator.free(boxed_locals);
    defer lowerer.hidden_local_types.deinit();
    defer lowerer.loop_stack.deinit();
    var instructions = std.array_list.Managed(ir.Instruction).init(allocator);
    for (function_decl.locals) |local| {
        if (!local.is_param or !lowerer.isBoxedLocal(local.id)) continue;
        const value_reg = lowerer.freshRegister();
        try instructions.append(.{ .load_local = .{ .dst = value_reg, .local = local.id } });
        try lowerer.initializeBoxedLocal(&instructions, local.id, try lowerResolvedType(program, local.ty), value_reg);
    }
    const terminated = try lowerer.lowerStatements(&instructions, function_decl.body);

    if (!terminated and (instructions.items.len == 0 or instructions.items[instructions.items.len - 1] != .ret)) {
        try instructions.append(.{ .ret = .{ .src = null } });
    }

    return .{
        .id = function_decl.id,
        .name = function_decl.name,
        .execution = function_decl.execution,
        .is_extern = false,
        .foreign = null,
        .param_types = try lowerParamTypes(allocator, program, function_decl.params),
        .param_ownership = try lowerParamOwnership(allocator, function_decl.params),
        .return_type = try lowerResolvedType(program, function_decl.return_type),
        .return_ownership = lowerOwnershipMode(function_decl.return_ownership),
        .register_count = lowerer.next_register,
        .local_count = lowerer.next_local,
        .local_types = try lowerAllLocalTypesBoxed(allocator, program, function_decl.locals, lowerer.hidden_local_types.items, boxed_locals),
        .instructions = try instructions.toOwnedSlice(),
    };
}

fn lowerGeneratedCallbackFunction(
    allocator: std.mem.Allocator,
    program: model.Program,
    state: *FunctionLoweringState,
    function_id: u32,
    function_name: []const u8,
    execution: runtime_abi.FunctionExecution,
    callback: model.hir.CallbackExpr,
) !ir.Function {
    const local_remap = try buildCallbackLocalRemap(allocator, callback);
    defer allocator.free(local_remap);
    const callback_local_count = callbackLocalCount(local_remap);
    const original_boxed_locals = try collectBoxedLocals(allocator, callback.locals.len, callback.body);
    defer allocator.free(original_boxed_locals);
    const boxed_locals = try remapBoxedLocals(allocator, local_remap, callback_local_count, original_boxed_locals);
    for (callback.captures) |capture| {
        const mapped = remapLocalId(local_remap, capture.local_id);
        if (capture.by_ref and mapped < boxed_locals.len) boxed_locals[mapped] = true;
    }
    var lowerer = Lowerer{
        .allocator = allocator,
        .program = program,
        .state = state,
        .execution = execution,
        .function_name = function_name,
        .next_register = 0,
        .next_label = 0,
        .next_local = callback_local_count,
        .hidden_local_types = std.array_list.Managed(ir.ValueType).init(allocator),
        .loop_stack = std.array_list.Managed(Lowerer.LoopLabels).init(allocator),
        .boxed_locals = boxed_locals,
        .local_remap = local_remap,
    };
    defer allocator.free(boxed_locals);
    defer lowerer.hidden_local_types.deinit();
    defer lowerer.loop_stack.deinit();

    var instructions = std.array_list.Managed(ir.Instruction).init(allocator);
    for (callback.captures, 0..) |capture, index| {
        const param_slot: u32 = @intCast(callback.params.len + index);
        const capture_local = lowerer.mapLocal(capture.local_id);
        if (param_slot == capture_local) continue;
        const reg = lowerer.freshRegister();
        try instructions.append(.{ .load_local = .{ .dst = reg, .local = param_slot } });
        try instructions.append(.{ .store_local = .{ .src = reg, .local = capture_local } });
    }
    const terminated = try lowerer.lowerStatements(&instructions, callback.body);
    if (!terminated and (instructions.items.len == 0 or instructions.items[instructions.items.len - 1] != .ret)) {
        try instructions.append(.{ .ret = .{ .src = null } });
    }

    return .{
        .id = function_id,
        .name = function_name,
        .execution = execution,
        .is_extern = false,
        .foreign = null,
        .param_types = try lowerCallbackParamTypes(allocator, program, callback),
        .param_ownership = try lowerCallbackParamOwnership(allocator, callback),
        .return_type = try lowerResolvedType(program, callback.return_type),
        .return_ownership = .owned,
        .register_count = lowerer.next_register,
        .local_count = lowerer.next_local,
        .local_types = try lowerCallbackLocalTypes(allocator, program, callback, local_remap, callback_local_count, lowerer.hidden_local_types.items, boxed_locals),
        .instructions = try instructions.toOwnedSlice(),
    };
}

fn countCallbacksInStatements(statements: []const model.Statement) u32 {
    var count: u32 = 0;
    for (statements) |statement| count += countCallbacksInStatement(statement);
    return count;
}

fn countCallbacksInStatement(statement: model.Statement) u32 {
    return switch (statement) {
        .let_stmt => |node| if (node.value) |value| countCallbacksInExpr(value) else 0,
        .assign_stmt => |node| countCallbacksInExpr(node.target) + countCallbacksInExpr(node.value),
        .expr_stmt => |node| countCallbacksInExpr(node.expr),
        .if_stmt => |node| blk: {
            var count = countCallbacksInExpr(node.condition) + countCallbacksInStatements(node.then_body);
            if (node.else_body) |else_body| count += countCallbacksInStatements(else_body);
            break :blk count;
        },
        .for_stmt => |node| countCallbacksInExpr(node.iterator) + countCallbacksInStatements(node.body),
        .while_stmt => |node| countCallbacksInExpr(node.condition) + countCallbacksInStatements(node.body),
        .break_stmt, .continue_stmt => 0,
        .match_stmt => |node| blk: {
            var count = countCallbacksInExpr(node.subject);
            for (node.arms) |arm| {
                count += countCallbacksInPattern(arm.pattern);
                if (arm.guard) |guard| count += countCallbacksInExpr(guard);
                count += countCallbacksInStatements(arm.body);
            }
            break :blk count;
        },
        .switch_stmt => |node| blk: {
            var count = countCallbacksInExpr(node.subject);
            for (node.cases) |case_node| {
                count += countCallbacksInExpr(case_node.pattern);
                count += countCallbacksInStatements(case_node.body);
            }
            if (node.default_body) |default_body| count += countCallbacksInStatements(default_body);
            break :blk count;
        },
        .return_stmt => |node| if (node.value) |value| countCallbacksInExpr(value) else 0,
    };
}

fn countCallbacksInPattern(pattern: model.MatchPattern) u32 {
    return switch (pattern) {
        .variant => |node| if (node.inner) |inner| countCallbacksInPattern(inner.*) else 0,
        .binding => 0,
    };
}

fn countCallbacksInExpr(expr: *model.Expr) u32 {
    return switch (expr.*) {
        .callback => |node| 1 + countCallbacksInStatements(node.body),
        .construct => |node| blk: {
            var count: u32 = 0;
            for (node.fields) |field| count += countCallbacksInExpr(field.value);
            break :blk count;
        },
        .construct_enum_variant => |node| if (node.payload) |payload| countCallbacksInExpr(payload) else 0,
        .native_state => |node| countCallbacksInExpr(node.value),
        .native_user_data => |node| countCallbacksInExpr(node.state),
        .native_recover => |node| countCallbacksInExpr(node.value),
        .call => |node| blk: {
            var count: u32 = 0;
            for (node.args) |arg| count += countCallbacksInExpr(arg);
            break :blk count;
        },
        .virtual_call => |node| blk: {
            var count = countCallbacksInExpr(node.receiver);
            for (node.args) |arg| count += countCallbacksInExpr(arg);
            break :blk count;
        },
        .call_value => |node| blk: {
            var count = countCallbacksInExpr(node.callee);
            for (node.args) |arg| count += countCallbacksInExpr(arg);
            break :blk count;
        },
        .parent_view => |node| countCallbacksInExpr(node.object),
        .c_string_to_string => |node| countCallbacksInExpr(node.value),
        .array_len => |node| countCallbacksInExpr(node.object),
        .string_len => |node| countCallbacksInExpr(node.object),
        .field => |node| countCallbacksInExpr(node.object),
        .binary => |node| countCallbacksInExpr(node.lhs) + countCallbacksInExpr(node.rhs),
        .conditional => |node| countCallbacksInExpr(node.condition) + countCallbacksInExpr(node.then_expr) + countCallbacksInExpr(node.else_expr),
        .unary => |node| countCallbacksInExpr(node.operand),
        .cast => |node| countCallbacksInExpr(node.operand),
        .array => |node| blk: {
            var count: u32 = 0;
            for (node.elements) |element| count += countCallbacksInExpr(element);
            break :blk count;
        },
        .builder_array => |node| countCallbacksInBuilderBlock(node.builder),
        .index => |node| countCallbacksInExpr(node.object) + countCallbacksInExpr(node.index),
        else => 0,
    };
}

fn countCallbacksInBuilderBlock(builder: model.BuilderBlock) u32 {
    var count: u32 = 0;
    for (builder.items) |item| {
        count += switch (item) {
            .expr => |value| countCallbacksInExpr(value.expr),
            .if_item => |value| countCallbacksInExpr(value.condition) + countCallbacksInBuilderBlock(value.then_block) + (if (value.else_block) |else_block| countCallbacksInBuilderBlock(else_block) else 0),
            .for_item => |value| countCallbacksInExpr(value.iterator) + countCallbacksInBuilderBlock(value.body),
            .switch_item => |value| blk: {
                var inner = countCallbacksInExpr(value.subject);
                for (value.cases) |case_node| {
                    inner += countCallbacksInExpr(case_node.pattern);
                    inner += countCallbacksInBuilderBlock(case_node.body);
                }
                if (value.default_block) |default_block| inner += countCallbacksInBuilderBlock(default_block);
                break :blk inner;
            },
        };
    }
    return count;
}

fn lowerAllLocalTypes(
    allocator: std.mem.Allocator,
    program: model.Program,
    locals: []const model.LocalSymbol,
    hidden_locals: []const ir.ValueType,
) ![]ir.ValueType {
    const lowered = try allocator.alloc(ir.ValueType, locals.len + hidden_locals.len);
    for (locals, 0..) |local, index| {
        lowered[index] = try lowerResolvedType(program, local.ty);
    }
    for (hidden_locals, 0..) |local, index| {
        lowered[locals.len + index] = local;
    }
    return lowered;
}

fn lowerAllLocalTypesBoxed(
    allocator: std.mem.Allocator,
    program: model.Program,
    locals: []const model.LocalSymbol,
    hidden_locals: []const ir.ValueType,
    boxed_locals: []const bool,
) ![]ir.ValueType {
    const lowered = try lowerAllLocalTypes(allocator, program, locals, hidden_locals);
    for (boxed_locals, 0..) |boxed, index| {
        if (boxed and index < lowered.len) lowered[index] = .{ .kind = .raw_ptr, .name = "CaptureCell" };
    }
    return lowered;
}

const unmapped_local = std.math.maxInt(u32);

fn buildCallbackLocalRemap(allocator: std.mem.Allocator, callback: model.hir.CallbackExpr) ![]u32 {
    var max_local: u32 = 0;
    for (callback.locals) |local| max_local = @max(max_local, local.id);
    for (callback.captures) |capture| max_local = @max(max_local, capture.local_id);

    const remap = try allocator.alloc(u32, @as(usize, @intCast(max_local)) + 1);
    @memset(remap, unmapped_local);

    for (callback.params, 0..) |param, index| {
        if (param.id < remap.len) remap[param.id] = @intCast(index);
    }
    for (callback.captures, 0..) |capture, index| {
        if (capture.local_id < remap.len) remap[capture.local_id] = @intCast(callback.params.len + index);
    }

    var next_local: u32 = @intCast(callback.params.len + callback.captures.len);
    for (callback.locals) |local| {
        if (local.id >= remap.len or remap[local.id] != unmapped_local) continue;
        remap[local.id] = next_local;
        next_local += 1;
    }

    return remap;
}

fn remapLocalId(remap: []const u32, local: u32) u32 {
    if (local >= remap.len) return local;
    const mapped = remap[local];
    return if (mapped == unmapped_local) local else mapped;
}

fn callbackLocalCount(remap: []const u32) u32 {
    var count: u32 = 0;
    for (remap) |mapped| {
        if (mapped == unmapped_local) continue;
        count = @max(count, mapped + 1);
    }
    return count;
}

fn remapBoxedLocals(allocator: std.mem.Allocator, remap: []const u32, local_count: u32, original: []const bool) ![]bool {
    const boxed = try allocator.alloc(bool, local_count);
    @memset(boxed, false);
    for (original, 0..) |is_boxed, local| {
        if (!is_boxed) continue;
        const mapped = remapLocalId(remap, @intCast(local));
        if (mapped < boxed.len) boxed[mapped] = true;
    }
    return boxed;
}

fn lowerCallbackLocalTypes(
    allocator: std.mem.Allocator,
    program: model.Program,
    callback: model.hir.CallbackExpr,
    local_remap: []const u32,
    local_count: u32,
    hidden_locals: []const ir.ValueType,
    boxed_locals: []const bool,
) ![]ir.ValueType {
    const lowered = try allocator.alloc(ir.ValueType, @as(usize, @intCast(local_count)) + hidden_locals.len);
    for (lowered) |*slot| slot.* = .{ .kind = .void };
    for (callback.locals) |local| {
        const mapped = remapLocalId(local_remap, local.id);
        if (mapped < local_count) lowered[mapped] = try lowerResolvedType(program, local.ty);
    }
    for (hidden_locals, 0..) |hidden, index| {
        lowered[@as(usize, @intCast(local_count)) + index] = hidden;
    }
    for (boxed_locals, 0..) |boxed, index| {
        if (boxed and index < lowered.len) lowered[index] = .{ .kind = .raw_ptr, .name = "CaptureCell" };
    }
    for (callback.captures, 0..) |capture, index| {
        const param_slot = callback.params.len + index;
        if (param_slot < lowered.len) lowered[param_slot] = if (capture.by_ref) .{ .kind = .raw_ptr, .name = "CaptureCell" } else try lowerResolvedType(program, capture.ty);
    }
    return lowered;
}

fn lowerParamTypes(allocator: std.mem.Allocator, program: model.Program, params: []const model.Parameter) ![]ir.ValueType {
    const lowered = try allocator.alloc(ir.ValueType, params.len);
    for (params, 0..) |param, index| {
        lowered[index] = try lowerResolvedType(program, param.ty);
    }
    return lowered;
}

fn lowerParamOwnership(allocator: std.mem.Allocator, params: []const model.Parameter) ![]const ir.OwnershipMode {
    const lowered = try allocator.alloc(ir.OwnershipMode, params.len);
    for (params, 0..) |param, index| lowered[index] = lowerOwnershipMode(param.ownership);
    return lowered;
}

fn lowerOwnershipModeSlice(allocator: std.mem.Allocator, modes: []const model.OwnershipMode) ![]const ir.OwnershipMode {
    const lowered = try allocator.alloc(ir.OwnershipMode, modes.len);
    for (modes, 0..) |mode, index| lowered[index] = lowerOwnershipMode(mode);
    return lowered;
}

fn lowerCaptureOwnershipSlice(allocator: std.mem.Allocator, captures: []const model.Capture) ![]const ir.OwnershipMode {
    const lowered = try allocator.alloc(ir.OwnershipMode, captures.len);
    for (captures, 0..) |capture, index| lowered[index] = lowerOwnershipMode(capture.ownership);
    return lowered;
}

fn lowerCallbackParamTypes(allocator: std.mem.Allocator, program: model.Program, callback: model.hir.CallbackExpr) ![]ir.ValueType {
    const lowered = try allocator.alloc(ir.ValueType, callback.params.len + callback.captures.len);
    for (callback.params, 0..) |param, index| {
        lowered[index] = try lowerResolvedType(program, param.ty);
    }
    for (callback.captures, 0..) |capture, index| {
        lowered[callback.params.len + index] = if (capture.by_ref) .{ .kind = .raw_ptr, .name = "CaptureCell" } else try lowerResolvedType(program, capture.ty);
    }
    return lowered;
}

fn lowerCallbackParamOwnership(allocator: std.mem.Allocator, callback: model.hir.CallbackExpr) ![]const ir.OwnershipMode {
    const lowered = try allocator.alloc(ir.OwnershipMode, callback.params.len + callback.captures.len);
    for (callback.params, 0..) |param, index| lowered[index] = lowerOwnershipMode(param.ownership);
    for (callback.captures, 0..) |capture, index| {
        lowered[callback.params.len + index] = lowerOwnershipMode(capture.ownership);
    }
    return lowered;
}

fn lowerOwnershipMode(mode: model.OwnershipMode) ir.OwnershipMode {
    return @enumFromInt(@intFromEnum(mode));
}

const lowerResolvedTypeSlice = type_impl.lowerResolvedTypeSlice;

const collectBoxedLocals = boxed_impl.collectBoxedLocals;

fn lowerExprStatement(lowerer: *Lowerer, instructions: *std.array_list.Managed(ir.Instruction), expr: *model.Expr) !void {
    switch (expr.*) {
        .call => |call| {
            if (call.trailing_builder != null) return error.UnsupportedExecutableFeature;
            if (std.mem.eql(u8, call.callee_name, "array.append")) {
                if (call.args.len != 2) return error.UnsupportedExecutableFeature;
                // `arr[i].xs.append(v)`: the receiver array is reached through an array
                // element materialized by value, so the append only grows a transient
                // copy unless the element is written back. `lowerMutableObject` records
                // that write-back (none for a plain local-array receiver).
                var writebacks = places_impl.WritebackList.init(lowerer.allocator);
                defer writebacks.deinit();
                const array = try places_impl.lowerMutableObject(lowerer, instructions, call.args[0], &writebacks);
                const src = try lowerer.lowerExpr(instructions, call.args[1]);
                try instructions.append(.{ .array_append = .{
                    .array = array,
                    .src = src,
                } });
                try places_impl.emitWritebacks(instructions, &writebacks);
                return;
            }
            if (std.mem.eql(u8, call.callee_name, "print")) {
                if (call.args.len != 1) return error.UnsupportedExecutableFeature;
                const reg = try lowerer.lowerExpr(instructions, call.args[0]);
                try instructions.append(.{ .print = .{
                    .src = reg,
                    .ty = try lowerResolvedType(lowerer.program, model.hir.exprType(call.args[0].*)),
                } });
                return;
            }
            if (call.function_id == null) return error.UnsupportedExecutableFeature;
            // An array-element argument passed to a `borrow mut` parameter is mutated by
            // the callee in place on the VM's materialized element copy; persist those
            // mutations back into the array after the call.
            var writebacks = places_impl.WritebackList.init(lowerer.allocator);
            defer writebacks.deinit();
            const arg_regs = try places_impl.lowerDirectCallArgs(lowerer, instructions, call.args, call.function_id.?, &writebacks);
            try instructions.append(.{ .call = .{
                .callee = call.function_id.?,
                .args = arg_regs,
                .dst = null,
            } });
            try places_impl.emitWritebacks(instructions, &writebacks);
        },
        .builder_array => |node| {
            _ = try lowerBuilderArrayExpr(lowerer, instructions, node);
        },
        .call_value => |call| {
            const callee = try lowerer.lowerExpr(instructions, call.callee);
            var args = std.array_list.Managed(u32).init(lowerer.allocator);
            defer args.deinit();
            for (call.args) |arg| try args.append(try lowerer.lowerExpr(instructions, arg));
            try instructions.append(.{ .call_value = .{
                .callee = callee,
                .args = try args.toOwnedSlice(),
                .param_types = try lowerResolvedTypeSlice(lowerer.allocator, lowerer.program, call.param_types),
                .param_ownership = try lowerOwnershipModeSlice(lowerer.allocator, call.param_ownership),
                .return_type = try lowerResolvedType(lowerer.program, call.ty),
                .dst = null,
            } });
        },
        .virtual_call => |call| {
            const receiver = try lowerer.lowerExpr(instructions, call.receiver);
            var args = std.array_list.Managed(u32).init(lowerer.allocator);
            defer args.deinit();
            for (call.args) |arg| try args.append(try lowerer.lowerExpr(instructions, arg));
            try instructions.append(.{ .call_virtual = .{
                .receiver = receiver,
                .static_type_name = call.static_type_name,
                .method_name = call.method_name,
                .args = try args.toOwnedSlice(),
                .return_ty = if (call.ty.kind == .unknown) .{ .kind = .void } else try lowerResolvedType(lowerer.program, call.ty),
                .dst = null,
            } });
        },
        .callback => return error.UnsupportedExecutableFeature,
        else => return error.UnsupportedExecutableFeature,
    }
}

fn lowerBuilderArrayExpr(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    node: model.hir.BuilderArrayExpr,
) !u32 {
    const len_reg = lowerer.freshRegister();
    try instructions.append(.{ .const_int = .{ .dst = len_reg, .value = 0 } });
    const dst = lowerer.freshRegister();
    try instructions.append(.{ .alloc_array = .{
        .dst = dst,
        .len = len_reg,
        .ty = try lowerResolvedType(lowerer.program, node.ty),
    } });
    try emitBuilderArrayItems(lowerer, instructions, dst, node.builder);
    return dst;
}

fn emitBuilderArrayItems(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    array_reg: u32,
    builder: model.BuilderBlock,
) !void {
    for (builder.items) |item| {
        switch (item) {
            .expr => |expr_item| {
                const value_reg = try lowerer.lowerExpr(instructions, expr_item.expr);
                try instructions.append(.{ .array_append = .{ .array = array_reg, .src = value_reg } });
            },
            .if_item => |if_item| {
                const condition_reg = try lowerer.lowerExpr(instructions, if_item.condition);
                const then_label = lowerer.freshLabel();
                const else_label = lowerer.freshLabel();
                const end_label = lowerer.freshLabel();
                try instructions.append(.{ .branch = .{
                    .condition = condition_reg,
                    .true_label = then_label,
                    .false_label = else_label,
                } });
                try instructions.append(.{ .label = .{ .id = then_label } });
                try emitBuilderArrayItems(lowerer, instructions, array_reg, if_item.then_block);
                try instructions.append(.{ .jump = .{ .label = end_label } });
                try instructions.append(.{ .label = .{ .id = else_label } });
                if (if_item.else_block) |else_block| try emitBuilderArrayItems(lowerer, instructions, array_reg, else_block);
                try instructions.append(.{ .label = .{ .id = end_label } });
            },
            .for_item => |for_item| {
                switch (for_item.iterator.*) {
                    .array => |iterator| {
                        const binding_ty = try type_impl.lowerResolvedType(lowerer.program, for_item.binding_ty);
                        for (iterator.elements) |element| {
                            const element_reg = try lowerer.lowerExpr(instructions, element);
                            try lowerer.storeValueToLocal(instructions, for_item.binding_local_id, binding_ty, element_reg);
                            try emitBuilderArrayItems(lowerer, instructions, array_reg, for_item.body);
                        }
                    },
                    else => {
                        const binding_ty = try type_impl.lowerResolvedType(lowerer.program, for_item.binding_ty);
                        const iterator_reg = try lowerer.lowerExpr(instructions, for_item.iterator);
                        const len_reg = lowerer.freshRegister();
                        try instructions.append(.{ .array_len = .{ .dst = len_reg, .array = iterator_reg } });
                        const index_local = try lowerer.freshHiddenLocal(.{ .kind = .integer, .name = "I64" });
                        const zero_reg = lowerer.freshRegister();
                        try instructions.append(.{ .const_int = .{ .dst = zero_reg, .value = 0 } });
                        try instructions.append(.{ .store_local = .{ .local = index_local, .src = zero_reg } });

                        const loop_label = lowerer.freshLabel();
                        const body_label = lowerer.freshLabel();
                        const end_label = lowerer.freshLabel();
                        try instructions.append(.{ .label = .{ .id = loop_label } });
                        const index_reg = lowerer.freshRegister();
                        try instructions.append(.{ .load_local = .{ .dst = index_reg, .local = index_local } });
                        const cmp_reg = lowerer.freshRegister();
                        try instructions.append(.{ .compare = .{ .dst = cmp_reg, .lhs = index_reg, .rhs = len_reg, .op = .less } });
                        try instructions.append(.{ .branch = .{ .condition = cmp_reg, .true_label = body_label, .false_label = end_label } });
                        try instructions.append(.{ .label = .{ .id = body_label } });
                        const item_reg = lowerer.freshRegister();
                        try instructions.append(.{ .array_get = .{ .dst = item_reg, .array = iterator_reg, .index = index_reg, .ty = binding_ty } });
                        try lowerer.storeValueToLocal(instructions, for_item.binding_local_id, binding_ty, item_reg);
                        try instructions.append(.{ .scope_enter = .{} });
                        try emitBuilderArrayItems(lowerer, instructions, array_reg, for_item.body);
                        try instructions.append(.{ .scope_exit = .{ .locals = &.{for_item.binding_local_id} } });
                        const one_reg = lowerer.freshRegister();
                        try instructions.append(.{ .const_int = .{ .dst = one_reg, .value = 1 } });
                        const next_reg = lowerer.freshRegister();
                        try instructions.append(.{ .add = .{ .dst = next_reg, .lhs = index_reg, .rhs = one_reg } });
                        try instructions.append(.{ .store_local = .{ .local = index_local, .src = next_reg } });
                        try instructions.append(.{ .jump = .{ .label = loop_label } });
                        try instructions.append(.{ .label = .{ .id = end_label } });
                    },
                }
            },
            .switch_item => |switch_item| {
                const subject_reg = try lowerer.lowerExpr(instructions, switch_item.subject);
                const subject_ty = try type_impl.lowerExecutableCompareOperandType(lowerer.program, model.hir.exprType(switch_item.subject.*), .equal);
                const end_label = lowerer.freshLabel();
                var used_end_label = false;
                for (switch_item.cases) |case_node| {
                    const pattern_reg = try lowerer.lowerExpr(instructions, case_node.pattern);
                    const pattern_ty = try type_impl.lowerExecutableCompareOperandType(lowerer.program, model.hir.exprType(case_node.pattern.*), .equal);
                    if (!type_impl.valueTypesEqual(subject_ty, pattern_ty)) return error.UnsupportedExecutableFeature;
                    const compare_reg = lowerer.freshRegister();
                    const case_label = lowerer.freshLabel();
                    const next_label = lowerer.freshLabel();
                    const normalized = try normalizeCompareOperands(lowerer, instructions, subject_ty, subject_reg, pattern_reg);
                    try instructions.append(.{ .compare = .{ .dst = compare_reg, .lhs = normalized.lhs, .rhs = normalized.rhs, .op = .equal } });
                    try instructions.append(.{ .branch = .{ .condition = compare_reg, .true_label = case_label, .false_label = next_label } });
                    try instructions.append(.{ .label = .{ .id = case_label } });
                    try emitBuilderArrayItems(lowerer, instructions, array_reg, case_node.body);
                    try instructions.append(.{ .jump = .{ .label = end_label } });
                    used_end_label = true;
                    try instructions.append(.{ .label = .{ .id = next_label } });
                }
                if (switch_item.default_block) |default_block| try emitBuilderArrayItems(lowerer, instructions, array_reg, default_block);
                if (used_end_label) try instructions.append(.{ .label = .{ .id = end_label } });
            },
        }
    }
}

const CompareOperands = struct {
    lhs: u32,
    rhs: u32,
};

fn normalizeCompareOperands(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    operand_vt: ir.ValueType,
    lhs: u32,
    rhs: u32,
) !CompareOperands {
    if (operand_vt.kind != .enum_instance) return .{ .lhs = lhs, .rhs = rhs };

    // Enum equality compares discriminant tags, not heap value identity. Without this,
    // `e == E.A` and builder-switch pattern matches compare boxed enum handles and never
    // agree for equal discriminants.
    const lhs_tag = lowerer.freshRegister();
    try instructions.append(.{ .enum_tag = .{ .dst = lhs_tag, .src = lhs } });
    const rhs_tag = lowerer.freshRegister();
    try instructions.append(.{ .enum_tag = .{ .dst = rhs_tag, .src = rhs } });
    return .{ .lhs = lhs_tag, .rhs = rhs_tag };
}

pub const Lowerer = struct {
    allocator: std.mem.Allocator,
    program: model.Program,
    state: *FunctionLoweringState,
    execution: runtime_abi.FunctionExecution,
    function_name: []const u8,
    next_register: u32,
    next_label: u32,
    next_local: u32,
    hidden_local_types: std.array_list.Managed(ir.ValueType),
    loop_stack: std.array_list.Managed(LoopLabels),
    boxed_locals: []const bool,
    local_remap: ?[]const u32 = null,

    const LoopLabels = struct {
        break_label: u32,
        continue_label: u32,
    };

    pub fn freshRegister(self: *Lowerer) u32 {
        const reg = self.next_register;
        self.next_register += 1;
        return reg;
    }

    pub fn freshLabel(self: *Lowerer) u32 {
        const label = self.next_label;
        self.next_label += 1;
        return label;
    }

    pub fn freshHiddenLocal(self: *Lowerer, ty: ir.ValueType) !u32 {
        const local = self.next_local;
        self.next_local += 1;
        try self.hidden_local_types.append(ty);
        return local;
    }

    pub fn mapLocal(self: *const Lowerer, local: u32) u32 {
        if (self.local_remap) |remap| return remapLocalId(remap, local);
        return local;
    }

    pub fn isBoxedLocal(self: *Lowerer, local: u32) bool {
        const mapped = self.mapLocal(local);
        return mapped < self.boxed_locals.len and self.boxed_locals[mapped];
    }

    pub fn isBoxedStorageLocal(self: *Lowerer, local: u32) bool {
        return local < self.boxed_locals.len and self.boxed_locals[local];
    }

    fn lowerCallbackExpr(self: *Lowerer, node: model.hir.CallbackExpr) !u32 {
        const function_id = self.state.next_generated_function_id;
        self.state.next_generated_function_id += 1;
        const function_name = try std.fmt.allocPrint(self.allocator, "{s}$callback_{d}", .{ self.function_name, function_id });
        try self.state.generated_functions.append(try lowerGeneratedCallbackFunction(
            self.allocator,
            self.program,
            self.state,
            function_id,
            function_name,
            self.execution,
            node,
        ));
        return function_id;
    }

    pub fn lowerStatements(self: *Lowerer, instructions: *std.array_list.Managed(ir.Instruction), statements: []const model.Statement) !bool {
        for (statements) |statement| {
            if (try self.lowerStatement(instructions, statement)) return true;
        }
        return false;
    }

    fn lowerStatement(self: *Lowerer, instructions: *std.array_list.Managed(ir.Instruction), statement: model.Statement) !bool {
        switch (statement) {
            .let_stmt => |node| {
                const local_id = self.mapLocal(node.local_id);
                if (node.is_reborrow) {
                    // Reborrow (`var r = t` over a borrow): bind the local as a
                    // non-owning alias of the source pointer. No box, no clone — both
                    // bindings reference the same storage, mutations are shared, and
                    // the alias is not freed at scope exit (the borrow's owner frees).
                    if (node.value) |value| {
                        const reg = try self.lowerExpr(instructions, value);
                        try instructions.append(.{ .store_local = .{ .local = local_id, .src = reg, .borrow = true } });
                    }
                    return false;
                }
                if (self.isBoxedLocal(node.local_id)) {
                    try self.initializeBoxedLocal(instructions, local_id, try lowerResolvedType(self.program, node.ty), null);
                }
                if (node.value) |value| {
                    const reg = try self.lowerExpr(instructions, value);
                    if (self.isBoxedLocal(node.local_id)) {
                        try self.storeValueToLocal(instructions, local_id, try lowerResolvedType(self.program, node.ty), reg);
                        return false;
                    }
                    if ((try lowerResolvedType(self.program, node.ty)).kind == .ffi_struct) {
                        const dst_ptr = self.freshRegister();
                        try instructions.append(.{ .load_local = .{ .dst = dst_ptr, .local = local_id } });
                        try instructions.append(.{ .copy_indirect = .{
                            .dst_ptr = dst_ptr,
                            .src_ptr = reg,
                            .type_name = node.ty.name orelse return error.UnsupportedExecutableFeature,
                        } });
                    } else {
                        try instructions.append(.{ .store_local = .{ .local = local_id, .src = reg } });
                    }
                }
                return false;
            },
            .assign_stmt => |node| {
                try lowerAssignmentStatement(self, instructions, self.program, node);
                return false;
            },
            .expr_stmt => |node| {
                try lowerExprStatement(self, instructions, node.expr);
                return false;
            },
            .if_stmt => |node| return statement_impl.lowerIfStatement(self, instructions, node),
            .for_stmt => |node| return statement_impl.lowerForStatement(self, instructions, node),
            .while_stmt => |node| return statement_impl.lowerWhileStatement(self, instructions, node),
            .break_stmt => {
                const labels = self.loop_stack.getLast();
                try instructions.append(.{ .jump = .{ .label = labels.break_label } });
                return true;
            },
            .continue_stmt => {
                const labels = self.loop_stack.getLast();
                try instructions.append(.{ .jump = .{ .label = labels.continue_label } });
                return true;
            },
            .match_stmt => |node| return statement_impl.lowerMatchStatement(self, instructions, node),
            .switch_stmt => |node| return statement_impl.lowerSwitchStatement(self, instructions, node),
            .return_stmt => |node| {
                const src = if (node.value) |value| try self.lowerReturnExpr(instructions, value) else null;
                try instructions.append(.{ .ret = .{ .src = src } });
                return true;
            },
        }
    }

    fn lowerReturnExpr(self: *Lowerer, instructions: *std.array_list.Managed(ir.Instruction), expr: *model.Expr) anyerror!u32 {
        if (expr.* == .local and !self.isBoxedLocal(expr.local.local_id)) {
            const dst = self.freshRegister();
            try instructions.append(.{ .load_local = .{
                .dst = dst,
                .local = self.mapLocal(expr.local.local_id),
                .ownership = .move,
            } });
            return dst;
        }
        if (expr.* == .field) {
            const field_ty = try lowerResolvedType(self.program, expr.field.ty);
            if (field_ty.kind == .ffi_struct) {
                const ptr = try self.lowerExpr(instructions, expr);
                const dst = self.freshRegister();
                try instructions.append(.{ .load_indirect = .{
                    .dst = dst,
                    .ptr = ptr,
                    .ty = field_ty,
                } });
                return dst;
            }
        }
        return self.lowerExpr(instructions, expr);
    }

    pub fn lowerExpr(self: *Lowerer, instructions: *std.array_list.Managed(ir.Instruction), expr: *model.Expr) anyerror!u32 {
        return switch (expr.*) {
            .integer => |node| blk: {
                const dst = self.freshRegister();
                try instructions.append(.{ .const_int = .{ .dst = dst, .value = node.value } });
                break :blk dst;
            },
            .float => |node| blk: {
                const dst = self.freshRegister();
                try instructions.append(.{ .const_float = .{ .dst = dst, .value = node.value } });
                break :blk dst;
            },
            .boolean => |node| blk: {
                const dst = self.freshRegister();
                try instructions.append(.{ .const_bool = .{ .dst = dst, .value = node.value } });
                break :blk dst;
            },
            .null_ptr => |node| blk: {
                _ = node;
                const dst = self.freshRegister();
                try instructions.append(.{ .const_null_ptr = .{ .dst = dst } });
                break :blk dst;
            },
            .function_ref => |node| blk: {
                const dst = self.freshRegister();
                try instructions.append(.{ .const_function = .{
                    .dst = dst,
                    .function_id = node.function_id,
                    .representation = switch (node.representation) {
                        .callable_value => .callable_value,
                        .native_callback => .native_callback,
                    },
                } });
                break :blk dst;
            },
            .callback => |node| blk: {
                const function_id = try self.lowerCallbackExpr(node);
                const dst = self.freshRegister();
                if (node.captures.len == 0) {
                    try instructions.append(.{ .const_function = .{
                        .dst = dst,
                        .function_id = function_id,
                        .representation = .callable_value,
                    } });
                } else {
                    var captures = std.array_list.Managed(u32).init(self.allocator);
                    defer captures.deinit();
                    for (node.captures) |capture| {
                        const reg = self.freshRegister();
                        const source_local = self.mapLocal(capture.source_local_id);
                        if (capture.by_ref) {
                            if (self.isBoxedLocal(capture.source_local_id)) {
                                try instructions.append(.{ .load_local = .{ .dst = reg, .local = source_local } });
                            } else {
                                try instructions.append(.{ .local_ptr = .{ .dst = reg, .local = source_local } });
                            }
                        } else {
                            try instructions.append(.{ .load_local = .{ .dst = reg, .local = source_local, .ownership = lowerOwnershipMode(capture.ownership) } });
                        }
                        try captures.append(reg);
                    }
                    try instructions.append(.{ .const_closure = .{
                        .dst = dst,
                        .function_id = function_id,
                        .captures = try captures.toOwnedSlice(),
                        .capture_ownership = try lowerCaptureOwnershipSlice(self.allocator, node.captures),
                    } });
                }
                break :blk dst;
            },
            .call_value => |node| blk: {
                const callee = try self.lowerExpr(instructions, node.callee);
                var args = std.array_list.Managed(u32).init(self.allocator);
                defer args.deinit();
                for (node.args) |arg| try args.append(try self.lowerExpr(instructions, arg));
                const dst = if (node.ty.kind == .void) null else self.freshRegister();
                try instructions.append(.{ .call_value = .{
                    .callee = callee,
                    .args = try args.toOwnedSlice(),
                    .param_types = try lowerResolvedTypeSlice(self.allocator, self.program, node.param_types),
                    .param_ownership = try lowerOwnershipModeSlice(self.allocator, node.param_ownership),
                    .return_type = try lowerResolvedType(self.program, node.ty),
                    .dst = dst,
                } });
                break :blk dst orelse return error.UnsupportedExecutableFeature;
            },
            .virtual_call => |node| blk: {
                const receiver = try self.lowerExpr(instructions, node.receiver);
                var args = std.array_list.Managed(u32).init(self.allocator);
                defer args.deinit();
                for (node.args) |arg| try args.append(try self.lowerExpr(instructions, arg));
                const dst = if (node.ty.kind == .void) null else self.freshRegister();
                try instructions.append(.{ .call_virtual = .{
                    .receiver = receiver,
                    .static_type_name = node.static_type_name,
                    .method_name = node.method_name,
                    .args = try args.toOwnedSlice(),
                    .return_ty = try lowerResolvedType(self.program, node.ty),
                    .dst = dst,
                } });
                break :blk dst orelse return error.UnsupportedExecutableFeature;
            },
            .namespace_ref => |node| blk: {
                if (try lowerNamespaceRefExpr(self, instructions, node.path)) |lowered| {
                    break :blk lowered;
                }
                return error.UnsupportedExecutableFeature;
            },
            .array => |node| blk: {
                const len_reg = self.freshRegister();
                try instructions.append(.{ .const_int = .{
                    .dst = len_reg,
                    .value = @as(i64, @intCast(node.elements.len)),
                } });
                const dst = self.freshRegister();
                try instructions.append(.{ .alloc_array = .{
                    .dst = dst,
                    .len = len_reg,
                    .ty = try lowerResolvedType(self.program, node.ty),
                } });
                for (node.elements, 0..) |element, index| {
                    const index_reg = self.freshRegister();
                    try instructions.append(.{ .const_int = .{
                        .dst = index_reg,
                        .value = @as(i64, @intCast(index)),
                    } });
                    const value_reg = try self.lowerExpr(instructions, element);
                    try instructions.append(.{ .array_set = .{
                        .array = dst,
                        .index = index_reg,
                        .src = value_reg,
                    } });
                }
                break :blk dst;
            },
            .builder_array => |node| try lowerBuilderArrayExpr(self, instructions, node),
            .native_state => |node| blk: {
                const type_name = node.ty.name orelse return error.UnsupportedExecutableFeature;
                const src = try self.lowerExpr(instructions, node.value);
                const dst = self.freshRegister();
                try instructions.append(.{ .alloc_native_state = .{
                    .dst = dst,
                    .src = src,
                    .type_name = type_name,
                    .type_id = nativeStateTypeId(type_name),
                } });
                break :blk dst;
            },
            .native_user_data => |node| blk: {
                break :blk try self.lowerExpr(instructions, node.state);
            },
            .native_recover => |node| blk: {
                const state = try self.lowerExpr(instructions, node.value);
                const type_name = node.ty.name orelse return error.UnsupportedExecutableFeature;
                const dst = self.freshRegister();
                try instructions.append(.{ .recover_native_state = .{
                    .dst = dst,
                    .state = state,
                    .type_name = type_name,
                    .type_id = nativeStateTypeId(type_name),
                } });
                break :blk dst;
            },
            .c_string_to_string => |node| blk: {
                const src = try self.lowerExpr(instructions, node.value);
                const dst = self.freshRegister();
                try instructions.append(.{ .c_string_to_string = .{
                    .dst = dst,
                    .src = src,
                } });
                break :blk dst;
            },
            .index => |node| blk: {
                const array_reg = try self.lowerExpr(instructions, node.object);
                const index_reg = try self.lowerExpr(instructions, node.index);
                const dst = self.freshRegister();
                try instructions.append(.{ .array_get = .{
                    .dst = dst,
                    .array = array_reg,
                    .index = index_reg,
                    .ty = try lowerResolvedType(self.program, node.ty),
                } });
                break :blk dst;
            },
            .unary => |node| blk: {
                const src = try self.lowerExpr(instructions, node.operand);
                const operand_ty = model.hir.exprType(node.operand.*);
                const dst = self.freshRegister();
                switch (node.op) {
                    .negate => {
                        _ = try lowerExecutableNumericType(self.program, operand_ty);
                        try instructions.append(.{ .unary = .{
                            .dst = dst,
                            .src = src,
                            .op = .negate,
                        } });
                    },
                    .not => {
                        _ = try lowerExecutableBooleanType(self.program, operand_ty);
                        try instructions.append(.{ .unary = .{
                            .dst = dst,
                            .src = src,
                            .op = .not,
                        } });
                    },
                }
                break :blk dst;
            },
            .string => |node| blk: {
                const dst = self.freshRegister();
                try instructions.append(.{ .const_string = .{ .dst = dst, .value = node.value } });
                break :blk dst;
            },
            .construct => |node| blk: {
                const type_decl = findTypeDeclByName(self.program, node.ty.name orelse return error.UnsupportedExecutableFeature) orelse return error.UnsupportedExecutableFeature;
                const dst = self.freshRegister();
                try instructions.append(.{ .alloc_struct = .{
                    .dst = dst,
                    .type_name = type_decl.name,
                } });

                var filled = try self.allocator.alloc(bool, type_decl.fields.len);
                defer self.allocator.free(filled);
                @memset(filled, false);
                var next_index: usize = 0;

                for (node.fields) |field_init| {
                    const field_index = try resolveConstructFieldIndex(type_decl, filled, &next_index, field_init);
                    if (field_index >= type_decl.fields.len) return error.UnsupportedExecutableFeature;
                    if (filled[field_index]) return error.UnsupportedExecutableFeature;

                    const field_decl = type_decl.fields[field_index];
                    const field_value = try self.lowerExpr(instructions, field_init.value);
                    const ptr_reg = self.freshRegister();
                    const field_ty = try lowerResolvedType(self.program, field_decl.ty);
                    try instructions.append(.{ .field_ptr = .{
                        .dst = ptr_reg,
                        .base = dst,
                        .base_type_name = type_decl.name,
                        .field_index = @as(u32, @intCast(field_index)),
                        .field_ty = field_ty,
                    } });
                    if (field_ty.kind == .ffi_struct) {
                        try instructions.append(.{ .copy_indirect = .{
                            .dst_ptr = ptr_reg,
                            .src_ptr = field_value,
                            .type_name = field_ty.name orelse return error.UnsupportedExecutableFeature,
                        } });
                    } else {
                        try instructions.append(.{ .store_indirect = .{
                            .ptr = ptr_reg,
                            .src = field_value,
                            .ty = field_ty,
                        } });
                    }
                    filled[field_index] = true;
                }

                switch (node.fill_mode) {
                    .defaults => {
                        for (type_decl.fields, 0..) |field_decl, index| {
                            if (filled[index]) continue;
                            if (fieldDeclIsTypeConstant(field_decl, type_decl.name)) continue;
                            const default_value = field_decl.default_value orelse return error.UnsupportedExecutableFeature;
                            const field_value = try self.lowerExpr(instructions, default_value);
                            const ptr_reg = self.freshRegister();
                            const field_ty = try lowerResolvedType(self.program, field_decl.ty);
                            try instructions.append(.{ .field_ptr = .{
                                .dst = ptr_reg,
                                .base = dst,
                                .base_type_name = type_decl.name,
                                .field_index = @as(u32, @intCast(index)),
                                .field_ty = field_ty,
                            } });
                            if (field_ty.kind == .ffi_struct) {
                                try instructions.append(.{ .copy_indirect = .{
                                    .dst_ptr = ptr_reg,
                                    .src_ptr = field_value,
                                    .type_name = field_ty.name orelse return error.UnsupportedExecutableFeature,
                                } });
                            } else {
                                try instructions.append(.{ .store_indirect = .{
                                    .ptr = ptr_reg,
                                    .src = field_value,
                                    .ty = field_ty,
                                } });
                            }
                        }
                    },
                    .zeroed_ffi_c_layout => {},
                }

                break :blk dst;
            },
            .construct_enum_variant => |node| blk: {
                const dst = self.freshRegister();
                try instructions.append(.{ .alloc_enum = .{
                    .dst = dst,
                    .enum_type_name = node.enum_name,
                    .discriminant = node.discriminant,
                    .payload_src = if (node.payload) |payload| try self.lowerExpr(instructions, payload) else null,
                } });
                break :blk dst;
            },
            .call => |node| blk: {
                if (node.function_id == null) return error.UnsupportedExecutableFeature;
                if (node.ty.kind == .void) return error.UnsupportedExecutableFeature;
                // Persist `borrow mut` array-element arguments after the call (see the
                // statement-call path); other arguments lower by value unchanged.
                var writebacks = places_impl.WritebackList.init(self.allocator);
                defer writebacks.deinit();
                const arg_regs = try places_impl.lowerDirectCallArgs(self, instructions, node.args, node.function_id.?, &writebacks);
                const dst = self.freshRegister();
                try instructions.append(.{ .call = .{
                    .callee = node.function_id.?,
                    .args = arg_regs,
                    .dst = dst,
                } });
                try places_impl.emitWritebacks(instructions, &writebacks);
                break :blk dst;
            },
            .local => |node| blk: {
                const dst = self.freshRegister();
                const local_id = self.mapLocal(node.local_id);
                if (self.isBoxedLocal(node.local_id)) {
                    const ptr = self.freshRegister();
                    try instructions.append(.{ .load_local = .{ .dst = ptr, .local = local_id } });
                    try instructions.append(.{ .load_indirect = .{ .dst = dst, .ptr = ptr, .ty = try lowerResolvedType(self.program, node.ty) } });
                } else {
                    try instructions.append(.{ .load_local = .{ .dst = dst, .local = local_id, .ownership = lowerOwnershipMode(node.ownership) } });
                }
                break :blk dst;
            },
            .parent_view => |node| blk: {
                const base = try self.lowerExpr(instructions, node.object);
                if (node.offset == 0) break :blk base;
                const dst = self.freshRegister();
                try instructions.append(.{ .subobject_ptr = .{
                    .dst = dst,
                    .base = base,
                    .offset = node.offset,
                } });
                break :blk dst;
            },
            .array_len => |node| blk: {
                const array_reg = try self.lowerExpr(instructions, node.object);
                const dst = self.freshRegister();
                try instructions.append(.{ .array_len = .{
                    .dst = dst,
                    .array = array_reg,
                } });
                break :blk dst;
            },
            .string_len => |node| blk: {
                const string_reg = try self.lowerExpr(instructions, node.object);
                const dst = self.freshRegister();
                try instructions.append(.{ .string_len = .{
                    .dst = dst,
                    .string = string_reg,
                } });
                break :blk dst;
            },
            .field => |node| blk: {
                const object_reg = try self.lowerExpr(instructions, node.object);
                const field_ty = try lowerResolvedType(self.program, node.ty);
                if (model.hir.exprType(node.object.*).kind == .native_state_view) {
                    const dst = self.freshRegister();
                    try instructions.append(.{ .native_state_field_get = .{
                        .dst = dst,
                        .state = object_reg,
                        .field_index = node.field_index,
                        .field_ty = field_ty,
                    } });
                    break :blk dst;
                }
                const field_ptr = self.freshRegister();
                try instructions.append(.{ .field_ptr = .{
                    .dst = field_ptr,
                    .base = object_reg,
                    .base_type_name = node.container_type_name,
                    .field_index = node.field_index,
                    .field_ty = field_ty,
                } });
                if (field_ty.kind == .ffi_struct) break :blk field_ptr;
                const dst = self.freshRegister();
                try instructions.append(.{ .load_indirect = .{
                    .dst = dst,
                    .ptr = field_ptr,
                    .ty = field_ty,
                } });
                break :blk dst;
            },
            .binary => |node| blk: {
                const lhs = try self.lowerExpr(instructions, node.lhs);
                switch (node.op) {
                    .logical_and, .logical_or => break :blk try self.lowerLogicalBinaryExpr(instructions, node, lhs),
                    else => {},
                }

                const rhs = try self.lowerExpr(instructions, node.rhs);
                const dst = self.freshRegister();
                switch (node.op) {
                    .add => {
                        _ = try lowerExecutableNumericType(self.program, model.hir.exprType(node.lhs.*));
                        try instructions.append(.{ .add = .{ .dst = dst, .lhs = lhs, .rhs = rhs } });
                    },
                    .subtract => {
                        _ = try lowerExecutableNumericType(self.program, model.hir.exprType(node.lhs.*));
                        try instructions.append(.{ .subtract = .{ .dst = dst, .lhs = lhs, .rhs = rhs } });
                    },
                    .multiply => {
                        _ = try lowerExecutableNumericType(self.program, model.hir.exprType(node.lhs.*));
                        try instructions.append(.{ .multiply = .{ .dst = dst, .lhs = lhs, .rhs = rhs } });
                    },
                    .divide => {
                        _ = try lowerExecutableNumericType(self.program, model.hir.exprType(node.lhs.*));
                        try instructions.append(.{ .divide = .{ .dst = dst, .lhs = lhs, .rhs = rhs } });
                    },
                    .modulo => {
                        _ = try lowerExecutableNumericType(self.program, model.hir.exprType(node.lhs.*));
                        try instructions.append(.{ .modulo = .{ .dst = dst, .lhs = lhs, .rhs = rhs } });
                    },
                    .equal, .not_equal, .less, .less_equal, .greater, .greater_equal => {
                        const operand_vt = try lowerExecutableCompareOperandType(self.program, model.hir.exprType(node.lhs.*), node.op);
                        const normalized = try normalizeCompareOperands(self, instructions, operand_vt, lhs, rhs);
                        try instructions.append(.{ .compare = .{
                            .dst = dst,
                            .lhs = normalized.lhs,
                            .rhs = normalized.rhs,
                            .op = lowerCompareOp(node.op),
                        } });
                    },
                    .logical_and, .logical_or => unreachable,
                }
                break :blk dst;
            },
            .cast => |node| blk: {
                const src = try self.lowerExpr(instructions, node.operand);
                const target_vt = try lowerResolvedType(self.program, node.ty);
                const dst = self.freshRegister();
                try instructions.append(.{ .convert = .{ .dst = dst, .src = src, .target = target_vt.kind } });
                break :blk dst;
            },
            .conditional => |node| try self.lowerConditionalExpr(instructions, node),
        };
    }

    fn lowerLogicalBinaryExpr(
        self: *Lowerer,
        instructions: *std.array_list.Managed(ir.Instruction),
        node: model.hir.BinaryExpr,
        lhs: u32,
    ) anyerror!u32 {
        _ = try lowerExecutableBooleanType(self.program, model.hir.exprType(node.lhs.*));
        _ = try lowerExecutableBooleanType(self.program, model.hir.exprType(node.rhs.*));

        const result_local = try self.freshHiddenLocal(.{ .kind = .boolean });
        const rhs_label = self.freshLabel();
        const short_label = self.freshLabel();
        const end_label = self.freshLabel();

        try instructions.append(.{ .branch = .{
            .condition = lhs,
            .true_label = if (node.op == .logical_and) rhs_label else short_label,
            .false_label = if (node.op == .logical_and) short_label else rhs_label,
        } });

        try instructions.append(.{ .label = .{ .id = short_label } });
        const short_value = self.freshRegister();
        try instructions.append(.{ .const_bool = .{
            .dst = short_value,
            .value = node.op == .logical_or,
        } });
        try instructions.append(.{ .store_local = .{ .local = result_local, .src = short_value } });
        try instructions.append(.{ .jump = .{ .label = end_label } });

        try instructions.append(.{ .label = .{ .id = rhs_label } });
        const rhs = try self.lowerExpr(instructions, node.rhs);
        try instructions.append(.{ .store_local = .{ .local = result_local, .src = rhs } });
        try instructions.append(.{ .jump = .{ .label = end_label } });

        try instructions.append(.{ .label = .{ .id = end_label } });
        const dst = self.freshRegister();
        try instructions.append(.{ .load_local = .{ .dst = dst, .local = result_local } });
        return dst;
    }

    fn lowerConditionalExpr(
        self: *Lowerer,
        instructions: *std.array_list.Managed(ir.Instruction),
        node: model.hir.ConditionalExpr,
    ) anyerror!u32 {
        _ = try lowerExecutableBooleanType(self.program, model.hir.exprType(node.condition.*));
        const result_ty = try lowerResolvedType(self.program, node.ty);
        const result_local = try self.freshHiddenLocal(result_ty);

        const condition_reg = try self.lowerExpr(instructions, node.condition);
        const then_label = self.freshLabel();
        const else_label = self.freshLabel();
        const end_label = self.freshLabel();

        try instructions.append(.{ .branch = .{
            .condition = condition_reg,
            .true_label = then_label,
            .false_label = else_label,
        } });

        try instructions.append(.{ .label = .{ .id = then_label } });
        const then_value = try self.lowerExpr(instructions, node.then_expr);
        try self.storeValueToLocal(instructions, result_local, result_ty, then_value);
        try instructions.append(.{ .jump = .{ .label = end_label } });

        try instructions.append(.{ .label = .{ .id = else_label } });
        const else_value = try self.lowerExpr(instructions, node.else_expr);
        try self.storeValueToLocal(instructions, result_local, result_ty, else_value);
        try instructions.append(.{ .jump = .{ .label = end_label } });

        try instructions.append(.{ .label = .{ .id = end_label } });
        const dst = self.freshRegister();
        try instructions.append(.{ .load_local = .{ .dst = dst, .local = result_local } });
        return dst;
    }

    pub fn storeValueToLocal(
        self: *Lowerer,
        instructions: *std.array_list.Managed(ir.Instruction),
        local: u32,
        ty: ir.ValueType,
        src: u32,
    ) !void {
        if (self.isBoxedStorageLocal(local)) {
            const ptr = self.freshRegister();
            try instructions.append(.{ .load_local = .{ .dst = ptr, .local = local } });
            try instructions.append(.{ .store_indirect = .{ .ptr = ptr, .src = src, .ty = ty } });
            return;
        }
        if (ty.kind == .ffi_struct) {
            const dst_ptr = self.freshRegister();
            try instructions.append(.{ .load_local = .{ .dst = dst_ptr, .local = local } });
            try instructions.append(.{ .copy_indirect = .{
                .dst_ptr = dst_ptr,
                .src_ptr = src,
                .type_name = ty.name orelse return error.UnsupportedExecutableFeature,
            } });
            return;
        }
        try instructions.append(.{ .store_local = .{ .local = local, .src = src } });
    }

    fn initializeBoxedLocal(
        self: *Lowerer,
        instructions: *std.array_list.Managed(ir.Instruction),
        local: u32,
        ty: ir.ValueType,
        initial_value: ?u32,
    ) !void {
        const cell_local = try self.freshHiddenLocal(ty);
        const ptr = self.freshRegister();
        try instructions.append(.{ .local_ptr = .{ .dst = ptr, .local = cell_local } });
        try instructions.append(.{ .store_local = .{ .local = local, .src = ptr } });
        if (initial_value) |src| {
            try instructions.append(.{ .store_indirect = .{ .ptr = ptr, .src = src, .ty = ty } });
        }
    }
};

fn lowerCompareOp(op: model.hir.BinaryOp) ir.CompareOp {
    return switch (op) {
        .equal => .equal,
        .not_equal => .not_equal,
        .less => .less,
        .less_equal => .less_equal,
        .greater => .greater,
        .greater_equal => .greater_equal,
        else => unreachable,
    };
}

fn cloneLoweredFunctionBatch(allocator: std.mem.Allocator, batch: LoweredFunctionBatch) !LoweredFunctionBatch {
    const generated_functions = try allocator.alloc(ir.Function, batch.generated_functions.len);
    for (batch.generated_functions, 0..) |function_decl, index| {
        generated_functions[index] = try cloneFunction(allocator, function_decl);
    }
    return .{
        .primary = try cloneFunction(allocator, batch.primary),
        .generated_functions = generated_functions,
    };
}

fn cloneFunction(allocator: std.mem.Allocator, function_decl: ir.Function) !ir.Function {
    return .{
        .id = function_decl.id,
        .name = try allocator.dupe(u8, function_decl.name),
        .execution = function_decl.execution,
        .is_extern = function_decl.is_extern,
        .foreign = if (function_decl.foreign) |foreign| .{
            .library_name = try allocator.dupe(u8, foreign.library_name),
            .symbol_name = try allocator.dupe(u8, foreign.symbol_name),
            .calling_convention = foreign.calling_convention,
        } else null,
        .param_types = try cloneValueTypeSlice(allocator, function_decl.param_types),
        .param_ownership = try cloneOwnershipModeSlice(allocator, function_decl.param_ownership),
        .return_type = function_decl.return_type,
        .return_ownership = function_decl.return_ownership,
        .register_count = function_decl.register_count,
        .local_count = function_decl.local_count,
        .local_types = try cloneValueTypeSlice(allocator, function_decl.local_types),
        .instructions = try cloneInstructionSlice(allocator, function_decl.instructions),
    };
}

fn cloneValueTypeSlice(allocator: std.mem.Allocator, items: []const ir.ValueType) ![]const ir.ValueType {
    if (items.len == 0) return &.{};
    const cloned = try allocator.alloc(ir.ValueType, items.len);
    @memcpy(cloned, items);
    return cloned;
}

fn cloneOwnershipModeSlice(allocator: std.mem.Allocator, items: []const ir.OwnershipMode) ![]const ir.OwnershipMode {
    if (items.len == 0) return &.{};
    const cloned = try allocator.alloc(ir.OwnershipMode, items.len);
    @memcpy(cloned, items);
    return cloned;
}

fn cloneInstructionSlice(allocator: std.mem.Allocator, items: []const ir.Instruction) ![]ir.Instruction {
    if (items.len == 0) return &.{};
    const cloned = try allocator.alloc(ir.Instruction, items.len);
    for (items, 0..) |instruction, index| {
        cloned[index] = try cloneInstruction(allocator, instruction);
    }
    return cloned;
}

fn cloneInstruction(allocator: std.mem.Allocator, instruction: ir.Instruction) !ir.Instruction {
    return switch (instruction) {
        .const_closure => |value| .{ .const_closure = .{
            .dst = value.dst,
            .function_id = value.function_id,
            .captures = try cloneU32Slice(allocator, value.captures),
            .capture_ownership = try cloneOwnershipModeSlice(allocator, value.capture_ownership),
        } },
        .call => |value| .{ .call = .{
            .callee = value.callee,
            .args = try cloneU32Slice(allocator, value.args),
            .dst = value.dst,
        } },
        .call_virtual => |value| .{ .call_virtual = .{
            .receiver = value.receiver,
            .static_type_name = try allocator.dupe(u8, value.static_type_name),
            .method_name = try allocator.dupe(u8, value.method_name),
            .args = try cloneU32Slice(allocator, value.args),
            .return_ty = value.return_ty,
            .dst = value.dst,
        } },
        .call_value => |value| .{ .call_value = .{
            .callee = value.callee,
            .args = try cloneU32Slice(allocator, value.args),
            .param_types = try cloneValueTypeSlice(allocator, value.param_types),
            .param_ownership = try cloneOwnershipModeSlice(allocator, value.param_ownership),
            .return_type = value.return_type,
            .dst = value.dst,
        } },
        .scope_exit => |value| .{ .scope_exit = .{
            .locals = try cloneU32Slice(allocator, value.locals),
        } },
        else => instruction,
    };
}

fn cloneU32Slice(allocator: std.mem.Allocator, items: []const u32) ![]const u32 {
    if (items.len == 0) return &.{};
    const cloned = try allocator.alloc(u32, items.len);
    @memcpy(cloned, items);
    return cloned;
}

test "lowers sparse FFI construction by zero-filling omitted fields" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    const field_value = try allocator.create(model.Expr);
    field_value.* = .{ .integer = .{
        .value = 7,
        .span = .{ .start = 0, .end = 0 },
    } };

    const construct_value = try allocator.create(model.Expr);
    construct_value.* = .{ .construct = .{
        .type_name = "Example",
        .fields = &.{.{
            .field_name = "b",
            .field_index = 1,
            .value = field_value,
            .span = .{ .start = 0, .end = 0 },
        }},
        .fill_mode = .zeroed_ffi_c_layout,
        .ty = .{ .kind = .named, .name = "Example" },
        .span = .{ .start = 0, .end = 0 },
    } };

    const program = model.Program{
        .imports = &.{},
        .annotations = &.{},
        .capabilities = &.{},
        .constructs = &.{},
        .types = &.{.{
            .name = "Example",
            .fields = &.{
                .{ .name = "a", .owner_type_name = "Example", .storage = .mutable, .slot_index = 0, .ty = .{ .kind = .integer, .name = "U8" }, .explicit_type = true, .default_value = null, .annotations = &.{}, .span = .{ .start = 0, .end = 0 } },
                .{ .name = "b", .owner_type_name = "Example", .storage = .mutable, .slot_index = 1, .ty = .{ .kind = .integer, .name = "U8" }, .explicit_type = true, .default_value = null, .annotations = &.{}, .span = .{ .start = 0, .end = 0 } },
                .{ .name = "c", .owner_type_name = "Example", .storage = .mutable, .slot_index = 2, .ty = .{ .kind = .integer, .name = "U8" }, .explicit_type = true, .default_value = null, .annotations = &.{}, .span = .{ .start = 0, .end = 0 } },
            },
            .ffi = .{ .ffi_struct = .{ .layout = "c", .span = .{ .start = 0, .end = 0 } } },
            .span = .{ .start = 0, .end = 0 },
        }},
        .forms = &.{},
        .functions = &.{.{
            .id = 0,
            .name = "entry",
            .is_main = true,
            .execution = .runtime,
            .is_extern = false,
            .foreign = null,
            .annotations = &.{},
            .params = &.{},
            .locals = &.{.{ .id = 0, .name = "value", .ty = .{ .kind = .named, .name = "Example" }, .span = .{ .start = 0, .end = 0 } }},
            .return_type = .{ .kind = .void },
            .body = &.{
                .{ .let_stmt = .{ .local_id = 0, .ty = .{ .kind = .named, .name = "Example" }, .explicit_type = false, .value = construct_value, .span = .{ .start = 0, .end = 0 } } },
                .{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } },
            },
            .span = .{ .start = 0, .end = 0 },
        }},
        .entry_index = 0,
    };

    const lowered = try lowerProgram(allocator, program);
    const instructions = lowered.functions[0].instructions;

    var saw_field_b = false;
    var touched_other_field = false;
    for (instructions) |instruction| {
        if (instruction != .field_ptr) continue;
        if (instruction.field_ptr.field_index == 1) {
            saw_field_b = true;
        } else {
            touched_other_field = true;
        }
    }

    try std.testing.expect(saw_field_b);
    try std.testing.expect(!touched_other_field);
}

test "parallel root-function lowering preserves deterministic callback ids" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const callback_ty: model.ResolvedType = .{ .kind = .callback, .name = "Callback" };

    const callback_body = &.{model.Statement{
        .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } },
    }};

    const callback_exprs = try allocator.alloc(*model.Expr, 4);
    for (callback_exprs) |*slot| {
        const expr = try allocator.create(model.Expr);
        expr.* = .{ .callback = .{
            .params = &.{},
            .captures = &.{},
            .locals = &.{},
            .body = callback_body,
            .return_type = .{ .kind = .void },
            .ty = callback_ty,
            .span = .{ .start = 0, .end = 0 },
        } };
        slot.* = expr;
    }

    const call_exprs = try allocator.alloc(*model.Expr, 3);
    for (call_exprs, 0..) |*slot, index| {
        const expr = try allocator.create(model.Expr);
        expr.* = .{ .call = .{
            .callee_name = switch (index) {
                0 => "helper_one",
                1 => "helper_two",
                else => "helper_three",
            },
            .function_id = @as(u32, @intCast(index + 1)),
            .args = &.{},
            .ty = .{ .kind = .void },
            .span = .{ .start = 0, .end = 0 },
        } };
        slot.* = expr;
    }

    const callback_local: model.LocalSymbol = .{
        .id = 0,
        .name = "cb",
        .ty = callback_ty,
        .span = .{ .start = 0, .end = 0 },
    };

    const entry_body = &.{
        model.Statement{ .let_stmt = .{
            .local_id = 0,
            .ty = callback_ty,
            .explicit_type = false,
            .value = callback_exprs[0],
            .span = .{ .start = 0, .end = 0 },
        } },
        model.Statement{ .expr_stmt = .{ .expr = call_exprs[0], .span = .{ .start = 0, .end = 0 } } },
        model.Statement{ .expr_stmt = .{ .expr = call_exprs[1], .span = .{ .start = 0, .end = 0 } } },
        model.Statement{ .expr_stmt = .{ .expr = call_exprs[2], .span = .{ .start = 0, .end = 0 } } },
        model.Statement{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } },
    };

    const helper_one_body = &.{
        model.Statement{ .let_stmt = .{
            .local_id = 0,
            .ty = callback_ty,
            .explicit_type = false,
            .value = callback_exprs[1],
            .span = .{ .start = 0, .end = 0 },
        } },
        model.Statement{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } },
    };
    const helper_two_body = &.{
        model.Statement{ .let_stmt = .{
            .local_id = 0,
            .ty = callback_ty,
            .explicit_type = false,
            .value = callback_exprs[2],
            .span = .{ .start = 0, .end = 0 },
        } },
        model.Statement{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } },
    };
    const helper_three_body = &.{
        model.Statement{ .let_stmt = .{
            .local_id = 0,
            .ty = callback_ty,
            .explicit_type = false,
            .value = callback_exprs[3],
            .span = .{ .start = 0, .end = 0 },
        } },
        model.Statement{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } },
    };

    const program = model.Program{
        .imports = &.{},
        .annotations = &.{},
        .capabilities = &.{},
        .constructs = &.{},
        .types = &.{},
        .forms = &.{},
        .functions = &.{
            .{
                .id = 0,
                .name = "entry",
                .is_main = true,
                .execution = .runtime,
                .annotations = &.{},
                .params = &.{},
                .locals = &.{callback_local},
                .return_type = .{ .kind = .void },
                .body = entry_body,
                .span = .{ .start = 0, .end = 0 },
            },
            .{
                .id = 1,
                .name = "helper_one",
                .is_main = false,
                .execution = .runtime,
                .annotations = &.{},
                .params = &.{},
                .locals = &.{callback_local},
                .return_type = .{ .kind = .void },
                .body = helper_one_body,
                .span = .{ .start = 0, .end = 0 },
            },
            .{
                .id = 2,
                .name = "helper_two",
                .is_main = false,
                .execution = .runtime,
                .annotations = &.{},
                .params = &.{},
                .locals = &.{callback_local},
                .return_type = .{ .kind = .void },
                .body = helper_two_body,
                .span = .{ .start = 0, .end = 0 },
            },
            .{
                .id = 3,
                .name = "helper_three",
                .is_main = false,
                .execution = .runtime,
                .annotations = &.{},
                .params = &.{},
                .locals = &.{callback_local},
                .return_type = .{ .kind = .void },
                .body = helper_three_body,
                .span = .{ .start = 0, .end = 0 },
            },
        },
        .entry_index = 0,
    };

    const lowered = try lowerProgramWithOptions(allocator, program, .{ .worker_count_override = 2 });
    try std.testing.expectEqual(@as(usize, 8), lowered.functions.len);
    try std.testing.expectEqual(@as(usize, 0), lowered.entry_index);

    for (lowered.functions[0..4], 0..) |function_decl, index| {
        try std.testing.expectEqual(@as(u32, @intCast(index)), function_decl.id);
    }
    for (lowered.functions[4..], 0..) |function_decl, index| {
        try std.testing.expectEqual(@as(u32, @intCast(index + 4)), function_decl.id);
    }
}
