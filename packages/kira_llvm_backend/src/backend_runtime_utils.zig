const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const llvm = @import("llvm_c.zig");
const clang_driver = @import("clang_driver.zig");
const toolchain = @import("toolchain.zig");

pub fn freeStringList(allocator: std.mem.Allocator, list: *std.array_list.Managed([]const u8)) void {
    for (list.items) |item| allocator.free(item);
    list.deinit();
}

pub fn freeSymbolNames(allocator: std.mem.Allocator, symbols: *std.AutoHashMapUnmanaged(u32, []const u8)) void {
    var iterator = symbols.iterator();
    while (iterator.next()) |entry| {
        allocator.free(entry.value_ptr.*);
    }
    symbols.deinit(allocator);
}

pub fn writeTextFile(path: []const u8, data: []const u8) !void {
    if (std.fs.path.isAbsolute(path)) {
        const file = try std.Io.Dir.createFileAbsolute(std.Options.debug_io, path, .{ .truncate = true });
        defer file.close(std.Options.debug_io);
        try file.writeStreamingAll(std.Options.debug_io, data);
        return;
    }
    try std.Io.Dir.cwd().writeFile(std.Options.debug_io, .{
        .sub_path = path,
        .data = data,
    });
}

pub fn emitObjectFileViaClang(
    allocator: std.mem.Allocator,
    api: *const llvm.Api,
    module_ref: llvm.c.LLVMModuleRef,
    object_path: []const u8,
) !void {
    const ir_text_z = api.LLVMPrintModuleToString(module_ref);
    defer api.LLVMDisposeMessage(ir_text_z);

    const ir_text = std.mem.span(ir_text_z);
    const ir_path = try std.fmt.allocPrint(allocator, "{s}.ll", .{object_path});
    defer allocator.free(ir_path);
    defer std.Io.Dir.cwd().deleteFile(std.Options.debug_io, ir_path) catch {};

    try std.Io.Dir.cwd().writeFile(std.Options.debug_io, .{
        .sub_path = ir_path,
        .data = ir_text,
    });

    const llvm_toolchain = try toolchain.Toolchain.discover(allocator);
    const clang_path = try llvm_toolchain.clangPath(allocator);
    defer allocator.free(clang_path);
    var argv = std.array_list.Managed([]const u8).init(allocator);
    try argv.append(clang_path);
    try clang_driver.appendHostClangDriverArgs(allocator, &argv);
    try argv.appendSlice(&.{ "-c", "-x", "ir", "-o", object_path, ir_path });
    const process_environ = inheritedProcessEnviron();
    var io_impl: std.Io.Threaded = .init(std.heap.smp_allocator, .{ .environ = process_environ });
    defer io_impl.deinit();
    const result = try std.process.run(allocator, io_impl.io(), .{
        .argv = argv.items,
        .stdout_limit = .limited(512 * 1024),
        .stderr_limit = .limited(512 * 1024),
    });
    defer allocator.free(result.stdout);
    defer allocator.free(result.stderr);

    if (result.term != .exited or result.term.exited != 0) {
        if (result.stdout.len != 0) std.debug.print("{s}", .{result.stdout});
        if (result.stderr.len != 0) std.debug.print("{s}", .{result.stderr});
        return error.ObjectEmissionFailed;
    }
}

pub fn inheritedProcessEnviron() std.process.Environ {
    return switch (builtin.os.tag) {
        .windows => .{ .block = .global },
        .wasi, .emscripten, .freestanding, .other => .empty,
        else => .{ .block = .{ .slice = currentPosixEnvironBlock() } },
    };
}

fn currentPosixEnvironBlock() [:null]const ?[*:0]const u8 {
    if (!builtin.link_libc) return &.{};

    const environ = std.c.environ;
    var len: usize = 0;
    while (environ[len] != null) : (len += 1) {}
    return environ[0..len :null];
}

pub fn inferRegisterTypes(allocator: std.mem.Allocator, program: ir.Program, function_decl: ir.Function) ![]ir.ValueType {
    const register_types = try allocator.alloc(ir.ValueType, function_decl.register_count);
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
            .compare => |value| register_types[value.dst] = .{ .kind = .boolean },
            .unary => |value| register_types[value.dst] = switch (value.op) {
                .negate => register_types[value.src],
                .not => .{ .kind = .boolean },
            },
            .store_local => {},
            .load_local => |value| register_types[value.dst] = function_decl.local_types[value.local],
            .local_ptr => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = "LocalPtr" },
            .subobject_ptr => |value| register_types[value.dst] = register_types[value.base],
            .field_ptr => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = value.field_ty.name },
            .recover_native_state => |value| register_types[value.dst] = .{ .kind = .raw_ptr, .name = value.type_name },
            .native_state_field_get => |value| register_types[value.dst] = value.field_ty,
            .c_string_to_string => |value| register_types[value.dst] = .{ .kind = .string },
            .array_len => |value| register_types[value.dst] = .{ .kind = .integer, .name = "I64" },
            .array_get => |value| register_types[value.dst] = value.ty,
            .enum_tag => |value| register_types[value.dst] = .{ .kind = .integer, .name = "I64" },
            .enum_payload => |value| register_types[value.dst] = value.payload_ty,
            .array_set, .array_append, .native_state_field_set => {},
            .load_indirect => |value| register_types[value.dst] = value.ty,
            .store_indirect, .copy_indirect, .branch, .jump, .label => {},
            .print => {},
            .call => |value| if (value.dst) |dst| {
                const callee_decl = functionById(program, value.callee) orelse return error.UnknownFunction;
                register_types[dst] = callee_decl.return_type;
            },
            .call_virtual => |value| if (value.dst) |dst| {
                register_types[dst] = value.return_ty;
            },
            .call_value => |value| if (value.dst) |dst| {
                register_types[dst] = value.return_type;
            },
            .ret => {},
        }
    }
    return register_types;
}

pub fn functionExecutionById(program: ir.Program, function_id: u32) ?runtime_abi.FunctionExecution {
    for (program.functions) |function_decl| {
        if (function_decl.id == function_id) return function_decl.execution;
    }
    return null;
}

pub fn functionById(program: ir.Program, function_id: u32) ?ir.Function {
    for (program.functions) |function_decl| {
        if (function_decl.id == function_id) return function_decl;
    }
    return null;
}
