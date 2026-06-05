const std = @import("std");
const builtin = @import("builtin");
const ir = @import("kira_ir");
const runtime_abi = @import("kira_runtime_abi");
const backend_api = @import("kira_backend_api");
const build_options = @import("kira_llvm_build_options");
const llvm = @import("llvm_c.zig");
const runtime_symbols = @import("runtime_symbols.zig");
const runtime_utils = @import("backend_runtime_utils.zig");
const platform_utils = @import("backend_platform_utils.zig");
const parent = @import("backend.zig");
const shouldLowerFunction = parent.shouldLowerFunction;
pub const freeStringList = runtime_utils.freeStringList;
pub const freeSymbolNames = runtime_utils.freeSymbolNames;
pub const writeTextFile = runtime_utils.writeTextFile;
pub const emitObjectFileViaClang = runtime_utils.emitObjectFileViaClang;
pub const inheritedProcessEnviron = runtime_utils.inheritedProcessEnviron;
pub const inferRegisterTypes = runtime_utils.inferRegisterTypes;
pub const functionExecutionById = runtime_utils.functionExecutionById;
pub const functionById = runtime_utils.functionById;
pub const resolveExecution = platform_utils.resolveExecution;
pub const hostTargetTriple = platform_utils.hostTargetTriple;
pub const targetTriple = platform_utils.targetTriple;
pub const ensureParentDir = platform_utils.ensureParentDir;
pub const allocPrintZ = platform_utils.allocPrintZ;
const PrintValueRef = struct {
    ty: ir.ValueType,
    ssa_name: []const u8,
};

pub fn writePrintInstruction(
    allocator: std.mem.Allocator,
    writer: anytype,
    program: *const ir.Program,
    globals: *std.array_list.Managed([]const u8),
    value_type: ir.ValueType,
    src: u32,
    string_state: *usize,
    temp_counter: *usize,
) !void {
    const root_name = try std.fmt.allocPrint(allocator, "%r{d}", .{src});
    defer allocator.free(root_name);

    try writePrintedValue(allocator, writer, program, globals, .{
        .ty = value_type,
        .ssa_name = root_name,
    }, string_state, temp_counter);
    try writer.writeAll("  call void @\"kira_native_write_newline\"()\n");
}

fn writePrintedValue(
    allocator: std.mem.Allocator,
    writer: anytype,
    program: *const ir.Program,
    globals: *std.array_list.Managed([]const u8),
    value_ref: PrintValueRef,
    string_state: *usize,
    temp_counter: *usize,
) anyerror!void {
    switch (value_ref.ty.kind) {
        .void => try writePrintLiteral(allocator, writer, globals, "void", string_state, temp_counter),
        .integer => {
            try writer.print("  call void @\"kira_native_write_i64\"(i64 {s})\n", .{value_ref.ssa_name});
        },
        .float => {
            if (value_ref.ty.name != null and std.mem.eql(u8, value_ref.ty.name.?, "F32")) {
                const temp_index = temp_counter.*;
                temp_counter.* += 1;
                try writer.print("  %print.float.ext.{d} = fpext float {s} to double\n", .{ temp_index, value_ref.ssa_name });
                try writer.print("  call void @\"kira_native_write_f64\"(double %print.float.ext.{d})\n", .{temp_index});
            } else {
                try writer.print("  call void @\"kira_native_write_f64\"(double {s})\n", .{value_ref.ssa_name});
            }
        },
        .string => try writeStringValue(writer, value_ref.ssa_name, temp_counter),
        .boolean => try writeBooleanValue(writer, value_ref.ssa_name, temp_counter),
        .enum_instance => try writeEnumValue(allocator, writer, program, globals, value_ref, string_state, temp_counter),
        .construct_any, .raw_ptr => {
            try writer.print("  call void @\"kira_native_write_ptr\"(i64 {s})\n", .{value_ref.ssa_name});
        },
        .array => try writeArraySummary(allocator, writer, globals, value_ref.ssa_name, string_state, temp_counter),
        .ffi_struct => try writeStructValue(allocator, writer, program, globals, value_ref, string_state, temp_counter),
    }
}

fn writeStructValue(
    allocator: std.mem.Allocator,
    writer: anytype,
    program: *const ir.Program,
    globals: *std.array_list.Managed([]const u8),
    value_ref: PrintValueRef,
    string_state: *usize,
    temp_counter: *usize,
) anyerror!void {
    const type_name = value_ref.ty.name orelse {
        try writer.print("  call void @\"kira_native_write_ptr\"(i64 {s})\n", .{value_ref.ssa_name});
        return;
    };
    const type_decl = findTypeDecl(program, type_name);
    if (type_decl == null) {
        try writePrintLiteral(allocator, writer, globals, type_name, string_state, temp_counter);
        try writePrintLiteral(allocator, writer, globals, "@", string_state, temp_counter);
        try writer.print("  call void @\"kira_native_write_ptr\"(i64 {s})\n", .{value_ref.ssa_name});
        return;
    }

    try writePrintLiteral(allocator, writer, globals, type_name, string_state, temp_counter);
    try writePrintLiteral(allocator, writer, globals, "(", string_state, temp_counter);

    const base_temp = temp_counter.*;
    temp_counter.* += 1;
    try writer.print("  %print.struct.ptr.{d} = inttoptr i64 {s} to ptr\n", .{ base_temp, value_ref.ssa_name });
    const base_ptr_name = try std.fmt.allocPrint(allocator, "%print.struct.ptr.{d}", .{base_temp});
    defer allocator.free(base_ptr_name);

    for (type_decl.?.fields, 0..) |field_decl, index| {
        if (index != 0) {
            try writePrintLiteral(allocator, writer, globals, ", ", string_state, temp_counter);
        }
        try writePrintLiteral(allocator, writer, globals, field_decl.name, string_state, temp_counter);
        try writePrintLiteral(allocator, writer, globals, ": ", string_state, temp_counter);

        const field_ref = try loadFieldValue(allocator, writer, program, type_decl.?.name, field_decl, base_ptr_name, index, temp_counter);
        defer allocator.free(field_ref.ssa_name);
        try writePrintedValue(allocator, writer, program, globals, field_ref, string_state, temp_counter);
    }

    try writePrintLiteral(allocator, writer, globals, ")", string_state, temp_counter);
}

fn writeEnumValue(
    allocator: std.mem.Allocator,
    writer: anytype,
    program: *const ir.Program,
    globals: *std.array_list.Managed([]const u8),
    value_ref: PrintValueRef,
    string_state: *usize,
    temp_counter: *usize,
) anyerror!void {
    const enum_name = value_ref.ty.name orelse {
        try writer.print("  call void @\"kira_native_write_ptr\"(i64 {s})\n", .{value_ref.ssa_name});
        return;
    };
    const enum_decl = findEnumDecl(program, enum_name) orelse {
        try writer.print("  call void @\"kira_native_write_ptr\"(i64 {s})\n", .{value_ref.ssa_name});
        return;
    };

    const temp_index = temp_counter.*;
    temp_counter.* += 1;
    try writer.print("  %print.enum.ptr.{d} = inttoptr i64 {s} to ptr\n", .{ temp_index, value_ref.ssa_name });
    try writer.print("  %print.enum.tag.ptr.{d} = getelementptr inbounds i64, ptr %print.enum.ptr.{d}, i64 0\n", .{ temp_index, temp_index });
    try writer.print("  %print.enum.tag.{d} = load i64, ptr %print.enum.tag.ptr.{d}\n", .{ temp_index, temp_index });
    try writer.print("  %print.enum.payload.ptr.{d} = getelementptr inbounds i64, ptr %print.enum.ptr.{d}, i64 1\n", .{ temp_index, temp_index });
    try writer.print("  %print.enum.payload.raw.{d} = load i64, ptr %print.enum.payload.ptr.{d}\n", .{ temp_index, temp_index });

    const done_label = temp_counter.*;
    temp_counter.* += 1;
    const default_label = temp_counter.*;
    temp_counter.* += 1;
    var case_labels = std.array_list.Managed(usize).init(allocator);
    defer case_labels.deinit();
    var next_labels = std.array_list.Managed(usize).init(allocator);
    defer next_labels.deinit();
    for (enum_decl.variants) |_| {
        try case_labels.append(temp_counter.*);
        temp_counter.* += 1;
    }
    if (enum_decl.variants.len > 1) {
        for (0..enum_decl.variants.len - 1) |_| {
            try next_labels.append(temp_counter.*);
            temp_counter.* += 1;
        }
    }

    for (enum_decl.variants, 0..) |variant_decl, index| {
        try writer.print("  %print.enum.expected.{d}.{d} = add i64 0, {d}\n", .{ temp_index, index, variant_decl.discriminant });
        try writer.print("  %print.enum.cmp.{d}.{d} = icmp eq i64 %print.enum.tag.{d}, %print.enum.expected.{d}.{d}\n", .{
            temp_index, index, temp_index, temp_index, index,
        });
        if (index + 1 < enum_decl.variants.len) {
            try writer.print("  br i1 %print.enum.cmp.{d}.{d}, label %print_enum_case_{d}, label %print_enum_next_{d}\n", .{
                temp_index, index, case_labels.items[index], next_labels.items[index],
            });
            try writer.print("print_enum_next_{d}:\n", .{next_labels.items[index]});
        } else {
            try writer.print("  br i1 %print.enum.cmp.{d}.{d}, label %print_enum_case_{d}, label %print_enum_default_{d}\n", .{
                temp_index, index, case_labels.items[index], default_label,
            });
        }
    }

    for (enum_decl.variants, 0..) |variant_decl, index| {
        try writer.print("print_enum_case_{d}:\n", .{case_labels.items[index]});
        const full_name = try std.fmt.allocPrint(allocator, "{s}.{s}", .{ enum_name, variant_decl.name });
        defer allocator.free(full_name);
        try writePrintLiteral(allocator, writer, globals, full_name, string_state, temp_counter);
        if (variant_decl.payload_ty) |payload_ty| {
            try writePrintLiteral(allocator, writer, globals, "(", string_state, temp_counter);
            const payload_ref = try unpackEnumPayloadValue(allocator, writer, payload_ty, temp_index, temp_counter);
            defer allocator.free(payload_ref.ssa_name);
            try writePrintedValue(allocator, writer, program, globals, payload_ref, string_state, temp_counter);
            try writePrintLiteral(allocator, writer, globals, ")", string_state, temp_counter);
        }
        try writer.print("  br label %print_enum_done_{d}\n", .{done_label});
    }

    try writer.print("print_enum_default_{d}:\n", .{default_label});
    try writer.print("  call void @\"kira_native_write_ptr\"(i64 {s})\n", .{value_ref.ssa_name});
    try writer.print("  br label %print_enum_done_{d}\n", .{done_label});
    try writer.print("print_enum_done_{d}:\n", .{done_label});
}

fn unpackEnumPayloadValue(
    allocator: std.mem.Allocator,
    writer: anytype,
    payload_ty: ir.ValueType,
    temp_index: usize,
    temp_counter: *usize,
) anyerror!PrintValueRef {
    switch (payload_ty.kind) {
        .integer, .construct_any, .raw_ptr, .ffi_struct, .array, .enum_instance => {
            return .{
                .ty = payload_ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.enum.payload.raw.{d}", .{temp_index}),
            };
        },
        .boolean => {
            const bool_index = temp_counter.*;
            temp_counter.* += 1;
            try writer.print("  %print.enum.payload.bool.{d} = trunc i64 %print.enum.payload.raw.{d} to i1\n", .{ bool_index, temp_index });
            return .{
                .ty = payload_ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.enum.payload.bool.{d}", .{bool_index}),
            };
        },
        .float => {
            const float_index = temp_counter.*;
            temp_counter.* += 1;
            try writer.print("  %print.enum.payload.float64.{d} = bitcast i64 %print.enum.payload.raw.{d} to double\n", .{ float_index, temp_index });
            if (payload_ty.name != null and std.mem.eql(u8, payload_ty.name.?, "F32")) {
                try writer.print("  %print.enum.payload.float.{d} = fptrunc double %print.enum.payload.float64.{d} to float\n", .{ float_index, float_index });
                return .{
                    .ty = payload_ty,
                    .ssa_name = try std.fmt.allocPrint(allocator, "%print.enum.payload.float.{d}", .{float_index}),
                };
            }
            return .{
                .ty = payload_ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.enum.payload.float64.{d}", .{float_index}),
            };
        },
        .string => {
            const string_index = temp_counter.*;
            temp_counter.* += 1;
            try writer.print("  %print.enum.payload.string.ptr.{d} = inttoptr i64 %print.enum.payload.raw.{d} to ptr\n", .{ string_index, temp_index });
            try writer.print("  %print.enum.payload.string.{d} = load %kira.string, ptr %print.enum.payload.string.ptr.{d}\n", .{ string_index, string_index });
            return .{
                .ty = payload_ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.enum.payload.string.{d}", .{string_index}),
            };
        },
        .void => return error.UnsupportedExecutableFeature,
    }
}

fn loadFieldValue(
    allocator: std.mem.Allocator,
    writer: anytype,
    program: *const ir.Program,
    owner_type_name: []const u8,
    field_decl: ir.Field,
    base_ptr_name: []const u8,
    field_index_value: usize,
    temp_counter: *usize,
) anyerror!PrintValueRef {
    const temp_index = temp_counter.*;
    temp_counter.* += 1;
    const owner_ref_name = typeRefName(owner_type_name);
    try writer.print(
        "  %print.field.ptr.{d} = getelementptr inbounds {s}, ptr {s}, i32 0, i32 {d}\n",
        .{ temp_index, owner_ref_name, base_ptr_name, field_index_value },
    );

    switch (field_decl.ty.kind) {
        .integer => {
            const abi_type = try llvmIndirectLoadTypeText(allocator, program, field_decl.ty);
            defer allocator.free(abi_type);
            try writer.print("  %print.field.raw.{d} = load {s}, ptr %print.field.ptr.{d}\n", .{ temp_index, abi_type, temp_index });
            if (std.mem.eql(u8, abi_type, "i64")) {
                return .{
                    .ty = field_decl.ty,
                    .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.raw.{d}", .{temp_index}),
                };
            }
            try writer.print("  %print.field.value.{d} = sext {s} %print.field.raw.{d} to i64\n", .{ temp_index, abi_type, temp_index });
            return .{
                .ty = field_decl.ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.value.{d}", .{temp_index}),
            };
        },
        .float => {
            const abi_type = try llvmIndirectLoadTypeText(allocator, program, field_decl.ty);
            defer allocator.free(abi_type);
            try writer.print("  %print.field.raw.{d} = load {s}, ptr %print.field.ptr.{d}\n", .{ temp_index, abi_type, temp_index });
            if (std.mem.eql(u8, abi_type, "float")) {
                try writer.print("  %print.field.value.{d} = fpext float %print.field.raw.{d} to double\n", .{ temp_index, temp_index });
                return .{
                    .ty = field_decl.ty,
                    .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.value.{d}", .{temp_index}),
                };
            }
            return .{
                .ty = field_decl.ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.raw.{d}", .{temp_index}),
            };
        },
        .string => {
            try writer.print("  %print.field.value.{d} = load %kira.string, ptr %print.field.ptr.{d}\n", .{ temp_index, temp_index });
            return .{
                .ty = field_decl.ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.value.{d}", .{temp_index}),
            };
        },
        .boolean => {
            try writer.print("  %print.field.raw.{d} = load i8, ptr %print.field.ptr.{d}\n", .{ temp_index, temp_index });
            try writer.print("  %print.field.value.{d} = trunc i8 %print.field.raw.{d} to i1\n", .{ temp_index, temp_index });
            return .{
                .ty = field_decl.ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.value.{d}", .{temp_index}),
            };
        },
        .construct_any, .raw_ptr, .array, .enum_instance => {
            try writer.print("  %print.field.rawptr.{d} = load ptr, ptr %print.field.ptr.{d}\n", .{ temp_index, temp_index });
            try writer.print("  %print.field.value.{d} = ptrtoint ptr %print.field.rawptr.{d} to i64\n", .{ temp_index, temp_index });
            return .{
                .ty = field_decl.ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.value.{d}", .{temp_index}),
            };
        },
        .ffi_struct => {
            try writer.print("  %print.field.value.{d} = ptrtoint ptr %print.field.ptr.{d} to i64\n", .{ temp_index, temp_index });
            return .{
                .ty = field_decl.ty,
                .ssa_name = try std.fmt.allocPrint(allocator, "%print.field.value.{d}", .{temp_index}),
            };
        },
        .void => return error.UnsupportedExecutableFeature,
    }
}

fn writeArraySummary(
    allocator: std.mem.Allocator,
    writer: anytype,
    globals: *std.array_list.Managed([]const u8),
    value_name: []const u8,
    string_state: *usize,
    temp_counter: *usize,
) !void {
    try writePrintLiteral(allocator, writer, globals, "array(len: ", string_state, temp_counter);
    const temp_index = temp_counter.*;
    temp_counter.* += 1;
    try writer.print("  %print.array.ptr.{d} = inttoptr i64 {s} to ptr\n", .{ temp_index, value_name });
    try writer.print("  %print.array.len.{d} = call i64 @\"kira_array_len\"(ptr %print.array.ptr.{d})\n", .{ temp_index, temp_index });
    try writer.print("  call void @\"kira_native_write_i64\"(i64 %print.array.len.{d})\n", .{temp_index});
    try writePrintLiteral(allocator, writer, globals, ")", string_state, temp_counter);
}

fn writeBooleanValue(writer: anytype, value_name: []const u8, temp_counter: *usize) !void {
    const temp_index = temp_counter.*;
    temp_counter.* += 1;
    try writer.print("  %print.bool.ptr.{d} = select i1 {s}, ptr @kira_bool_true, ptr @kira_bool_false\n", .{ temp_index, value_name });
    try writer.print("  %print.bool.val.{d} = load %kira.string, ptr %print.bool.ptr.{d}\n", .{ temp_index, temp_index });
    try writer.print("  %print.str.ptr.{d} = extractvalue %kira.string %print.bool.val.{d}, 0\n", .{ temp_index, temp_index });
    try writer.print("  %print.str.len.{d} = extractvalue %kira.string %print.bool.val.{d}, 1\n", .{ temp_index, temp_index });
    try writer.print("  call void @\"kira_native_write_string\"(ptr %print.str.ptr.{d}, i64 %print.str.len.{d})\n", .{ temp_index, temp_index });
}

fn writeStringValue(writer: anytype, value_name: []const u8, temp_counter: *usize) !void {
    const temp_index = temp_counter.*;
    temp_counter.* += 1;
    try writer.print("  %print.str.ptr.{d} = extractvalue %kira.string {s}, 0\n", .{ temp_index, value_name });
    try writer.print("  %print.str.len.{d} = extractvalue %kira.string {s}, 1\n", .{ temp_index, value_name });
    try writer.print("  call void @\"kira_native_write_string\"(ptr %print.str.ptr.{d}, i64 %print.str.len.{d})\n", .{ temp_index, temp_index });
}

fn writePrintLiteral(
    allocator: std.mem.Allocator,
    writer: anytype,
    globals: *std.array_list.Managed([]const u8),
    value: []const u8,
    string_state: *usize,
    temp_counter: *usize,
) !void {
    const string_index = string_state.*;
    string_state.* += 1;
    try appendStringGlobals(allocator, globals, string_index, value);
    const temp_index = temp_counter.*;
    temp_counter.* += 1;
    try writer.print("  %print.literal.{d} = load %kira.string, ptr @kira_str_{d}\n", .{ temp_index, string_index });
    const literal_name = try std.fmt.allocPrint(allocator, "%print.literal.{d}", .{temp_index});
    defer allocator.free(literal_name);
    try writeStringValue(writer, literal_name, temp_counter);
}

pub fn appendStringGlobals(
    allocator: std.mem.Allocator,
    globals: *std.array_list.Managed([]const u8),
    index: usize,
    value: []const u8,
) !void {
    const data_name = try std.fmt.allocPrint(allocator, "kira_str_{d}_data", .{index});
    defer allocator.free(data_name);
    const struct_name = try std.fmt.allocPrint(allocator, "kira_str_{d}", .{index});
    defer allocator.free(struct_name);

    var data_line: std.Io.Writer.Allocating = .init(allocator);
    errdefer data_line.deinit();
    var data_writer = &data_line.writer;
    try data_writer.print("@{s} = private unnamed_addr constant [{d} x i8] c\"", .{ data_name, value.len + 1 });
    try writeLlvmStringLiteral(data_writer, value);
    try data_writer.writeAll("\\00\"\n");
    try globals.append(try data_line.toOwnedSlice());

    var struct_line: std.Io.Writer.Allocating = .init(allocator);
    errdefer struct_line.deinit();
    var struct_writer = &struct_line.writer;
    try struct_writer.print("@{s} = private unnamed_addr constant %kira.string {{ ptr getelementptr inbounds ([{d} x i8], ptr @{s}, i64 0, i64 0), i64 {d} }}\n", .{
        struct_name,
        value.len + 1,
        data_name,
        value.len,
    });
    try globals.append(try struct_line.toOwnedSlice());
}

pub fn writeLlvmSymbol(writer: anytype, symbol: []const u8) !void {
    try writer.writeAll("@\"");
    try writeLlvmEscapedBytes(writer, symbol);
    try writer.writeByte('"');
}

pub fn writeLlvmStringLiteral(writer: anytype, bytes: []const u8) !void {
    try writeLlvmEscapedBytes(writer, bytes);
}

pub fn writeLlvmEscapedBytes(writer: anytype, bytes: []const u8) !void {
    for (bytes) |byte| {
        if (byte >= 0x20 and byte <= 0x7e and byte != '\\' and byte != '"') {
            try writer.writeByte(byte);
        } else {
            try writer.writeByte('\\');
            try writer.writeByte(hexDigit(byte >> 4));
            try writer.writeByte(hexDigit(byte & 0x0f));
        }
    }
}

pub fn writeLlvmFloatLiteral(writer: anytype, value: f64) !void {
    const truncated = @trunc(value);
    if (truncated == value and value >= @as(f64, @floatFromInt(std.math.minInt(i64))) and value <= @as(f64, @floatFromInt(std.math.maxInt(i64)))) {
        try writer.print("{d}.0", .{@as(i64, @intFromFloat(value))});
        return;
    }
    try writer.print("{d}", .{value});
}

pub fn hexDigit(value: u8) u8 {
    const index: usize = @intCast(value & 0x0f);
    return "0123456789ABCDEF"[index];
}

pub fn appendTypeDefinitions(allocator: std.mem.Allocator, writer: anytype, program: *const ir.Program) !void {
    for (program.types) |type_decl| {
        if (type_decl.ffi) |ffi_info| {
            if (ffi_info != .ffi_struct) continue;
        }
        try writer.writeAll(typeRefName(type_decl.name));
        try writer.writeAll(" = type { ");
        if (type_decl.fields.len == 0) {
            try writer.writeAll("i8");
        } else {
            for (type_decl.fields, 0..) |field_decl, index| {
                if (index != 0) try writer.writeAll(", ");
                try writer.writeAll(try llvmFieldAbiTypeText(allocator, program, field_decl.ty));
            }
        }
        try writer.writeAll(" }\n");
    }
    if (program.types.len > 0) try writer.writeByte('\n');
}

pub fn typeRefName(name: []const u8) []const u8 {
    return switch (name.len) {
        0 => "%t.anon",
        else => std.fmt.allocPrint(std.heap.page_allocator, "%t.{s}", .{name}) catch "%t.invalid",
    };
}

pub fn findTypeDecl(program: *const ir.Program, name: []const u8) ?ir.TypeDecl {
    for (program.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
    }
    return null;
}

pub fn findEnumDecl(program: *const ir.Program, name: []const u8) ?ir.EnumTypeDecl {
    for (program.enums) |enum_decl| {
        if (std.mem.eql(u8, enum_decl.name, name)) return enum_decl;
    }
    return null;
}

pub fn llvmFieldAbiTypeText(allocator: std.mem.Allocator, program: *const ir.Program, value_type: ir.ValueType) ![]const u8 {
    switch (value_type.kind) {
        .void => return allocator.dupe(u8, "void"),
        .string => return allocator.dupe(u8, "%kira.string"),
        .boolean => return allocator.dupe(u8, "i8"),
        .construct_any, .raw_ptr, .enum_instance => {
            if (value_type.name) |name| {
                if (findTypeDecl(program, name)) |type_decl| {
                    if (type_decl.ffi) |ffi_info| {
                        return switch (ffi_info) {
                            .array => |info| std.fmt.allocPrint(allocator, "[{d} x {s}]", .{ info.count, try llvmFieldAbiTypeText(allocator, program, info.element) }),
                            .alias => |info| llvmFieldAbiTypeText(allocator, program, info.target),
                            else => allocator.dupe(u8, "ptr"),
                        };
                    }
                }
            }
            return allocator.dupe(u8, "ptr");
        },
        .integer => return allocator.dupe(u8, integerAbiTypeName(value_type.name)),
        .float => return allocator.dupe(u8, floatAbiTypeName(value_type.name)),
        .array => return allocator.dupe(u8, "ptr"),
        .ffi_struct => return allocator.dupe(u8, typeRefName(value_type.name orelse return error.UnsupportedExecutableFeature)),
    }
}

pub fn integerAbiTypeName(name: ?[]const u8) []const u8 {
    const value = name orelse "I64";
    if (std.mem.eql(u8, value, "I8") or std.mem.eql(u8, value, "U8")) return "i8";
    if (std.mem.eql(u8, value, "I16") or std.mem.eql(u8, value, "U16")) return "i16";
    if (std.mem.eql(u8, value, "I32") or std.mem.eql(u8, value, "U32")) return "i32";
    return "i64";
}

pub fn floatAbiTypeName(name: ?[]const u8) []const u8 {
    if (name) |value| {
        if (std.mem.eql(u8, value, "F32")) return "float";
        if (std.mem.eql(u8, value, "F64")) return "double";
    }
    return "double";
}

pub fn llvmValueTypeText(value_type: ir.ValueType) []const u8 {
    return switch (value_type.kind) {
        .void => "void",
        .integer => "i64",
        .float => floatAbiTypeName(value_type.name),
        .string => "%kira.string",
        .boolean => "i1",
        .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => "i64",
    };
}

pub fn llvmCompareValueTypeText(value_type: ir.ValueType) []const u8 {
    return switch (value_type.kind) {
        .integer, .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => "i64",
        .float => floatAbiTypeName(value_type.name),
        .boolean => "i1",
        else => unreachable,
    };
}

pub fn llvmComparePredicate(value_type: ir.ValueType, op: ir.CompareOp) ![]const u8 {
    return switch (value_type.kind) {
        .integer => switch (op) {
            .equal => "eq",
            .not_equal => "ne",
            .less => "slt",
            .less_equal => "sle",
            .greater => "sgt",
            .greater_equal => "sge",
        },
        .float => switch (op) {
            .equal => "oeq",
            .not_equal => "one",
            .less => "olt",
            .less_equal => "ole",
            .greater => "ogt",
            .greater_equal => "oge",
        },
        .boolean, .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => switch (op) {
            .equal => "eq",
            .not_equal => "ne",
            else => error.UnsupportedExecutableFeature,
        },
        else => error.UnsupportedExecutableFeature,
    };
}

pub fn llvmCallTypeText(value_type: ir.ValueType, is_extern: bool) []const u8 {
    if (!is_extern) return llvmValueTypeText(value_type);
    return switch (value_type.kind) {
        .construct_any, .array, .raw_ptr, .enum_instance => "ptr",
        .integer => integerAbiTypeName(value_type.name),
        .float => floatAbiTypeName(value_type.name),
        .boolean => "i1",
        .ffi_struct => typeRefName(value_type.name orelse "anon"),
        else => llvmValueTypeText(value_type),
    };
}

pub fn externReturnUsesSRet(program: *const ir.Program, value_type: ir.ValueType) bool {
    if (builtin.os.tag != .windows) return false;
    if (value_type.kind != .ffi_struct) return false;
    const layout = valueTypeLayout(program, value_type) catch return false;
    return layout.size > 8;
}

pub fn externReturnAbiTypeText(allocator: std.mem.Allocator, program: *const ir.Program, value_type: ir.ValueType) ![]const u8 {
    if (value_type.kind == .ffi_struct and !externReturnUsesSRet(program, value_type)) {
        const layout = try valueTypeLayout(program, value_type);
        const bits = @max(layout.size, 1) * 8;
        return std.fmt.allocPrint(allocator, "i{d}", .{bits});
    }
    return allocator.dupe(u8, llvmCallTypeText(value_type, true));
}

pub fn externParamAbiTypeText(allocator: std.mem.Allocator, program: *const ir.Program, value_type: ir.ValueType) ![]const u8 {
    if (value_type.kind == .ffi_struct) {
        const layout = try valueTypeLayout(program, value_type);
        if (layout.size <= 8) {
            return std.fmt.allocPrint(allocator, "i{d}", .{@max(layout.size, 1) * 8});
        }
    }
    return allocator.dupe(u8, llvmCallTypeText(value_type, true));
}

pub fn llvmLocalStorageTypeText(allocator: std.mem.Allocator, program: *const ir.Program, value_type: ir.ValueType) ![]const u8 {
    _ = program;
    return switch (value_type.kind) {
        .construct_any, .array, .ffi_struct, .enum_instance => allocator.dupe(u8, "i64"),
        else => allocator.dupe(u8, llvmValueTypeText(value_type)),
    };
}

pub fn isPointerLikeValueType(value_type: ir.ValueType) bool {
    return value_type.kind == .construct_any or value_type.kind == .array or value_type.kind == .raw_ptr or value_type.kind == .ffi_struct or value_type.kind == .enum_instance;
}

pub fn fieldIndex(program: *const ir.Program, owner_type_name: []const u8, field_name: []const u8) ?usize {
    const type_decl = findTypeDecl(program, owner_type_name) orelse return null;
    for (type_decl.fields, 0..) |field_decl, index| {
        if (std.mem.eql(u8, field_decl.name, field_name)) return index;
    }
    return null;
}

pub fn fieldType(program: *const ir.Program, owner_type_name: []const u8, field_name: []const u8) ?ir.ValueType {
    const type_decl = findTypeDecl(program, owner_type_name) orelse return null;
    for (type_decl.fields) |field_decl| {
        if (std.mem.eql(u8, field_decl.name, field_name)) return field_decl.ty;
    }
    return null;
}

pub fn llvmIndirectLoadTypeText(allocator: std.mem.Allocator, program: *const ir.Program, value_type: ir.ValueType) ![]const u8 {
    return switch (value_type.kind) {
        .integer, .float, .boolean, .construct_any, .raw_ptr, .ffi_struct, .enum_instance => llvmFieldAbiTypeText(allocator, program, value_type),
        else => allocator.dupe(u8, llvmValueTypeText(value_type)),
    };
}

pub fn llvmFieldStoreValuePrefix(writer: anytype, dst_reg: u32) !void {
    try writer.writeAll("  %r");
    try writer.print("{d}", .{dst_reg});
    try writer.writeAll(" = ");
}

pub fn writeLlvmLabelName(writer: anytype, label: u32) !void {
    try writer.print("kira_label_{d}", .{label});
}

pub fn countStringConstants(function_decl: ir.Function) usize {
    var count: usize = 0;
    for (function_decl.instructions) |instruction| {
        switch (instruction) {
            .const_string => count += 1,
            else => {},
        }
    }
    return count;
}

const TypeLayout = struct {
    size: usize,
    alignment: usize,
};

// Byte size of a value type under the native ABI. Used by the C-API backend's FFI
// marshalling to decide small-struct-as-integer passing, mirroring the text backend's
// externParamAbiTypeText/externReturnAbiTypeText sizing.
pub fn valueAbiSize(program: *const ir.Program, value_type: ir.ValueType) !usize {
    return (try valueTypeLayout(program, value_type)).size;
}

fn valueTypeLayout(program: *const ir.Program, value_type: ir.ValueType) anyerror!TypeLayout {
    return switch (value_type.kind) {
        .void => .{ .size = 0, .alignment = 1 },
        .boolean => .{ .size = 1, .alignment = 1 },
        .integer => integerLayout(value_type.name),
        .float => if (value_type.name != null and std.mem.eql(u8, value_type.name.?, "F32"))
            .{ .size = 4, .alignment = 4 }
        else
            .{ .size = 8, .alignment = 8 },
        .string => .{ .size = 16, .alignment = 8 },
        .construct_any, .array, .raw_ptr, .enum_instance => .{ .size = @sizeOf(usize), .alignment = @alignOf(usize) },
        .ffi_struct => try ffiTypeLayout(program, value_type.name orelse return error.UnsupportedExecutableFeature),
    };
}

fn integerLayout(name: ?[]const u8) TypeLayout {
    const value = name orelse return .{ .size = 8, .alignment = 8 };
    if (std.mem.eql(u8, value, "I8") or std.mem.eql(u8, value, "U8")) return .{ .size = 1, .alignment = 1 };
    if (std.mem.eql(u8, value, "I16") or std.mem.eql(u8, value, "U16")) return .{ .size = 2, .alignment = 2 };
    if (std.mem.eql(u8, value, "I32") or std.mem.eql(u8, value, "U32")) return .{ .size = 4, .alignment = 4 };
    return .{ .size = 8, .alignment = 8 };
}

fn ffiTypeLayout(program: *const ir.Program, type_name: []const u8) anyerror!TypeLayout {
    const type_decl = findTypeDecl(program, type_name) orelse return error.UnsupportedExecutableFeature;
    if (type_decl.ffi) |ffi_info| {
        switch (ffi_info) {
            .ffi_struct => {},
            .pointer, .callback => return .{ .size = @sizeOf(usize), .alignment = @alignOf(usize) },
            .alias => |alias| return valueTypeLayout(program, alias.target),
            .array => |array| {
                const element = try valueTypeLayout(program, array.element);
                return .{
                    .size = alignForward(element.size, element.alignment) * array.count,
                    .alignment = element.alignment,
                };
            },
        }
    }

    var offset: usize = 0;
    var max_alignment: usize = 1;
    if (type_decl.fields.len == 0) {
        return .{ .size = 1, .alignment = 1 };
    }
    for (type_decl.fields) |field| {
        const field_layout = try valueTypeLayout(program, field.ty);
        max_alignment = @max(max_alignment, field_layout.alignment);
        offset = alignForward(offset, field_layout.alignment);
        offset += field_layout.size;
    }
    return .{
        .size = alignForward(offset, max_alignment),
        .alignment = max_alignment,
    };
}

fn alignForward(value: usize, alignment: usize) usize {
    if (alignment <= 1) return value;
    return std.mem.alignForward(usize, value, alignment);
}

pub fn bridgeTagValue(value_type: ir.ValueType) u8 {
    return switch (value_type.kind) {
        .void => 0,
        .integer => 1,
        .float => 2,
        .string => 3,
        .boolean => 4,
        .construct_any, .array, .raw_ptr, .ffi_struct, .enum_instance => 5,
    };
}

pub fn requiresTextIrFallback(program: ir.Program, mode: backend_api.BackendMode) bool {
    return platform_utils.requiresTextIrFallback(shouldLowerFunction, functionExecutionById, program, mode);
}

pub fn functionDeclNeedsTextIrFallback(program: ir.Program, function_decl: ir.Function, mode: backend_api.BackendMode) bool {
    return platform_utils.functionDeclNeedsTextIrFallback(shouldLowerFunction, functionExecutionById, program, function_decl, mode);
}
