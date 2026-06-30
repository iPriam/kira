//! Declarative macro template instantiation: deep-clone an `expand` template applying a per-call
//! substitution environment (fragment parameters -> arguments/temporaries, hygienic renames). Split
//! from macro_expand.zig (Core Law #5); the user-code walk and call-site logic live there.

const std = @import("std");
const syntax = @import("kira_syntax_model");
const source_pkg = @import("kira_source");
const expand = @import("macro_expand.zig");

const ast = syntax.ast;
const Span = source_pkg.Span;
const Expander = expand.Expander;
const Env = expand.Env;
const StatementList = std.array_list.Managed(ast.Statement);

// --- Hygiene: collect template-introduced bindings --------------------------

/// Assign a fresh gensym to every identifier the template introduces (let / for bindings) that is
/// not a fragment parameter, so the expansion can never collide with or capture a caller name.
pub fn collectIntroduced(exp: *Expander, macro: ast.MacroDecl, statements: []const ast.Statement, env: *Env) anyerror!void {
    for (statements) |statement| {
        switch (statement) {
            .let_stmt => |s| try introduce(exp, macro, s.name, env),
            .for_stmt => |s| {
                try introduce(exp, macro, s.binding_name, env);
                try collectIntroduced(exp, macro, s.body.statements, env);
            },
            .if_stmt => |s| {
                try collectIntroduced(exp, macro, s.then_block.statements, env);
                if (s.else_block) |eb| try collectIntroduced(exp, macro, eb.statements, env);
            },
            .while_stmt => |s| try collectIntroduced(exp, macro, s.body.statements, env),
            else => {},
        }
    }
}

fn introduce(exp: *Expander, macro: ast.MacroDecl, name: []const u8, env: *Env) !void {
    // Fragment parameters cross the boundary as written; everything else is renamed hygienically.
    for (macro.params) |param| {
        if (std.mem.eql(u8, param.name, name)) return;
    }
    if (env.renames.contains(name)) return;
    try env.renames.put(exp.allocator, name, try exp.gensym(name));
}

// --- Template instantiation (deep clone + substitution) ---------------------

pub fn instantiateStatement(exp: *Expander, env: *Env, statement: ast.Statement, out: *StatementList) anyerror!void {
    switch (statement) {
        .let_stmt => |s| {
            var ns = s;
            ns.name = renamed(env, s.name);
            ns.annotations = &.{};
            if (s.value) |value| ns.value = try instantiateExpr(exp, env, value);
            try out.append(.{ .let_stmt = ns });
        },
        .assign_stmt => |s| {
            const target = try instantiateExpr(exp, env, s.target);
            const value = try instantiateExpr(exp, env, s.value);
            try out.append(.{ .assign_stmt = .{ .target = target, .value = value, .span = s.span } });
        },
        .expr_stmt => |s| {
            const value = try instantiateExpr(exp, env, s.expr);
            try out.append(.{ .expr_stmt = .{ .expr = value, .span = s.span } });
        },
        .return_stmt => |s| {
            var ns = s;
            if (s.value) |value| ns.value = try instantiateExpr(exp, env, value);
            try out.append(.{ .return_stmt = ns });
        },
        .if_stmt => |s| {
            const condition = try instantiateExpr(exp, env, s.condition);
            const then_block = try instantiateBlock(exp, env, s.then_block);
            const else_block = if (s.else_block) |eb| try instantiateBlock(exp, env, eb) else null;
            try out.append(.{ .if_stmt = .{ .condition = condition, .then_block = then_block, .else_block = else_block, .span = s.span } });
        },
        .for_stmt => |s| {
            var ns = s;
            ns.binding_name = renamed(env, s.binding_name);
            ns.iterator = try instantiateExpr(exp, env, s.iterator);
            if (s.range_end) |re| ns.range_end = try instantiateExpr(exp, env, re);
            ns.body = try instantiateBlock(exp, env, s.body);
            try out.append(.{ .for_stmt = ns });
        },
        .while_stmt => |s| {
            const condition = try instantiateExpr(exp, env, s.condition);
            const body = try instantiateBlock(exp, env, s.body);
            try out.append(.{ .while_stmt = .{ .condition = condition, .body = body, .span = s.span } });
        },
        .break_stmt, .continue_stmt => try out.append(statement),
        else => {
            try exp.err("KMAC014", "unsupported construct in macro template", "This statement form is not yet supported inside a declarative macro's `expand` block.", stmtSpan(statement), "unsupported here", "Use let / assignment / expression / return / if / for / while inside a macro template.");
        },
    }
}

fn instantiateBlock(exp: *Expander, env: *Env, block: ast.Block) anyerror!ast.Block {
    var out = StatementList.init(exp.allocator);
    for (block.statements) |statement| try instantiateStatement(exp, env, statement, &out);
    return .{ .statements = try out.toOwnedSlice(), .span = block.span };
}

pub fn instantiateExpr(exp: *Expander, env: *Env, expr: *ast.Expr) anyerror!*ast.Expr {
    switch (expr.*) {
        .identifier => |e| {
            if (e.name.segments.len == 1) {
                const name = e.name.segments[0].text;
                if (env.params.get(name)) |replacement| {
                    switch (replacement) {
                        .expr_temp => |temp| return exp.makeIdent(temp, e.span),
                        .place_expr => |place| return cloneExpr(exp, place),
                    }
                }
                if (env.renames.get(name)) |gensym_name| return exp.makeIdent(gensym_name, e.span);
            }
            return cloneExpr(exp, expr);
        },
        .integer, .float, .string, .bool => return cloneExpr(exp, expr),
        .binary => |e| return makeExpr(exp, .{ .binary = .{ .op = e.op, .lhs = try instantiateExpr(exp, env, e.lhs), .rhs = try instantiateExpr(exp, env, e.rhs), .span = e.span } }),
        .unary => |e| return makeExpr(exp, .{ .unary = .{ .op = e.op, .operand = try instantiateExpr(exp, env, e.operand), .span = e.span } }),
        .ownership => |e| return makeExpr(exp, .{ .ownership = .{ .op = e.op, .operand = try instantiateExpr(exp, env, e.operand), .span = e.span } }),
        .try_expr => |e| return makeExpr(exp, .{ .try_expr = .{ .operand = try instantiateExpr(exp, env, e.operand), .span = e.span } }),
        .member => |e| return makeExpr(exp, .{ .member = .{ .object = try instantiateExpr(exp, env, e.object), .member = e.member, .span = e.span } }),
        .index => |e| return makeExpr(exp, .{ .index = .{ .object = try instantiateExpr(exp, env, e.object), .index = try instantiateExpr(exp, env, e.index), .span = e.span } }),
        .conditional => |e| return makeExpr(exp, .{ .conditional = .{ .condition = try instantiateExpr(exp, env, e.condition), .then_expr = try instantiateExpr(exp, env, e.then_expr), .else_expr = try instantiateExpr(exp, env, e.else_expr), .span = e.span } }),
        .array => |e| {
            const elements = try exp.allocator.alloc(*ast.Expr, e.elements.len);
            for (e.elements, 0..) |element, i| elements[i] = try instantiateExpr(exp, env, element);
            return makeExpr(exp, .{ .array = .{ .elements = elements, .span = e.span } });
        },
        .call => |e| {
            if (e.is_macro) {
                try exp.err("KMAC015", "nested macro invocation", "Invoking a macro from inside another macro's `expand` block is not yet supported.", e.span, "nested macro call here", "Inline the expansion or wait for procedural macros.");
                return exp.makeIntZero(e.span);
            }
            const args = try exp.allocator.alloc(ast.CallArg, e.args.len);
            for (e.args, 0..) |arg, i| args[i] = .{ .label = arg.label, .value = try instantiateExpr(exp, env, arg.value), .span = arg.span };
            return makeExpr(exp, .{ .call = .{ .callee = try instantiateExpr(exp, env, e.callee), .args = args, .trailing_builder = e.trailing_builder, .trailing_callback = e.trailing_callback, .is_macro = false, .span = e.span } });
        },
        else => return cloneExpr(exp, expr),
    }
}

fn renamed(env: *Env, name: []const u8) []const u8 {
    return env.renames.get(name) orelse name;
}

fn makeExpr(exp: *Expander, value: ast.Expr) !*ast.Expr {
    const node = try exp.allocator.create(ast.Expr);
    node.* = value;
    return node;
}

/// Structural deep clone (no substitution) for leaf / pass-through expressions and `place` args.
fn cloneExpr(exp: *Expander, expr: *ast.Expr) anyerror!*ast.Expr {
    switch (expr.*) {
        .integer, .float, .string, .bool, .identifier => {
            const node = try exp.allocator.create(ast.Expr);
            node.* = expr.*;
            return node;
        },
        .binary => |e| return makeExpr(exp, .{ .binary = .{ .op = e.op, .lhs = try cloneExpr(exp, e.lhs), .rhs = try cloneExpr(exp, e.rhs), .span = e.span } }),
        .unary => |e| return makeExpr(exp, .{ .unary = .{ .op = e.op, .operand = try cloneExpr(exp, e.operand), .span = e.span } }),
        .ownership => |e| return makeExpr(exp, .{ .ownership = .{ .op = e.op, .operand = try cloneExpr(exp, e.operand), .span = e.span } }),
        .try_expr => |e| return makeExpr(exp, .{ .try_expr = .{ .operand = try cloneExpr(exp, e.operand), .span = e.span } }),
        .member => |e| return makeExpr(exp, .{ .member = .{ .object = try cloneExpr(exp, e.object), .member = e.member, .span = e.span } }),
        .index => |e| return makeExpr(exp, .{ .index = .{ .object = try cloneExpr(exp, e.object), .index = try cloneExpr(exp, e.index), .span = e.span } }),
        .conditional => |e| return makeExpr(exp, .{ .conditional = .{ .condition = try cloneExpr(exp, e.condition), .then_expr = try cloneExpr(exp, e.then_expr), .else_expr = try cloneExpr(exp, e.else_expr), .span = e.span } }),
        .array => |e| {
            const elements = try exp.allocator.alloc(*ast.Expr, e.elements.len);
            for (e.elements, 0..) |element, i| elements[i] = try cloneExpr(exp, element);
            return makeExpr(exp, .{ .array = .{ .elements = elements, .span = e.span } });
        },
        else => {
            // Pass-through node kinds (callback, struct literals, native_*, builder) are reused by
            // reference; they do not participate in fragment substitution.
            const node = try exp.allocator.create(ast.Expr);
            node.* = expr.*;
            return node;
        },
    }
}

fn stmtSpan(statement: ast.Statement) Span {
    return switch (statement) {
        inline else => |s| s.span,
    };
}
