//! Compile-time evaluator for procedural macros (`comptime macro`).
//!
//! Procedural macros run arbitrary Kira at compile time. This is a focused tree-walking
//! interpreter over the `expand` body that operates on compile-time `Value`s — including a `Syntax`
//! value modeled as Kira *source text*. `quote { ... }` renders to source (filling `#{}` splices by
//! value type); the macro-expansion pass re-parses the returned `Syntax` with `parser.parseSource`
//! and splices the resulting declarations. The evaluator covers the reflection surface
//! attribute/derive macros need: `Declaration.{name,fields,syntax}`, `Field.{name,type}`,
//! `Identifier.asString`, `TypeRef.asSyntax`, `Syntax.join`, array `.append`, and `Diagnostics.error`.

const std = @import("std");
const syntax = @import("kira_syntax_model");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");

const ast = syntax.ast;
const Span = source_pkg.Span;

pub const EvalError = error{ MacroEvalError, OutOfMemory };

pub const Field = struct {
    name: []const u8,
    type_text: []const u8,
};

pub const Declaration = struct {
    name: []const u8,
    fields: []Field,
    syntax: []const u8,
    span: Span,
};

pub const Value = union(enum) {
    int: i64,
    boolean: bool,
    string: []const u8,
    syntax: []const u8,
    identifier: []const u8,
    array: *std.array_list.Managed(Value),
    declaration: Declaration,
    field: Field,
    type_ref: []const u8,
    void_value,
};

pub const Evaluator = struct {
    allocator: std.mem.Allocator,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
    env: std.StringHashMapUnmanaged(Value) = .{},

    fn fail(self: *Evaluator, message: []const u8, span: Span) EvalError {
        diagnostics.appendOwned(self.allocator, self.diags, .{
            .severity = .@"error",
            .code = "KMAC020",
            .title = "comptime macro evaluation error",
            .message = message,
            .labels = &.{diagnostics.primaryLabel(span, "while expanding this macro")},
            .help = "This construct is not supported by the compile-time macro evaluator yet.",
        }) catch {};
        return error.MacroEvalError;
    }

    /// Run an attribute/derive macro's `expand(target: Declaration) -> Syntax` and return the
    /// rendered source text of the returned `Syntax`, or null if the macro emitted a diagnostic.
    pub fn runOnDeclaration(self: *Evaluator, macro: ast.MacroDecl, target: Declaration) EvalError!?[]const u8 {
        const expand_fn = macro.expand_fn orelse return self.fail("comptime macro has no expand function", macro.span);
        const body = expand_fn.body orelse return self.fail("comptime macro expand has no body", macro.span);
        if (expand_fn.params.len >= 1) {
            try self.env.put(self.allocator, expand_fn.params[0].name, .{ .declaration = target });
        }
        const result = try self.evalBlock(body.statements);
        const value = result orelse return self.fail("comptime macro expand did not return a Syntax value", macro.span);
        return try self.renderSplice(value);
    }

    /// Run a function-position macro's `expand(input: Syntax) -> Syntax` with `input` bound to the
    /// rendered call arguments, and return the generated source text.
    pub fn runOnInput(self: *Evaluator, macro: ast.MacroDecl, input_text: []const u8) EvalError!?[]const u8 {
        const expand_fn = macro.expand_fn orelse return self.fail("comptime macro has no expand function", macro.span);
        const body = expand_fn.body orelse return self.fail("comptime macro expand has no body", macro.span);
        if (expand_fn.params.len >= 1) {
            try self.env.put(self.allocator, expand_fn.params[0].name, .{ .syntax = input_text });
        }
        const result = try self.evalBlock(body.statements);
        const value = result orelse return self.fail("comptime macro expand did not return a Syntax value", macro.span);
        return try self.renderSplice(value);
    }

    fn evalBlock(self: *Evaluator, statements: []const ast.Statement) EvalError!?Value {
        for (statements) |statement| {
            if (try self.evalStatement(statement)) |returned| return returned;
        }
        return null;
    }

    fn evalStatement(self: *Evaluator, statement: ast.Statement) EvalError!?Value {
        switch (statement) {
            .let_stmt => |s| {
                const value = if (s.value) |v| try self.evalExpr(v) else Value.void_value;
                try self.env.put(self.allocator, s.name, value);
                return null;
            },
            .assign_stmt => |s| {
                if (s.target.* != .identifier or s.target.identifier.name.segments.len != 1) {
                    return self.fail("comptime macro can only assign to a simple variable", s.span);
                }
                const value = try self.evalExpr(s.value);
                try self.env.put(self.allocator, s.target.identifier.name.segments[0].text, value);
                return null;
            },
            .expr_stmt => |s| {
                _ = try self.evalExpr(s.expr);
                return null;
            },
            .return_stmt => |s| {
                if (s.value) |v| return try self.evalExpr(v);
                return Value.void_value;
            },
            .if_stmt => |s| {
                const cond = try self.evalExpr(s.condition);
                if (cond != .boolean) return self.fail("comptime macro if-condition must be a Bool", s.span);
                if (cond.boolean) {
                    return try self.evalBlock(s.then_block.statements);
                } else if (s.else_block) |eb| {
                    return try self.evalBlock(eb.statements);
                }
                return null;
            },
            .for_stmt => |s| {
                const iterable = try self.evalExpr(s.iterator);
                if (iterable != .array) return self.fail("comptime macro for-loop must iterate an array", s.span);
                const saved = self.env.get(s.binding_name);
                for (iterable.array.items) |element| {
                    try self.env.put(self.allocator, s.binding_name, element);
                    if (try self.evalBlock(s.body.statements)) |returned| return returned;
                }
                if (saved) |prev| try self.env.put(self.allocator, s.binding_name, prev);
                return null;
            },
            .while_stmt => |s| {
                var guard: usize = 0;
                while (true) {
                    const cond = try self.evalExpr(s.condition);
                    if (cond != .boolean) return self.fail("comptime macro while-condition must be a Bool", s.span);
                    if (!cond.boolean) break;
                    if (try self.evalBlock(s.body.statements)) |returned| return returned;
                    guard += 1;
                    if (guard > 1_000_000) return self.fail("comptime macro while-loop did not terminate", s.span);
                }
                return null;
            },
            else => return self.fail("statement form not supported in a comptime macro body", stmtSpan(statement)),
        }
    }

    fn evalExpr(self: *Evaluator, expr: *ast.Expr) EvalError!Value {
        switch (expr.*) {
            .integer => |e| return .{ .int = e.value },
            .bool => |e| return .{ .boolean = e.value },
            .string => |e| return .{ .string = e.value },
            .identifier => |e| {
                if (e.name.segments.len == 1) {
                    if (self.env.get(e.name.segments[0].text)) |value| return value;
                }
                return self.fail("unknown identifier in comptime macro", e.span);
            },
            .array => |e| {
                const list = try self.allocator.create(std.array_list.Managed(Value));
                list.* = std.array_list.Managed(Value).init(self.allocator);
                for (e.elements) |element| try list.append(try self.evalExpr(element));
                return .{ .array = list };
            },
            .binary => |e| return self.evalBinary(e),
            .unary => |e| {
                const operand = try self.evalExpr(e.operand);
                if (e.op == .not and operand == .boolean) return .{ .boolean = !operand.boolean };
                if (e.op == .negate and operand == .int) return .{ .int = -operand.int };
                return self.fail("unsupported unary operator in comptime macro", e.span);
            },
            .member => |e| {
                const object = try self.evalExpr(e.object);
                return self.memberAccess(object, e.member, e.span);
            },
            .call => |e| return self.evalCall(e),
            .quote => |e| return self.renderQuote(e),
            else => return self.fail("expression form not supported in a comptime macro body", exprSpan(expr.*)),
        }
    }

    fn evalBinary(self: *Evaluator, e: ast.BinaryExpr) EvalError!Value {
        const lhs = try self.evalExpr(e.lhs);
        const rhs = try self.evalExpr(e.rhs);
        switch (e.op) {
            .add => {
                if (lhs == .int and rhs == .int) return .{ .int = lhs.int + rhs.int };
                // Otherwise string concatenation of the plain forms.
                const left = try self.plainString(lhs, e.span);
                const right = try self.plainString(rhs, e.span);
                return .{ .string = try std.mem.concat(self.allocator, u8, &.{ left, right }) };
            },
            .subtract => return self.intOp(lhs, rhs, e.span, sub),
            .multiply => return self.intOp(lhs, rhs, e.span, mul),
            .divide => return self.intOp(lhs, rhs, e.span, divFn),
            .modulo => return self.intOp(lhs, rhs, e.span, modFn),
            .equal => return .{ .boolean = try self.valuesEqual(lhs, rhs, e.span) },
            .not_equal => return .{ .boolean = !(try self.valuesEqual(lhs, rhs, e.span)) },
            .less => return self.intCmp(lhs, rhs, e.span, ltFn),
            .less_equal => return self.intCmp(lhs, rhs, e.span, leFn),
            .greater => return self.intCmp(lhs, rhs, e.span, gtFn),
            .greater_equal => return self.intCmp(lhs, rhs, e.span, geFn),
            .logical_and => return .{ .boolean = (lhs == .boolean and lhs.boolean) and (rhs == .boolean and rhs.boolean) },
            .logical_or => return .{ .boolean = (lhs == .boolean and lhs.boolean) or (rhs == .boolean and rhs.boolean) },
        }
    }

    fn intOp(self: *Evaluator, lhs: Value, rhs: Value, span: Span, comptime f: fn (i64, i64) i64) EvalError!Value {
        if (lhs != .int or rhs != .int) return self.fail("comptime macro arithmetic requires Int operands", span);
        return .{ .int = f(lhs.int, rhs.int) };
    }

    fn intCmp(self: *Evaluator, lhs: Value, rhs: Value, span: Span, comptime f: fn (i64, i64) bool) EvalError!Value {
        if (lhs != .int or rhs != .int) return self.fail("comptime macro comparison requires Int operands", span);
        return .{ .boolean = f(lhs.int, rhs.int) };
    }

    fn valuesEqual(self: *Evaluator, lhs: Value, rhs: Value, span: Span) EvalError!bool {
        if (lhs == .int and rhs == .int) return lhs.int == rhs.int;
        if (lhs == .boolean and rhs == .boolean) return lhs.boolean == rhs.boolean;
        const left = try self.plainString(lhs, span);
        const right = try self.plainString(rhs, span);
        return std.mem.eql(u8, left, right);
    }

    fn evalCall(self: *Evaluator, e: ast.CallExpr) EvalError!Value {
        if (e.callee.* != .member) return self.fail("comptime macro call must be a method call", e.span);
        const method = e.callee.member.member;
        const object_expr = e.callee.member.object;

        // Static type methods: `Syntax.join(...)`, `Diagnostics.error(...)`.
        if (object_expr.* == .identifier and object_expr.identifier.name.segments.len == 1) {
            const type_name = object_expr.identifier.name.segments[0].text;
            if (self.env.get(type_name) == null) {
                if (std.mem.eql(u8, type_name, "Syntax") and std.mem.eql(u8, method, "join")) {
                    return self.syntaxJoin(e);
                }
                if (std.mem.eql(u8, type_name, "Diagnostics") and std.mem.eql(u8, method, "error")) {
                    return self.diagnosticsError(e);
                }
            }
        }

        const receiver = try self.evalExpr(object_expr);
        return self.callMethod(receiver, method, e);
    }

    fn callMethod(self: *Evaluator, receiver: Value, method: []const u8, e: ast.CallExpr) EvalError!Value {
        switch (receiver) {
            .array => |list| {
                if (std.mem.eql(u8, method, "append")) {
                    if (e.args.len != 1) return self.fail("append expects one argument", e.span);
                    try list.append(try self.evalExpr(e.args[0].value));
                    return Value.void_value;
                }
                if (std.mem.eql(u8, method, "len")) return .{ .int = @intCast(list.items.len) };
            },
            .identifier => |name| {
                if (std.mem.eql(u8, method, "asString")) return .{ .string = name };
            },
            .syntax => |text| {
                if (std.mem.eql(u8, method, "identifiers")) return self.syntaxIdentifiers(text);
            },
            .type_ref => |text| {
                if (std.mem.eql(u8, method, "asSyntax")) return .{ .syntax = text };
            },
            else => {},
        }
        return self.fail("unknown comptime macro method", e.span);
    }

    fn memberAccess(self: *Evaluator, object: Value, member: []const u8, span: Span) EvalError!Value {
        switch (object) {
            .declaration => |decl| {
                if (std.mem.eql(u8, member, "name")) return .{ .identifier = decl.name };
                if (std.mem.eql(u8, member, "syntax")) return .{ .syntax = decl.syntax };
                if (std.mem.eql(u8, member, "fields")) {
                    const list = try self.allocator.create(std.array_list.Managed(Value));
                    list.* = std.array_list.Managed(Value).init(self.allocator);
                    for (decl.fields) |field| try list.append(.{ .field = field });
                    return .{ .array = list };
                }
            },
            .field => |field| {
                if (std.mem.eql(u8, member, "name")) return .{ .identifier = field.name };
                if (std.mem.eql(u8, member, "type")) return .{ .type_ref = field.type_text };
            },
            else => {},
        }
        return self.fail("unknown member in comptime macro", span);
    }

    fn syntaxJoin(self: *Evaluator, e: ast.CallExpr) EvalError!Value {
        if (e.args.len < 1) return self.fail("Syntax.join expects items and a separator", e.span);
        const items = try self.evalExpr(e.args[0].value);
        if (items != .array) return self.fail("Syntax.join expects an array", e.span);
        var separator: []const u8 = ", ";
        if (e.args.len >= 2) {
            const sep = try self.evalExpr(e.args[1].value);
            separator = try self.plainString(sep, e.span);
        }
        var out = std.array_list.Managed(u8).init(self.allocator);
        for (items.array.items, 0..) |item, i| {
            if (i != 0) try out.appendSlice(separator);
            try out.appendSlice(try self.renderSplice(item));
        }
        return .{ .syntax = try out.toOwnedSlice() };
    }

    fn diagnosticsError(self: *Evaluator, e: ast.CallExpr) EvalError!Value {
        if (e.args.len < 1) return self.fail("Diagnostics.error expects a message", e.span);
        const message = try self.evalExpr(e.args[0].value);
        const text = try self.plainString(message, e.span);
        diagnostics.appendOwned(self.allocator, self.diags, .{
            .severity = .@"error",
            .code = "KMAC021",
            .title = "macro diagnostic",
            .message = try self.allocator.dupe(u8, text),
            .labels = &.{diagnostics.primaryLabel(e.span, "reported by a comptime macro")},
            .help = "This error was raised by a procedural macro via Diagnostics.error.",
        }) catch {};
        return error.MacroEvalError;
    }

    fn syntaxIdentifiers(self: *Evaluator, text: []const u8) EvalError!Value {
        const list = try self.allocator.create(std.array_list.Managed(Value));
        list.* = std.array_list.Managed(Value).init(self.allocator);
        var i: usize = 0;
        while (i < text.len) {
            const c = text[i];
            if (std.ascii.isAlphabetic(c) or c == '_') {
                const start = i;
                while (i < text.len and (std.ascii.isAlphanumeric(text[i]) or text[i] == '_')) : (i += 1) {}
                try list.append(.{ .identifier = text[start..i] });
            } else {
                i += 1;
            }
        }
        return .{ .array = list };
    }

    fn renderQuote(self: *Evaluator, quote: ast.QuoteExpr) EvalError!Value {
        var out = std.array_list.Managed(u8).init(self.allocator);
        for (quote.parts) |part| {
            switch (part) {
                .text => |t| try out.appendSlice(t),
                .splice => |expr| {
                    const value = try self.evalExpr(expr);
                    try out.appendSlice(try self.renderSplice(value));
                },
            }
        }
        return .{ .syntax = try out.toOwnedSlice() };
    }

    /// Type-directed splice rendering: Syntax as-is, Identifier bare, String quoted, Int/Bool as
    /// literals, arrays concatenated (newline between elements, valid for statement/decl lists).
    fn renderSplice(self: *Evaluator, value: Value) EvalError![]const u8 {
        return switch (value) {
            .syntax => |t| t,
            .identifier => |name| name,
            .type_ref => |t| t,
            .string => |s| try std.mem.concat(self.allocator, u8, &.{ "\"", s, "\"" }),
            .int => |n| try std.fmt.allocPrint(self.allocator, "{d}", .{n}),
            .boolean => |b| if (b) "true" else "false",
            .array => |list| blk: {
                var out = std.array_list.Managed(u8).init(self.allocator);
                for (list.items, 0..) |item, i| {
                    if (i != 0) try out.append('\n');
                    try out.appendSlice(try self.renderSplice(item));
                }
                break :blk try out.toOwnedSlice();
            },
            .void_value => "",
            else => return self.fail("cannot splice this value type", Span.init(0, 0)),
        };
    }

    /// The plain (unquoted) string form, used for string concatenation and comparison.
    fn plainString(self: *Evaluator, value: Value, span: Span) EvalError![]const u8 {
        return switch (value) {
            .string => |s| s,
            .identifier => |name| name,
            .syntax => |t| t,
            .type_ref => |t| t,
            .int => |n| try std.fmt.allocPrint(self.allocator, "{d}", .{n}),
            .boolean => |b| if (b) "true" else "false",
            else => self.fail("value is not stringable in a comptime macro", span),
        };
    }
};

fn sub(a: i64, b: i64) i64 {
    return a - b;
}
fn mul(a: i64, b: i64) i64 {
    return a * b;
}
fn divFn(a: i64, b: i64) i64 {
    return @divTrunc(a, b);
}
fn modFn(a: i64, b: i64) i64 {
    return @rem(a, b);
}
fn ltFn(a: i64, b: i64) bool {
    return a < b;
}
fn leFn(a: i64, b: i64) bool {
    return a <= b;
}
fn gtFn(a: i64, b: i64) bool {
    return a > b;
}
fn geFn(a: i64, b: i64) bool {
    return a >= b;
}

fn stmtSpan(statement: ast.Statement) Span {
    return switch (statement) {
        inline else => |s| s.span,
    };
}

fn exprSpan(expr: ast.Expr) Span {
    return switch (expr) {
        inline else => |e| e.span,
    };
}

/// Render an expression back to source text (for a function macro's `input` arguments).
pub fn exprToText(allocator: std.mem.Allocator, expr: ast.Expr) error{OutOfMemory}![]const u8 {
    switch (expr) {
        .identifier => |e| return qualifiedNameText(allocator, e.name),
        .integer => |e| return std.fmt.allocPrint(allocator, "{d}", .{e.value}),
        .float => |e| return std.fmt.allocPrint(allocator, "{d}", .{e.value}),
        .bool => |e| return allocator.dupe(u8, if (e.value) "true" else "false"),
        .string => |e| return std.mem.concat(allocator, u8, &.{ "\"", e.value, "\"" }),
        .member => |e| return std.mem.concat(allocator, u8, &.{ try exprToText(allocator, e.object.*), ".", e.member }),
        .binary => |e| return std.mem.concat(allocator, u8, &.{ try exprToText(allocator, e.lhs.*), " ", binaryOpText(e.op), " ", try exprToText(allocator, e.rhs.*) }),
        else => return allocator.dupe(u8, ""),
    }
}

fn binaryOpText(op: ast.BinaryOp) []const u8 {
    return switch (op) {
        .add => "+",
        .subtract => "-",
        .multiply => "*",
        .divide => "/",
        .modulo => "%",
        .equal => "==",
        .not_equal => "!=",
        .less => "<",
        .less_equal => "<=",
        .greater => ">",
        .greater_equal => ">=",
        .logical_and => "&&",
        .logical_or => "||",
    };
}

/// Render a `TypeExpr` back to source text (for `Field.type`).
pub fn typeToText(allocator: std.mem.Allocator, ty: ast.TypeExpr) error{OutOfMemory}![]const u8 {
    switch (ty) {
        .named => |name| return qualifiedNameText(allocator, name),
        .array => |arr| return std.mem.concat(allocator, u8, &.{ "[", try typeToText(allocator, arr.element_type.*), "]" }),
        .generic => |gen| {
            var out = std.array_list.Managed(u8).init(allocator);
            try out.appendSlice(try qualifiedNameText(allocator, gen.base));
            try out.append('<');
            for (gen.args, 0..) |arg, i| {
                if (i != 0) try out.appendSlice(", ");
                try out.appendSlice(try typeToText(allocator, arg.*));
            }
            try out.append('>');
            return out.toOwnedSlice();
        },
        .ownership => |own| return typeToText(allocator, own.target.*),
        .any => return allocator.dupe(u8, "Any"),
        .function => return allocator.dupe(u8, "Function"),
    }
}

fn qualifiedNameText(allocator: std.mem.Allocator, name: ast.QualifiedName) error{OutOfMemory}![]const u8 {
    var out = std.array_list.Managed(u8).init(allocator);
    for (name.segments, 0..) |segment, i| {
        if (i != 0) try out.append('.');
        try out.appendSlice(segment.text);
    }
    return out.toOwnedSlice();
}
