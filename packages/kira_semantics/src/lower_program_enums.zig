const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");

pub fn lowerEnumDecl(ctx: *shared.Context, enum_decl: syntax.ast.EnumDecl) !model.EnumDecl {
    var names = std.StringHashMapUnmanaged(source_pkg.Span){};
    defer names.deinit(ctx.allocator);
    var variants = std.array_list.Managed(model.EnumVariantHir).init(ctx.allocator);

    for (enum_decl.variants, 0..) |variant_decl, index| {
        if (names.get(variant_decl.name)) |previous_span| {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM103",
                .title = "duplicate enum variant",
                .message = try std.fmt.allocPrint(ctx.allocator, "The enum '{s}' declares variant '{s}' more than once.", .{ enum_decl.name, variant_decl.name }),
                .labels = &.{
                    diagnostics.primaryLabel(variant_decl.span, "duplicate enum variant appears here"),
                    diagnostics.secondaryLabel(previous_span, "first enum variant was declared here"),
                },
                .help = "Keep each enum variant name unique within the declaration.",
            });
            return error.DiagnosticsEmitted;
        }
        try names.put(ctx.allocator, variant_decl.name, variant_decl.span);

        var payload_ty: ?model.ResolvedType = null;
        if (variant_decl.associated_type) |associated_type| {
            payload_ty = try shared.typeFromSyntaxChecked(ctx, associated_type.*);
        }
        const default_value = if (variant_decl.default_value) |value| try lowerEnumDefaultExpr(ctx, value) else null;
        if (payload_ty == null and default_value != null) {
            payload_ty = model.hir.exprType(default_value.?.*);
        }
        if (payload_ty != null and default_value != null) {
            const actual_ty = model.hir.exprType(default_value.?.*);
            if (!shared.canAssignExactly(payload_ty.?, actual_ty)) {
                try shared.emitTypeMismatch(ctx.allocator, ctx.diagnostics, variant_decl.span, payload_ty.?, actual_ty);
                return error.DiagnosticsEmitted;
            }
        }
        try variants.append(.{
            .name = try ctx.allocator.dupe(u8, variant_decl.name),
            .discriminant = @as(u32, @intCast(index)),
            .payload_ty = payload_ty,
            .default_value = default_value,
            .span = variant_decl.span,
        });
    }

    return .{
        .name = try ctx.allocator.dupe(u8, enum_decl.name),
        .type_params = try cloneStringSlice(ctx.allocator, enum_decl.type_params),
        .variants = try variants.toOwnedSlice(),
        .span = enum_decl.span,
    };
}

pub fn monomorphizeEnum(ctx: *shared.Context, base_decl: model.EnumDecl, concrete_args: []const model.ResolvedType) !model.EnumDecl {
    if (base_decl.type_params.len == 0) return base_decl;
    const concrete_name = try concreteEnumName(ctx.allocator, base_decl.name, concrete_args);
    if (ctx.concrete_enums) |concrete_enums| {
        if (concrete_enums.get(concrete_name)) |existing| return existing;
    }

    var variants = std.array_list.Managed(model.EnumVariantHir).init(ctx.allocator);
    for (base_decl.variants) |variant_decl| {
        try variants.append(.{
            .name = try ctx.allocator.dupe(u8, variant_decl.name),
            .discriminant = variant_decl.discriminant,
            .payload_ty = if (variant_decl.payload_ty) |payload_ty| try substituteType(base_decl.type_params, concrete_args, payload_ty) else null,
            .default_value = variant_decl.default_value,
            .span = variant_decl.span,
        });
    }

    const lowered: model.EnumDecl = .{
        .name = concrete_name,
        .type_params = &.{},
        .variants = try variants.toOwnedSlice(),
        .span = base_decl.span,
    };
    if (ctx.concrete_enums) |concrete_enums| try concrete_enums.put(ctx.allocator, lowered.name, lowered);
    return lowered;
}

pub fn registerGenericEnumInstantiations(ctx: *shared.Context, program: syntax.ast.Program) !void {
    for (program.decls) |decl| {
        switch (decl) {
            .enum_decl => |enum_decl| {
                for (enum_decl.variants) |variant_decl| {
                    if (variant_decl.associated_type) |associated_type| try registerTypeExpr(ctx, associated_type.*);
                    if (variant_decl.default_value) |default_value| try registerExpr(ctx, default_value);
                }
            },
            .function_decl => |function_decl| try registerFunctionDecl(ctx, function_decl),
            .type_decl => |type_decl| try registerTypeDecl(ctx, type_decl),
            .construct_decl => |construct_decl| try registerConstructDecl(ctx, construct_decl),
            .construct_form_decl => |form_decl| try registerConstructFormDecl(ctx, form_decl),
            .annotation_decl => |annotation_decl| {
                for (annotation_decl.parameters) |param| try registerTypeExpr(ctx, param.type_expr.*);
            },
            .capability_decl => {},
            // Extension declarations contribute fluent modifiers, validated separately; they
            // introduce no enums/types to pre-register here.
            .extend_decl => {},
        }
    }
}

pub fn concreteEnumName(allocator: std.mem.Allocator, base_name: []const u8, concrete_args: []const model.ResolvedType) ![]const u8 {
    var text = std.array_list.Managed(u8).init(allocator);
    try text.appendSlice(base_name);
    for (concrete_args) |arg| {
        try text.appendSlice("__");
        const arg_text = try shared.typeTextFromResolved(allocator, arg);
        for (arg_text) |byte| {
            if (std.ascii.isAlphanumeric(byte)) {
                try text.append(byte);
            } else {
                try text.append('_');
            }
        }
    }
    return text.toOwnedSlice();
}

fn registerTypeDecl(ctx: *shared.Context, type_decl: syntax.ast.TypeDecl) anyerror!void {
    for (type_decl.members) |member| {
        switch (member) {
            .field_decl => |field_decl| {
                if (field_decl.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                if (field_decl.value) |value| try registerExpr(ctx, value);
            },
            .function_decl => |function_decl| try registerFunctionDecl(ctx, function_decl),
            .content_section => |content| try registerBuilderBlock(ctx, content.builder),
            .properties_section => |properties| {
                for (properties.entries) |entry| try registerExpr(ctx, entry.value);
            },
            .lifecycle_hook => |hook| try registerBlock(ctx, hook.body),
            .named_rule => |rule| {
                if (rule.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                if (rule.value) |value| try registerExpr(ctx, value);
                if (rule.block) |block| try registerBlock(ctx, block);
            },
        }
    }
}

fn registerConstructDecl(ctx: *shared.Context, construct_decl: syntax.ast.ConstructDecl) anyerror!void {
    for (construct_decl.sections) |section| {
        for (section.entries) |entry| {
            switch (entry) {
                .annotation_spec => |spec| {
                    if (spec.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                    if (spec.default_value) |value| try registerExpr(ctx, value);
                },
                .field_decl => |field_decl| {
                    if (field_decl.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                    if (field_decl.value) |value| try registerExpr(ctx, value);
                },
                .lifecycle_hook => |hook| try registerBlock(ctx, hook.body),
                .function_signature => |signature| {
                    for (signature.params) |param| if (param.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                    if (signature.return_type) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                },
                .property_schema => |property| {
                    if (property.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                    if (property.default_value) |value| try registerExpr(ctx, value);
                },
                .content_channel, .content_directive, .content_projection => {},
                .named_rule => |rule| {
                    if (rule.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                    if (rule.value) |value| try registerExpr(ctx, value);
                    if (rule.block) |block| try registerBlock(ctx, block);
                },
            }
        }
    }
}

fn registerConstructFormDecl(ctx: *shared.Context, form_decl: syntax.ast.ConstructFormDecl) anyerror!void {
    for (form_decl.params) |param| if (param.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
    for (form_decl.body.members) |member| {
        switch (member) {
            .field_decl => |field_decl| {
                if (field_decl.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                if (field_decl.value) |value| try registerExpr(ctx, value);
            },
            .function_decl => |function_decl| try registerFunctionDecl(ctx, function_decl),
            .content_section => |content| try registerBuilderBlock(ctx, content.builder),
            .properties_section => |properties| {
                for (properties.entries) |entry| try registerExpr(ctx, entry.value);
            },
            .lifecycle_hook => |hook| try registerBlock(ctx, hook.body),
            .named_rule => |rule| {
                if (rule.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                if (rule.value) |value| try registerExpr(ctx, value);
                if (rule.block) |block| try registerBlock(ctx, block);
            },
        }
    }
}

fn registerFunctionDecl(ctx: *shared.Context, function_decl: syntax.ast.FunctionDecl) anyerror!void {
    for (function_decl.params) |param| if (param.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
    if (function_decl.return_type) |return_type| try registerTypeExpr(ctx, return_type.*);
    if (function_decl.body) |body| try registerBlock(ctx, body);
}

fn registerBlock(ctx: *shared.Context, block: syntax.ast.Block) anyerror!void {
    for (block.statements) |statement| {
        switch (statement) {
            .let_stmt => |node| {
                if (node.type_expr) |type_expr| try registerTypeExpr(ctx, type_expr.*);
                if (node.value) |value| try registerExpr(ctx, value);
            },
            .assign_stmt => |node| {
                try registerExpr(ctx, node.target);
                try registerExpr(ctx, node.value);
            },
            .expr_stmt => |node| try registerExpr(ctx, node.expr),
            .return_stmt => |node| if (node.value) |value| try registerExpr(ctx, value),
            .if_stmt => |node| {
                try registerExpr(ctx, node.condition);
                try registerBlock(ctx, node.then_block);
                if (node.else_block) |else_block| try registerBlock(ctx, else_block);
            },
            .for_stmt => |node| {
                try registerExpr(ctx, node.iterator);
                try registerBlock(ctx, node.body);
            },
            .while_stmt => |node| {
                try registerExpr(ctx, node.condition);
                try registerBlock(ctx, node.body);
            },
            .match_stmt => |node| {
                try registerExpr(ctx, node.subject);
                for (node.arms) |arm| {
                    for (arm.patterns) |pattern| try registerMatchPattern(ctx, pattern);
                    if (arm.guard) |guard| try registerExpr(ctx, guard);
                    try registerBlock(ctx, arm.body);
                }
            },
            .switch_stmt => |node| {
                try registerExpr(ctx, node.subject);
                for (node.cases) |case_node| {
                    try registerExpr(ctx, case_node.pattern);
                    try registerBlock(ctx, case_node.body);
                }
                if (node.default_block) |default_block| try registerBlock(ctx, default_block);
            },
            .attempt_stmt => |node| {
                try registerBlock(ctx, .{ .statements = node.body, .span = node.span });
                for (node.handlers) |handler| try registerBlock(ctx, handler.body);
            },
            .break_stmt, .continue_stmt => {},
        }
    }
}

fn registerBuilderBlock(ctx: *shared.Context, block: syntax.ast.BuilderBlock) anyerror!void {
    for (block.items) |item| {
        switch (item) {
            .expr => |value| try registerExpr(ctx, value.expr),
            .if_item => |value| {
                try registerExpr(ctx, value.condition);
                try registerBuilderBlock(ctx, value.then_block);
                if (value.else_block) |else_block| try registerBuilderBlock(ctx, else_block);
            },
            .for_item => |value| {
                try registerExpr(ctx, value.iterator);
                try registerBuilderBlock(ctx, value.body);
            },
            .switch_item => |value| {
                try registerExpr(ctx, value.subject);
                for (value.cases) |case_node| {
                    try registerExpr(ctx, case_node.pattern);
                    try registerBuilderBlock(ctx, case_node.body);
                }
                if (value.default_block) |default_block| try registerBuilderBlock(ctx, default_block);
            },
        }
    }
}

fn registerExpr(ctx: *shared.Context, expr: *syntax.ast.Expr) anyerror!void {
    ctx.lower_depth += 1;
    defer ctx.lower_depth -= 1;
    try shared.checkLoweringDepth(ctx, shared.exprSpan(expr.*));
    switch (expr.*) {
        .array => |node| for (node.elements) |element| try registerExpr(ctx, element),
        .callback => |node| try registerBlock(ctx, node.body),
        .struct_literal => |node| for (node.fields) |field| try registerExpr(ctx, field.value),
        .native_state => |node| try registerExpr(ctx, node.value),
        .native_user_data => |node| try registerExpr(ctx, node.state),
        .native_recover => |node| {
            try registerTypeExpr(ctx, node.state_type.*);
            try registerExpr(ctx, node.value);
        },
        .ownership => |node| try registerExpr(ctx, node.operand),
        .unary => |node| try registerExpr(ctx, node.operand),
        .binary => |node| {
            try registerExpr(ctx, node.lhs);
            try registerExpr(ctx, node.rhs);
        },
        .conditional => |node| {
            try registerExpr(ctx, node.condition);
            try registerExpr(ctx, node.then_expr);
            try registerExpr(ctx, node.else_expr);
        },
        .member => |node| try registerExpr(ctx, node.object),
        .index => |node| {
            try registerExpr(ctx, node.object);
            try registerExpr(ctx, node.index);
        },
        .call => |node| {
            try registerExpr(ctx, node.callee);
            for (node.args) |arg| try registerExpr(ctx, arg.value);
            if (node.trailing_builder) |builder| try registerBuilderBlock(ctx, builder);
            if (node.trailing_callback) |callback| try registerBlock(ctx, callback.body);
        },
        else => {},
    }
}

fn registerMatchPattern(ctx: *shared.Context, pattern: syntax.ast.MatchPattern) anyerror!void {
    switch (pattern) {
        .bare_variant => {},
        .destructure => |node| try registerMatchPattern(ctx, node.inner.*),
        .as_binding => |node| try registerMatchPattern(ctx, node.inner.*),
    }
}

fn registerTypeExpr(ctx: *shared.Context, ty: syntax.ast.TypeExpr) anyerror!void {
    switch (ty) {
        .generic => |info| {
            for (info.args) |arg| try registerTypeExpr(ctx, arg.*);
            var arg_types = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
            for (info.args) |arg| try arg_types.append(try shared.typeFromSyntaxChecked(ctx, arg.*));
            const base_name = info.base.segments[info.base.segments.len - 1].text;
            const base_decl = if (ctx.enum_headers) |headers| headers.get(base_name) else null;
            if (base_decl) |resolved_base| _ = try monomorphizeEnum(ctx, resolved_base, arg_types.items);
        },
        .array => |info| try registerTypeExpr(ctx, info.element_type.*),
        .function => |info| {
            for (info.params) |param| try registerTypeExpr(ctx, param.*);
            try registerTypeExpr(ctx, info.result.*);
        },
        .ownership => |info| try registerTypeExpr(ctx, info.target.*),
        .any => |info| try registerTypeExpr(ctx, info.target.*),
        .named => {},
    }
}

fn lowerEnumDefaultExpr(ctx: *shared.Context, expr: *syntax.ast.Expr) !*model.Expr {
    const lowered = try ctx.allocator.create(model.Expr);
    lowered.* = switch (expr.*) {
        .integer => |node| .{ .integer = .{ .value = node.value, .span = node.span } },
        .float => |node| .{ .float = .{ .value = node.value, .span = node.span } },
        .string => |node| .{ .string = .{ .value = try ctx.allocator.dupe(u8, node.value), .span = node.span } },
        .bool => |node| .{ .boolean = .{ .value = node.value, .span = node.span } },
        .unary => |node| blk: {
            const operand = try lowerEnumDefaultExpr(ctx, node.operand);
            break :blk .{ .unary = .{
                .op = @enumFromInt(@intFromEnum(node.op)),
                .operand = operand,
                .ty = model.hir.exprType(operand.*),
                .span = node.span,
            } };
        },
        else => {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM049",
                .title = "unsupported field default value",
                .message = "Enum variant defaults currently require literal values.",
                .labels = &.{diagnostics.primaryLabel(enumDefaultSpan(expr.*), "unsupported enum variant default value")},
                .help = "Use a literal default such as `0`, `true`, or `\"text\"`.",
            });
            return error.DiagnosticsEmitted;
        },
    };
    return lowered;
}

fn substituteType(type_params: [][]const u8, concrete_args: []const model.ResolvedType, ty: model.ResolvedType) !model.ResolvedType {
    if (ty.kind == .named and ty.name != null) {
        for (type_params, 0..) |type_param, index| {
            if (std.mem.eql(u8, ty.name.?, type_param)) return concrete_args[index];
        }
    }
    return ty;
}

fn cloneStringSlice(allocator: std.mem.Allocator, values: []const []const u8) ![][]const u8 {
    const cloned = try allocator.alloc([]const u8, values.len);
    for (values, 0..) |value, index| cloned[index] = try allocator.dupe(u8, value);
    return cloned;
}

fn enumDefaultSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        .integer => |node| node.span,
        .float => |node| node.span,
        .string => |node| node.span,
        .bool => |node| node.span,
        .array => |node| node.span,
        .builder_array => |node| node.span,
        .callback => |node| node.span,
        .struct_literal => |node| node.span,
        .native_state => |node| node.span,
        .native_user_data => |node| node.span,
        .native_recover => |node| node.span,
        .ownership => |node| node.span,
        .unary => |node| node.span,
        .binary => |node| node.span,
        .conditional => |node| node.span,
        .identifier => |node| node.span,
        .member => |node| node.span,
        .index => |node| node.span,
        .call => |node| node.span,
        .try_expr => |node| node.span,
    };
}
