const std = @import("std");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const parent = @import("parser.zig");
const Parser = parent.Parser;

// Declarative macro:
//
//   macro Name(value: expr, target: place) {
//       expand {
//           ... template using `value` / `target` ...
//       }
//   }
//
// Fragment kinds (`expr`, `place`) and the `expand` body keyword are contextual identifiers, not
// reserved words. The result is a `MacroDecl` with `kind = .declarative`; the macro-expansion pass
// consumes it before semantics.
pub fn parseDeclarativeMacroDecl(
    self: *Parser,
    annotations: []const syntax.ast.Annotation,
) !syntax.ast.MacroDecl {
    const macro_token = try self.expect(.kw_macro, "expected 'macro'", "declarative macros start with 'macro'");
    const name_token = try self.expect(.identifier, "expected macro name", "name the macro here");

    const params = try parseMacroParams(self);

    _ = try self.expect(.l_brace, "expected '{' to start macro body", "open the macro body here");
    try expectContextualKeyword(self, "expand", "expected 'expand'", "a declarative macro body is an 'expand { ... }' block", "Write 'expand { ... }' containing the template.");
    const block = try self.parseBlock();
    const close = try self.expect(.r_brace, "expected '}' to close macro body", "close the macro body here");

    return .{
        .annotations = annotations,
        .kind = .declarative,
        .name = name_token.lexeme,
        .params = params,
        .expand_block = block,
        .applies_to = &.{},
        .expand_fn = null,
        .span = source_pkg.Span.init(macro_token.span.start, close.span.end),
    };
}

fn parseMacroParams(self: *Parser) ![]syntax.ast.MacroParam {
    _ = try self.expect(.l_paren, "expected '(' after macro name", "open the fragment parameter list here");
    var params = std.array_list.Managed(syntax.ast.MacroParam).init(self.allocator);
    while (!self.at(.r_paren) and !self.at(.eof)) {
        const name_token = try self.expect(.identifier, "expected fragment parameter name", "write the fragment parameter name here");
        _ = try self.expect(.colon, "expected ':' after fragment parameter name", "write the fragment kind after ':'");
        const kind_token = try self.expect(.identifier, "expected fragment kind", "write 'expr' or 'place' here");
        const kind: syntax.ast.FragmentKind = if (std.mem.eql(u8, kind_token.lexeme, "expr"))
            .expr
        else if (std.mem.eql(u8, kind_token.lexeme, "place"))
            .place
        else {
            try self.emitUnexpectedToken(
                "unknown fragment kind",
                kind_token,
                "a fragment kind must be 'expr' or 'place'",
                "Use 'expr' for a single expression (evaluated once) or 'place' for an assignable lvalue.",
            );
            return error.DiagnosticsEmitted;
        };
        try params.append(.{
            .name = name_token.lexeme,
            .kind = kind,
            .span = source_pkg.Span.init(name_token.span.start, kind_token.span.end),
        });
        if (!self.match(.comma)) break;
    }
    _ = try self.expect(.r_paren, "expected ')' after fragment parameters", "close the fragment parameter list here");
    return params.toOwnedSlice();
}

// Procedural macro:
//
//   comptime macro Name {
//       kind { function | attribute | derive }
//       appliesTo { struct, class, enum }     // attribute / derive only
//       expand(input: Syntax) -> Syntax { ... }
//   }
//
// `kind`, `appliesTo`, and `expand` are contextual identifiers. The `expand` function body is
// ordinary Kira (run at compile time by the macro evaluator) and may contain `quote { ... }`.
pub fn parseProceduralMacroDecl(
    self: *Parser,
    annotations: []const syntax.ast.Annotation,
) !syntax.ast.MacroDecl {
    const macro_token = try self.expect(.kw_macro, "expected 'macro'", "procedural macros are written 'comptime macro'");
    const name_token = try self.expect(.identifier, "expected macro name", "name the macro here");
    _ = try self.expect(.l_brace, "expected '{' to start the comptime macro body", "open the macro body here");

    var kind: ?syntax.ast.MacroKind = null;
    var applies_to = std.array_list.Managed(syntax.ast.MacroTargetKind).init(self.allocator);
    var expand_fn: ?syntax.ast.FunctionDecl = null;

    while (!self.at(.r_brace) and !self.at(.eof)) {
        if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "kind")) {
            _ = self.advance();
            _ = try self.expect(.l_brace, "expected '{' after 'kind'", "write the macro kind here");
            // 'function' is a keyword; 'attribute'/'derive' are contextual identifiers.
            const kind_token = self.advance();
            kind = if (std.mem.eql(u8, kind_token.lexeme, "function"))
                .proc_function
            else if (std.mem.eql(u8, kind_token.lexeme, "attribute"))
                .proc_attribute
            else if (std.mem.eql(u8, kind_token.lexeme, "derive"))
                .proc_derive
            else {
                try self.emitUnexpectedToken("unknown macro kind", kind_token, "a comptime macro kind must be 'function', 'attribute', or 'derive'", "Use one of 'function', 'attribute', or 'derive'.");
                return error.DiagnosticsEmitted;
            };
            _ = try self.expect(.r_brace, "expected '}' after macro kind", "close the kind section here");
        } else if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "appliesTo")) {
            _ = self.advance();
            _ = try self.expect(.l_brace, "expected '{' after 'appliesTo'", "list the legal targets here");
            while (!self.at(.r_brace) and !self.at(.eof)) {
                // 'struct'/'class'/'enum' are keywords.
                const target_token = self.advance();
                const target: syntax.ast.MacroTargetKind = if (std.mem.eql(u8, target_token.lexeme, "struct"))
                    .struct_target
                else if (std.mem.eql(u8, target_token.lexeme, "class"))
                    .class_target
                else if (std.mem.eql(u8, target_token.lexeme, "enum"))
                    .enum_target
                else {
                    try self.emitUnexpectedToken("unknown macro target", target_token, "an 'appliesTo' target must be 'struct', 'class', or 'enum'", "Use 'struct', 'class', or 'enum'.");
                    return error.DiagnosticsEmitted;
                };
                try applies_to.append(target);
                if (!self.match(.comma)) break;
            }
            _ = try self.expect(.r_brace, "expected '}' after appliesTo targets", "close the appliesTo section here");
        } else if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "expand")) {
            const expand_token = self.advance();
            const params = try self.parseParamList();
            const return_type = try self.parseOptionalReturnType();
            const body = try self.parseBlock();
            expand_fn = .{
                .annotations = &.{},
                .name = "expand",
                .params = params,
                .return_type = return_type,
                .body = body,
                .span = expand_token.span,
            };
        } else {
            try self.emitUnexpectedToken("expected comptime macro section", self.peek(), "a comptime macro body has 'kind', 'appliesTo', and 'expand' sections", "Write 'kind { function }', optional 'appliesTo { ... }', and 'expand(...) -> Syntax { ... }'.");
            return error.DiagnosticsEmitted;
        }
    }
    const close = try self.expect(.r_brace, "expected '}' to close the comptime macro", "close the macro body here");

    const resolved_kind = kind orelse {
        try self.emitUnexpectedToken("comptime macro is missing 'kind'", name_token, "every comptime macro needs a 'kind { ... }' section", "Add 'kind { function }', 'kind { attribute }', or 'kind { derive }'.");
        return error.DiagnosticsEmitted;
    };

    return .{
        .annotations = annotations,
        .kind = resolved_kind,
        .name = name_token.lexeme,
        .params = &.{},
        .expand_block = null,
        .applies_to = try applies_to.toOwnedSlice(),
        .expand_fn = expand_fn,
        .span = source_pkg.Span.init(macro_token.span.start, close.span.end),
    };
}

// `quote { ... }`: capture the body as literal token runs interleaved with `#{ expr }` splices.
pub fn parseQuoteExpr(self: *Parser) !*syntax.ast.Expr {
    const quote_token = try self.expect(.kw_quote, "expected 'quote'", "quote blocks start with 'quote'");
    _ = try self.expect(.l_brace, "expected '{' to start the quote body", "open the quote body here");

    var parts = std.array_list.Managed(syntax.ast.QuotePart).init(self.allocator);
    var text = std.array_list.Managed(u8).init(self.allocator);
    var depth: usize = 0;
    // Reconstruct token text faithfully by source adjacency: insert a separating space only where
    // the original source had whitespace between two tokens. This makes `foo_#{name}` glue into a
    // single identifier (`foo_` and `#{` are adjacent in source) while `a + b` keeps its spaces.
    var last_end: ?usize = null;

    while (!self.at(.eof)) {
        if (self.at(.r_brace) and depth == 0) break;
        if (self.at(.hash_brace)) {
            const hash = self.peek();
            if (last_end) |e| {
                if (hash.span.start > e) try text.append(' ');
            }
            if (text.items.len != 0) {
                try parts.append(.{ .text = try text.toOwnedSlice() });
                text = std.array_list.Managed(u8).init(self.allocator);
            }
            _ = self.advance(); // '#{'
            const expr = try self.parseExpression();
            const close_splice = try self.expect(.r_brace, "expected '}' to close a '#{ ... }' splice", "close the splice here");
            try parts.append(.{ .splice = expr });
            last_end = close_splice.span.end;
            continue;
        }
        const token = self.peek();
        if (last_end) |e| {
            if (token.span.start > e) try text.append(' ');
        }
        _ = self.advance();
        if (token.kind == .l_brace) depth += 1;
        if (token.kind == .r_brace) depth -= 1;
        try text.appendSlice(token.lexeme);
        last_end = token.span.end;
    }
    if (text.items.len != 0) try parts.append(.{ .text = try text.toOwnedSlice() });
    const close = try self.expect(.r_brace, "expected '}' to close the quote body", "close the quote body here");

    const node = try self.allocator.create(syntax.ast.Expr);
    node.* = .{ .quote = .{ .parts = try parts.toOwnedSlice(), .span = source_pkg.Span.init(quote_token.span.start, close.span.end) } };
    return node;
}

fn expectContextualKeyword(
    self: *Parser,
    word: []const u8,
    title: []const u8,
    label: []const u8,
    help: []const u8,
) !void {
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, word)) {
        _ = self.advance();
        return;
    }
    try self.emitUnexpectedToken(title, self.peek(), label, help);
    return error.DiagnosticsEmitted;
}
