const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const parent = @import("parser.zig");
const Parser = parent.Parser;
const exprSpan = parent.exprSpan;
const typeSpan = parent.typeSpan;
const cloneQualifiedName = Parser.cloneQualifiedName;
const tokenDescription = parent.tokenDescription;
const unexpectedTokenLabel = parent.unexpectedTokenLabel;
const expectedTokenHelp = parent.expectedTokenHelp;

fn isOwnershipUnaryOperandStart(kind: syntax.TokenKind) bool {
    return switch (kind) {
        .identifier,
        .integer,
        .float,
        .string,
        .kw_true,
        .kw_false,
        .minus,
        .bang,
        => true,
        else => false,
    };
}

fn looksLikeOwnershipUnary(self: *Parser) bool {
    if (!self.at(.identifier)) return false;
    const keyword = self.peek().lexeme;
    if (!(std.mem.eql(u8, keyword, "move") or std.mem.eql(u8, keyword, "copy"))) return false;
    return isOwnershipUnaryOperandStart(self.peekNext().kind);
}

pub fn parseTypeExpr(self: *Parser) anyerror!*syntax.ast.TypeExpr {
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "borrow")) {
        const start = self.advance().span.start;
        const mode: syntax.ast.OwnershipMode = if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "mut")) blk: {
            _ = self.advance();
            break :blk .borrow_mut;
        } else .borrow_read;
        const target = try self.parseTypeExpr();
        const node = try self.allocator.create(syntax.ast.TypeExpr);
        node.* = .{ .ownership = .{
            .mode = mode,
            .target = target,
            .span = source_pkg.Span.init(start, typeSpan(target.*).end),
        } };
        return node;
    }

    if (self.at(.identifier) and (std.mem.eql(u8, self.peek().lexeme, "move") or std.mem.eql(u8, self.peek().lexeme, "copy"))) {
        const token = self.advance();
        const target = try self.parseTypeExpr();
        const node = try self.allocator.create(syntax.ast.TypeExpr);
        node.* = .{ .ownership = .{
            .mode = if (std.mem.eql(u8, token.lexeme, "move")) .move else .copy,
            .target = target,
            .span = source_pkg.Span.init(token.span.start, typeSpan(target.*).end),
        } };
        return node;
    }

    if (self.at(.identifier) and (std.mem.eql(u8, self.peek().lexeme, "any") or std.mem.eql(u8, self.peek().lexeme, "some"))) {
        const existential = std.mem.eql(u8, self.peek().lexeme, "some");
        const start = self.advance().span.start;
        const target = try self.parseTypeExpr();
        const node = try self.allocator.create(syntax.ast.TypeExpr);
        node.* = .{ .any = .{
            .target = target,
            .span = source_pkg.Span.init(start, typeSpan(target.*).end),
            .existential = existential,
        } };
        return node;
    }

    if (self.match(.l_bracket)) {
        const start = self.previous().span.start;
        const element_type = try self.parseTypeExpr();
        const close = try self.expect(.r_bracket, "expected ']' after array type", "close the array type here");
        const node = try self.allocator.create(syntax.ast.TypeExpr);
        node.* = .{ .array = .{
            .element_type = element_type,
            .span = source_pkg.Span.init(start, close.span.end),
        } };
        return node;
    }

    if (self.match(.l_paren)) {
        const start = self.previous().span.start;
        var params = std.array_list.Managed(*syntax.ast.TypeExpr).init(self.allocator);
        while (!self.at(.r_paren) and !self.at(.eof)) {
            try params.append(try self.parseTypeExpr());
            if (!self.match(.comma)) break;
        }
        _ = try self.expect(.r_paren, "expected ')' after function type parameters", "close the function parameter type list here");
        _ = try self.expect(.arrow, "expected '->' in function type", "write `->` before the function result type");
        const result = try self.parseTypeExpr();
        const node = try self.allocator.create(syntax.ast.TypeExpr);
        node.* = .{ .function = .{
            .params = try params.toOwnedSlice(),
            .result = result,
            .span = source_pkg.Span.init(start, typeSpan(result.*).end),
        } };
        return node;
    }

    const name = try self.parseQualifiedName("expected type name");
    const node = try self.allocator.create(syntax.ast.TypeExpr);
    if (self.match(.less)) {
        var args = std.array_list.Managed(*syntax.ast.TypeExpr).init(self.allocator);
        while (!self.at(.greater) and !self.at(.eof)) {
            try args.append(try self.parseTypeExpr());
            if (!self.match(.comma)) break;
        }
        const close = try self.expect(.greater, "expected '>' after generic type arguments", "close the generic type argument list here");
        node.* = .{ .generic = .{
            .base = name,
            .args = try args.toOwnedSlice(),
            .span = source_pkg.Span.init(name.span.start, close.span.end),
        } };
    } else {
        node.* = .{ .named = name };
    }
    return node;
}

pub fn parseExpression(self: *Parser) anyerror!*syntax.ast.Expr {
    // Depth guard: every level of expression nesting (parens, call args, array /
    // struct literals, closures, match scrutinees) recurses through here, so one
    // bound here prevents the recursive-descent parser from overflowing the native
    // stack on pathologically deep input.
    self.expr_depth += 1;
    defer self.expr_depth -= 1;
    if (self.expr_depth > Parser.max_expr_depth) {
        const token = self.peek();
        const detail = try std.fmt.allocPrint(
            self.allocator,
            "Kira stopped parsing after {d} levels of nested expression to avoid a stack overflow.",
            .{Parser.max_expr_depth},
        );
        try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
            .severity = .@"error",
            .code = "KPAR014",
            .title = "expression nesting too deep",
            .message = detail,
            .labels = &.{
                diagnostics.primaryLabel(token.span, "expression nested too deeply here"),
            },
            .help = "Split this into smaller subexpressions or named `let` bindings.",
        });
        return error.DiagnosticsEmitted;
    }
    return self.parseConditional();
}

pub fn parseExpressionWithoutTrailingBlockCall(self: *Parser) anyerror!*syntax.ast.Expr {
    const previous_setting = self.allow_trailing_block_call;
    self.allow_trailing_block_call = false;
    defer self.allow_trailing_block_call = previous_setting;
    return self.parseExpression();
}

pub fn makeIdentifierExpr(self: *Parser, token: syntax.Token) !*syntax.ast.Expr {
    const name = try self.makeSingleSegmentName(token);
    const expr = try self.allocator.create(syntax.ast.Expr);
    expr.* = .{ .identifier = .{
        .name = name,
        .span = token.span,
    } };
    return expr;
}

fn parseNativeStateBuiltin(self: *Parser, token: syntax.Token) anyerror!*syntax.ast.Expr {
    _ = try self.expect(.l_paren, "expected '(' after nativeState", "open the native state expression here");
    const value = try self.parseExpression();
    const close = try self.expect(.r_paren, "expected ')' after nativeState value", "close the native state expression here");
    const expr = try self.allocator.create(syntax.ast.Expr);
    expr.* = .{ .native_state = .{
        .value = value,
        .span = source_pkg.Span.init(token.span.start, close.span.end),
    } };
    return expr;
}

fn parseNativeUserDataBuiltin(self: *Parser, token: syntax.Token) anyerror!*syntax.ast.Expr {
    _ = try self.expect(.l_paren, "expected '(' after nativeUserData", "open the native userdata expression here");
    const state = try self.parseExpression();
    const close = try self.expect(.r_paren, "expected ')' after nativeUserData value", "close the native userdata expression here");
    const expr = try self.allocator.create(syntax.ast.Expr);
    expr.* = .{ .native_user_data = .{
        .state = state,
        .span = source_pkg.Span.init(token.span.start, close.span.end),
    } };
    return expr;
}

fn parseNativeRecoverBuiltin(self: *Parser, token: syntax.Token) anyerror!*syntax.ast.Expr {
    _ = try self.expect(.less, "expected '<' after nativeRecover", "write the recovered type here");
    const state_type = try self.parseTypeExpr();
    _ = try self.expect(.greater, "expected '>' after nativeRecover type", "close the recovered type here");
    _ = try self.expect(.l_paren, "expected '(' after nativeRecover type", "open the native recover expression here");
    const value = try self.parseExpression();
    const close = try self.expect(.r_paren, "expected ')' after nativeRecover value", "close the native recover expression here");
    const expr = try self.allocator.create(syntax.ast.Expr);
    expr.* = .{ .native_recover = .{
        .state_type = state_type,
        .value = value,
        .span = source_pkg.Span.init(token.span.start, close.span.end),
    } };
    return expr;
}

pub fn looksLikeStructLiteral(self: *Parser) bool {
    if (!self.at(.l_brace)) return false;
    const next = self.peekNext().kind;
    if (next == .r_brace) return true;
    if (next != .identifier) return false;
    return self.peekAhead(2).kind == .colon;
}

pub fn parseStructLiteral(self: *Parser, type_expr: *syntax.ast.Expr) anyerror!*syntax.ast.Expr {
    const type_name = try self.qualifiedNameFromExpr(type_expr);
    _ = try self.expect(.l_brace, "expected '{' to start struct literal", "open the struct literal here");
    var fields = std.array_list.Managed(syntax.ast.StructLiteralField).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        const field_name = try self.expect(.identifier, "expected struct field name", "write the field name here");
        _ = try self.expect(.colon, "expected ':' after field name", "use ':' between the field name and value");
        const value = try self.parseExpression();
        try fields.append(.{
            .name = field_name.lexeme,
            .value = value,
            .span = source_pkg.Span.init(field_name.span.start, exprSpan(value.*).end),
        });
        _ = self.match(.comma);
        _ = self.match(.semicolon);
    }
    const close = try self.expect(.r_brace, "expected '}' after struct literal", "close the struct literal here");
    const node = try self.allocator.create(syntax.ast.Expr);
    node.* = .{ .struct_literal = .{
        .type_name = type_name,
        .fields = try fields.toOwnedSlice(),
        .span = source_pkg.Span.init(exprSpan(type_expr.*).start, close.span.end),
    } };
    return node;
}

pub fn qualifiedNameFromExpr(self: *Parser, expr: *syntax.ast.Expr) anyerror!syntax.ast.QualifiedName {
    return switch (expr.*) {
        .identifier => |node| cloneQualifiedName(self.allocator, node.name),
        .member => |node| blk: {
            const object_name = try self.qualifiedNameFromExpr(node.object);
            const segments = try self.allocator.alloc(syntax.ast.NameSegment, object_name.segments.len + 1);
            @memcpy(segments[0..object_name.segments.len], object_name.segments);
            segments[object_name.segments.len] = .{
                .text = node.member,
                .span = .{ .start = node.span.end - node.member.len, .end = node.span.end },
            };
            break :blk .{
                .segments = segments,
                .span = source_pkg.Span.init(object_name.span.start, node.span.end),
            };
        },
        else => {
            try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
                .severity = .@"error",
                .code = "KPAR013",
                .title = "struct literal requires a type name",
                .message = "Kira expected a named type before this struct literal.",
                .labels = &.{
                    diagnostics.primaryLabel(exprSpan(expr.*), "this expression is not a type name"),
                },
                .help = "Write a type name such as `Rect { width: 10.0 }`.",
            });
            return error.DiagnosticsEmitted;
        },
    };
}

pub fn parseConditional(self: *Parser) anyerror!*syntax.ast.Expr {
    const condition = try self.parseLogicalOr();
    if (!self.match(.question)) return condition;

    const then_expr = try self.parseExpression();
    _ = try self.expect(.colon, "expected ':' in conditional expression", "separate the true and false branches with ':'");
    const else_expr = try self.parseExpression();
    const node = try self.allocator.create(syntax.ast.Expr);
    node.* = .{ .conditional = .{
        .condition = condition,
        .then_expr = then_expr,
        .else_expr = else_expr,
        .span = source_pkg.Span.init(exprSpan(condition.*).start, exprSpan(else_expr.*).end),
    } };
    return node;
}

pub fn parseLogicalOr(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parseLogicalAnd();
    while (self.match(.pipe_pipe)) {
        const operator = self.previous();
        const rhs = try self.parseLogicalAnd();
        expr = try self.makeBinaryExpr(operator, expr, rhs);
    }
    return expr;
}

pub fn parseLogicalAnd(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parseEquality();
    while (self.match(.amp_amp)) {
        const operator = self.previous();
        const rhs = try self.parseEquality();
        expr = try self.makeBinaryExpr(operator, expr, rhs);
    }
    return expr;
}

pub fn parseEquality(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parseComparison();
    while (self.match(.equal_equal) or self.match(.bang_equal)) {
        const operator = self.previous();
        const rhs = try self.parseComparison();
        expr = try self.makeBinaryExpr(operator, expr, rhs);
    }
    return expr;
}

pub fn parseComparison(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parseTerm();
    while (self.match(.less) or self.match(.less_equal) or self.match(.greater) or self.match(.greater_equal)) {
        const operator = self.previous();
        const rhs = try self.parseTerm();
        expr = try self.makeBinaryExpr(operator, expr, rhs);
    }
    return expr;
}

pub fn parseTerm(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parseFactor();
    while (self.match(.plus) or self.match(.minus)) {
        const operator = self.previous();
        const rhs = try self.parseFactor();
        expr = try self.makeBinaryExpr(operator, expr, rhs);
    }
    return expr;
}

pub fn parseFactor(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parseUnary();
    while (self.match(.star) or self.match(.slash) or self.match(.percent)) {
        const operator = self.previous();
        const rhs = try self.parseUnary();
        expr = try self.makeBinaryExpr(operator, expr, rhs);
    }
    return expr;
}

pub fn parseUnary(self: *Parser) anyerror!*syntax.ast.Expr {
    if (self.match(.kw_try)) {
        const try_token = self.previous();
        const operand = try self.parseUnary();
        const node = try self.allocator.create(syntax.ast.Expr);
        node.* = .{ .try_expr = .{
            .operand = operand,
            .span = source_pkg.Span.init(try_token.span.start, exprSpan(operand.*).end),
        } };
        return node;
    }

    if (looksLikeOwnershipUnary(self)) {
        const token = self.advance();
        const operand = try self.parseUnary();
        const node = try self.allocator.create(syntax.ast.Expr);
        node.* = .{ .ownership = .{
            .op = if (std.mem.eql(u8, token.lexeme, "move")) .move else .copy,
            .operand = operand,
            .span = source_pkg.Span.init(token.span.start, exprSpan(operand.*).end),
        } };
        return node;
    }

    if (self.match(.minus) or self.match(.bang)) {
        const operator = self.previous();
        const operand = try self.parseUnary();
        const node = try self.allocator.create(syntax.ast.Expr);
        node.* = .{ .unary = .{
            .op = switch (operator.kind) {
                .minus => .negate,
                .bang => .not,
                else => unreachable,
            },
            .operand = operand,
            .span = source_pkg.Span.init(operator.span.start, exprSpan(operand.*).end),
        } };
        return node;
    }
    return self.parsePostfix();
}

pub fn parsePostfix(self: *Parser) anyerror!*syntax.ast.Expr {
    var expr = try self.parsePrimary();

    while (true) {
        // `name!(args)` — a macro call. The trailing `!` immediately before `(` distinguishes it
        // from an ordinary call; `callee` (an identifier) names the macro. The macro-expansion
        // pass replaces this node before semantics.
        if (self.at(.bang) and self.peekNext().kind == .l_paren) {
            _ = self.advance(); // '!'
            _ = self.advance(); // '('
            const outer_block_call = self.allow_trailing_block_call;
            self.allow_trailing_block_call = true;
            var macro_args = std.array_list.Managed(syntax.ast.CallArg).init(self.allocator);
            while (!self.at(.r_paren) and !self.at(.eof)) {
                const start_token = self.peek();
                const value = try self.parseExpression();
                try macro_args.append(.{
                    .label = null,
                    .value = value,
                    .span = source_pkg.Span.init(start_token.span.start, exprSpan(value.*).end),
                });
                if (!self.match(.comma)) break;
            }
            const close = try self.expect(.r_paren, "expected ')' after macro arguments", "close the macro argument list here");
            self.allow_trailing_block_call = outer_block_call;
            const node = try self.allocator.create(syntax.ast.Expr);
            node.* = .{ .call = .{
                .callee = expr,
                .args = try macro_args.toOwnedSlice(),
                .trailing_builder = null,
                .trailing_callback = null,
                .is_macro = true,
                .span = source_pkg.Span.init(exprSpan(expr.*).start, close.span.end),
            } };
            expr = node;
            continue;
        }
        if (self.match(.dot)) {
            const member_token = try self.expect(.identifier, "expected member name after '.'", "write the member name here");
            const node = try self.allocator.create(syntax.ast.Expr);
            node.* = .{ .member = .{
                .object = expr,
                .member = member_token.lexeme,
                .span = source_pkg.Span.init(exprSpan(expr.*).start, member_token.span.end),
            } };
            expr = node;
            continue;
        }
        if (self.match(.l_bracket)) {
            // `[ ... ]` is self-delimited; re-enable trailing-block parsing inside
            // it so a struct-literal index works even in a control-flow header.
            const outer_block_call = self.allow_trailing_block_call;
            self.allow_trailing_block_call = true;
            const index = try self.parseExpression();
            self.allow_trailing_block_call = outer_block_call;
            const close = try self.expect(.r_bracket, "expected ']' after index expression", "close the index expression here");
            const node = try self.allocator.create(syntax.ast.Expr);
            node.* = .{ .index = .{
                .object = expr,
                .index = index,
                .span = source_pkg.Span.init(exprSpan(expr.*).start, close.span.end),
            } };
            expr = node;
            continue;
        }
        if (self.match(.l_paren)) {
            // Argument lists are delimited by parens, so the trailing-block
            // ambiguity that `allow_trailing_block_call` guards against cannot
            // arise here. Re-enable it while parsing arguments so nested
            // builder/callback calls still work even inside a control-flow
            // condition, then restore the outer setting to decide whether *this*
            // call may take a trailing block.
            const outer_block_call = self.allow_trailing_block_call;
            self.allow_trailing_block_call = true;
            var args = std.array_list.Managed(syntax.ast.CallArg).init(self.allocator);
            while (!self.at(.r_paren) and !self.at(.eof)) {
                const start_token = self.peek();
                var label: ?[]const u8 = null;
                if (self.at(.identifier) and (self.peekNext().kind == .colon or self.peekNext().kind == .equal)) {
                    label = self.advance().lexeme;
                    _ = self.advance();
                }
                const value = try self.parseExpression();
                try args.append(.{
                    .label = label,
                    .value = value,
                    .span = source_pkg.Span.init(start_token.span.start, exprSpan(value.*).end),
                });
                if (!self.match(.comma)) break;
            }
            const close = try self.expect(.r_paren, "expected ')' after call arguments", "close the argument list here");
            self.allow_trailing_block_call = outer_block_call;
            var trailing_builder: ?syntax.ast.BuilderBlock = null;
            var trailing_callback: ?syntax.ast.CallbackBlock = null;
            var end = close.span.end;
            // Only attach a trailing block when the current context permits it.
            // In a control-flow header (`if f(x) { ... }`) the `{` belongs to the
            // block, not to the call.
            if (self.allow_trailing_block_call and self.at(.l_brace)) {
                if (self.looksLikeCallbackBlock()) {
                    trailing_callback = try self.parseCallbackBlock();
                    end = trailing_callback.?.span.end;
                } else if (self.looksLikeCallbackBlockMissingIn()) {
                    trailing_callback = try self.parseCallbackBlock();
                    end = trailing_callback.?.span.end;
                } else {
                    trailing_builder = try self.parseBuilderBlock();
                    end = trailing_builder.?.span.end;
                }
            }
            const node = try self.allocator.create(syntax.ast.Expr);
            node.* = .{ .call = .{
                .callee = expr,
                .args = try args.toOwnedSlice(),
                .trailing_builder = trailing_builder,
                .trailing_callback = trailing_callback,
                .span = source_pkg.Span.init(exprSpan(expr.*).start, end),
            } };
            expr = node;
            continue;
        }
        if (self.allow_trailing_block_call and self.at(.l_brace) and !self.looksLikeStructLiteral()) {
            var trailing_builder: ?syntax.ast.BuilderBlock = null;
            var trailing_callback: ?syntax.ast.CallbackBlock = null;
            var end: usize = exprSpan(expr.*).end;
            if (self.looksLikeCallbackBlock()) {
                trailing_callback = try self.parseCallbackBlock();
                end = trailing_callback.?.span.end;
            } else if (self.looksLikeCallbackBlockMissingIn()) {
                trailing_callback = try self.parseCallbackBlock();
                end = trailing_callback.?.span.end;
            } else {
                trailing_builder = try self.parseBuilderBlock();
                end = trailing_builder.?.span.end;
            }
            const node = try self.allocator.create(syntax.ast.Expr);
            node.* = .{ .call = .{
                .callee = expr,
                .args = &.{},
                .trailing_builder = trailing_builder,
                .trailing_callback = trailing_callback,
                .span = source_pkg.Span.init(exprSpan(expr.*).start, end),
            } };
            expr = node;
            continue;
        }
        // A trailing `{ ... }` is only a struct literal when the context permits
        // a trailing block. In a control-flow header (`if cond {}`, `while c {}`,
        // `for x in xs {}`, `match s {}`, `switch s {}`) the `{` opens the body,
        // not a struct literal — otherwise an empty body `{}` is misparsed as an
        // empty struct literal (KPAR013 / a misleading downstream error). A
        // struct literal that genuinely belongs in a condition must be
        // parenthesized (`if (Foo { x: 1 }).ok {}`); parens re-enable the flag.
        if (self.allow_trailing_block_call and self.at(.l_brace) and self.looksLikeStructLiteral()) {
            expr = try self.parseStructLiteral(expr);
            continue;
        }
        break;
    }

    return expr;
}

pub fn parsePrimary(self: *Parser) anyerror!*syntax.ast.Expr {
    if (self.at(.kw_quote)) {
        return self.parseQuoteExpr();
    }
    if (self.at(.l_brace) and self.looksLikeCallbackBlock()) {
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .callback = try self.parseCallbackBlock() };
        return expr;
    }
    if (self.match(.integer)) {
        const token = self.previous();
        const value = parseIntegerLiteral(token.lexeme) catch {
            try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
                .severity = .@"error",
                .code = "KPAR003",
                .title = "integer literal is out of range",
                .message = "This integer literal does not fit in Kira's current 64-bit integer range.",
                .labels = &.{
                    diagnostics.primaryLabel(token.span, "integer literal is too large"),
                },
                .help = "Use a smaller integer literal.",
            });
            return error.DiagnosticsEmitted;
        };
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .integer = .{ .value = value, .span = token.span } };
        return expr;
    }
    if (self.match(.float)) {
        const token = self.previous();
        const value = std.fmt.parseFloat(f64, token.lexeme) catch {
            try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
                .severity = .@"error",
                .code = "KPAR004",
                .title = "invalid float literal",
                .message = "This floating-point literal could not be parsed.",
                .labels = &.{
                    diagnostics.primaryLabel(token.span, "invalid float literal"),
                },
                .help = "Use a literal such as `12.0`.",
            });
            return error.DiagnosticsEmitted;
        };
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .float = .{ .value = value, .span = token.span } };
        return expr;
    }
    if (self.match(.string)) {
        const token = self.previous();
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .string = .{ .value = token.lexeme, .span = token.span } };
        return expr;
    }
    if (self.match(.kw_true) or self.match(.kw_false)) {
        const token = self.previous();
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .bool = .{ .value = token.kind == .kw_true, .span = token.span } };
        return expr;
    }
    if (self.match(.identifier)) {
        const token = self.previous();
        if (std.mem.eql(u8, token.lexeme, "nativeState") and self.at(.l_paren)) {
            return parseNativeStateBuiltin(self, token);
        }
        if (std.mem.eql(u8, token.lexeme, "nativeUserData") and self.at(.l_paren)) {
            return parseNativeUserDataBuiltin(self, token);
        }
        if (std.mem.eql(u8, token.lexeme, "nativeRecover") and self.at(.less)) {
            return parseNativeRecoverBuiltin(self, token);
        }
        return try self.makeIdentifierExpr(token);
    }
    if (self.match(.dollar)) {
        const start = self.previous().span.start;
        const token = try self.expect(.identifier, "expected binding name after '$'", "write the state or binding name to project here");
        const name = try self.makeSingleSegmentName(token);
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .identifier = .{
            .name = name,
            .span = source_pkg.Span.init(start, token.span.end),
        } };
        return expr;
    }
    if (self.match(.l_paren)) {
        // A parenthesized group is explicitly delimited, so trailing blocks are
        // unambiguous inside it even within a control-flow condition.
        const outer_block_call = self.allow_trailing_block_call;
        self.allow_trailing_block_call = true;
        const expr = try self.parseExpression();
        self.allow_trailing_block_call = outer_block_call;
        _ = try self.expect(.r_paren, "expected ')' after grouped expression", "close the grouped expression here");
        return expr;
    }
    if (self.match(.l_bracket)) {
        const start = self.previous().span.start;
        // `[ ... ]` is self-delimited, so the trailing-block ambiguity that
        // `allow_trailing_block_call` guards against in a control-flow header
        // cannot arise inside it. Re-enable it while parsing elements so a struct
        // literal element (`for p in [Foo { x: 1 }] {}`) still parses, then
        // restore the outer setting (same pattern as the `(` argument list).
        const outer_block_call = self.allow_trailing_block_call;
        self.allow_trailing_block_call = true;
        defer self.allow_trailing_block_call = outer_block_call;
        var elements = std.array_list.Managed(*syntax.ast.Expr).init(self.allocator);
        while (!self.at(.r_bracket) and !self.at(.eof)) {
            try elements.append(try self.parseExpression());
            if (!self.match(.comma)) break;
        }
        const close = try self.expect(.r_bracket, "expected ']' after array literal", "close the array literal here");
        const expr = try self.allocator.create(syntax.ast.Expr);
        expr.* = .{ .array = .{
            .elements = try elements.toOwnedSlice(),
            .span = source_pkg.Span.init(start, close.span.end),
        } };
        return expr;
    }

    const token = self.peek();
    const detail = try std.fmt.allocPrint(
        self.allocator,
        "Kira expected an expression here, but found {s}.",
        .{tokenDescription(token.kind)},
    );
    try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
        .severity = .@"error",
        .code = "KPAR002",
        .title = "expected expression",
        .message = detail,
        .labels = &.{
            diagnostics.primaryLabel(token.span, unexpectedTokenLabel(token.kind)),
        },
        .help = "Insert a literal, name, call, collection literal, or parenthesized expression.",
    });
    return error.DiagnosticsEmitted;
}

fn parseIntegerLiteral(lexeme: []const u8) !i64 {
    if (lexeme.len > 2 and lexeme[0] == '0' and (lexeme[1] == 'x' or lexeme[1] == 'X')) {
        return std.fmt.parseInt(i64, lexeme[2..], 16);
    }
    return std.fmt.parseInt(i64, lexeme, 10);
}
