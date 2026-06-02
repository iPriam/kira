const std = @import("std");
const syntax = @import("kira_ksl_syntax_model");
const shader_model = @import("kira_shader_model");
const shader_ir = @import("kira_shader_ir");
const parent = @import("analyzer.zig");
const utils = @import("analyzer_utils.zig");

const Analyzer = parent.Analyzer;
const ShaderScope = parent.ShaderScope;
const qualifiedKey = utils.qualifiedKey;
const qualifiedNameText = utils.qualifiedNameText;
const builtinType = utils.builtinType;
const findField = utils.findField;
const typeEql = utils.typeEql;
const intrinsicFromName = utils.intrinsicFromName;
const inferBinaryType = utils.inferBinaryType;
const isVectorType = utils.isVectorType;
const vectorScalar = utils.vectorScalar;

pub const FunctionScope = struct {
    analyzer: *Analyzer,
    current_module_alias: ?[]const u8,
    shader_scope: ?*const ShaderScope,
    return_type: shader_model.Type,
    locals: std.StringHashMap(shader_model.Type),
    params: std.StringHashMap(shader_model.Type),

    pub fn init(
        analyzer: *Analyzer,
        current_module_alias: ?[]const u8,
        shader_scope: ?*const ShaderScope,
        params: []const shader_ir.ParamDecl,
        return_type: shader_model.Type,
    ) !FunctionScope {
        var scope = FunctionScope{
            .analyzer = analyzer,
            .current_module_alias = current_module_alias,
            .shader_scope = shader_scope,
            .return_type = return_type,
            .locals = std.StringHashMap(shader_model.Type).init(analyzer.allocator),
            .params = std.StringHashMap(shader_model.Type).init(analyzer.allocator),
        };
        for (params) |param_decl| try scope.params.put(param_decl.name, param_decl.ty);
        return scope;
    }

    pub fn lowerBlock(self: *FunctionScope, block: syntax.ast.Block) anyerror!shader_ir.Block {
        var statements = std.array_list.Managed(shader_ir.Statement).init(self.analyzer.allocator);
        for (block.statements) |statement| {
            try statements.append(try self.lowerStatement(statement));
        }
        return .{
            .statements = try statements.toOwnedSlice(),
            .span = block.span,
        };
    }

    fn lowerStatement(self: *FunctionScope, statement: syntax.ast.Statement) anyerror!shader_ir.Statement {
        return switch (statement) {
            .let_stmt => |let_stmt| .{ .let_stmt = try self.lowerLet(let_stmt) },
            .assign_stmt => |assign_stmt| .{ .assign_stmt = try self.lowerAssign(assign_stmt) },
            .expr_stmt => |expr_stmt| .{ .expr_stmt = .{
                .expr = try self.lowerExpr(expr_stmt.expr, null),
                .span = expr_stmt.span,
            } },
            .return_stmt => |return_stmt| .{ .return_stmt = try self.lowerReturn(return_stmt) },
            .if_stmt => |if_stmt| .{ .if_stmt = try self.lowerIf(if_stmt) },
        };
    }

    fn lowerLet(self: *FunctionScope, let_stmt: syntax.ast.LetStatement) anyerror!shader_ir.LetStatement {
        if (let_stmt.ty == null and let_stmt.value == null) {
            try self.analyzer.emitDiagnostic("KSL012", "let declaration needs a type or value", let_stmt.span, "Add a type annotation, an initializer, or both.");
            return error.DiagnosticsEmitted;
        }

        const declared_ty = if (let_stmt.ty) |ty| try self.analyzer.resolveTypeRef(self.current_module_alias, ty.*) else null;
        const value = if (let_stmt.value) |value_expr| try self.lowerExpr(value_expr, declared_ty) else null;
        const final_ty = declared_ty orelse value.?.ty;
        try self.locals.put(let_stmt.name, final_ty);

        return .{
            .name = let_stmt.name,
            .ty = final_ty,
            .value = value,
            .span = let_stmt.span,
        };
    }

    fn lowerAssign(self: *FunctionScope, assign_stmt: syntax.ast.AssignStatement) anyerror!shader_ir.AssignStatement {
        const target = try self.lowerExpr(assign_stmt.target, null);
        try self.validateWritable(assign_stmt.target, target);
        const value = try self.lowerExpr(assign_stmt.value, target.ty);
        if (!typeEql(target.ty, value.ty)) {
            try self.analyzer.emitDiagnostic("KSL061", "assignment type mismatch", assign_stmt.span, "Make the assigned value match the target type exactly.");
            return error.DiagnosticsEmitted;
        }
        return .{
            .target = target,
            .value = value,
            .span = assign_stmt.span,
        };
    }

    fn lowerReturn(self: *FunctionScope, return_stmt: syntax.ast.ReturnStatement) anyerror!shader_ir.ReturnStatement {
        const value = if (return_stmt.value) |expr| try self.lowerExpr(expr, self.return_type) else null;
        if (self.return_type != .void and value == null) {
            try self.analyzer.emitDiagnostic("KSL013", "missing return value", return_stmt.span, "Return a value that matches the function result type.");
            return error.DiagnosticsEmitted;
        }
        if (value) |value_expr| {
            if (!typeEql(self.return_type, value_expr.ty)) {
                try self.analyzer.emitDiagnostic("KSL014", "return type mismatch", return_stmt.span, "Return a value that matches the function result type.");
                return error.DiagnosticsEmitted;
            }
        }
        return .{
            .value = value,
            .span = return_stmt.span,
        };
    }

    fn lowerIf(self: *FunctionScope, if_stmt: syntax.ast.IfStatement) anyerror!shader_ir.IfStatement {
        const condition = try self.lowerExpr(if_stmt.condition, .{ .scalar = .bool });
        if (condition.ty != .scalar or condition.ty.scalar != .bool) {
            try self.analyzer.emitDiagnostic("KSL015", "if condition must be Bool", syntax.ast.exprSpan(if_stmt.condition.*), "Use a boolean expression in the `if` condition.");
            return error.DiagnosticsEmitted;
        }
        return .{
            .condition = condition,
            .then_block = try self.lowerBlock(if_stmt.then_block),
            .else_block = if (if_stmt.else_block) |else_block| try self.lowerBlock(else_block) else null,
            .span = if_stmt.span,
        };
    }

    fn lowerExpr(self: *FunctionScope, expr: *const syntax.ast.Expr, expected_ty: ?shader_model.Type) anyerror!*shader_ir.Expr {
        return switch (expr.*) {
            .bool => |value| try self.allocExpr(.{
                .ty = .{ .scalar = .bool },
                .span = value.span,
                .node = .{ .const_value = .{ .bool = value.value } },
            }),
            .float => |value| try self.allocExpr(.{
                .ty = .{ .scalar = .float },
                .span = value.span,
                .node = .{ .const_value = .{ .float = try std.fmt.parseFloat(f32, value.text) } },
            }),
            .integer => |value| try self.lowerInteger(value, expected_ty),
            .identifier => |value| try self.lowerIdentifier(value, expected_ty),
            .unary => |value| try self.lowerUnary(value),
            .binary => |value| try self.lowerBinary(value),
            .member => |value| try self.lowerMember(value, expected_ty),
            .index => |value| try self.lowerIndex(value),
            .call => |value| try self.lowerCall(value),
            .string => {
                try self.analyzer.emitDiagnostic("KSL091", "unsupported KSL construct", syntax.ast.exprSpan(expr.*), "String values are not part of KSL v1.");
                return error.DiagnosticsEmitted;
            },
        };
    }

    fn lowerInteger(self: *FunctionScope, value: syntax.ast.IntegerLiteral, expected_ty: ?shader_model.Type) anyerror!*shader_ir.Expr {
        const ty = expected_ty orelse {
            try self.analyzer.emitDiagnostic("KSL021", "ambiguous integer literal", value.span, "Write an explicit `Int` or `UInt` context for this literal.");
            return error.DiagnosticsEmitted;
        };
        if (ty != .scalar or (ty.scalar != .int and ty.scalar != .uint)) {
            try self.analyzer.emitDiagnostic("KSL021", "ambiguous integer literal", value.span, "Write an explicit `Int` or `UInt` context for this literal.");
            return error.DiagnosticsEmitted;
        }
        return try self.allocExpr(.{
            .ty = ty,
            .span = value.span,
            .node = .{ .const_value = switch (ty.scalar) {
                .int => .{ .int = try std.fmt.parseInt(i32, value.text, 10) },
                .uint => .{ .uint = try std.fmt.parseInt(u32, value.text, 10) },
                else => unreachable,
            } },
        });
    }

    fn lowerIdentifier(self: *FunctionScope, value: syntax.ast.IdentifierExpr, expected_ty: ?shader_model.Type) anyerror!*shader_ir.Expr {
        _ = expected_ty;
        const name = try qualifiedNameText(self.analyzer.allocator, value.name);
        if (value.name.segments.len == 1) {
            if (self.locals.get(name)) |local_ty| {
                return try self.allocExpr(.{
                    .ty = local_ty,
                    .span = value.span,
                    .node = .{ .name = .{ .kind = .local, .name = name } },
                });
            }
            if (self.params.get(name)) |param_ty| {
                return try self.allocExpr(.{
                    .ty = param_ty,
                    .span = value.span,
                    .node = .{ .name = .{ .kind = .param, .name = name } },
                });
            }
            if (self.shader_scope) |shader_scope| {
                for (shader_scope.options) |option_decl| {
                    if (std.mem.eql(u8, option_decl.name, name)) {
                        return try self.allocExpr(.{
                            .ty = option_decl.ty,
                            .span = value.span,
                            .node = .{ .name = .{ .kind = .option, .name = name } },
                        });
                    }
                }
                for (shader_scope.resources) |resource_decl| {
                    if (std.mem.eql(u8, resource_decl.name, name)) {
                        return try self.allocExpr(.{
                            .ty = resource_decl.ty,
                            .span = value.span,
                            .node = .{ .name = .{ .kind = .resource, .name = name } },
                        });
                    }
                }
            }
            if (self.lookupFunction(self.current_module_alias, name)) |_| {
                return try self.allocExpr(.{
                    .ty = .{ .void = {} },
                    .span = value.span,
                    .node = .{ .name = .{ .kind = if (self.current_module_alias == null) .function else .imported_function, .name = name, .module_alias = self.current_module_alias } },
                });
            }
        }

        try self.analyzer.emitDiagnostic("KSL016", "unknown name", value.span, "Declare the name before it is used or import the module that defines it.");
        return error.DiagnosticsEmitted;
    }

    fn lowerUnary(self: *FunctionScope, unary_expr: syntax.ast.UnaryExpr) anyerror!*shader_ir.Expr {
        const operand = try self.lowerExpr(unary_expr.operand, null);
        return try self.allocExpr(.{
            .ty = operand.ty,
            .span = unary_expr.span,
            .node = .{ .unary = .{
                .op = switch (unary_expr.op) {
                    .neg => .neg,
                    .not => .not,
                },
                .operand = operand,
            } },
        });
    }

    fn lowerBinary(self: *FunctionScope, binary_expr: syntax.ast.BinaryExpr) anyerror!*shader_ir.Expr {
        const left = try self.lowerExpr(binary_expr.left, null);
        const right = try self.lowerExpr(binary_expr.right, left.ty);
        const result_ty = try inferBinaryType(self.analyzer, binary_expr, left.ty, right.ty);
        return try self.allocExpr(.{
            .ty = result_ty,
            .span = binary_expr.span,
            .node = .{ .binary = .{
                .op = @enumFromInt(@intFromEnum(binary_expr.op)),
                .left = left,
                .right = right,
            } },
        });
    }

    fn lowerMember(self: *FunctionScope, member_expr: syntax.ast.MemberExpr, expected_ty: ?shader_model.Type) anyerror!*shader_ir.Expr {
        _ = expected_ty;
        if (member_expr.object.* == .identifier and member_expr.object.identifier.name.segments.len == 1) {
            const module_name = member_expr.object.identifier.name.segments[0].text;
            if (self.lookupFunction(module_name, member_expr.name)) |_| {
                return try self.allocExpr(.{
                    .ty = .{ .void = {} },
                    .span = member_expr.span,
                    .node = .{ .name = .{ .kind = .imported_function, .name = member_expr.name, .module_alias = module_name } },
                });
            }
        }

        const object = try self.lowerExpr(member_expr.object, null);
        if (isVectorType(object.ty)) {
            return try self.allocExpr(.{
                .ty = .{ .scalar = vectorScalar(object.ty).? },
                .span = member_expr.span,
                .node = .{ .member = .{ .object = object, .name = member_expr.name } },
            });
        }
        if (object.ty == .runtime_array and std.mem.eql(u8, member_expr.name, "count")) {
            return try self.allocExpr(.{
                .ty = .{ .scalar = .uint },
                .span = member_expr.span,
                .node = .{ .member = .{ .object = object, .name = member_expr.name } },
            });
        }
        if (object.ty == .struct_ref) {
            const type_decl = self.analyzer.resolved_types.get(object.ty.struct_ref) orelse {
                try self.analyzer.emitDiagnostic("KSL011", "unknown type", member_expr.span, "Declare the type before it is used or import the module that defines it.");
                return error.DiagnosticsEmitted;
            };
            const field_decl = findField(type_decl.fields, member_expr.name) orelse {
                try self.analyzer.emitDiagnostic("KSL017", "unknown field", member_expr.span, "Use a field that exists on the struct type.");
                return error.DiagnosticsEmitted;
            };
            return try self.allocExpr(.{
                .ty = field_decl.ty,
                .span = member_expr.span,
                .node = .{ .member = .{ .object = object, .name = member_expr.name } },
            });
        }

        try self.analyzer.emitDiagnostic("KSL017", "unknown field", member_expr.span, "Use a field that exists on the value type.");
        return error.DiagnosticsEmitted;
    }

    fn lowerIndex(self: *FunctionScope, index_expr: syntax.ast.IndexExpr) anyerror!*shader_ir.Expr {
        const object = try self.lowerExpr(index_expr.object, null);
        const index = try self.lowerExpr(index_expr.index, .{ .scalar = .uint });
        if (object.ty != .runtime_array) {
            try self.analyzer.emitDiagnostic("KSL018", "indexing is not valid here", index_expr.span, "Only storage runtime arrays are indexable in KSL v1.");
            return error.DiagnosticsEmitted;
        }
        return try self.allocExpr(.{
            .ty = object.ty.runtime_array.*,
            .span = index_expr.span,
            .node = .{ .index = .{ .object = object, .index = index } },
        });
    }

    fn lowerCall(self: *FunctionScope, call_expr: syntax.ast.CallExpr) anyerror!*shader_ir.Expr {
        if (call_expr.callee.* == .identifier) {
            const callee_name = try qualifiedNameText(self.analyzer.allocator, call_expr.callee.identifier.name);
            if (builtinType(callee_name)) |constructor_ty| {
                return try self.lowerCallArgs(call_expr, .{ .constructor = constructor_ty }, constructor_ty);
            }
            if (intrinsicFromName(callee_name)) |intrinsic| {
                return try self.lowerIntrinsicCall(call_expr, intrinsic);
            }
            if (self.lookupFunction(self.current_module_alias, callee_name)) |function_decl| {
                return try self.lowerResolvedFunctionCall(call_expr, .{ .kind = if (self.current_module_alias == null) .function else .imported_function, .name = callee_name, .module_alias = self.current_module_alias }, function_decl.return_type);
            }
        }
        if (call_expr.callee.* == .member and call_expr.callee.member.object.* == .identifier and call_expr.callee.member.object.identifier.name.segments.len == 1) {
            const module_alias = call_expr.callee.member.object.identifier.name.segments[0].text;
            const function_decl = self.lookupFunction(module_alias, call_expr.callee.member.name) orelse {
                try self.analyzer.emitDiagnostic("KSL019", "unknown function", call_expr.span, "Declare the function before it is called or import the module that defines it.");
                return error.DiagnosticsEmitted;
            };
            return try self.lowerResolvedFunctionCall(call_expr, .{
                .kind = .imported_function,
                .name = call_expr.callee.member.name,
                .module_alias = module_alias,
            }, function_decl.return_type);
        }

        try self.analyzer.emitDiagnostic("KSL019", "unknown function", call_expr.span, "Declare the function before it is called or import the module that defines it.");
        return error.DiagnosticsEmitted;
    }

    fn lowerResolvedFunctionCall(self: *FunctionScope, call_expr: syntax.ast.CallExpr, callee: shader_ir.NameRef, return_ty: shader_model.Type) anyerror!*shader_ir.Expr {
        return try self.lowerCallArgs(call_expr, .{ .function = callee }, return_ty);
    }

    fn lowerIntrinsicCall(self: *FunctionScope, call_expr: syntax.ast.CallExpr, intrinsic: shader_ir.Intrinsic) anyerror!*shader_ir.Expr {
        const return_ty: shader_model.Type = switch (intrinsic) {
            .normalize, .sin => blk: {
                if (call_expr.args.len != 1) {
                    try self.analyzer.emitDiagnostic("KSL020", "invalid intrinsic call", call_expr.span, "Pass the expected arguments to the intrinsic.");
                    return error.DiagnosticsEmitted;
                }
                const first = try self.lowerExpr(call_expr.args[0], null);
                break :blk first.ty;
            },
            .length => blk: {
                if (call_expr.args.len != 1) {
                    try self.analyzer.emitDiagnostic("KSL020", "invalid intrinsic call", call_expr.span, "Pass the expected arguments to the intrinsic.");
                    return error.DiagnosticsEmitted;
                }
                break :blk .{ .scalar = .float };
            },
            .sample => .{ .vector = .{ .scalar = .float, .width = 4 } },
            .mul => blk: {
                if (call_expr.args.len != 2) {
                    try self.analyzer.emitDiagnostic("KSL020", "invalid intrinsic call", call_expr.span, "Pass the expected arguments to the intrinsic.");
                    return error.DiagnosticsEmitted;
                }
                const left = try self.lowerExpr(call_expr.args[0], null);
                const right = try self.lowerExpr(call_expr.args[1], null);
                if (left.ty == .matrix and right.ty == .vector) break :blk .{ .vector = .{ .scalar = .float, .width = right.ty.vector.width } };
                if (left.ty == .matrix and right.ty == .matrix) break :blk left.ty;
                if (typeEql(left.ty, right.ty)) break :blk left.ty;
                break :blk right.ty;
            },
            .dot, .pow, .atan2, .smoothstep => blk: {
                const expected_args: usize = if (intrinsic == .smoothstep) 3 else 2;
                if (call_expr.args.len != expected_args) {
                    try self.analyzer.emitDiagnostic("KSL020", "invalid intrinsic call", call_expr.span, "Pass the expected arguments to the intrinsic.");
                    return error.DiagnosticsEmitted;
                }
                break :blk .{ .scalar = .float };
            },
        };
        return try self.lowerCallArgs(call_expr, .{ .intrinsic = intrinsic }, return_ty);
    }

    fn lowerCallArgs(self: *FunctionScope, call_expr: syntax.ast.CallExpr, callee: shader_ir.Callee, return_ty: shader_model.Type) anyerror!*shader_ir.Expr {
        var args = std.array_list.Managed(*shader_ir.Expr).init(self.analyzer.allocator);
        for (call_expr.args) |arg| try args.append(try self.lowerExpr(arg, null));
        return try self.allocExpr(.{
            .ty = return_ty,
            .span = call_expr.span,
            .node = .{ .call = .{
                .callee = callee,
                .args = try args.toOwnedSlice(),
            } },
        });
    }

    fn validateWritable(self: *FunctionScope, target_expr: *const syntax.ast.Expr, lowered_target: *const shader_ir.Expr) anyerror!void {
        switch (target_expr.*) {
            .identifier => |value| {
                const name = try qualifiedNameText(self.analyzer.allocator, value.name);
                if (self.params.contains(name)) {
                    try self.analyzer.emitDiagnostic("KSL071", "resource is not writable", value.span, "Shader parameters are read-only.");
                    return error.DiagnosticsEmitted;
                }
                if (self.shader_scope) |shader_scope| {
                    for (shader_scope.resources) |resource_decl| {
                        if (!std.mem.eql(u8, resource_decl.name, name)) continue;
                        if (resource_decl.kind == .uniform or resource_decl.access == .read) {
                            try self.analyzer.emitDiagnostic("KSL071", "resource is not writable", value.span, "Write to a local value or move writable state into a `storage read_write` resource.");
                            return error.DiagnosticsEmitted;
                        }
                        if (resource_decl.access == .read_write and shader_scope.stage != .compute) {
                            try self.analyzer.emitDiagnostic("KSL071", "resource is not writable", value.span, "Writable storage resources are compute-only in KSL v1.");
                            return error.DiagnosticsEmitted;
                        }
                    }
                }
            },
            .member => |value| try self.validateWritable(value.object, lowered_target),
            .index => |value| try self.validateWritable(value.object, lowered_target),
            else => {},
        }
    }

    fn lookupFunction(self: *FunctionScope, module_alias: ?[]const u8, name: []const u8) ?shader_ir.FunctionDecl {
        const key = qualifiedKey(self.analyzer.allocator, module_alias, name) catch return null;
        if (self.analyzer.resolved_functions.get(key)) |function_decl| return function_decl;
        if (self.analyzer.function_sources.get(key)) |source_info| {
            return self.analyzer.lowerFunctionDecl(source_info, key, null) catch null;
        }
        return null;
    }

    fn allocExpr(self: *FunctionScope, expr: shader_ir.Expr) anyerror!*shader_ir.Expr {
        const value = try self.analyzer.allocator.create(shader_ir.Expr);
        value.* = expr;
        return value;
    }
};
