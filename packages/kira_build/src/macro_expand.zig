//! Declarative macro expansion (frontend AST -> AST).
//!
//! Runs after parsing / import merging and before semantic analysis. It collects every
//! declarative `macro` declaration, removes the macro declarations from the program, and replaces
//! every `name!(args)` call with the macro's expanded template. Because the output is ordinary
//! `syntax.ast`, all backends (VM, LLVM/native, hybrid, WASM) see identical post-expansion code —
//! macro parity is structural.
//!
//! Semantics implemented here:
//!   * `expr` fragments are evaluated exactly once: each is bound to a hygienic `let` temporary at
//!     the call site (hoisted before the enclosing statement) and the temporary is substituted at
//!     every occurrence. Single evaluation, ownership unchanged.
//!   * `place` fragments are substituted as an assignable lvalue path (clone per occurrence).
//!   * Identifiers introduced by the template (anything not a fragment parameter) are hygienic:
//!     each expansion renames them to a fresh gensym, so a template `temporary` can never collide
//!     with or capture a caller name.
//!   * In statement position the template's statements are spliced in place. In expression
//!     position the template's leading statements are hoisted and its trailing expression becomes
//!     the value (KMAC005 if the template produces no trailing expression value).
//!
//! Procedural `comptime macro`s are not handled here (the parser rejects them until the
//! compile-time evaluator lands); see docs/macros.md.

const std = @import("std");
const syntax = @import("kira_syntax_model");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const parser = @import("kira_parser");
const inst = @import("macro_instantiate.zig");
const eval = @import("macro_eval.zig");
const proc = @import("macro_procedural.zig");

const ast = syntax.ast;
const Span = source_pkg.Span;
const StatementList = std.array_list.Managed(ast.Statement);

const max_expansion_depth: u32 = 256;

/// Substitution target for a fragment parameter within a single expansion.
pub const Replacement = union(enum) {
    /// Hygienic temporary identifier name bound once at the call site (`expr` fragment).
    expr_temp: []const u8,
    /// Caller-supplied lvalue expression, deep-cloned at each occurrence (`place` fragment).
    place_expr: *ast.Expr,
};

pub const Env = struct {
    params: std.StringHashMapUnmanaged(Replacement) = .{},
    renames: std.StringHashMapUnmanaged([]const u8) = .{},

    fn deinit(self: *Env, allocator: std.mem.Allocator) void {
        self.params.deinit(allocator);
        self.renames.deinit(allocator);
    }
};

pub const Expander = struct {
    allocator: std.mem.Allocator,
    macros: std.StringHashMapUnmanaged(ast.MacroDecl) = .{},
    // Procedural attribute/derive macros, keyed by name (invoked via `@Name` / `@Derive(Name)`).
    proc_macros: std.StringHashMapUnmanaged(ast.MacroDecl) = .{},
    // Procedural function-position macros, keyed by name (invoked via top-level `name!(args)`).
    func_macros: std.StringHashMapUnmanaged(ast.MacroDecl) = .{},
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
    gensym_counter: u64 = 0,
    depth: u32 = 0,

    pub fn err(self: *Expander, code: []const u8, title: []const u8, message: []const u8, span: Span, label: []const u8, help: []const u8) !void {
        try diagnostics.appendOwned(self.allocator, self.diags, .{
            .severity = .@"error",
            .code = code,
            .title = title,
            .message = message,
            .labels = &.{diagnostics.primaryLabel(span, label)},
            .help = help,
        });
    }

    pub fn gensym(self: *Expander, base: []const u8) ![]const u8 {
        const name = try std.fmt.allocPrint(self.allocator, "__macro_{d}_{s}", .{ self.gensym_counter, base });
        self.gensym_counter += 1;
        return name;
    }

    pub fn makeIdent(self: *Expander, name: []const u8, span: Span) !*ast.Expr {
        const segments = try self.allocator.alloc(ast.NameSegment, 1);
        segments[0] = .{ .text = name, .span = span };
        const node = try self.allocator.create(ast.Expr);
        node.* = .{ .identifier = .{ .name = .{ .segments = segments, .span = span }, .span = span } };
        return node;
    }

    pub fn makeIntZero(self: *Expander, span: Span) !*ast.Expr {
        const node = try self.allocator.create(ast.Expr);
        node.* = .{ .integer = .{ .value = 0, .span = span } };
        return node;
    }

    fn makeLet(self: *Expander, name: []const u8, value: *ast.Expr, span: Span) ast.Statement {
        _ = self;
        return .{ .let_stmt = .{
            .annotations = &.{},
            .storage = .immutable,
            .name = name,
            .type_expr = null,
            .value = value,
            .span = span,
        } };
    }
};

/// Expand macros and, if expansion produced any error-severity diagnostics, return
/// `error.DiagnosticsEmitted` so the caller bails before semantics (avoiding confusing secondary
/// errors from placeholder nodes). The single entry point every frontend path should use.
pub fn expandAndCheck(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    const before = diags.items.len;
    const expanded = try expandMacros(allocator, program, diags);
    for (diags.items[before..]) |diag| {
        if (diag.severity == .@"error") return error.DiagnosticsEmitted;
    }
    return expanded;
}

/// Expand all declarative macros in `program`. Returns a new program with macro declarations
/// removed and every macro call replaced. Diagnostics for malformed calls are appended to `diags`.
pub fn expandMacros(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    var exp = Expander{ .allocator = allocator, .diags = diags };
    defer exp.macros.deinit(allocator);
    defer exp.proc_macros.deinit(allocator);
    defer exp.func_macros.deinit(allocator);

    for (program.decls) |decl| {
        if (decl != .macro_decl) continue;
        const macro = decl.macro_decl;
        switch (macro.kind) {
            .declarative => try exp.macros.put(allocator, macro.name, macro),
            .proc_attribute, .proc_derive => try exp.proc_macros.put(allocator, macro.name, macro),
            .proc_function => try exp.func_macros.put(allocator, macro.name, macro),
        }
    }
    if (exp.macros.count() == 0 and exp.proc_macros.count() == 0 and exp.func_macros.count() == 0) return program;

    // `decls` is the source of truth semantics lowers from. A `function_decl` also appears in the
    // separate `functions` list, so we expand each function body exactly once here (walking it in
    // `decls`) and rebuild `functions` from the expanded decls — walking both lists would expand
    // every macro twice and double every diagnostic.
    var decls = std.array_list.Managed(syntax.ast.Decl).init(allocator);
    var decl_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    var functions = std.array_list.Managed(syntax.ast.FunctionDecl).init(allocator);
    const have_origins = program.decl_origins.len == program.decls.len;
    for (program.decls, 0..) |decl, index| {
        if (decl == .macro_decl) continue;
        const origin = if (have_origins) program.decl_origins[index] else syntax.ast.DeclOrigin{};

        // Top-level `name!(args)` function-macro invocation: run the macro and splice the
        // declarations it generates in its place.
        if (decl == .macro_invocation) {
            const generated = try proc.expandMacroInvocation(&exp, decl.macro_invocation);
            for (generated) |gen_decl| {
                const gen_walked = try walkDecl(&exp, gen_decl);
                try decls.append(gen_walked);
                if (gen_walked == .function_decl) try functions.append(gen_walked.function_decl);
                if (have_origins) try decl_origins.append(origin);
            }
            continue;
        }

        // Run attribute/derive macros attached to this declaration, stripping their annotations and
        // appending the declarations they generate (each re-parsed from the macro's `Syntax` output).
        const processed = try proc.applyDeclMacros(&exp, decl);
        const walked = try walkDecl(&exp, processed.decl);
        try decls.append(walked);
        if (walked == .function_decl) try functions.append(walked.function_decl);
        if (have_origins) try decl_origins.append(origin);

        for (processed.generated) |gen_decl| {
            const gen_walked = try walkDecl(&exp, gen_decl);
            try decls.append(gen_walked);
            if (gen_walked == .function_decl) try functions.append(gen_walked.function_decl);
            if (have_origins) try decl_origins.append(origin);
        }
    }

    return .{
        .imports = program.imports,
        .decls = try decls.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .import_origins = program.import_origins,
        .decl_origins = if (have_origins) try decl_origins.toOwnedSlice() else program.decl_origins,
        .function_origins = program.function_origins,
    };
}

// --- Declaration bodies -----------------------------------------------------
//
// Macro calls inside member function bodies of constructs / types / forms / extends are expanded
// by walking those function bodies. Non-function members are passed through unchanged.

fn walkDecl(exp: *Expander, decl: ast.Decl) !ast.Decl {
    switch (decl) {
        .function_decl => |d| {
            var nd = d;
            if (d.body) |body| nd.body = try walkBlock(exp, body);
            return .{ .function_decl = nd };
        },
        .type_decl => |d| {
            var nd = d;
            nd.members = try walkBodyMembers(exp, d.members);
            return .{ .type_decl = nd };
        },
        .construct_decl => |d| {
            var nd = d;
            nd.members = try walkBodyMembers(exp, d.members);
            return .{ .construct_decl = nd };
        },
        .construct_form_decl => |d| {
            var nd = d;
            var body = d.body;
            body.members = try walkBodyMembers(exp, d.body.members);
            nd.body = body;
            return .{ .construct_form_decl = nd };
        },
        .extend_decl => |d| {
            var nd = d;
            nd.members = try walkBodyMembers(exp, d.members);
            return .{ .extend_decl = nd };
        },
        else => return decl,
    }
}

fn walkBodyMembers(exp: *Expander, members: []ast.BodyMember) ![]ast.BodyMember {
    var out = std.array_list.Managed(ast.BodyMember).init(exp.allocator);
    for (members) |member| {
        switch (member) {
            .function_decl => |f| {
                var nf = f;
                if (f.body) |body| nf.body = try walkBlock(exp, body);
                try out.append(.{ .function_decl = nf });
            },
            .named_rule => |r| {
                // `test { ... }` / `expect { ... }` blocks (e.g. inside a `Test` form) carry macro
                // calls that must expand.
                var nr = r;
                var pre = StatementList.init(exp.allocator);
                if (r.value) |value| nr.value = try walkExpr(exp, value, &pre);
                if (r.block) |block| {
                    var body = try walkBlock(exp, block);
                    if (pre.items.len != 0) {
                        var merged = StatementList.init(exp.allocator);
                        try merged.appendSlice(pre.items);
                        try merged.appendSlice(body.statements);
                        body.statements = try merged.toOwnedSlice();
                    }
                    nr.block = body;
                }
                try out.append(.{ .named_rule = nr });
            },
            .lifecycle_hook => |h| {
                var nh = h;
                nh.body = try walkBlock(exp, h.body);
                try out.append(.{ .lifecycle_hook = nh });
            },
            else => try out.append(member),
        }
    }
    return out.toOwnedSlice();
}

// --- User-code walk: find macro calls, splice expansions --------------------

fn walkBlock(exp: *Expander, block: ast.Block) anyerror!ast.Block {
    return .{ .statements = try walkStatements(exp, block.statements), .span = block.span };
}

fn walkStatements(exp: *Expander, statements: []ast.Statement) anyerror![]ast.Statement {
    var out = StatementList.init(exp.allocator);
    for (statements) |statement| try walkStatement(exp, statement, &out);
    return out.toOwnedSlice();
}

fn walkStatement(exp: *Expander, statement: ast.Statement, out: *StatementList) anyerror!void {
    switch (statement) {
        .expr_stmt => |s| {
            if (macroCall(s.expr)) |_| {
                try expandCallAsStatements(exp, s.expr.call, out);
                return;
            }
            const value = try walkExpr(exp, s.expr, out);
            try out.append(.{ .expr_stmt = .{ .expr = value, .span = s.span } });
        },
        .let_stmt => |s| {
            var ns = s;
            if (s.value) |value| ns.value = try walkExpr(exp, value, out);
            try out.append(.{ .let_stmt = ns });
        },
        .assign_stmt => |s| {
            const target = try walkExpr(exp, s.target, out);
            const value = try walkExpr(exp, s.value, out);
            try out.append(.{ .assign_stmt = .{ .target = target, .value = value, .span = s.span } });
        },
        .return_stmt => |s| {
            var ns = s;
            if (s.value) |value| ns.value = try walkExpr(exp, value, out);
            try out.append(.{ .return_stmt = ns });
        },
        .if_stmt => |s| {
            // The condition is evaluated once, so hoisting its single-eval temporaries before the
            // `if` is sound.
            const condition = try walkExpr(exp, s.condition, out);
            const then_block = try walkBlock(exp, s.then_block);
            const else_block = if (s.else_block) |eb| try walkBlock(exp, eb) else null;
            try out.append(.{ .if_stmt = .{ .condition = condition, .then_block = then_block, .else_block = else_block, .span = s.span } });
        },
        .for_stmt => |s| {
            var ns = s;
            ns.iterator = try walkExpr(exp, s.iterator, out);
            if (s.range_end) |re| ns.range_end = try walkExpr(exp, re, out);
            ns.body = try walkBlock(exp, s.body);
            try out.append(.{ .for_stmt = ns });
        },
        .while_stmt => |s| {
            // A while condition is re-evaluated each iteration; a macro that would hoist a
            // single-eval temporary before the loop would change its meaning, so reject that.
            var cond_pre = StatementList.init(exp.allocator);
            const condition = try walkExpr(exp, s.condition, &cond_pre);
            if (cond_pre.items.len != 0) {
                try exp.err("KMAC013", "macro in while-condition", "An `expr` fragment macro that needs a single-evaluation temporary cannot be used directly in a `while` condition.", s.span, "this condition expands to hoisted statements", "Bind the macro result to a `let` before the loop, or use the macro in the loop body.");
                try out.appendSlice(cond_pre.items);
            }
            const body = try walkBlock(exp, s.body);
            try out.append(.{ .while_stmt = .{ .condition = condition, .body = body, .span = s.span } });
        },
        .match_stmt => |s| {
            var ns = s;
            ns.subject = try walkExpr(exp, s.subject, out);
            var arms = std.array_list.Managed(ast.MatchArm).init(exp.allocator);
            for (s.arms) |arm| {
                var na = arm;
                na.body = try walkBlock(exp, arm.body);
                try arms.append(na);
            }
            ns.arms = try arms.toOwnedSlice();
            try out.append(.{ .match_stmt = ns });
        },
        .switch_stmt => |s| {
            var ns = s;
            ns.subject = try walkExpr(exp, s.subject, out);
            var cases = std.array_list.Managed(ast.SwitchCase).init(exp.allocator);
            for (s.cases) |case| {
                var nc = case;
                nc.body = try walkBlock(exp, case.body);
                try cases.append(nc);
            }
            ns.cases = try cases.toOwnedSlice();
            if (s.default_block) |db| ns.default_block = try walkBlock(exp, db);
            try out.append(.{ .switch_stmt = ns });
        },
        .attempt_stmt => |s| {
            var ns = s;
            ns.body = try walkStatements(exp, s.body);
            var handlers = std.array_list.Managed(ast.HandleCase).init(exp.allocator);
            for (s.handlers) |handler| {
                var nh = handler;
                nh.body = try walkBlock(exp, handler.body);
                try handlers.append(nh);
            }
            ns.handlers = try handlers.toOwnedSlice();
            try out.append(.{ .attempt_stmt = ns });
        },
        .break_stmt, .continue_stmt => try out.append(statement),
    }
}

/// Walk an expression for nested macro calls. `pre` accumulates hoisted single-eval `let`
/// statements that must execute before the enclosing statement.
fn walkExpr(exp: *Expander, expr: *ast.Expr, pre: *StatementList) anyerror!*ast.Expr {
    switch (expr.*) {
        .call => |c| {
            if (c.is_macro) return expandCallAsExpr(exp, c, pre);
            expr.call.callee = try walkExpr(exp, c.callee, pre);
            for (expr.call.args) |*arg| arg.value = try walkExpr(exp, arg.value, pre);
            return expr;
        },
        .binary => {
            expr.binary.lhs = try walkExpr(exp, expr.binary.lhs, pre);
            expr.binary.rhs = try walkExpr(exp, expr.binary.rhs, pre);
            return expr;
        },
        .unary => {
            expr.unary.operand = try walkExpr(exp, expr.unary.operand, pre);
            return expr;
        },
        .ownership => {
            expr.ownership.operand = try walkExpr(exp, expr.ownership.operand, pre);
            return expr;
        },
        .try_expr => {
            expr.try_expr.operand = try walkExpr(exp, expr.try_expr.operand, pre);
            return expr;
        },
        .member => {
            expr.member.object = try walkExpr(exp, expr.member.object, pre);
            return expr;
        },
        .index => {
            expr.index.object = try walkExpr(exp, expr.index.object, pre);
            expr.index.index = try walkExpr(exp, expr.index.index, pre);
            return expr;
        },
        .conditional => {
            expr.conditional.condition = try walkExpr(exp, expr.conditional.condition, pre);
            expr.conditional.then_expr = try walkExpr(exp, expr.conditional.then_expr, pre);
            expr.conditional.else_expr = try walkExpr(exp, expr.conditional.else_expr, pre);
            return expr;
        },
        .array => {
            for (expr.array.elements) |*element| element.* = try walkExpr(exp, element.*, pre);
            return expr;
        },
        .struct_literal => {
            for (expr.struct_literal.fields) |*field| field.value = try walkExpr(exp, field.value, pre);
            return expr;
        },
        else => return expr,
    }
}

// --- Macro call expansion ---------------------------------------------------

fn macroCall(expr: *ast.Expr) ?ast.CallExpr {
    if (expr.* == .call and expr.call.is_macro) return expr.call;
    return null;
}

fn lookupMacro(exp: *Expander, call: ast.CallExpr) ?ast.MacroDecl {
    if (call.callee.* != .identifier) return null;
    const segments = call.callee.identifier.name.segments;
    if (segments.len != 1) return null;
    return exp.macros.get(segments[0].text);
}

/// Build the per-call substitution environment: hoist `expr` fragments into `pre` as single-eval
/// `let`s, register `place` fragments for clone-on-use, and gensym all template-introduced names.
/// Returns false (after emitting a diagnostic) if the call is malformed.
fn buildEnv(exp: *Expander, macro: ast.MacroDecl, call: ast.CallExpr, env: *Env, pre: *StatementList) !bool {
    if (call.args.len != macro.params.len) {
        const msg = try std.fmt.allocPrint(exp.allocator, "macro '{s}' expects {d} fragment(s) but got {d}", .{ macro.name, macro.params.len, call.args.len });
        try exp.err("KMAC002", "wrong macro fragment count", msg, call.span, "fragment count does not match the macro declaration", "Pass exactly one argument per declared fragment parameter.");
        return false;
    }
    for (macro.params, 0..) |param, index| {
        const arg_value = try walkExpr(exp, call.args[index].value, pre);
        switch (param.kind) {
            .expr => {
                const temp = try exp.gensym(param.name);
                try pre.append(exp.makeLet(temp, arg_value, call.span));
                try env.params.put(exp.allocator, param.name, .{ .expr_temp = temp });
            },
            .place => {
                if (!isLvalue(arg_value)) {
                    const msg = try std.fmt.allocPrint(exp.allocator, "fragment '{s}' is a `place` and requires an assignable argument", .{param.name});
                    try exp.err("KMAC004", "macro place argument is not assignable", msg, call.span, "this argument is not an assignable lvalue", "Pass a variable, field, or index target for a `place` fragment.");
                    return false;
                }
                try env.params.put(exp.allocator, param.name, .{ .place_expr = arg_value });
            },
        }
    }
    if (macro.expand_block) |block| try inst.collectIntroduced(exp, macro, block.statements, env);
    return true;
}

fn expandCallAsStatements(exp: *Expander, call: ast.CallExpr, out: *StatementList) !void {
    if (exp.depth >= max_expansion_depth) {
        try exp.err("KMAC010", "macro expansion too deep", "Macro expansion exceeded the recursion limit; this usually means a macro expands to itself.", call.span, "expansion limit reached here", "Remove the recursive macro invocation.");
        return;
    }
    const macro = lookupMacro(exp, call) orelse {
        // A procedural `kind { function }` macro can also be invoked in statement position; its
        // expansion re-parses as a statement list spliced in place.
        if (proc.lookupFuncMacro(exp, call)) |func_macro| {
            exp.depth += 1;
            defer exp.depth -= 1;
            const stmts = try proc.expandFuncMacroStatements(exp, func_macro, call);
            for (stmts) |statement| try walkStatement(exp, statement, out);
            return;
        }
        try emitUnknownMacro(exp, call);
        return;
    };
    exp.depth += 1;
    defer exp.depth -= 1;

    var env = Env{};
    defer env.deinit(exp.allocator);
    if (!try buildEnv(exp, macro, call, &env, out)) return;

    const block = macro.expand_block orelse return;
    for (block.statements) |statement| try inst.instantiateStatement(exp, &env, statement, out);
}

fn expandCallAsExpr(exp: *Expander, call: ast.CallExpr, pre: *StatementList) !*ast.Expr {
    if (exp.depth >= max_expansion_depth) {
        try exp.err("KMAC010", "macro expansion too deep", "Macro expansion exceeded the recursion limit; this usually means a macro expands to itself.", call.span, "expansion limit reached here", "Remove the recursive macro invocation.");
        return exp.makeIntZero(call.span);
    }
    const macro = lookupMacro(exp, call) orelse {
        // A procedural `kind { function }` macro can also be invoked in expression position; its
        // expansion must re-parse as a single expression, which becomes the value here.
        if (proc.lookupFuncMacro(exp, call)) |func_macro| {
            exp.depth += 1;
            defer exp.depth -= 1;
            const expr = (try proc.expandFuncMacroExpr(exp, func_macro, call)) orelse return exp.makeIntZero(call.span);
            return walkExpr(exp, expr, pre);
        }
        try emitUnknownMacro(exp, call);
        return exp.makeIntZero(call.span);
    };
    exp.depth += 1;
    defer exp.depth -= 1;

    var env = Env{};
    defer env.deinit(exp.allocator);
    if (!try buildEnv(exp, macro, call, &env, pre)) return exp.makeIntZero(call.span);

    const block = macro.expand_block orelse return exp.makeIntZero(call.span);
    if (block.statements.len == 0 or block.statements[block.statements.len - 1] != .expr_stmt) {
        const msg = try std.fmt.allocPrint(exp.allocator, "macro '{s}' is used as a value but its expand block does not end in an expression", .{macro.name});
        try exp.err("KMAC005", "statement-only macro used as a value", msg, call.span, "this macro produces no value here", "End the macro's `expand` block with an expression, or use the macro in statement position.");
        return exp.makeIntZero(call.span);
    }
    // Hoist the template's leading statements; its trailing expression is the value.
    const leading = block.statements[0 .. block.statements.len - 1];
    for (leading) |statement| try inst.instantiateStatement(exp, &env, statement, pre);
    const tail = block.statements[block.statements.len - 1].expr_stmt.expr;
    return inst.instantiateExpr(exp, &env, tail);
}

fn emitUnknownMacro(exp: *Expander, call: ast.CallExpr) !void {
    const name = if (call.callee.* == .identifier and call.callee.identifier.name.segments.len == 1)
        call.callee.identifier.name.segments[0].text
    else
        "<expression>";
    const msg = try std.fmt.allocPrint(exp.allocator, "no declarative macro named '{s}' is in scope", .{name});
    try exp.err("KMAC001", "unknown macro", msg, call.span, "this macro is not declared", "Declare it with `macro {name}(...) { expand { ... } }` or remove the `!` to call a function.");
}

fn isLvalue(expr: *ast.Expr) bool {
    return switch (expr.*) {
        .identifier, .member, .index => true,
        else => false,
    };
}
