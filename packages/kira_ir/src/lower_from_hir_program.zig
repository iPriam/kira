const std = @import("std");
const ir = @import("ir.zig");
const model = @import("kira_semantics_model");
const runtime_abi = @import("kira_runtime_abi");
const parent = @import("lower_from_hir.zig");
const places = @import("lower_from_hir_places.zig");
const Lowerer = parent.Lowerer;
const lowerProgram = parent.lowerProgram;
const lowerResolvedType = parent.lowerResolvedType;

pub fn lowerEnumTypeDecls(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable_functions: std.AutoHashMapUnmanaged(u32, void),
) ![]ir.EnumTypeDecl {
    var referenced = std.StringHashMapUnmanaged(void){};
    defer referenced.deinit(allocator);

    for (program.functions) |function_decl| {
        if (!reachable_functions.contains(function_decl.id)) continue;
        for (function_decl.params) |param| try markReferencedType(allocator, program, &referenced, param.ty);
        try markReferencedType(allocator, program, &referenced, function_decl.return_type);
        for (function_decl.locals) |local| try markReferencedType(allocator, program, &referenced, local.ty);
        for (function_decl.body) |statement| try markReferencedTypesInStatement(allocator, program, &referenced, statement);
    }

    var enums = std.array_list.Managed(ir.EnumTypeDecl).init(allocator);
    for (program.enums) |enum_decl| {
        if (!referenced.contains(enum_decl.name)) continue;
        var variants = std.array_list.Managed(ir.EnumVariantIr).init(allocator);
        for (enum_decl.variants) |variant_decl| {
            try variants.append(.{
                .name = try allocator.dupe(u8, variant_decl.name),
                .discriminant = variant_decl.discriminant,
                .payload_ty = if (variant_decl.payload_ty) |payload_ty| try lowerResolvedType(program, payload_ty) else null,
            });
        }
        try enums.append(.{
            .name = try allocator.dupe(u8, enum_decl.name),
            .variants = try variants.toOwnedSlice(),
        });
    }
    return enums.toOwnedSlice();
}

fn markReferencedTypesInStatement(
    allocator: std.mem.Allocator,
    program: model.Program,
    referenced: *std.StringHashMapUnmanaged(void),
    statement: model.Statement,
) anyerror!void {
    switch (statement) {
        .let_stmt => |node| {
            try markReferencedType(allocator, program, referenced, node.ty);
            if (node.value) |value| try markReferencedTypesInExpr(allocator, program, referenced, value);
        },
        .assign_stmt => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.target);
            try markReferencedTypesInExpr(allocator, program, referenced, node.value);
        },
        .expr_stmt => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.expr),
        .if_stmt => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.condition);
            for (node.then_body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
            if (node.else_body) |else_body| for (else_body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
        },
        .for_stmt => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.iterator);
            for (node.body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
        },
        .while_stmt => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.condition);
            for (node.body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
        },
        .break_stmt, .continue_stmt => {},
        .match_stmt => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.subject);
            for (node.arms) |arm| {
                try markReferencedTypesInPattern(allocator, program, referenced, arm.pattern);
                if (arm.guard) |guard| try markReferencedTypesInExpr(allocator, program, referenced, guard);
                for (arm.body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
            }
        },
        .switch_stmt => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.subject);
            for (node.cases) |case_node| {
                try markReferencedTypesInExpr(allocator, program, referenced, case_node.pattern);
                for (case_node.body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
            }
            if (node.default_body) |default_body| for (default_body) |inner| try markReferencedTypesInStatement(allocator, program, referenced, inner);
        },
        .return_stmt => |node| if (node.value) |value| try markReferencedTypesInExpr(allocator, program, referenced, value),
    }
}

fn markReferencedTypesInExpr(
    allocator: std.mem.Allocator,
    program: model.Program,
    referenced: *std.StringHashMapUnmanaged(void),
    expr: *model.Expr,
) anyerror!void {
    try markReferencedType(allocator, program, referenced, model.hir.exprType(expr.*));
    switch (expr.*) {
        .construct => |node| for (node.fields) |field| try markReferencedTypesInExpr(allocator, program, referenced, field.value),
        .construct_enum_variant => |node| if (node.payload) |payload| try markReferencedTypesInExpr(allocator, program, referenced, payload),
        .native_state => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.value),
        .native_user_data => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.state),
        .native_recover => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.value),
        .call => |node| for (node.args) |arg| try markReferencedTypesInExpr(allocator, program, referenced, arg),
        .virtual_call => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.receiver);
            for (node.args) |arg| try markReferencedTypesInExpr(allocator, program, referenced, arg);
        },
        .call_value => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.callee);
            for (node.args) |arg| try markReferencedTypesInExpr(allocator, program, referenced, arg);
        },
        .parent_view => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.object),
        .c_string_to_string => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.value),
        .array_len => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.object),
        .field => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.object),
        .string_len => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.object),
        .binary => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.lhs);
            try markReferencedTypesInExpr(allocator, program, referenced, node.rhs);
        },
        .conditional => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.condition);
            try markReferencedTypesInExpr(allocator, program, referenced, node.then_expr);
            try markReferencedTypesInExpr(allocator, program, referenced, node.else_expr);
        },
        .unary => |node| try markReferencedTypesInExpr(allocator, program, referenced, node.operand),
        .array => |node| for (node.elements) |element| try markReferencedTypesInExpr(allocator, program, referenced, element),
        .index => |node| {
            try markReferencedTypesInExpr(allocator, program, referenced, node.object);
            try markReferencedTypesInExpr(allocator, program, referenced, node.index);
        },
        .callback => |node| for (node.body) |statement| try markReferencedTypesInStatement(allocator, program, referenced, statement),
        .builder_array => |node| try markReferencedTypesInBuilderBlock(allocator, program, referenced, node.builder),
        else => {},
    }
}

fn markReferencedTypesInBuilderBlock(
    allocator: std.mem.Allocator,
    program: model.Program,
    referenced: *std.StringHashMapUnmanaged(void),
    builder: model.BuilderBlock,
) anyerror!void {
    for (builder.items) |item| {
        switch (item) {
            .expr => |expr_item| try markReferencedTypesInExpr(allocator, program, referenced, expr_item.expr),
            .if_item => |if_item| {
                try markReferencedTypesInExpr(allocator, program, referenced, if_item.condition);
                try markReferencedTypesInBuilderBlock(allocator, program, referenced, if_item.then_block);
                if (if_item.else_block) |else_block| try markReferencedTypesInBuilderBlock(allocator, program, referenced, else_block);
            },
            .for_item => |for_item| {
                try markReferencedType(allocator, program, referenced, for_item.binding_ty);
                try markReferencedTypesInExpr(allocator, program, referenced, for_item.iterator);
                try markReferencedTypesInBuilderBlock(allocator, program, referenced, for_item.body);
            },
            .switch_item => |switch_item| {
                try markReferencedTypesInExpr(allocator, program, referenced, switch_item.subject);
                for (switch_item.cases) |case_node| {
                    try markReferencedTypesInExpr(allocator, program, referenced, case_node.pattern);
                    try markReferencedTypesInBuilderBlock(allocator, program, referenced, case_node.body);
                }
                if (switch_item.default_block) |default_block| try markReferencedTypesInBuilderBlock(allocator, program, referenced, default_block);
            },
        }
    }
}

fn markReferencedTypesInPattern(
    allocator: std.mem.Allocator,
    program: model.Program,
    referenced: *std.StringHashMapUnmanaged(void),
    pattern: model.MatchPattern,
) anyerror!void {
    switch (pattern) {
        .variant => |node| {
            if (node.payload_ty) |payload_ty| try markReferencedType(allocator, program, referenced, payload_ty);
            if (node.inner) |inner| try markReferencedTypesInPattern(allocator, program, referenced, inner.*);
            if (node.as_binding_ty) |binding_ty| try markReferencedType(allocator, program, referenced, binding_ty);
        },
        .binding => |node| try markReferencedType(allocator, program, referenced, node.ty),
    }
}

pub fn lowerConstructs(allocator: std.mem.Allocator, program: model.Program) ![]ir.Construct {
    const lowered = try allocator.alloc(ir.Construct, program.constructs.len);
    for (program.constructs, 0..) |construct_decl, index| {
        lowered[index] = .{ .name = try allocator.dupe(u8, construct_decl.name) };
    }
    return lowered;
}

pub fn lowerConstructImplementations(allocator: std.mem.Allocator, program: model.Program) ![]ir.ConstructImplementation {
    const lowered = try allocator.alloc(ir.ConstructImplementation, program.forms.len);
    for (program.forms, 0..) |form_decl, index| {
        lowered[index] = .{
            .type_name = try allocator.dupe(u8, form_decl.name),
            .construct_constraint = .{ .construct_name = try allocator.dupe(u8, form_decl.construct.construct_name) },
            .families = try cloneStringSlice(allocator, form_decl.families),
            .fields = try lowerFieldTypes(allocator, program, form_decl.fields),
            .has_content = form_decl.content != null,
            .lifecycle_hooks = try lowerLifecycleHooks(allocator, form_decl.lifecycle_hooks),
        };
    }
    return lowered;
}

fn lowerLifecycleHooks(allocator: std.mem.Allocator, hooks: []const model.LifecycleHook) ![]ir.LifecycleHook {
    const lowered = try allocator.alloc(ir.LifecycleHook, hooks.len);
    for (hooks, 0..) |hook_decl, index| {
        lowered[index] = .{ .name = try allocator.dupe(u8, hook_decl.name) };
    }
    return lowered;
}

fn cloneStringSlice(allocator: std.mem.Allocator, values: []const []const u8) ![]const []const u8 {
    const lowered = try allocator.alloc([]const u8, values.len);
    for (values, 0..) |value, index| lowered[index] = try allocator.dupe(u8, value);
    return lowered;
}
pub fn lowerTypeDecls(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable_functions: std.AutoHashMapUnmanaged(u32, void),
) ![]ir.TypeDecl {
    var referenced = std.StringHashMapUnmanaged(void){};
    defer referenced.deinit(allocator);

    for (program.functions) |function_decl| {
        if (!reachable_functions.contains(function_decl.id)) continue;
        for (function_decl.params) |param| try markReferencedType(allocator, program, &referenced, param.ty);
        try markReferencedType(allocator, program, &referenced, function_decl.return_type);
        for (function_decl.locals) |local| try markReferencedType(allocator, program, &referenced, local.ty);
        for (function_decl.body) |statement| try markReferencedTypesInStatement(allocator, program, &referenced, statement);
    }

    var types = std.array_list.Managed(ir.TypeDecl).init(allocator);
    for (program.types) |type_decl| {
        if (!referenced.contains(type_decl.name)) continue;
        try types.append(.{
            .name = try allocator.dupe(u8, type_decl.name),
            .kind = @enumFromInt(@intFromEnum(type_decl.kind)),
            .execution = type_decl.execution,
            .fields = try lowerFieldTypes(allocator, program, type_decl.fields),
            .methods = try lowerMethodMembers(allocator, program, type_decl.methods),
            .ffi = if (type_decl.ffi) |ffi_info| try lowerFfiTypeInfo(allocator, program, ffi_info) else null,
        });
    }
    return types.toOwnedSlice();
}

pub fn markReachableFunction(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    function_id: u32,
) anyerror!void {
    if (reachable.contains(function_id)) return;
    try reachable.put(allocator, function_id, {});

    for (program.functions) |function_decl| {
        if (function_decl.id != function_id) continue;
        if (function_decl.is_extern) return;
        for (function_decl.body) |statement| try markReachableStatement(allocator, program, reachable, statement);
        return;
    }
}

pub fn markReachableStatement(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    statement: model.Statement,
) anyerror!void {
    switch (statement) {
        .let_stmt => |node| if (node.value) |value| try markReachableExpr(allocator, program, reachable, value),
        .assign_stmt => |node| {
            try markReachableExpr(allocator, program, reachable, node.target);
            try markReachableExpr(allocator, program, reachable, node.value);
        },
        .expr_stmt => |node| try markReachableExpr(allocator, program, reachable, node.expr),
        .if_stmt => |node| {
            try markReachableExpr(allocator, program, reachable, node.condition);
            for (node.then_body) |inner| try markReachableStatement(allocator, program, reachable, inner);
            if (node.else_body) |else_body| for (else_body) |inner| try markReachableStatement(allocator, program, reachable, inner);
        },
        .for_stmt => |node| {
            try markReachableExpr(allocator, program, reachable, node.iterator);
            for (node.body) |inner| try markReachableStatement(allocator, program, reachable, inner);
        },
        .while_stmt => |node| {
            try markReachableExpr(allocator, program, reachable, node.condition);
            for (node.body) |inner| try markReachableStatement(allocator, program, reachable, inner);
        },
        .break_stmt, .continue_stmt => {},
        .match_stmt => |node| {
            try markReachableExpr(allocator, program, reachable, node.subject);
            for (node.arms) |arm| {
                try markReachablePattern(allocator, program, reachable, arm.pattern);
                if (arm.guard) |guard| try markReachableExpr(allocator, program, reachable, guard);
                for (arm.body) |inner| try markReachableStatement(allocator, program, reachable, inner);
            }
        },
        .switch_stmt => |node| {
            try markReachableExpr(allocator, program, reachable, node.subject);
            for (node.cases) |case_node| {
                try markReachableExpr(allocator, program, reachable, case_node.pattern);
                for (case_node.body) |inner| try markReachableStatement(allocator, program, reachable, inner);
            }
            if (node.default_body) |default_body| for (default_body) |inner| try markReachableStatement(allocator, program, reachable, inner);
        },
        .return_stmt => |node| if (node.value) |value| try markReachableExpr(allocator, program, reachable, value),
    }
}

pub fn markReachableExpr(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    expr: *model.Expr,
) anyerror!void {
    try markReachableClassMethodsForExpr(allocator, program, reachable, expr);
    switch (expr.*) {
        .construct => |node| {
            for (node.fields) |field| try markReachableExpr(allocator, program, reachable, field.value);
        },
        .construct_enum_variant => |node| {
            if (node.payload) |payload| try markReachableExpr(allocator, program, reachable, payload);
        },
        .native_state => |node| try markReachableExpr(allocator, program, reachable, node.value),
        .native_user_data => |node| try markReachableExpr(allocator, program, reachable, node.state),
        .native_recover => |node| try markReachableExpr(allocator, program, reachable, node.value),
        .call => |node| {
            if (node.function_id) |function_id| try markReachableFunction(allocator, program, reachable, function_id);
            for (node.args) |arg| try markReachableExpr(allocator, program, reachable, arg);
        },
        .virtual_call => |node| {
            try markReachableVirtualMethods(allocator, program, reachable, node.static_type_name, node.method_name);
            try markReachableExpr(allocator, program, reachable, node.receiver);
            for (node.args) |arg| try markReachableExpr(allocator, program, reachable, arg);
        },
        .function_ref => |node| try markReachableFunction(allocator, program, reachable, node.function_id),
        .namespace_ref => |node| {
            if (functionIdByName(program, node.path)) |function_id| {
                try markReachableFunction(allocator, program, reachable, function_id);
            }
        },
        .callback => |node| {
            for (node.body) |statement| try markReachableStatement(allocator, program, reachable, statement);
        },
        .builder_array => |node| try markReachableBuilderBlock(allocator, program, reachable, node.builder),
        .call_value => |node| {
            try markReachableExpr(allocator, program, reachable, node.callee);
            for (node.args) |arg| try markReachableExpr(allocator, program, reachable, arg);
        },
        .parent_view => |node| try markReachableExpr(allocator, program, reachable, node.object),
        .c_string_to_string => |node| try markReachableExpr(allocator, program, reachable, node.value),
        .array_len => |node| try markReachableExpr(allocator, program, reachable, node.object),
        .string_len => |node| try markReachableExpr(allocator, program, reachable, node.object),
        .field => |node| try markReachableExpr(allocator, program, reachable, node.object),
        .binary => |node| {
            try markReachableExpr(allocator, program, reachable, node.lhs);
            try markReachableExpr(allocator, program, reachable, node.rhs);
        },
        .conditional => |node| {
            try markReachableExpr(allocator, program, reachable, node.condition);
            try markReachableExpr(allocator, program, reachable, node.then_expr);
            try markReachableExpr(allocator, program, reachable, node.else_expr);
        },
        .unary => |node| try markReachableExpr(allocator, program, reachable, node.operand),
        .array => |node| for (node.elements) |element| try markReachableExpr(allocator, program, reachable, element),
        .index => |node| {
            try markReachableExpr(allocator, program, reachable, node.object);
            try markReachableExpr(allocator, program, reachable, node.index);
        },
        else => {},
    }
}

fn markReachableBuilderBlock(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    builder: model.BuilderBlock,
) anyerror!void {
    for (builder.items) |item| {
        switch (item) {
            .expr => |expr_item| try markReachableExpr(allocator, program, reachable, expr_item.expr),
            .if_item => |if_item| {
                try markReachableExpr(allocator, program, reachable, if_item.condition);
                try markReachableBuilderBlock(allocator, program, reachable, if_item.then_block);
                if (if_item.else_block) |else_block| try markReachableBuilderBlock(allocator, program, reachable, else_block);
            },
            .for_item => |for_item| {
                try markReachableExpr(allocator, program, reachable, for_item.iterator);
                try markReachableBuilderBlock(allocator, program, reachable, for_item.body);
            },
            .switch_item => |switch_item| {
                try markReachableExpr(allocator, program, reachable, switch_item.subject);
                for (switch_item.cases) |case_node| {
                    try markReachableExpr(allocator, program, reachable, case_node.pattern);
                    try markReachableBuilderBlock(allocator, program, reachable, case_node.body);
                }
                if (switch_item.default_block) |default_block| try markReachableBuilderBlock(allocator, program, reachable, default_block);
            },
        }
    }
}

fn markReachableVirtualMethods(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    static_type_name: []const u8,
    method_name: []const u8,
) anyerror!void {
    for (program.types) |type_decl| {
        const participates = if (std.mem.eql(u8, type_decl.name, static_type_name))
            true
        else
            formSatisfiesFamily(program, type_decl.name, static_type_name);
        if (!participates) continue;
        for (type_decl.methods) |method_decl| {
            if (!std.mem.eql(u8, method_decl.name, method_name)) continue;
            if (functionIdByName(program, method_decl.full_name)) |function_id| {
                try markReachableFunction(allocator, program, reachable, function_id);
            }
        }
    }
}

fn formSatisfiesFamily(program: model.Program, type_name: []const u8, family: []const u8) bool {
    for (program.forms) |form_decl| {
        if (!std.mem.eql(u8, form_decl.name, type_name)) continue;
        for (form_decl.families) |candidate| {
            if (std.mem.eql(u8, candidate, family)) return true;
        }
    }
    return false;
}

pub fn markReferencedType(
    allocator: std.mem.Allocator,
    program: model.Program,
    referenced: *std.StringHashMapUnmanaged(void),
    ty: model.ResolvedType,
) !void {
    const name = switch (ty.kind) {
        .named, .enum_instance, .native_state, .native_state_view => ty.name orelse return,
        else => return,
    };
    if (referenced.contains(name)) return;
    try referenced.put(allocator, name, {});

    for (program.types) |type_decl| {
        if (!std.mem.eql(u8, type_decl.name, name)) continue;
        for (type_decl.fields) |field_decl| try markReferencedType(allocator, program, referenced, field_decl.ty);
        if (type_decl.ffi) |ffi_info| {
            switch (ffi_info) {
                .pointer => |value| try markReferencedType(allocator, program, referenced, .{ .kind = .named, .name = value.target_name }),
                .alias => |value| try markReferencedType(allocator, program, referenced, value.target),
                .array => |value| try markReferencedType(allocator, program, referenced, value.element),
                .callback => |value| {
                    for (value.params) |param| try markReferencedType(allocator, program, referenced, param);
                    try markReferencedType(allocator, program, referenced, value.result);
                },
                .ffi_struct => {},
            }
        }
        break;
    }
    for (program.enums) |enum_decl| {
        if (!std.mem.eql(u8, enum_decl.name, name)) continue;
        for (enum_decl.variants) |variant_decl| {
            if (variant_decl.payload_ty) |payload_ty| try markReferencedType(allocator, program, referenced, payload_ty);
        }
        break;
    }
}

fn markReachableClassMethodsForExpr(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    expr: *model.Expr,
) anyerror!void {
    const expr_ty = model.hir.exprType(expr.*);
    if (expr_ty.kind != .named or expr_ty.name == null) return;
    const type_decl = typeDeclByName(program, expr_ty.name.?) orelse return;
    if (type_decl.kind != .class) return;
    for (type_decl.methods) |method_decl| {
        if (functionIdByName(program, method_decl.full_name)) |function_id| {
            try markReachableFunction(allocator, program, reachable, function_id);
        }
    }
}

fn typeDeclByName(program: model.Program, name: []const u8) ?model.TypeDecl {
    for (program.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
    }
    return null;
}

pub fn lowerFieldTypes(allocator: std.mem.Allocator, program: model.Program, fields: []const model.Field) ![]ir.Field {
    const lowered = try allocator.alloc(ir.Field, fields.len);
    for (fields, 0..) |field_decl, index| {
        lowered[index] = .{
            .name = try allocator.dupe(u8, field_decl.name),
            .ty = try lowerResolvedType(program, field_decl.ty),
        };
    }
    return lowered;
}

pub fn lowerFfiTypeInfo(allocator: std.mem.Allocator, program: model.Program, ffi_info: model.NamedTypeInfo) !ir.FfiTypeInfo {
    return switch (ffi_info) {
        .ffi_struct => .ffi_struct,
        .pointer => |value| .{ .pointer = .{ .target_name = try allocator.dupe(u8, value.target_name) } },
        .alias => |value| .{ .alias = .{ .target = try lowerResolvedType(program, value.target) } },
        .array => |value| .{ .array = .{
            .element = try lowerResolvedType(program, value.element),
            .count = value.count,
        } },
        .callback => |value| blk: {
            var params = std.array_list.Managed(ir.ValueType).init(allocator);
            for (value.params) |param| try params.append(try lowerResolvedType(program, param));
            break :blk .{ .callback = .{
                .params = try params.toOwnedSlice(),
                .result = try lowerResolvedType(program, value.result),
            } };
        },
    };
}

pub fn lowerAssignmentStatement(
    lowerer: *Lowerer,
    instructions: *std.array_list.Managed(ir.Instruction),
    program: model.Program,
    node: model.AssignStatement,
) !void {
    const value_reg = try lowerer.lowerExpr(instructions, node.value);
    switch (node.target.*) {
        .local => |target| {
            const local_id = lowerer.mapLocal(target.local_id);
            const local_ty = try lowerResolvedType(program, target.ty);
            if (lowerer.isBoxedLocal(target.local_id)) {
                const ptr_reg = lowerer.freshRegister();
                try instructions.append(.{ .load_local = .{ .dst = ptr_reg, .local = local_id } });
                try instructions.append(.{ .store_indirect = .{ .ptr = ptr_reg, .src = value_reg, .ty = local_ty } });
            } else if (local_ty.kind == .ffi_struct) {
                const dst_ptr = lowerer.freshRegister();
                try instructions.append(.{ .load_local = .{ .dst = dst_ptr, .local = local_id } });
                try instructions.append(.{ .copy_indirect = .{
                    .dst_ptr = dst_ptr,
                    .src_ptr = value_reg,
                    .type_name = local_ty.name orelse return error.UnsupportedExecutableFeature,
                } });
            } else {
                try instructions.append(.{ .store_local = .{ .local = local_id, .src = value_reg } });
            }
        },
        .field => |target| {
            const target_ty = try lowerResolvedType(program, target.ty);
            // Peephole: `arr[i].f = v` storing a *scalar* field of an array element.
            // The general path below materializes the element by value (a deep
            // `array_get` clone) and writes it back with `array_set` — a full
            // ~40-field LayoutNode clone per node geometry write in the layout
            // engine. Borrow the element in place instead and store the scalar
            // through the alias: no clone, no write-back. Sound because a scalar
            // field owns no heap (plain overwrite) and the array keeps ownership;
            // the native backend already mutates the element through its pointer,
            // preserving vm/llvm/hybrid parity. Restricted to a direct `arr[i]`
            // element with a scalar field.
            if (target.object.* == .index and target_ty.kind != .ffi_struct and
                model.hir.exprType(target.object.*).kind != .native_state_view)
            {
                const inner = target.object.index;
                const elem_ty = try lowerResolvedType(program, inner.ty);
                if (elem_ty.kind == .ffi_struct) {
                    const array_reg = try lowerer.lowerExpr(instructions, inner.object);
                    const index_reg = try lowerer.lowerExpr(instructions, inner.index);
                    const elem_reg = lowerer.freshRegister();
                    try instructions.append(.{ .array_get = .{
                        .dst = elem_reg,
                        .array = array_reg,
                        .index = index_reg,
                        .ty = elem_ty,
                        .borrow = true,
                    } });
                    const inplace_ptr = lowerer.freshRegister();
                    try instructions.append(.{ .field_ptr = .{
                        .dst = inplace_ptr,
                        .base = elem_reg,
                        .base_type_name = target.container_type_name,
                        .field_index = target.field_index,
                        .field_ty = target_ty,
                    } });
                    try instructions.append(.{ .store_indirect = .{
                        .ptr = inplace_ptr,
                        .src = value_reg,
                        .ty = target_ty,
                    } });
                    return;
                }
            }
            // Resolve the field's base object as a mutable place. When the place chain
            // roots at an array index (`arr[i].field`, `arr[i].a.b`, `arr[i].xs[j].f`,
            // ...) the element is materialized by value (the VM has no element-pointer
            // op), so the mutation only touches a transient copy unless the element is
            // written back into its array with `array_set`. `lowerMutableObject` mirrors
            // the read lowering of the same place and records those write-backs; plain
            // local-struct fields produce no write-backs and lower exactly as before.
            // Backend-agnostic: LLVM/native round-trip `array_get`/`array_set` the same.
            var writebacks = places.WritebackList.init(lowerer.allocator);
            defer writebacks.deinit();
            const base_reg = try places.lowerMutableObject(lowerer, instructions, target.object, &writebacks);
            if (model.hir.exprType(target.object.*).kind == .native_state_view) {
                try instructions.append(.{ .native_state_field_set = .{
                    .state = base_reg,
                    .field_index = target.field_index,
                    .src = value_reg,
                    .field_ty = target_ty,
                } });
                try places.emitWritebacks(instructions, &writebacks);
                return;
            }
            const ptr_reg = lowerer.freshRegister();
            try instructions.append(.{ .field_ptr = .{
                .dst = ptr_reg,
                .base = base_reg,
                .base_type_name = target.container_type_name,
                .field_index = target.field_index,
                .field_ty = target_ty,
            } });
            if (target_ty.kind == .ffi_struct) {
                try instructions.append(.{ .copy_indirect = .{
                    .dst_ptr = ptr_reg,
                    .src_ptr = value_reg,
                    .type_name = target_ty.name orelse return error.UnsupportedExecutableFeature,
                } });
            } else {
                try instructions.append(.{ .store_indirect = .{
                    .ptr = ptr_reg,
                    .src = value_reg,
                    .ty = target_ty,
                } });
            }
            try places.emitWritebacks(instructions, &writebacks);
        },
        .index => |target| {
            // `arr[i] = v` lowers to a plain `array_set`. When the indexed array is
            // itself rooted at an array element (`arr[i].xs[j] = v`), `lowerMutableObject`
            // materializes the parent element by value and records the `array_set`
            // write-back needed to persist the mutated nested array back into it.
            var writebacks = places.WritebackList.init(lowerer.allocator);
            defer writebacks.deinit();
            const array_reg = try places.lowerMutableObject(lowerer, instructions, target.object, &writebacks);
            const index_reg = try lowerer.lowerExpr(instructions, target.index);
            try instructions.append(.{ .array_set = .{
                .array = array_reg,
                .index = index_reg,
                .src = value_reg,
            } });
            try places.emitWritebacks(instructions, &writebacks);
        },
        else => return error.UnsupportedExecutableFeature,
    }
}

pub fn findTypeFieldDefaultExpr(program: model.Program, type_name: []const u8, field_name: []const u8) ?*model.Expr {
    for (program.types) |type_decl| {
        if (!std.mem.eql(u8, type_decl.name, type_name)) continue;
        for (type_decl.fields) |field_decl| {
            if (!std.mem.eql(u8, field_decl.name, field_name)) continue;
            return field_decl.default_value;
        }
    }
    return null;
}

pub fn fieldDeclIsTypeConstant(field_decl: model.Field, owner_type_name: []const u8) bool {
    return field_decl.storage == .immutable and field_decl.ty.kind == .named and field_decl.ty.name != null and std.mem.eql(u8, field_decl.ty.name.?, owner_type_name);
}

pub fn lowerMethodMembers(
    allocator: std.mem.Allocator,
    program: model.Program,
    methods: []const model.MethodMember,
) ![]ir.MethodMember {
    const lowered = try allocator.alloc(ir.MethodMember, methods.len);
    for (methods, 0..) |method_decl, index| {
        lowered[index] = .{
            .name = try allocator.dupe(u8, method_decl.name),
            .function_id = functionIdByName(program, method_decl.full_name) orelse return error.UnsupportedExecutableFeature,
            .receiver_offset = method_decl.receiver_offset,
        };
    }
    return lowered;
}

pub fn functionIdByName(program: model.Program, name: []const u8) ?u32 {
    for (program.functions) |function_decl| {
        if (std.mem.eql(u8, function_decl.name, name)) return function_decl.id;
    }
    return null;
}

fn markReachablePattern(
    allocator: std.mem.Allocator,
    program: model.Program,
    reachable: *std.AutoHashMapUnmanaged(u32, void),
    pattern: model.MatchPattern,
) anyerror!void {
    switch (pattern) {
        .variant => |node| if (node.inner) |inner| try markReachablePattern(allocator, program, reachable, inner.*),
        .binding => {},
    }
}

test "lowers zero-argument expression-statement calls even when return type is not resolved to void" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const callee_expr = try allocator.create(model.Expr);
    callee_expr.* = .{ .call = .{
        .callee_name = "helper",
        .function_id = 1,
        .args = &.{},
        .ty = .{ .kind = .unknown },
        .span = .{ .start = 0, .end = 0 },
    } };

    const program = model.Program{
        .imports = &.{},
        .constructs = &.{},
        .types = &.{},
        .forms = &.{},
        .functions = &.{
            .{
                .id = 0,
                .name = "entry",
                .is_main = true,
                .execution = .native,
                .is_extern = false,
                .foreign = null,
                .annotations = &.{},
                .params = &.{},
                .locals = &.{},
                .return_type = .{ .kind = .void },
                .body = &.{
                    .{ .expr_stmt = .{ .expr = callee_expr, .span = .{ .start = 0, .end = 0 } } },
                    .{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } },
                },
                .span = .{ .start = 0, .end = 0 },
            },
            .{
                .id = 1,
                .name = "helper",
                .is_main = false,
                .execution = .runtime,
                .is_extern = false,
                .foreign = null,
                .annotations = &.{},
                .params = &.{},
                .locals = &.{},
                .return_type = .{ .kind = .void },
                .body = &.{.{ .return_stmt = .{ .value = null, .span = .{ .start = 0, .end = 0 } } }},
                .span = .{ .start = 0, .end = 0 },
            },
        },
        .entry_index = 0,
    };

    const lowered = try lowerProgram(allocator, program);
    try std.testing.expectEqual(@as(usize, 2), lowered.functions.len);
    try std.testing.expect(lowered.functions[0].instructions[0] == .call);
}

test "lowers native callback state into dedicated IR instructions" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(@import("kira_diagnostics").Diagnostic).init(allocator);
    const source_pkg = @import("kira_source");
    const lexer = @import("kira_lexer");
    const parser = @import("kira_parser");
    const semantics = @import("kira_semantics");

    const source = try source_pkg.SourceFile.initOwned(
        allocator,
        "test.kira",
        "struct CounterState { var count: Int }\n" ++
            "@Native function onTick(data: RawPtr) { var state = nativeRecover<CounterState>(data); state.count = state.count + 1; return; }\n" ++
            "@Main function entry() { var state = nativeState(CounterState { count: 0 }); var token = nativeUserData(state); return; }",
    );
    const tokens = try lexer.tokenize(allocator, &source, &diags);
    const parsed = try parser.parse(allocator, tokens, &diags);
    const analyzed = try semantics.analyze(allocator, parsed, &diags);
    try std.testing.expectEqual(@as(usize, 0), diags.items.len);

    const lowered = try lowerProgram(allocator, analyzed);
    const callback = blk: {
        for (lowered.functions) |function_decl| {
            if (std.mem.eql(u8, function_decl.name, "onTick")) break :blk function_decl;
        }
        return error.TestUnexpectedResult;
    };
    const entry = blk: {
        for (lowered.functions) |function_decl| {
            if (std.mem.eql(u8, function_decl.name, "entry")) break :blk function_decl;
        }
        return error.TestUnexpectedResult;
    };

    try std.testing.expect(entry.instructions[0] == .alloc_struct);
    try std.testing.expect(entry.instructions[1] == .alloc_native_state);
    try std.testing.expect(callback.instructions[1] == .recover_native_state);
    var found_counter_state = false;
    for (lowered.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, "CounterState")) found_counter_state = true;
    }
    try std.testing.expect(found_counter_state);
}

test "lowers a deep array-element field store with an array_set write-back" {
    // Regression: `arr[i].inner.x = v` (a store nested 2+ projections below the array
    // index) must read-modify-write the element back via `array_set`; without the
    // write-back the VM silently dropped the store. Assert the lowered `entry` contains
    // an `array_get` for the element and an `array_set` *after* the field `store_indirect`.
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(@import("kira_diagnostics").Diagnostic).init(allocator);
    const source_pkg = @import("kira_source");
    const lexer = @import("kira_lexer");
    const parser = @import("kira_parser");
    const semantics = @import("kira_semantics");

    const source = try source_pkg.SourceFile.initOwned(
        allocator,
        "test.kira",
        "struct Inner { var x: Int }\n" ++
            "struct Outer { var inner: Inner }\n" ++
            "@Main function entry() { var arr: [Outer] = []; arr.append(Outer { inner: Inner { x: 1 } }); arr[0].inner.x = 77; return; }",
    );
    const tokens = try lexer.tokenize(allocator, &source, &diags);
    const parsed = try parser.parse(allocator, tokens, &diags);
    const analyzed = try semantics.analyze(allocator, parsed, &diags);
    try std.testing.expectEqual(@as(usize, 0), diags.items.len);

    const lowered = try lowerProgram(allocator, analyzed);
    const entry = blk: {
        for (lowered.functions) |function_decl| {
            if (std.mem.eql(u8, function_decl.name, "entry")) break :blk function_decl;
        }
        return error.TestUnexpectedResult;
    };

    var saw_array_get = false;
    var last_store_indirect: ?usize = null;
    var array_set_after_store = false;
    for (entry.instructions, 0..) |instruction, index| {
        switch (instruction) {
            .array_get => saw_array_get = true,
            .store_indirect => last_store_indirect = index,
            .array_set => {
                if (last_store_indirect) |store_index| {
                    if (index > store_index) array_set_after_store = true;
                }
            },
            else => {},
        }
    }
    try std.testing.expect(saw_array_get);
    try std.testing.expect(array_set_after_store);
}

test "lowers construct metadata and any construct types into IR" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(@import("kira_diagnostics").Diagnostic).init(allocator);
    const source_pkg = @import("kira_source");
    const lexer = @import("kira_lexer");
    const parser = @import("kira_parser");
    const semantics = @import("kira_semantics");

    const source = try source_pkg.SourceFile.initOwned(
        allocator,
        "test.kira",
        "construct Widget { lifecycle { onAppear() {} } }\n" ++
            "Widget Button() { let title: String = \"Hi\" content { } onAppear() { return; } }\n" ++
            "@Runtime function accept(value: any Widget) { return; }\n" ++
            "@Main function entry() { return; }",
    );
    const tokens = try lexer.tokenize(allocator, &source, &diags);
    const parsed = try parser.parse(allocator, tokens, &diags);
    const analyzed = try semantics.analyze(allocator, parsed, &diags);
    try std.testing.expectEqual(@as(usize, 0), diags.items.len);

    const lowered = try lowerProgram(allocator, analyzed);
    try std.testing.expectEqual(@as(usize, 1), lowered.constructs.len);
    try std.testing.expectEqualStrings("Widget", lowered.constructs[0].name);
    try std.testing.expectEqual(@as(usize, 1), lowered.construct_implementations.len);
    try std.testing.expectEqualStrings("Button", lowered.construct_implementations[0].type_name);
    try std.testing.expectEqualStrings("Widget", lowered.construct_implementations[0].construct_constraint.construct_name);
    try std.testing.expect(lowered.construct_implementations[0].has_content);
    try std.testing.expectEqual(@as(usize, 1), lowered.construct_implementations[0].lifecycle_hooks.len);
    try std.testing.expectEqualStrings("onAppear", lowered.construct_implementations[0].lifecycle_hooks[0].name);

    const accept = blk: {
        for (analyzed.functions) |function_decl| {
            if (std.mem.eql(u8, function_decl.name, "accept")) break :blk function_decl;
        }
        return error.TestUnexpectedResult;
    };
    const lowered_param = try lowerResolvedType(analyzed, accept.params[0].ty);
    try std.testing.expectEqual(ir.ValueType.Kind.construct_any, lowered_param.kind);
    try std.testing.expectEqualStrings("Widget", lowered_param.construct_constraint.?.construct_name);
}
