//! Native-bridge unit tests for the VM: closure/struct materialization
//! across the native boundary, hybrid cleanup stress, and native-state
//! preservation/recovery.

const std = @import("std");
const bytecode = @import("kira_bytecode");
const runtime_abi = @import("kira_runtime_abi");
const vm_mod = @import("vm.zig");
const Vm = vm_mod.Vm;
const NativeStateBox = vm_mod.NativeStateBox;
const native_layout = @import("native_layout.zig");
const ArrayObject = @import("ownership.zig").ArrayObject;
const construct_any_test = @import("vm_construct_any_test_helpers.zig");

test "materializes native closure struct captures using external metadata" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const backend_fields = [_]bytecode.Field{
        .{ .name = "id", .ty = .{ .kind = .integer, .name = "U32" } },
    };
    const pipeline_fields = [_]bytecode.Field{
        .{ .name = "handle", .ty = .{ .kind = .ffi_struct, .name = "BackendPipelineHandle" } },
        .{ .name = "id", .ty = .{ .kind = .integer, .name = "U32" } },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "BackendPipelineHandle", .fields = @constCast(&backend_fields) },
        .{ .name = "RenderPipeline", .fields = @constCast(&pipeline_fields) },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = &.{},
        .entry_function_id = null,
    };

    const native_pipeline = try arena.allocator().alloc(u8, 8);
    @memset(native_pipeline, 0);
    (@as(*u32, @ptrFromInt(@intFromPtr(native_pipeline.ptr)))).* = 65537;
    (@as(*u32, @ptrFromInt(@intFromPtr(native_pipeline.ptr) + 4))).* = 65537;

    const NativeClosure = extern struct {
        function_id: i64,
        capture_count: i64,
        captures: [1]runtime_abi.BridgeValue,
    };
    const native_closure = try arena.allocator().create(NativeClosure);
    native_closure.* = .{
        .function_id = 457,
        .capture_count = 1,
        .captures = .{runtime_abi.bridgeValueFromValue(.{ .raw_ptr = @intFromPtr(native_pipeline.ptr) })},
    };
    const capture_types = [_]bytecode.TypeRef{
        .{ .kind = .ffi_struct, .name = "RenderPipeline" },
    };

    const closure_ptr = try vm.materializeNativeClosure(&module, @intFromPtr(native_closure), &capture_types);
    const closure = vm.heap.getClosure(closure_ptr) orelse return error.TestExpectedEqual;
    try std.testing.expectEqual(@as(usize, 1), closure.captures.len);
    try std.testing.expect(closure.captures[0] == .raw_ptr);

    const pipeline_values: [*]const runtime_abi.Value = @ptrFromInt(closure.captures[0].raw_ptr);
    try std.testing.expect(pipeline_values[0] == .raw_ptr);
    try std.testing.expect(pipeline_values[1] == .integer);
    try std.testing.expectEqual(@as(i64, 65537), pipeline_values[1].integer);

    const handle_values: [*]const runtime_abi.Value = @ptrFromInt(pipeline_values[0].raw_ptr);
    try std.testing.expect(handle_values[0] == .integer);
    try std.testing.expectEqual(@as(i64, 65537), handle_values[0].integer);

    try std.testing.expectEqual(@as(usize, 2), vm.heap.stats.structs_current);
    vm.dropManagedValue(.{ .raw_ptr = closure_ptr });
    try std.testing.expectEqual(@as(usize, 0), vm.heap.stats.structs_current);
}

test "materializes tagged native closure captures as runtime closures" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const callback_capture_ty = bytecode.TypeRef{ .kind = .raw_ptr, .name = "(borrow FoundationUiContext) -> FoundationView" };
    const outer_local_types = [_]bytecode.TypeRef{
        .{ .kind = .raw_ptr, .name = "GraphicsFrame" },
        callback_capture_ty,
    };
    const inner_local_types = [_]bytecode.TypeRef{
        .{ .kind = .raw_ptr, .name = "FoundationUiContext" },
    };
    const functions = [_]bytecode.Function{
        .{
            .id = 100,
            .name = "innerBuilder",
            .param_count = 1,
            .register_count = 0,
            .local_count = 1,
            .local_types = @constCast(&inner_local_types),
            .instructions = &.{},
        },
        .{
            .id = 200,
            .name = "outerFrame",
            .param_count = 2,
            .register_count = 0,
            .local_count = 2,
            .local_types = @constCast(&outer_local_types),
            .instructions = &.{},
        },
    };
    const module = bytecode.Module{
        .functions = @constCast(&functions),
        .entry_function_id = null,
    };

    const NativeInnerClosure = extern struct {
        function_id: i64,
        capture_count: i64,
    };
    const inner = try arena.allocator().create(NativeInnerClosure);
    inner.* = .{
        .function_id = 100,
        .capture_count = 0,
    };

    const NativeOuterClosure = extern struct {
        function_id: i64,
        capture_count: i64,
        captures: [1]runtime_abi.BridgeValue,
    };
    const outer = try arena.allocator().create(NativeOuterClosure);
    outer.* = .{
        .function_id = 200,
        .capture_count = 1,
        .captures = .{runtime_abi.bridgeValueFromValue(.{ .raw_ptr = runtime_abi.tagNativeClosurePointer(@intFromPtr(inner)) })},
    };

    const outer_ptr = try vm.materializeNativeClosure(&module, runtime_abi.tagNativeClosurePointer(@intFromPtr(outer)), null);
    const outer_closure = vm.heap.getClosure(outer_ptr) orelse return error.TestExpectedEqual;
    try std.testing.expectEqual(@as(usize, 1), outer_closure.captures.len);
    try std.testing.expect(outer_closure.captures[0] == .raw_ptr);

    const inner_ptr = outer_closure.captures[0].raw_ptr;
    const inner_closure = vm.heap.getClosure(inner_ptr) orelse return error.TestExpectedEqual;
    try std.testing.expectEqual(@as(u32, 100), inner_closure.function_id);
    try std.testing.expectEqual(@as(usize, 0), inner_closure.captures.len);

    vm.dropManagedValue(.{ .raw_ptr = outer_ptr });
}

test "materializes callback fields when copying native structs to runtime" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const callback_ty = bytecode.TypeRef{ .kind = .raw_ptr, .name = "(borrow mut GraphicsFrame) -> Void" };
    const frame_handler_locals = [_]bytecode.TypeRef{
        .{ .kind = .raw_ptr, .name = "GraphicsFrame" },
    };
    const fields = [_]bytecode.Field{
        .{ .name = "frameHandler", .ty = callback_ty },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "GraphicsApplication", .fields = @constCast(&fields) },
    };
    const functions = [_]bytecode.Function{
        .{
            .id = 300,
            .name = "frameHandler",
            .param_count = 1,
            .register_count = 0,
            .local_count = 1,
            .local_types = @constCast(&frame_handler_locals),
            .instructions = &.{},
        },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = @constCast(&functions),
        .entry_function_id = null,
    };

    const NativeClosure = extern struct {
        function_id: i64,
        capture_count: i64,
    };
    const native_closure = try arena.allocator().create(NativeClosure);
    native_closure.* = .{
        .function_id = 300,
        .capture_count = 0,
    };

    const layout = try native_layout.structLayout(&module, "GraphicsApplication");
    const word_count = @max(1, std.math.divCeil(usize, layout.size, @sizeOf(u64)) catch unreachable);
    const native_words = try arena.allocator().alloc(u64, word_count);
    @memset(native_words, 0);
    const native_app_ptr = @intFromPtr(native_words.ptr);
    const handler_offset = try native_layout.fieldOffset(&module, "GraphicsApplication", 0);
    (@as(*usize, @ptrFromInt(native_app_ptr + handler_offset))).* = runtime_abi.tagNativeClosurePointer(@intFromPtr(native_closure));

    const runtime_app_ptr = try vm.copyStructFromNativeLayout(&module, "GraphicsApplication", native_app_ptr);
    const runtime_fields: [*]const runtime_abi.Value = @ptrFromInt(runtime_app_ptr);
    try std.testing.expect(runtime_fields[0] == .raw_ptr);
    try std.testing.expect(!runtime_abi.isTaggedNativeClosurePointer(runtime_fields[0].raw_ptr));
    const closure = vm.heap.getClosure(runtime_fields[0].raw_ptr) orelse return error.TestExpectedEqual;
    try std.testing.expectEqual(@as(u32, 300), closure.function_id);
    try std.testing.expectEqual(@as(usize, 0), closure.captures.len);

    vm.dropManagedValue(.{ .raw_ptr = runtime_app_ptr });
}

test "hybrid_native_bridge_materialization_cleanup" {
    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const point_fields = [_]bytecode.Field{
        .{ .name = "x", .ty = .{ .kind = .integer, .name = "I64" } },
        .{ .name = "y", .ty = .{ .kind = .integer, .name = "I64" } },
    };
    const batch_fields = [_]bytecode.Field{
        .{ .name = "points", .ty = .{ .kind = .array, .name = "Point" } },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "Point", .fields = @constCast(&point_fields) },
        .{ .name = "Batch", .fields = @constCast(&batch_fields) },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = &.{},
        .entry_function_id = null,
    };

    const point_ptr = try vm.allocateStruct(&module, "Point");
    const point_fields_ptr: [*]runtime_abi.Value = @ptrFromInt(point_ptr);
    point_fields_ptr[0] = .{ .integer = 11 };
    point_fields_ptr[1] = .{ .integer = 22 };

    const array_ptr = try vm.allocateArray(0);
    const array_object: *ArrayObject = @ptrFromInt(array_ptr);
    try vm.heap.appendArrayItem(array_object, .{ .raw_ptr = point_ptr });

    const native_array_ptr = try vm.copyArrayToNativeLayout(&module, .{ .kind = .array, .name = "Point" }, array_ptr);
    vm.destroyArrayNativeLayout(&module, .{ .kind = .array, .name = "Point" }, native_array_ptr);

    const batch_ptr = try vm.allocateStruct(&module, "Batch");
    const batch_fields_ptr: [*]runtime_abi.Value = @ptrFromInt(batch_ptr);
    batch_fields_ptr[0] = .{ .raw_ptr = array_ptr };

    const native_batch_ptr = try vm.lowerStructToNativeLayout(&module, "Batch", batch_ptr);
    vm.destroyStructNativeLayout(&module, "Batch", native_batch_ptr);

    vm.dropManagedValue(.{ .raw_ptr = batch_ptr });
    try std.testing.expectEqual(@as(usize, 0), vm.heap.count());
}

test "hybrid_native_bridge_repeated_materialization_cleanup" {
    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const point_fields = [_]bytecode.Field{
        .{ .name = "x", .ty = .{ .kind = .integer, .name = "I64" } },
        .{ .name = "y", .ty = .{ .kind = .integer, .name = "I64" } },
    };
    const batch_fields = [_]bytecode.Field{
        .{ .name = "points", .ty = .{ .kind = .array, .name = "Point" } },
        .{ .name = "weights", .ty = .{ .kind = .array, .name = "I64" } },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "Point", .fields = @constCast(&point_fields) },
        .{ .name = "Batch", .fields = @constCast(&batch_fields) },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = &.{},
        .entry_function_id = null,
    };

    for (0..200) |iteration| {
        const points_ptr = try vm.allocateArray(0);
        const points: *ArrayObject = @ptrFromInt(points_ptr);
        const weights_ptr = try vm.allocateArray(0);
        const weights: *ArrayObject = @ptrFromInt(weights_ptr);

        for (0..12) |index| {
            const point_ptr = try vm.allocateStruct(&module, "Point");
            const fields: [*]runtime_abi.Value = @ptrFromInt(point_ptr);
            fields[0] = .{ .integer = @intCast(iteration + index) };
            fields[1] = .{ .integer = @intCast((iteration + 1) * (index + 1)) };
            try vm.heap.appendArrayItem(points, .{ .raw_ptr = point_ptr });
            try vm.heap.appendArrayItem(weights, .{ .integer = @intCast(index) });
        }

        const native_points = try vm.copyArrayToNativeLayout(&module, .{ .kind = .array, .name = "Point" }, points_ptr);
        vm.destroyArrayNativeLayout(&module, .{ .kind = .array, .name = "Point" }, native_points);
        const native_weights = try vm.copyArrayToNativeLayout(&module, .{ .kind = .array, .name = "I64" }, weights_ptr);
        vm.destroyArrayNativeLayout(&module, .{ .kind = .array, .name = "I64" }, native_weights);

        const batch_ptr = try vm.allocateStruct(&module, "Batch");
        const batch_fields_ptr: [*]runtime_abi.Value = @ptrFromInt(batch_ptr);
        batch_fields_ptr[0] = .{ .raw_ptr = points_ptr };
        batch_fields_ptr[1] = .{ .raw_ptr = weights_ptr };
        const native_batch = try vm.lowerStructToNativeLayout(&module, "Batch", batch_ptr);
        vm.destroyStructNativeLayout(&module, "Batch", native_batch);

        vm.dropManagedValue(.{ .raw_ptr = batch_ptr });
        try std.testing.expectEqual(@as(usize, 0), vm.heap.count());
    }
}

test "hybrid_runtime_array_append_cleanup_stress" {
    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const module = bytecode.Module{
        .types = &.{},
        .functions = &.{},
        .entry_function_id = null,
    };

    for (0..300) |iteration| {
        const array_ptr = try vm.allocateArray(0);
        const array: *ArrayObject = @ptrFromInt(array_ptr);
        for (0..64) |index| {
            try vm.heap.appendArrayItem(array, .{ .integer = @intCast(iteration + index) });
        }

        const native_array_ptr = try vm.copyArrayToNativeLayout(&module, .{ .kind = .array, .name = "I64" }, array_ptr);
        const native_array: *ArrayObject = @ptrFromInt(native_array_ptr);
        for (0..32) |index| {
            const old_items = native_array.items[0..@max(native_array.len, 1)];
            const new_len = native_array.len + 1;
            const new_items = try vm.allocator.alloc(runtime_abi.BridgeValue, new_len);
            for (old_items[0..native_array.len], 0..) |item, item_index| {
                new_items[item_index] = item;
            }
            new_items[native_array.len] = runtime_abi.bridgeValueFromValue(.{ .integer = @intCast(index) });
            vm.allocator.free(old_items);
            native_array.items = new_items.ptr;
            native_array.len = new_len;
        }
        try vm.syncArrayFromNativeLayout(&module, .{ .kind = .array, .name = "I64" }, array_ptr, native_array_ptr);
        vm.destroyArrayNativeLayout(&module, .{ .kind = .array, .name = "I64" }, native_array_ptr);
        vm.dropManagedValue(.{ .raw_ptr = array_ptr });
        try std.testing.expectEqual(@as(usize, 0), vm.heap.count());
    }
}

test "hybrid_recursive_aggregate_sync_cleanup_stress" {
    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const point_fields = [_]bytecode.Field{
        .{ .name = "x", .ty = .{ .kind = .integer, .name = "I64" } },
        .{ .name = "y", .ty = .{ .kind = .integer, .name = "I64" } },
    };
    const row_fields = [_]bytecode.Field{
        .{ .name = "points", .ty = .{ .kind = .array, .name = "Point" } },
    };
    const scene_fields = [_]bytecode.Field{
        .{ .name = "rows", .ty = .{ .kind = .array, .name = "Row" } },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "Point", .fields = @constCast(&point_fields) },
        .{ .name = "Row", .fields = @constCast(&row_fields) },
        .{ .name = "Scene", .fields = @constCast(&scene_fields) },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = &.{},
        .entry_function_id = null,
    };

    for (0..150) |iteration| {
        const rows_ptr = try vm.allocateArray(0);
        const rows: *ArrayObject = @ptrFromInt(rows_ptr);
        for (0..5) |row_index| {
            const points_ptr = try vm.allocateArray(0);
            const points: *ArrayObject = @ptrFromInt(points_ptr);
            for (0..8) |point_index| {
                const point_ptr = try vm.allocateStruct(&module, "Point");
                const fields: [*]runtime_abi.Value = @ptrFromInt(point_ptr);
                fields[0] = .{ .integer = @intCast(iteration + row_index) };
                fields[1] = .{ .integer = @intCast(point_index) };
                try vm.heap.appendArrayItem(points, .{ .raw_ptr = point_ptr });
            }

            const row_ptr = try vm.allocateStruct(&module, "Row");
            const row_fields_ptr: [*]runtime_abi.Value = @ptrFromInt(row_ptr);
            row_fields_ptr[0] = .{ .raw_ptr = points_ptr };
            try vm.heap.appendArrayItem(rows, .{ .raw_ptr = row_ptr });
        }

        const scene_ptr = try vm.allocateStruct(&module, "Scene");
        const scene_fields_ptr: [*]runtime_abi.Value = @ptrFromInt(scene_ptr);
        scene_fields_ptr[0] = .{ .raw_ptr = rows_ptr };

        const native_scene = try vm.lowerStructToNativeLayout(&module, "Scene", scene_ptr);
        for (0..10) |_| {
            try vm.syncStructFromNativeLayout(&module, "Scene", scene_ptr, native_scene);
        }
        vm.destroyStructNativeLayout(&module, "Scene", native_scene);

        vm.dropManagedValue(.{ .raw_ptr = scene_ptr });
        try std.testing.expectEqual(@as(usize, 0), vm.heap.count());
    }
}

test "hybrid_native_state_field_set_releases_replaced_managed_values" {
    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const handle_fields = [_]bytecode.Field{
        .{ .name = "id", .ty = .{ .kind = .integer, .name = "I64" } },
    };
    const state_fields = [_]bytecode.Field{
        .{ .name = "handle", .ty = .{ .kind = .ffi_struct, .name = "Handle" } },
    };
    const types = [_]bytecode.TypeDecl{
        .{ .name = "Handle", .fields = @constCast(&handle_fields) },
        .{ .name = "State", .fields = @constCast(&state_fields) },
    };
    const local_types = [_]bytecode.TypeRef{
        .{ .kind = .raw_ptr, .name = "RawPtr" },
    };
    const instructions = [_]bytecode.Instruction{
        .{ .load_local = .{ .dst = 0, .local = 0 } },
        .{ .recover_native_state = .{ .dst = 1, .state = 0, .type_name = "State", .type_id = 77 } },
        .{ .alloc_struct = .{ .dst = 2, .type_name = "Handle" } },
        .{ .native_state_field_set = .{
            .state = 1,
            .field_index = 0,
            .src = 2,
            .field_ty = .{ .kind = .ffi_struct, .name = "Handle" },
        } },
        .{ .ret = .{} },
    };
    const module = bytecode.Module{
        .types = @constCast(&types),
        .functions = @constCast(&[_]bytecode.Function{
            .{
                .id = 0,
                .name = "replaceHandle",
                .param_count = 1,
                .register_count = 3,
                .local_count = 1,
                .local_types = @constCast(&local_types),
                .instructions = @constCast(&instructions),
            },
        }),
        .entry_function_id = null,
    };

    const native_payload = try vm.allocator.alloc(runtime_abi.BridgeValue, 1);
    native_payload[0] = runtime_abi.bridgeValueFromValue(.{ .raw_ptr = 0 });
    const box = try vm.allocator.create(NativeStateBox);
    box.* = NativeStateBox.init(&module, "State", 77, 1, @intFromPtr(native_payload.ptr));
    defer {
        if (box.runtime_payload != 0) {
            const runtime_payload: [*]runtime_abi.Value = @ptrFromInt(box.runtime_payload);
            vm.heap.dropValue(runtime_payload[0]);
            vm.allocator.free(@as([]runtime_abi.Value, runtime_payload[0..1]));
        }
        if (box.payload != 0) {
            const current_payload: [*]runtime_abi.BridgeValue = @ptrFromInt(box.payload);
            vm.allocator.free(@as([]runtime_abi.BridgeValue, current_payload[0..1]));
        }
        vm.allocator.destroy(box);
    }

    var buffer: [16]u8 = undefined;
    var writer = std.Io.Writer.fixed(&buffer);
    for (0..200) |_| {
        const result = try vm.runFunctionById(&module, 0, &.{.{ .raw_ptr = @intFromPtr(box) }}, &writer, .{});
        vm.dropManagedValue(result);
        try std.testing.expectEqual(@as(usize, 1), vm.heap.count());
    }
}

test "native state recovery mutates persistent payload" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{.{
            .name = "CounterState",
            .fields = @constCast(&[_]bytecode.Field{.{ .name = "count", .ty = .{ .kind = .integer, .name = "I64" } }}),
        }}),
        .functions = @constCast(&[_]bytecode.Function{.{
            .id = 0,
            .name = "main",
            .param_count = 0,
            .register_count = 9,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]bytecode.Instruction{
                .{ .alloc_struct = .{ .dst = 0, .type_name = "CounterState" } },
                .{ .alloc_native_state = .{ .dst = 1, .src = 0, .type_name = "CounterState", .type_id = 77 } },
                .{ .recover_native_state = .{ .dst = 2, .state = 1, .type_name = "CounterState", .type_id = 77 } },
                .{ .field_ptr = .{ .dst = 3, .base = 2, .base_type_name = "CounterState", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .const_int = .{ .dst = 4, .value = 9 } },
                .{ .store_indirect = .{ .ptr = 3, .src = 4, .ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .recover_native_state = .{ .dst = 5, .state = 1, .type_name = "CounterState", .type_id = 77 } },
                .{ .field_ptr = .{ .dst = 6, .base = 5, .base_type_name = "CounterState", .field_index = 0, .field_ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .load_indirect = .{ .dst = 7, .ptr = 6, .ty = .{ .kind = .integer, .name = "I64" } } },
                .{ .ret = .{ .src = 7 } },
            }),
        }}),
        .entry_function_id = 0,
    };

    var discard_buffer_3: [1]u8 = undefined;
    var discarding_3: std.Io.Writer.Discarding = .init(&discard_buffer_3);
    const result = try vm.runFunctionById(&module, 0, &.{}, &discarding_3.writer, .{});
    try std.testing.expectEqual(@as(i64, 9), result.integer);
}

test "native state field set clones borrowed callbacks" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const callback_ty = bytecode.TypeRef{ .kind = .raw_ptr, .name = "() -> I64" };
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{.{
            .name = "CallbackState",
            .fields = @constCast(&[_]bytecode.Field{.{ .name = "callback", .ty = callback_ty }}),
        }}),
        .functions = @constCast(&[_]bytecode.Function{
            .{
                .id = 0,
                .name = "main",
                .param_count = 0,
                .return_type = .{ .kind = .integer, .name = "I64" },
                .register_count = 6,
                .local_count = 1,
                .local_types = @constCast(&[_]bytecode.TypeRef{callback_ty}),
                .instructions = @constCast(&[_]bytecode.Instruction{
                    .{ .const_closure = .{ .dst = 0, .function_id = 1, .captures = &.{} } },
                    .{ .store_local = .{ .local = 0, .src = 0 } },
                    .{ .load_local = .{ .dst = 1, .local = 0 } },
                    .{ .alloc_struct = .{ .dst = 2, .type_name = "CallbackState" } },
                    .{ .alloc_native_state = .{ .dst = 3, .src = 2, .type_name = "CallbackState", .type_id = 91 } },
                    .{ .native_state_field_set = .{ .state = 3, .field_index = 0, .src = 1, .field_ty = callback_ty } },
                    .{ .load_local = .{ .dst = 0, .local = 0, .ownership = .move } },
                    .{ .const_int = .{ .dst = 0, .value = 0 } },
                    .{ .native_state_field_get = .{ .dst = 4, .state = 3, .field_index = 0, .field_ty = callback_ty } },
                    .{ .call_value = .{ .callee = 4, .args = &.{}, .dst = 5 } },
                    .{ .ret = .{ .src = 5 } },
                }),
            },
            .{
                .id = 1,
                .name = "callback",
                .param_count = 0,
                .return_type = .{ .kind = .integer, .name = "I64" },
                .register_count = 1,
                .local_count = 0,
                .local_types = &.{},
                .instructions = @constCast(&[_]bytecode.Instruction{
                    .{ .const_int = .{ .dst = 0, .value = 42 } },
                    .{ .ret = .{ .src = 0 } },
                }),
            },
        }),
        .entry_function_id = 0,
    };

    var discard_buffer: [1]u8 = undefined;
    var discarding: std.Io.Writer.Discarding = .init(&discard_buffer);
    const result = try vm.runFunctionById(&module, 0, &.{}, &discarding.writer, .{});
    try std.testing.expectEqual(@as(i64, 42), result.integer);
    try std.testing.expectEqual(@as(usize, 0), vm.heap.count());
}

test "vm deinit releases tracked native state boxes before recovery" {
    const handle_fields = [_]bytecode.Field{
        .{ .name = "id", .ty = .{ .kind = .integer, .name = "I64" } },
    };
    const state_fields = [_]bytecode.Field{
        .{ .name = "handle", .ty = .{ .kind = .ffi_struct, .name = "Handle" } },
    };
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{
            .{ .name = "Handle", .fields = @constCast(&handle_fields) },
            .{ .name = "State", .fields = @constCast(&state_fields) },
        }),
        .functions = &.{},
        .entry_function_id = null,
    };

    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const handle_ptr = try vm.allocateStruct(&module, "Handle");
    const handle_values: [*]runtime_abi.Value = @ptrFromInt(handle_ptr);
    handle_values[0] = .{ .integer = 99 };

    const state_ptr = try vm.allocateStruct(&module, "State");
    const state_values: [*]runtime_abi.Value = @ptrFromInt(state_ptr);
    state_values[0] = .{ .raw_ptr = handle_ptr };

    _ = try vm.allocateNativeState(&module, "State", 91, state_ptr);
    vm.dropManagedValue(.{ .raw_ptr = state_ptr });

    try std.testing.expectEqual(@as(usize, 1), vm.native_state_boxes.count());
}

test "vm deinit releases tracked native state boxes after materialization" {
    const mode_enum_ty = bytecode.TypeRef{ .kind = .enum_instance, .name = "Mode" };
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{.{
            .name = "State",
            .fields = @constCast(&[_]bytecode.Field{.{ .name = "mode", .ty = mode_enum_ty }}),
        }}),
        .enums = @constCast(&[_]bytecode.EnumTypeDecl{.{
            .name = "Mode",
            .variants = @constCast(&[_]bytecode.EnumVariantDecl{
                .{ .name = "None", .discriminant = 0 },
                .{ .name = "Surface", .discriminant = 1 },
            }),
        }}),
        .functions = &.{},
        .entry_function_id = null,
    };

    var vm = Vm.init(std.testing.allocator);
    defer vm.deinit();

    const state_ptr = try vm.allocateStruct(&module, "State");
    const state_values: [*]runtime_abi.Value = @ptrFromInt(state_ptr);
    state_values[0] = .{ .raw_ptr = try vm.allocateEnum("Mode", 1, .{ .void = {} }) };

    const state_token = try vm.allocateNativeState(&module, "State", 81, state_ptr);
    _ = try vm.recoverNativeState(&module, "State", state_token, 81);
    vm.dropManagedValue(.{ .raw_ptr = state_ptr });

    const box: *const NativeStateBox = @ptrFromInt(state_token);
    try std.testing.expectEqual(@as(usize, 1), vm.native_state_boxes.count());
    try std.testing.expectEqual(@as(usize, 0), box.payload);
    try std.testing.expect(box.runtime_payload != 0);
}

test "native state preserves nested enum values inside arrays of structs" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const layer_array_ty = bytecode.TypeRef{ .kind = .array, .name = "Layer" };
    const mode_enum_ty = bytecode.TypeRef{ .kind = .enum_instance, .name = "Mode" };
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{
            .{
                .name = "Layer",
                .fields = @constCast(&[_]bytecode.Field{.{ .name = "payload", .ty = mode_enum_ty }}),
            },
            .{
                .name = "State",
                .fields = @constCast(&[_]bytecode.Field{.{ .name = "layers", .ty = layer_array_ty }}),
            },
        }),
        .enums = @constCast(&[_]bytecode.EnumTypeDecl{.{
            .name = "Mode",
            .variants = @constCast(&[_]bytecode.EnumVariantDecl{
                .{ .name = "None", .discriminant = 0 },
                .{ .name = "Surface", .discriminant = 1 },
            }),
        }}),
        .functions = @constCast(&[_]bytecode.Function{.{
            .id = 0,
            .name = "main",
            .param_count = 0,
            .return_type = .{ .kind = .integer, .name = "I64" },
            .register_count = 13,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]bytecode.Instruction{
                .{ .const_int = .{ .dst = 0, .value = 1 } },
                .{ .alloc_struct = .{ .dst = 1, .type_name = "State" } },
                .{ .field_ptr = .{ .dst = 2, .base = 1, .base_type_name = "State", .field_index = 0, .field_ty = layer_array_ty } },
                .{ .alloc_array = .{ .dst = 3, .len = 0 } },
                .{ .alloc_struct = .{ .dst = 4, .type_name = "Layer" } },
                .{ .field_ptr = .{ .dst = 5, .base = 4, .base_type_name = "Layer", .field_index = 0, .field_ty = mode_enum_ty } },
                .{ .alloc_enum = .{ .dst = 6, .enum_type_name = "Mode", .discriminant = 1 } },
                .{ .store_indirect = .{ .ptr = 5, .src = 6, .ty = mode_enum_ty } },
                .{ .array_append = .{ .array = 3, .src = 4 } },
                .{ .store_indirect = .{ .ptr = 2, .src = 3, .ty = layer_array_ty } },
                .{ .alloc_native_state = .{ .dst = 7, .src = 1, .type_name = "State", .type_id = 77 } },
                .{ .recover_native_state = .{ .dst = 8, .state = 7, .type_name = "State", .type_id = 77 } },
                .{ .field_ptr = .{ .dst = 9, .base = 8, .base_type_name = "State", .field_index = 0, .field_ty = layer_array_ty } },
                .{ .load_indirect = .{ .dst = 10, .ptr = 9, .ty = layer_array_ty } },
                .{ .array_get = .{ .dst = 11, .array = 10, .index = 0, .ty = .{ .kind = .ffi_struct, .name = "Layer" } } },
                .{ .field_ptr = .{ .dst = 12, .base = 11, .base_type_name = "Layer", .field_index = 0, .field_ty = mode_enum_ty } },
                .{ .load_indirect = .{ .dst = 6, .ptr = 12, .ty = mode_enum_ty } },
                .{ .enum_tag = .{ .dst = 0, .src = 6 } },
                .{ .ret = .{ .src = 0 } },
            }),
        }}),
        .entry_function_id = 0,
    };

    var discard_buffer_4: [1]u8 = undefined;
    var discarding_4: std.Io.Writer.Discarding = .init(&discard_buffer_4);
    const result = try vm.runFunctionById(&module, 0, &.{}, &discarding_4.writer, .{});
    try std.testing.expectEqual(@as(i64, 1), result.integer);
}

test "native state preserves direct enum struct fields" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const mode_enum_ty = bytecode.TypeRef{ .kind = .enum_instance, .name = "Mode" };
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{.{
            .name = "State",
            .fields = @constCast(&[_]bytecode.Field{.{ .name = "mode", .ty = mode_enum_ty }}),
        }}),
        .enums = @constCast(&[_]bytecode.EnumTypeDecl{.{
            .name = "Mode",
            .variants = @constCast(&[_]bytecode.EnumVariantDecl{
                .{ .name = "None", .discriminant = 0 },
                .{ .name = "Surface", .discriminant = 1 },
            }),
        }}),
        .functions = @constCast(&[_]bytecode.Function{.{
            .id = 0,
            .name = "main",
            .param_count = 0,
            .return_type = .{ .kind = .integer, .name = "I64" },
            .register_count = 8,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]bytecode.Instruction{
                .{ .alloc_struct = .{ .dst = 0, .type_name = "State" } },
                .{ .field_ptr = .{ .dst = 1, .base = 0, .base_type_name = "State", .field_index = 0, .field_ty = mode_enum_ty } },
                .{ .alloc_enum = .{ .dst = 2, .enum_type_name = "Mode", .discriminant = 1 } },
                .{ .store_indirect = .{ .ptr = 1, .src = 2, .ty = mode_enum_ty } },
                .{ .alloc_native_state = .{ .dst = 3, .src = 0, .type_name = "State", .type_id = 81 } },
                .{ .recover_native_state = .{ .dst = 4, .state = 3, .type_name = "State", .type_id = 81 } },
                .{ .field_ptr = .{ .dst = 5, .base = 4, .base_type_name = "State", .field_index = 0, .field_ty = mode_enum_ty } },
                .{ .load_indirect = .{ .dst = 6, .ptr = 5, .ty = mode_enum_ty } },
                .{ .enum_tag = .{ .dst = 7, .src = 6 } },
                .{ .ret = .{ .src = 7 } },
            }),
        }}),
        .entry_function_id = 0,
    };

    var discard_buffer: [1]u8 = undefined;
    var discarding: std.Io.Writer.Discarding = .init(&discard_buffer);
    const result = try vm.runFunctionById(&module, 0, &.{}, &discarding.writer, .{});
    try std.testing.expectEqual(@as(i64, 1), result.integer);
}

test "native state recovery validates the expected type id" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .types = @constCast(&[_]bytecode.TypeDecl{.{
            .name = "CounterState",
            .fields = @constCast(&[_]bytecode.Field{.{ .name = "count", .ty = .{ .kind = .integer, .name = "I64" } }}),
        }}),
        .functions = @constCast(&[_]bytecode.Function{.{
            .id = 0,
            .name = "main",
            .param_count = 0,
            .register_count = 3,
            .local_count = 0,
            .local_types = &.{},
            .instructions = @constCast(&[_]bytecode.Instruction{
                .{ .alloc_struct = .{ .dst = 0, .type_name = "CounterState" } },
                .{ .alloc_native_state = .{ .dst = 1, .src = 0, .type_name = "CounterState", .type_id = 77 } },
                .{ .recover_native_state = .{ .dst = 2, .state = 1, .type_name = "CounterState", .type_id = 88 } },
                .{ .ret = .{ .src = null } },
            }),
        }}),
        .entry_function_id = 0,
    };

    var discard_buffer_5: [1]u8 = undefined;
    var discarding_5: std.Io.Writer.Discarding = .init(&discard_buffer_5);
    try std.testing.expectError(error.RuntimeFailure, vm.runFunctionById(&module, 0, &.{}, &discarding_5.writer, .{}));
    try std.testing.expect(std.mem.indexOf(u8, vm.lastError().?, "wrong state type") != null);
}

test "native construct-any fields materialize concrete widget values" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const any_widget = bytecode.TypeRef{
        .kind = .construct_any,
        .name = "any Widget",
        .construct_constraint = .{ .construct_name = "Widget" },
    };
    const module = bytecode.Module{
        .constructs = @constCast(&[_]bytecode.Construct{.{ .name = "Widget" }}),
        .construct_implementations = @constCast(&[_]bytecode.ConstructImplementation{
            .{ .type_name = "Button", .construct_constraint = .{ .construct_name = "Widget" }, .fields = &.{}, .has_content = false, .lifecycle_hooks = &.{} },
        }),
        .types = @constCast(&[_]bytecode.TypeDecl{
            .{ .name = "Button", .fields = &.{} },
            .{
                .name = "Layer",
                .fields = @constCast(&[_]bytecode.Field{
                    .{ .name = "content", .ty = any_widget },
                }),
            },
        }),
        .functions = &.{},
        .entry_function_id = null,
    };

    const button_ptr = try vm.allocateStruct(&module, "Button");
    defer vm.dropManagedValue(.{ .raw_ptr = button_ptr });
    const native_button = try construct_any_test.allocateHeaderedNativeStruct(&vm, &module, "Button", button_ptr);
    defer construct_any_test.destroyHeaderedNativeStruct(&vm, &module, "Button", native_button);

    const layer_ptr = try vm.allocateStruct(&module, "Layer");
    defer vm.dropManagedValue(.{ .raw_ptr = layer_ptr });
    const native_layer_ptr = try vm.lowerStructToNativeLayout(&module, "Layer", layer_ptr);
    defer vm.destroyStructNativeLayout(&module, "Layer", native_layer_ptr);

    const content_offset = try native_layout.fieldOffset(&module, "Layer", 0);
    (@as(*usize, @ptrFromInt(native_layer_ptr + content_offset))).* = native_button.payload_ptr;

    const runtime_layer_ptr = try vm.copyStructFromNativeLayout(&module, "Layer", native_layer_ptr);
    defer vm.dropManagedValue(.{ .raw_ptr = runtime_layer_ptr });
    const runtime_fields: [*]align(1) const runtime_abi.Value = @ptrFromInt(runtime_layer_ptr);
    try std.testing.expect(runtime_fields[0] == .raw_ptr);
    try std.testing.expectEqualStrings("Button", vm.heap.getStructTypeName(runtime_fields[0].raw_ptr).?);
}

test "native construct-any results materialize concrete widget values" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const any_widget = bytecode.TypeRef{
        .kind = .construct_any,
        .name = "any Widget",
        .construct_constraint = .{ .construct_name = "Widget" },
    };
    const module = bytecode.Module{
        .constructs = @constCast(&[_]bytecode.Construct{.{ .name = "Widget" }}),
        .construct_implementations = @constCast(&[_]bytecode.ConstructImplementation{
            .{ .type_name = "Button", .construct_constraint = .{ .construct_name = "Widget" }, .fields = &.{}, .has_content = false, .lifecycle_hooks = &.{} },
        }),
        .types = @constCast(&[_]bytecode.TypeDecl{
            .{ .name = "Button", .fields = &.{} },
        }),
        .functions = &.{},
        .entry_function_id = null,
    };

    const button_ptr = try vm.allocateStruct(&module, "Button");
    defer vm.dropManagedValue(.{ .raw_ptr = button_ptr });
    const native_button = try construct_any_test.allocateHeaderedNativeStruct(&vm, &module, "Button", button_ptr);
    defer construct_any_test.destroyHeaderedNativeStruct(&vm, &module, "Button", native_button);

    const result = try vm.materializeNativeResult(&module, any_widget, .{ .raw_ptr = native_button.payload_ptr });
    defer vm.dropManagedValue(result);
    try std.testing.expect(result == .raw_ptr);
    try std.testing.expectEqualStrings("Button", vm.heap.getStructTypeName(result.raw_ptr).?);
}

test "native widget arrays materialize concrete construct-any elements" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    var vm = Vm.init(arena.allocator());
    const module = bytecode.Module{
        .constructs = @constCast(&[_]bytecode.Construct{.{ .name = "Widget" }}),
        .construct_implementations = @constCast(&[_]bytecode.ConstructImplementation{
            .{ .type_name = "Button", .construct_constraint = .{ .construct_name = "Widget" }, .fields = &.{}, .has_content = false, .lifecycle_hooks = &.{} },
        }),
        .types = @constCast(&[_]bytecode.TypeDecl{
            .{ .name = "Button", .fields = &.{} },
        }),
        .functions = &.{},
        .entry_function_id = null,
    };

    const runtime_button_ptr = try vm.allocateStruct(&module, "Button");
    const native_button = try construct_any_test.allocateHeaderedNativeStruct(&vm, &module, "Button", runtime_button_ptr);
    vm.dropManagedValue(.{ .raw_ptr = runtime_button_ptr });
    defer construct_any_test.destroyHeaderedNativeStruct(&vm, &module, "Button", native_button);

    const native_array = try vm.allocator.create(ArrayObject);
    defer vm.allocator.destroy(native_array);
    const native_items = try vm.allocator.alloc(runtime_abi.BridgeValue, 1);
    defer vm.allocator.free(native_items);
    native_items[0] = runtime_abi.bridgeValueFromValue(.{ .raw_ptr = native_button.payload_ptr });
    native_array.* = .{
        .len = 1,
        .items = native_items.ptr,
    };

    const runtime_array_ptr = try vm.copyArrayFromNativeLayout(&module, .{ .kind = .array, .name = "any Widget" }, @intFromPtr(native_array));
    defer vm.dropManagedValue(.{ .raw_ptr = runtime_array_ptr });

    const runtime_array: *const ArrayObject = @ptrFromInt(runtime_array_ptr);
    const element = runtime_abi.bridgeValueToValue(runtime_array.items[0]);
    try std.testing.expect(element == .raw_ptr);
    try std.testing.expectEqualStrings("Button", vm.heap.getStructTypeName(element.raw_ptr).?);
}
