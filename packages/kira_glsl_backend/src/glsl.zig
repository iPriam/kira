const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const shader_model = @import("kira_shader_model");
const shader_ir = @import("kira_shader_ir");

pub const LoweredShader = struct {
    shader_name: []const u8,
    vertex_source: ?[]const u8 = null,
    fragment_source: ?[]const u8 = null,
};

pub fn lowerShader(
    allocator: std.mem.Allocator,
    program: shader_ir.Program,
    shader_decl: shader_ir.ShaderDecl,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !LoweredShader {
    if (shader_decl.kind == .compute) {
        try diagnostics.appendOwned(allocator, out_diagnostics, .{
            .severity = .@"error",
            .code = "KSL121",
            .title = "compute shaders are not supported by the GLSL 330 backend",
            .message = "The current KSL backend targets GLSL 330 for the repo's existing Sokol/OpenGL graphics path, and that path does not support compute shaders.",
            .help = "Use a graphics shader for the GLSL 330 backend, or add a future compute-capable backend before trying to build this shader.",
        });
        return error.DiagnosticsEmitted;
    }

    const vertex_stage = findStage(shader_decl.stages, .vertex) orelse return error.InvalidArguments;
    const fragment_stage = findStage(shader_decl.stages, .fragment);

    var lowerer = Lowerer{
        .allocator = allocator,
        .program = &program,
        .shader = &shader_decl,
    };

    return .{
        .shader_name = shader_decl.name,
        .vertex_source = try lowerer.emitStage(vertex_stage, fragment_stage),
        .fragment_source = if (fragment_stage) |stage| try lowerer.emitStage(stage, null) else null,
    };
}

const Lowerer = struct {
    allocator: std.mem.Allocator,
    program: *const shader_ir.Program,
    shader: *const shader_ir.ShaderDecl,

    fn emitStage(self: *Lowerer, stage: shader_ir.StageDecl, paired_fragment: ?shader_ir.StageDecl) ![]const u8 {
        var out: std.Io.Writer.Allocating = .init(self.allocator);
        try out.writer.writeAll("#version 330 core\n\n");
        try self.emitStructs(&out.writer);
        try self.emitOptions(&out.writer);
        try self.emitResources(&out.writer, stage.kind);
        try self.emitStageIo(&out.writer, stage, paired_fragment);
        try self.emitHelpers(&out.writer);
        try self.emitFunction(&out.writer, stage.entry);
        try self.emitMain(&out.writer, stage);
        return out.toOwnedSlice();
    }

    fn emitStructs(self: *Lowerer, writer: anytype) !void {
        for (self.program.types) |type_decl| {
            try writer.print("struct {s} {{\n", .{sanitizeName(type_decl.name)});
            for (type_decl.fields) |field_decl| {
                try writer.print("    {s} {s};\n", .{ glslTypeName(field_decl.ty), sanitizeName(field_decl.name) });
            }
            try writer.writeAll("};\n\n");
        }
    }

    fn emitOptions(self: *Lowerer, writer: anytype) !void {
        for (self.shader.options) |option_decl| {
            try writer.print("const {s} {s} = ", .{ glslTypeName(option_decl.ty), sanitizeName(option_decl.name) });
            try emitConstValue(writer, option_decl.default_value);
            try writer.writeAll(";\n");
        }
        if (self.shader.options.len != 0) try writer.writeByte('\n');
    }

    fn emitResources(self: *Lowerer, writer: anytype, stage: shader_model.Stage) !void {
        _ = stage;
        for (self.shader.groups) |group_decl| {
            for (group_decl.resources) |resource_decl| {
                switch (resource_decl.kind) {
                    .uniform => {
                        try writer.print("layout(std140) uniform {s}_Block {{\n", .{sanitizeName(resourceBlockName(group_decl.name, resource_decl.name))});
                        if (resource_decl.ty == .struct_ref) {
                            const type_decl = findType(self.program.types, resource_decl.ty.struct_ref) orelse return error.InvalidArguments;
                            for (type_decl.fields) |field_decl| {
                                try writer.print("    {s} {s};\n", .{ glslTypeName(field_decl.ty), sanitizeName(field_decl.name) });
                            }
                        } else {
                            try writer.print("    {s} value;\n", .{glslTypeName(resource_decl.ty)});
                        }
                        try writer.print("}} {s};\n\n", .{sanitizeName(resource_decl.name)});
                    },
                    .texture => {},
                    .sampler => {},
                    .storage => {
                        // Storage resources stay in reflection for now; GLSL 330 graphics lowering does not emit them.
                    },
                }
            }
        }

        for (self.shader.groups) |group_decl| {
            var textures = std.array_list.Managed(shader_ir.ResourceDecl).init(self.allocator);
            var samplers = std.array_list.Managed(shader_ir.ResourceDecl).init(self.allocator);
            for (group_decl.resources) |resource_decl| switch (resource_decl.kind) {
                .texture => try textures.append(resource_decl),
                .sampler => try samplers.append(resource_decl),
                else => {},
            };
            for (textures.items) |texture_decl| {
                for (samplers.items) |sampler_decl| {
                    if (texture_decl.ty != .texture or sampler_decl.ty != .sampler) continue;
                    try writer.print("uniform {s} {s};\n", .{
                        glslSamplerType(texture_decl.ty.texture),
                        sanitizeName(sampledUniformName(texture_decl.name, sampler_decl.name)),
                    });
                }
            }
            if (textures.items.len != 0 or samplers.items.len != 0) try writer.writeByte('\n');
        }
    }

    fn emitStageIo(self: *Lowerer, writer: anytype, stage: shader_ir.StageDecl, paired_fragment: ?shader_ir.StageDecl) !void {
        if (stage.kind == .vertex) {
            const input_type = findType(self.program.types, stage.input_type.?) orelse return error.InvalidArguments;
            var location: u32 = 0;
            for (input_type.fields) |field_decl| {
                if (field_decl.builtin != null) continue;
                try writer.print("layout(location = {d}) in {s} {s};\n", .{
                    location,
                    glslTypeName(field_decl.ty),
                    try prefixedName(self.allocator, "kira_attr_", field_decl.name),
                });
                location += 1;
            }

            const output_type = findType(self.program.types, stage.output_type.?) orelse return error.InvalidArguments;
            for (output_type.fields) |field_decl| {
                if (field_decl.builtin != null) continue;
                try writer.print("out {s} {s};\n", .{
                    glslTypeName(field_decl.ty),
                    try prefixedName(self.allocator, "kira_varying_", field_decl.name),
                });
            }
            try writer.writeByte('\n');
            return;
        }

        if (stage.kind == .fragment) {
            const input_type = findType(self.program.types, stage.input_type.?) orelse return error.InvalidArguments;
            for (input_type.fields) |field_decl| {
                if (field_decl.builtin != null) continue;
                try writer.print("in {s} {s};\n", .{
                    glslTypeName(field_decl.ty),
                    try prefixedName(self.allocator, "kira_varying_", field_decl.name),
                });
            }

            const output_type_name = if (stage.output_type) |name| name else if (paired_fragment) |fragment_stage| fragment_stage.output_type.? else return;
            const output_type = findType(self.program.types, output_type_name) orelse return error.InvalidArguments;
            var location: u32 = 0;
            for (output_type.fields) |field_decl| {
                try writer.print("layout(location = {d}) out {s} {s};\n", .{
                    location,
                    glslTypeName(field_decl.ty),
                    try prefixedName(self.allocator, "kira_frag_", field_decl.name),
                });
                location += 1;
            }
            try writer.writeByte('\n');
        }
    }

    fn emitHelpers(self: *Lowerer, writer: anytype) !void {
        for (self.program.functions) |function_decl| {
            try self.emitFunction(writer, function_decl);
        }
    }

    fn emitFunction(self: *Lowerer, writer: anytype, function_decl: shader_ir.FunctionDecl) !void {
        _ = self;
        try writer.print("{s} {s}(", .{ glslTypeName(function_decl.return_type), sanitizeName(function_decl.name) });
        for (function_decl.params, 0..) |param_decl, index| {
            if (index != 0) try writer.writeAll(", ");
            try writer.print("{s} {s}", .{ glslTypeName(param_decl.ty), sanitizeName(param_decl.name) });
        }
        try writer.writeAll(") ");
        try emitBlock(writer, function_decl.body, 0);
        try writer.writeAll("\n\n");
    }

    fn emitMain(self: *Lowerer, writer: anytype, stage: shader_ir.StageDecl) !void {
        try writer.writeAll("void main() {\n");
        if (stage.input_type) |input_type_name| {
            const input_type = findType(self.program.types, input_type_name) orelse return error.InvalidArguments;
            try writer.print("    {s} {s};\n", .{ sanitizeName(input_type.name), "kira_input" });
            for (input_type.fields) |field_decl| {
                const target = if (field_decl.builtin) |builtin| switch (builtin) {
                    .vertex_index => "uint(gl_VertexID)",
                    .instance_index => "uint(gl_InstanceID)",
                    .front_facing => "gl_FrontFacing",
                    .frag_coord => "gl_FragCoord",
                    else => continue,
                } else if (stage.kind == .vertex)
                    try prefixedName(self.allocator, "kira_attr_", field_decl.name)
                else
                    try prefixedName(self.allocator, "kira_varying_", field_decl.name);
                try writer.print("    kira_input.{s} = {s};\n", .{ sanitizeName(field_decl.name), target });
            }
        }

        if (stage.output_type) |output_type_name| {
            try writer.print("    {s} kira_output = {s}({s});\n", .{
                sanitizeName(output_type_name),
                sanitizeName(stage.entry.name),
                if (stage.input_type != null) "kira_input" else "",
            });
            if (stage.kind == .vertex) {
                const output_type = findType(self.program.types, output_type_name) orelse return error.InvalidArguments;
                for (output_type.fields) |field_decl| {
                    if (field_decl.builtin == .position) {
                        try writer.print("    gl_Position = kira_output.{s};\n", .{sanitizeName(field_decl.name)});
                    } else {
                        try writer.print("    {s} = kira_output.{s};\n", .{
                            try prefixedName(self.allocator, "kira_varying_", field_decl.name),
                            sanitizeName(field_decl.name),
                        });
                    }
                }
            } else if (stage.kind == .fragment) {
                const output_type = findType(self.program.types, output_type_name) orelse return error.InvalidArguments;
                for (output_type.fields) |field_decl| {
                    try writer.print("    {s} = kira_output.{s};\n", .{
                        try prefixedName(self.allocator, "kira_frag_", field_decl.name),
                        sanitizeName(field_decl.name),
                    });
                }
            }
        } else {
            try writer.print("    {s}({s});\n", .{
                sanitizeName(stage.entry.name),
                if (stage.input_type != null) "kira_input" else "",
            });
        }
        try writer.writeAll("}\n");
    }
};

fn emitBlock(writer: anytype, block: shader_ir.Block, indent_level: usize) anyerror!void {
    try writer.writeAll("{\n");
    for (block.statements) |statement| {
        try emitIndent(writer, indent_level + 1);
        try emitStatement(writer, statement, indent_level + 1);
    }
    try emitIndent(writer, indent_level);
    try writer.writeAll("}");
}

fn emitStatement(writer: anytype, statement: shader_ir.Statement, indent_level: usize) anyerror!void {
    switch (statement) {
        .let_stmt => |let_stmt| {
            try writer.print("{s} {s}", .{ glslTypeName(let_stmt.ty), sanitizeName(let_stmt.name) });
            if (let_stmt.value) |value| {
                try writer.writeAll(" = ");
                try emitExpr(writer, value);
            }
            try writer.writeAll(";\n");
        },
        .assign_stmt => |assign_stmt| {
            try emitExpr(writer, assign_stmt.target);
            try writer.writeAll(" = ");
            try emitExpr(writer, assign_stmt.value);
            try writer.writeAll(";\n");
        },
        .expr_stmt => |expr_stmt| {
            try emitExpr(writer, expr_stmt.expr);
            try writer.writeAll(";\n");
        },
        .return_stmt => |return_stmt| {
            if (return_stmt.value) |value| {
                try writer.writeAll("return ");
                try emitExpr(writer, value);
                try writer.writeAll(";\n");
            } else {
                try writer.writeAll("return;\n");
            }
        },
        .if_stmt => |if_stmt| {
            try writer.writeAll("if (");
            try emitExpr(writer, if_stmt.condition);
            try writer.writeAll(") ");
            try emitBlock(writer, if_stmt.then_block, indent_level);
            if (if_stmt.else_block) |else_block| {
                try writer.writeAll(" else ");
                try emitBlock(writer, else_block, indent_level);
            }
            try writer.writeAll("\n");
        },
    }
}

fn emitExpr(writer: anytype, expr: *const shader_ir.Expr) anyerror!void {
    switch (expr.node) {
        .const_value => |value| switch (value) {
            .bool => |bool_value| try writer.writeAll(if (bool_value) "true" else "false"),
            .int => |int_value| try writer.print("{d}", .{int_value}),
            .uint => |uint_value| try writer.print("{d}u", .{uint_value}),
            .float => |float_value| try writer.print("{d}", .{float_value}),
        },
        .name => |name_ref| try writer.writeAll(sanitizeName(name_ref.name)),
        .unary => |unary_expr| {
            try writer.writeAll(switch (unary_expr.op) {
                .neg => "-",
                .not => "!",
            });
            try emitExpr(writer, unary_expr.operand);
        },
        .binary => |binary_expr| {
            try writer.writeByte('(');
            try emitExpr(writer, binary_expr.left);
            try writer.writeAll(switch (binary_expr.op) {
                .add => " + ",
                .sub => " - ",
                .mul => " * ",
                .div => " / ",
                .less => " < ",
                .less_equal => " <= ",
                .greater => " > ",
                .greater_equal => " >= ",
                .equal => " == ",
                .not_equal => " != ",
            });
            try emitExpr(writer, binary_expr.right);
            try writer.writeByte(')');
        },
        .member => |member_expr| {
            if (expr.ty == .scalar and std.mem.eql(u8, member_expr.name, "count")) {
                try emitExpr(writer, member_expr.object);
                try writer.writeAll(".length()");
            } else {
                try emitExpr(writer, member_expr.object);
                try writer.print(".{s}", .{sanitizeName(member_expr.name)});
            }
        },
        .index => |index_expr| {
            try emitExpr(writer, index_expr.object);
            try writer.writeByte('[');
            try emitExpr(writer, index_expr.index);
            try writer.writeByte(']');
        },
        .call => |call_expr| switch (call_expr.callee) {
            .constructor => |ty| {
                try writer.print("{s}(", .{glslTypeName(ty)});
                try emitCallArgs(writer, call_expr.args);
                try writer.writeByte(')');
            },
            .function => |function_name| {
                try writer.print("{s}(", .{sanitizeName(function_name.name)});
                try emitCallArgs(writer, call_expr.args);
                try writer.writeByte(')');
            },
            .intrinsic => |intrinsic| switch (intrinsic) {
                .mul => {
                    try writer.writeByte('(');
                    try emitExpr(writer, call_expr.args[0]);
                    try writer.writeAll(" * ");
                    try emitExpr(writer, call_expr.args[1]);
                    try writer.writeByte(')');
                },
                .normalize => {
                    try writer.writeAll("normalize(");
                    try emitCallArgs(writer, call_expr.args);
                    try writer.writeByte(')');
                },
                .dot => {
                    try writer.writeAll("dot(");
                    try emitCallArgs(writer, call_expr.args);
                    try writer.writeByte(')');
                },
                .sample => {
                    const texture_name = switch (call_expr.args[0].node) {
                        .name => |name_ref| name_ref.name,
                        else => "unsupported_texture",
                    };
                    const sampler_name = switch (call_expr.args[1].node) {
                        .name => |name_ref| name_ref.name,
                        else => "unsupported_sampler",
                    };
                    try writer.print("texture({s}, ", .{sanitizeName(sampledUniformName(texture_name, sampler_name))});
                    try emitExpr(writer, call_expr.args[2]);
                    try writer.writeByte(')');
                },
            },
        },
    }
}

fn emitCallArgs(writer: anytype, args: []const *shader_ir.Expr) anyerror!void {
    for (args, 0..) |arg, index| {
        if (index != 0) try writer.writeAll(", ");
        try emitExpr(writer, arg);
    }
}

fn emitConstValue(writer: anytype, value: shader_ir.ConstValue) !void {
    switch (value) {
        .bool => |bool_value| try writer.writeAll(if (bool_value) "true" else "false"),
        .int => |int_value| try writer.print("{d}", .{int_value}),
        .uint => |uint_value| try writer.print("{d}u", .{uint_value}),
        .float => |float_value| try writer.print("{d}", .{float_value}),
    }
}

fn emitIndent(writer: anytype, level: usize) !void {
    for (0..level) |_| try writer.writeAll("    ");
}

fn glslTypeName(ty: shader_model.Type) []const u8 {
    return switch (ty) {
        .void => "void",
        .scalar => switch (ty.scalar) {
            .bool => "bool",
            .int => "int",
            .uint => "uint",
            .float => "float",
        },
        .vector => switch (ty.vector.scalar) {
            .float => switch (ty.vector.width) {
                2 => "vec2",
                3 => "vec3",
                else => "vec4",
            },
            .int => switch (ty.vector.width) {
                2 => "ivec2",
                3 => "ivec3",
                else => "ivec4",
            },
            .uint => switch (ty.vector.width) {
                2 => "uvec2",
                3 => "uvec3",
                else => "uvec4",
            },
            .bool => switch (ty.vector.width) {
                2 => "bvec2",
                3 => "bvec3",
                else => "bvec4",
            },
        },
        .matrix => "mat4",
        .struct_ref => sanitizeName(ty.struct_ref),
        .texture => glslSamplerType(ty.texture),
        .sampler => "sampler",
        .runtime_array => glslTypeName(ty.runtime_array.*),
    };
}

fn glslSamplerType(texture: shader_model.TextureDimension) []const u8 {
    return switch (texture) {
        .texture_2d => "sampler2D",
        .texture_cube => "samplerCube",
        .depth_2d => "sampler2DShadow",
    };
}

fn findStage(stages: []const shader_ir.StageDecl, stage: shader_model.Stage) ?shader_ir.StageDecl {
    for (stages) |stage_decl| {
        if (stage_decl.kind == stage) return stage_decl;
    }
    return null;
}

fn findType(types: []const shader_ir.TypeDecl, name: []const u8) ?shader_ir.TypeDecl {
    for (types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
    }
    return null;
}

fn sanitizeName(name: []const u8) []const u8 {
    if (reservedGlslReplacement(name)) |replacement| return replacement;
    return name;
}

fn reservedGlslReplacement(name: []const u8) ?[]const u8 {
    const reserved = [_]struct { name: []const u8, replacement: []const u8 }{
        .{ .name = "attribute", .replacement = "kira_attribute" },
        .{ .name = "const", .replacement = "kira_const" },
        .{ .name = "in", .replacement = "kira_in" },
        .{ .name = "input", .replacement = "kira_input_param" },
        .{ .name = "inout", .replacement = "kira_inout" },
        .{ .name = "out", .replacement = "kira_out" },
        .{ .name = "output", .replacement = "kira_output_param" },
        .{ .name = "uniform", .replacement = "kira_uniform" },
        .{ .name = "varying", .replacement = "kira_varying" },
        .{ .name = "buffer", .replacement = "kira_buffer" },
        .{ .name = "shared", .replacement = "kira_shared" },
        .{ .name = "coherent", .replacement = "kira_coherent" },
        .{ .name = "volatile", .replacement = "kira_volatile" },
        .{ .name = "restrict", .replacement = "kira_restrict" },
        .{ .name = "readonly", .replacement = "kira_readonly" },
        .{ .name = "writeonly", .replacement = "kira_writeonly" },
        .{ .name = "layout", .replacement = "kira_layout" },
        .{ .name = "centroid", .replacement = "kira_centroid" },
        .{ .name = "flat", .replacement = "kira_flat" },
        .{ .name = "smooth", .replacement = "kira_smooth" },
        .{ .name = "noperspective", .replacement = "kira_noperspective" },
        .{ .name = "patch", .replacement = "kira_patch" },
        .{ .name = "sample", .replacement = "kira_sample" },
        .{ .name = "break", .replacement = "kira_break" },
        .{ .name = "continue", .replacement = "kira_continue" },
        .{ .name = "do", .replacement = "kira_do" },
        .{ .name = "for", .replacement = "kira_for" },
        .{ .name = "while", .replacement = "kira_while" },
        .{ .name = "switch", .replacement = "kira_switch" },
        .{ .name = "case", .replacement = "kira_case" },
        .{ .name = "default", .replacement = "kira_default" },
        .{ .name = "if", .replacement = "kira_if" },
        .{ .name = "else", .replacement = "kira_else" },
        .{ .name = "subroutine", .replacement = "kira_subroutine" },
        .{ .name = "discard", .replacement = "kira_discard" },
        .{ .name = "return", .replacement = "kira_return" },
        .{ .name = "struct", .replacement = "kira_struct" },
        .{ .name = "void", .replacement = "kira_void" },
        .{ .name = "bool", .replacement = "kira_bool" },
        .{ .name = "int", .replacement = "kira_int" },
        .{ .name = "uint", .replacement = "kira_uint" },
        .{ .name = "float", .replacement = "kira_float" },
        .{ .name = "double", .replacement = "kira_double" },
        .{ .name = "vec2", .replacement = "kira_vec2" },
        .{ .name = "vec3", .replacement = "kira_vec3" },
        .{ .name = "vec4", .replacement = "kira_vec4" },
        .{ .name = "mat2", .replacement = "kira_mat2" },
        .{ .name = "mat3", .replacement = "kira_mat3" },
        .{ .name = "mat4", .replacement = "kira_mat4" },
        .{ .name = "sampler", .replacement = "kira_sampler" },
        .{ .name = "sampler2D", .replacement = "kira_sampler2D" },
        .{ .name = "samplerCube", .replacement = "kira_samplerCube" },
        .{ .name = "sampler2DShadow", .replacement = "kira_sampler2DShadow" },
        .{ .name = "true", .replacement = "kira_true" },
        .{ .name = "false", .replacement = "kira_false" },
    };
    for (reserved) |entry| {
        if (std.mem.eql(u8, name, entry.name)) return entry.replacement;
    }
    return null;
}

fn prefixedName(allocator: std.mem.Allocator, prefix: []const u8, name: []const u8) ![]const u8 {
    return std.fmt.allocPrint(allocator, "{s}{s}", .{ prefix, sanitizeName(name) });
}

fn resourceBlockName(group_name: []const u8, resource_name: []const u8) []const u8 {
    _ = group_name;
    return resource_name;
}

fn sampledUniformName(texture_name: []const u8, sampler_name: []const u8) []const u8 {
    _ = sampler_name;
    return texture_name;
}

test "sanitizes GLSL reserved local names" {
    try std.testing.expectEqualStrings("kira_out", sanitizeName("out"));
    try std.testing.expectEqualStrings("kira_input_param", sanitizeName("input"));
    try std.testing.expectEqualStrings("kira_output_param", sanitizeName("output"));
    try std.testing.expectEqualStrings("kira_inout", sanitizeName("inout"));
    try std.testing.expectEqualStrings("result", sanitizeName("result"));
}
