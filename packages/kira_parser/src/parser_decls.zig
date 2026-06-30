const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const parent = @import("parser.zig");
const Parser = parent.Parser;
const exprSpan = parent.exprSpan;
const typeSpan = parent.typeSpan;
const paramsEnd = parent.paramsEnd;
const sectionKind = parent.sectionKind;
const tokenDescription = parent.tokenDescription;
const unexpectedTokenLabel = parent.unexpectedTokenLabel;
const expectedTokenHelp = parent.expectedTokenHelp;
const cloneQualifiedName = parent.cloneQualifiedName;
pub fn parseTopLevelDecl(self: *Parser, annotations: []const syntax.ast.Annotation) !?syntax.ast.Decl {
    if (self.at(.kw_annotation)) {
        return .{ .annotation_decl = try self.parseAnnotationDeclWithAnnotations(annotations) };
    }
    if (self.at(.kw_capability)) {
        if (annotations.len != 0) {
            try self.emitUnexpectedToken(
                "capability declarations cannot be annotated",
                self.peek(),
                "capability declaration starts here",
                "Remove the preceding annotation usage.",
            );
            return error.DiagnosticsEmitted;
        }
        return .{ .capability_decl = try self.parseCapabilityDecl() };
    }
    if (self.at(.kw_enum)) {
        return .{ .enum_decl = try self.parseEnumDeclWithAnnotations(annotations) };
    }
    if (self.at(.kw_comptime)) {
        const comptime_token = self.advance();
        if (self.at(.kw_function)) {
            return .{ .function_decl = try self.parseFunctionDeclWithAnnotations(annotations, false, true) };
        }
        if (self.at(.kw_construct)) {
            return .{ .construct_decl = try self.parseConstructDeclWithAnnotations(annotations, true) };
        }
        if (self.at(.kw_macro)) {
            return .{ .macro_decl = try self.parseProceduralMacroDecl(annotations) };
        }
        try self.emitUnexpectedToken(
            "expected comptime declaration",
            self.peek(),
            "`comptime` applies to function, construct, or macro declarations",
            "Write `comptime function ...`, `comptime construct ...`, or `comptime macro ...`.",
        );
        _ = comptime_token;
        return error.DiagnosticsEmitted;
    }
    if (self.at(.kw_macro)) {
        return .{ .macro_decl = try self.parseDeclarativeMacroDecl(annotations) };
    }
    if (self.at(.kw_function)) {
        return .{ .function_decl = try self.parseFunctionDeclWithAnnotations(annotations, false, false) };
    }
    if (self.at(.kw_class)) {
        return .{ .type_decl = try self.parseTypeDeclWithAnnotations(annotations, .class) };
    }
    if (self.at(.kw_struct)) {
        return .{ .type_decl = try self.parseTypeDeclWithAnnotations(annotations, .struct_decl) };
    }
    if (self.at(.kw_type)) {
        try self.emitUnexpectedToken(
            "removed type declaration syntax",
            self.peek(),
            "`type` has been removed from Kira",
            "Use `struct` for value-oriented declarations or `class` for declarations that need inheritance.",
        );
        return error.DiagnosticsEmitted;
    }
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "static")) {
        try self.emitUnexpectedToken(
            "removed static keyword",
            self.peek(),
            "`static` has been removed and is not valid Kira syntax",
            "Use `let` for immutable members and `var` for mutable members.",
        );
        return error.DiagnosticsEmitted;
    }
    if (self.at(.kw_construct)) {
        return .{ .construct_decl = try self.parseConstructDeclWithAnnotations(annotations, false) };
    }
    if (self.at(.kw_extend)) {
        return .{ .extend_decl = try parseExtendDecl(self, annotations) };
    }
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "func")) {
        try self.emitUnexpectedToken(
            "outdated function declaration syntax",
            self.peek(),
            "`func` has been replaced by `function`",
            "Write `function` for function declarations.",
        );
        return error.DiagnosticsEmitted;
    }
    // Top-level function-macro invocation: `name!(args)` that expands to declarations.
    if (self.at(.identifier) and self.peekNext().kind == .bang) {
        const expr = try self.parseExpression();
        if (expr.* == .call and expr.call.is_macro) {
            _ = self.match(.semicolon);
            return .{ .macro_invocation = expr.call };
        }
        try self.emitUnexpectedToken("expected a macro invocation", self.peek(), "a top-level '!' must be a `name!(args)` macro call", "Write `name!(args)` to invoke a function macro.");
        return error.DiagnosticsEmitted;
    }
    if (self.looksLikeConstructFormDecl()) {
        return .{ .construct_form_decl = try self.parseConstructFormDeclWithAnnotations(annotations) };
    }

    const token = self.peek();
    try self.emitUnexpectedToken(
        "expected top-level declaration",
        token,
        "expected a declaration here",
        "Start a declaration with `annotation`, `capability`, `enum`, `class`, `struct`, `function`, `construct`, or a construct-defined declaration form such as `Widget Button(...) { ... }`.",
    );
    return error.DiagnosticsEmitted;
}

pub fn parseAnnotationDecl(self: *Parser) !syntax.ast.AnnotationDecl {
    const annotation_token = try self.expect(.kw_annotation, "expected 'annotation'", "annotation declarations start with 'annotation'");
    const name_token = try self.expect(.identifier, "expected annotation name", "name the annotation here");
    _ = try self.expect(.l_brace, "expected '{' to start annotation body", "open the annotation body here");
    var parameters = std.array_list.Managed(syntax.ast.AnnotationParameterDecl).init(self.allocator);
    var targets = std.array_list.Managed(syntax.ast.AnnotationTarget).init(self.allocator);
    var uses = std.array_list.Managed(syntax.ast.QualifiedName).init(self.allocator);
    var generated_members = std.array_list.Managed(syntax.ast.GeneratedMember).init(self.allocator);

    while (!self.at(.r_brace) and !self.at(.eof)) {
        if (self.at(.kw_targets)) {
            _ = self.advance();
            _ = try self.expect(.colon, "expected ':' after targets", "declare annotation targets after `targets:`");
            while (true) {
                try targets.append(try self.parseAnnotationTarget());
                if (!self.match(.comma)) break;
            }
            _ = self.match(.semicolon);
            continue;
        }
        if (self.at(.kw_uses)) {
            _ = self.advance();
            while (true) {
                try uses.append(try self.parseQualifiedName("expected capability name after 'uses'"));
                if (!self.match(.comma)) break;
            }
            _ = self.match(.semicolon);
            continue;
        }
        if (self.at(.kw_generated)) {
            try generated_members.appendSlice(try self.parseGeneratedBlock());
            continue;
        }

        const block_token = try self.expect(.identifier, "expected annotation declaration member", "annotation declarations support `targets: ...`, `uses ...`, `generated { ... }`, and `parameters { ... }`");
        if (!std.mem.eql(u8, block_token.lexeme, "parameters")) {
            try self.emitUnexpectedToken(
                "unsupported annotation declaration block",
                block_token,
                "unsupported annotation block here",
                "Use `targets: ...`, `uses CapabilityName`, `generated { ... }`, or `parameters { ... }` inside an annotation.",
            );
            return error.DiagnosticsEmitted;
        }
        _ = try self.expect(.l_brace, "expected '{' after parameters", "open the parameters block here");
        while (!self.at(.r_brace) and !self.at(.eof)) {
            try parameters.append(try self.parseAnnotationParameterDecl());
        }
        _ = try self.expect(.r_brace, "expected '}' to close parameters", "parameters block should end here");
    }

    const close = try self.expect(.r_brace, "expected '}' to close annotation body", "annotation body should end here");
    return .{
        .name = name_token.lexeme,
        .targets = try targets.toOwnedSlice(),
        .uses = try uses.toOwnedSlice(),
        .parameters = try parameters.toOwnedSlice(),
        .generated_members = try generated_members.toOwnedSlice(),
        .span = source_pkg.Span.init(annotation_token.span.start, close.span.end),
    };
}

pub fn parseAnnotationDeclWithAnnotations(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.AnnotationDecl {
    const decl = try self.parseAnnotationDecl();
    if (annotations.len == 0) return decl;
    var uses = std.array_list.Managed(syntax.ast.QualifiedName).init(self.allocator);
    try uses.appendSlice(decl.uses);
    for (annotations) |annotation| try uses.append(annotation.name);
    return .{
        .name = decl.name,
        .targets = decl.targets,
        .uses = try uses.toOwnedSlice(),
        .parameters = decl.parameters,
        .generated_members = decl.generated_members,
        .span = source_pkg.Span.init(annotations[0].span.start, decl.span.end),
    };
}

pub fn parseCapabilityDecl(self: *Parser) !syntax.ast.CapabilityDecl {
    const capability_token = try self.expect(.kw_capability, "expected 'capability'", "capability declarations start with 'capability'");
    const name_token = try self.expect(.identifier, "expected capability name", "name the capability here");
    _ = try self.expect(.l_brace, "expected '{' to start capability body", "open the capability body here");
    var generated_members = std.array_list.Managed(syntax.ast.GeneratedMember).init(self.allocator);

    while (!self.at(.r_brace) and !self.at(.eof)) {
        if (self.at(.kw_generated)) {
            try generated_members.appendSlice(try self.parseGeneratedBlock());
            continue;
        }
        try self.emitUnexpectedToken(
            "unsupported capability declaration member",
            self.peek(),
            "capabilities currently declare reusable generated members",
            "Use `generated { ... }` inside a capability.",
        );
        return error.DiagnosticsEmitted;
    }

    const close = try self.expect(.r_brace, "expected '}' to close capability body", "capability body should end here");
    return .{
        .name = name_token.lexeme,
        .generated_members = try generated_members.toOwnedSlice(),
        .span = source_pkg.Span.init(capability_token.span.start, close.span.end),
    };
}

pub fn parseEnumDecl(self: *Parser) !syntax.ast.EnumDecl {
    return self.parseEnumDeclWithAnnotations(&.{});
}

pub fn parseEnumDeclWithAnnotations(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.EnumDecl {
    const enum_token = try self.expect(.kw_enum, "expected 'enum'", "enum declarations start with 'enum'");
    const name_token = try self.expect(.identifier, "expected enum name", "name the enum here");
    var type_params = std.array_list.Managed([]const u8).init(self.allocator);
    if (self.match(.less)) {
        while (!self.at(.greater) and !self.at(.eof)) {
            const type_param = try self.expect(.identifier, "expected enum type parameter", "write the type parameter name here");
            try type_params.append(type_param.lexeme);
            if (!self.match(.comma)) break;
        }
        _ = try self.expect(.greater, "expected '>' after enum type parameters", "close the enum type parameter list here");
    }

    _ = try self.expect(.l_brace, "expected '{' to start enum body", "open the enum body here");
    var variants = std.array_list.Managed(syntax.ast.EnumVariantDecl).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        const variant_name = try self.expect(.identifier, "expected enum variant name", "name the enum variant here");
        var associated_type: ?*syntax.ast.TypeExpr = null;
        var default_value: ?*syntax.ast.Expr = null;
        var end = variant_name.span.end;

        if (self.match(.colon)) {
            associated_type = try self.parseTypeExpr();
            end = typeSpan(associated_type.?.*).end;
        } else if (self.match(.l_paren)) {
            associated_type = try self.parseTypeExpr();
            const close_payload = try self.expect(.r_paren, "expected ')' after enum payload type", "close the enum payload type here");
            end = close_payload.span.end;
        }

        if (self.match(.equal)) {
            default_value = try self.parseExpression();
            end = exprSpan(default_value.?.*).end;
        }

        _ = self.match(.semicolon);
        _ = self.match(.comma);
        try variants.append(.{
            .name = variant_name.lexeme,
            .associated_type = associated_type,
            .default_value = default_value,
            .span = source_pkg.Span.init(variant_name.span.start, end),
        });
    }

    const close = try self.expect(.r_brace, "expected '}' to close enum body", "enum body should end here");
    const start = if (annotations.len != 0) annotations[0].span.start else enum_token.span.start;
    return .{
        .annotations = annotations,
        .name = name_token.lexeme,
        .type_params = try type_params.toOwnedSlice(),
        .variants = try variants.toOwnedSlice(),
        .span = source_pkg.Span.init(start, close.span.end),
    };
}

pub fn parseAnnotationTarget(self: *Parser) !syntax.ast.AnnotationTarget {
    if (self.match(.kw_class)) return .class;
    if (self.match(.kw_struct)) return .struct_decl;
    if (self.match(.kw_function)) return .function;
    if (self.match(.kw_construct)) return .construct;
    if (self.match(.identifier)) {
        const token = self.previous();
        if (std.mem.eql(u8, token.lexeme, "field")) return .field;
    }
    try self.emitUnexpectedToken(
        "expected annotation target",
        self.peek(),
        "target must name a declaration kind",
        "Use targets such as `class`, `struct`, `function`, `construct`, or `field`.",
    );
    return error.DiagnosticsEmitted;
}

pub fn parseGeneratedBlock(self: *Parser) ![]syntax.ast.GeneratedMember {
    const generated_token = try self.expect(.kw_generated, "expected 'generated'", "generated member blocks start with 'generated'");
    _ = try self.expect(.l_brace, "expected '{' to start generated block", "open the generated block here");
    var members = std.array_list.Managed(syntax.ast.GeneratedMember).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        try members.append(try self.parseGeneratedMember());
    }
    _ = try self.expect(.r_brace, "expected '}' to close generated block", "generated block should end here");
    _ = generated_token;
    return members.toOwnedSlice();
}

pub fn parseGeneratedMember(self: *Parser) !syntax.ast.GeneratedMember {
    const start_token = self.peek();
    const overridable = self.match(.kw_overridable);
    if (self.at(.kw_function)) {
        const function_decl = try self.parseFunctionDeclWithAnnotations(&.{}, false, false);
        return .{
            .overridable = overridable,
            .member = .{ .function_decl = function_decl },
            .span = source_pkg.Span.init(start_token.span.start, function_decl.span.end),
        };
    }
    try self.emitUnexpectedToken(
        "expected generated member",
        self.peek(),
        "generated blocks currently support functions",
        "Write `function name(...) { ... }` or `overridable function name(...) { ... }`.",
    );
    return error.DiagnosticsEmitted;
}

pub fn parseAnnotationParameterDecl(self: *Parser) !syntax.ast.AnnotationParameterDecl {
    const name_token = try self.expect(.identifier, "expected annotation parameter name", "name the annotation parameter here");
    _ = try self.expect(.colon, "expected ':' after annotation parameter name", "declare the parameter type here");
    const type_expr = try self.parseTypeExpr();
    var default_value: ?*syntax.ast.Expr = null;
    var end = typeSpan(type_expr.*).end;
    if (self.match(.equal)) {
        default_value = try self.parseExpression();
        end = exprSpan(default_value.?.*).end;
    }
    _ = self.match(.semicolon);
    return .{
        .name = name_token.lexeme,
        .type_expr = type_expr,
        .default_value = default_value,
        .span = source_pkg.Span.init(name_token.span.start, end),
    };
}

pub fn parseAnnotations(self: *Parser) ![]syntax.ast.Annotation {
    var annotations = std.array_list.Managed(syntax.ast.Annotation).init(self.allocator);
    while (self.match(.at_sign)) {
        const at_token = self.previous();
        const name = try self.parseQualifiedName("expected annotation name after '@'");
        if (name.segments.len == 1 and std.mem.eql(u8, name.segments[0].text, "Doc")) {
            try self.emitUnexpectedToken(
                "removed @Doc annotation",
                at_token,
                "`@Doc` has been removed from Kira documentation syntax",
                "Use consecutive `///` documentation comments immediately above the declaration or member.",
            );
            return error.DiagnosticsEmitted;
        }
        var args = std.array_list.Managed(syntax.ast.AnnotationArg).init(self.allocator);
        var block: ?syntax.ast.AnnotationBlock = null;
        var end = name.span.end;

        if (self.match(.l_paren)) {
            while (!self.at(.r_paren) and !self.at(.eof)) {
                const start_token = self.peek();
                var label: ?[]const u8 = null;
                if (self.at(.identifier) and self.peekNext().kind == .colon) {
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
            const close = try self.expect(.r_paren, "expected ')' after annotation arguments", "close the annotation arguments here");
            end = close.span.end;
        }

        if (self.at(.l_brace)) {
            block = try self.parseAnnotationBlock();
            end = block.?.span.end;
        }

        try annotations.append(.{
            .name = name,
            .args = try args.toOwnedSlice(),
            .block = block,
            .span = source_pkg.Span.init(at_token.span.start, end),
        });
    }
    return annotations.toOwnedSlice();
}

pub fn parseAnnotationBlock(self: *Parser) !syntax.ast.AnnotationBlock {
    const open = try self.expect(.l_brace, "expected '{' to start annotation block", "open the annotation block here");
    var entries = std.array_list.Managed(syntax.ast.AnnotationBlockEntry).init(self.allocator);

    while (!self.at(.r_brace) and !self.at(.eof)) {
        if (self.at(.identifier) and self.peekNext().kind == .colon) {
            const name_token = self.advance();
            _ = self.advance();
            const value = try self.parseExpression();
            try entries.append(.{ .field = .{
                .name = name_token.lexeme,
                .value = value,
                .span = source_pkg.Span.init(name_token.span.start, exprSpan(value.*).end),
            } });
        } else {
            const value = try self.parseExpression();
            try entries.append(.{ .value = .{
                .value = value,
                .span = exprSpan(value.*),
            } });
        }
        _ = self.match(.semicolon);
    }

    const close = try self.expect(.r_brace, "expected '}' to close annotation block", "annotation block should end here");
    return .{
        .entries = try entries.toOwnedSlice(),
        .span = source_pkg.Span.init(open.span.start, close.span.end),
    };
}

pub fn parseFunctionDeclWithAnnotations(self: *Parser, annotations: []const syntax.ast.Annotation, is_override: bool, is_comptime: bool) !syntax.ast.FunctionDecl {
    const function_token = try self.expect(.kw_function, "expected 'function'", "function declarations start with 'function'");
    const name_token = try self.expect(.identifier, "expected function name", "name the function here");
    const params = try self.parseParamList();
    const return_type = try self.parseOptionalReturnType();
    var body: ?syntax.ast.Block = null;
    var end = if (return_type) |ty| typeSpan(ty.*).end else paramsEnd(params, name_token.span.end);
    if (self.match(.semicolon)) {
        end = self.previous().span.end;
    } else {
        body = try self.parseBlock();
        end = body.?.span.end;
    }
    const start = if (annotations.len > 0) annotations[0].span.start else function_token.span.start;
    return .{
        .annotations = annotations,
        .is_override = is_override,
        .is_comptime = is_comptime,
        .name = name_token.lexeme,
        .params = params,
        .return_type = return_type,
        .body = body,
        .span = source_pkg.Span.init(start, end),
    };
}

pub fn parseFunctionSignature(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.FunctionSignature {
    const function_token = try self.expect(.kw_function, "expected 'function'", "function signatures start with 'function'");
    const name_token = try self.expect(.identifier, "expected function name", "name the function here");
    const has_params = self.at(.l_paren);
    const params = if (has_params) try self.parseParamList() else try self.allocator.alloc(syntax.ast.ParamDecl, 0);
    const return_type = try self.parseOptionalReturnType();
    const end = if (return_type) |ty| typeSpan(ty.*).end else if (has_params) paramsEnd(params, name_token.span.end) else name_token.span.end;
    return .{
        .annotations = annotations,
        .name = name_token.lexeme,
        .params = params,
        .return_type = return_type,
        .span = source_pkg.Span.init(function_token.span.start, end),
    };
}

pub fn parseOptionalReturnType(self: *Parser) !?*syntax.ast.TypeExpr {
    if (self.match(.colon) or self.match(.arrow)) return self.parseTypeExpr();
    return null;
}

pub fn parseParamList(self: *Parser) ![]syntax.ast.ParamDecl {
    _ = try self.expect(.l_paren, "expected '(' after name", "open the parameter list here");
    var params = std.array_list.Managed(syntax.ast.ParamDecl).init(self.allocator);

    while (!self.at(.r_paren) and !self.at(.eof)) {
        const annotations = try self.parseAnnotations();
        const name_token = try self.expect(.identifier, "expected parameter name", "write the parameter name here");
        var type_expr: ?*syntax.ast.TypeExpr = null;
        var end = name_token.span.end;
        if (std.mem.eql(u8, name_token.lexeme, "_")) {
            const unlabeled_name = try self.expect(.identifier, "expected parameter name after '_'", "write the internal parameter name here");
            end = unlabeled_name.span.end;
            if (self.match(.colon)) {
                type_expr = try self.parseTypeExpr();
                end = typeSpan(type_expr.?.*).end;
            }
            try params.append(.{
                .annotations = annotations,
                .name = unlabeled_name.lexeme,
                .type_expr = type_expr,
                .span = source_pkg.Span.init(name_token.span.start, end),
            });
            if (!self.match(.comma)) break;
            continue;
        }
        if (self.match(.colon)) {
            type_expr = try self.parseTypeExpr();
            end = typeSpan(type_expr.?.*).end;
        }
        try params.append(.{
            .annotations = annotations,
            .name = name_token.lexeme,
            .type_expr = type_expr,
            .span = source_pkg.Span.init(name_token.span.start, end),
        });
        if (!self.match(.comma)) break;
    }

    _ = try self.expect(.r_paren, "expected ')' after parameters", "close the parameter list here");
    return params.toOwnedSlice();
}

pub fn parseTypeDeclWithAnnotations(self: *Parser, annotations: []const syntax.ast.Annotation, kind: syntax.ast.TypeKind) !syntax.ast.TypeDecl {
    const decl_token = if (kind == .class)
        try self.expect(.kw_class, "expected 'class'", "class declarations start with 'class'")
    else
        try self.expect(.kw_struct, "expected 'struct'", "struct declarations start with 'struct'");
    const name_token = try self.expect(.identifier, "expected declaration name", "name the declaration here");
    var parents = std.array_list.Managed(syntax.ast.QualifiedName).init(self.allocator);
    if (self.match(.kw_extends)) {
        if (kind == .struct_decl) {
            try self.emitUnexpectedToken(
                "struct cannot inherit",
                self.previous(),
                "`extends` is only valid on classes",
                "Use `class` when inheritance is intended, or remove the `extends` clause for a value-oriented struct.",
            );
            return error.DiagnosticsEmitted;
        }
        while (true) {
            try parents.append(try self.parseQualifiedName("expected parent type name after 'extends'"));
            if (!self.match(.comma)) break;
        }
    }
    _ = try self.expect(.l_brace, "expected '{' to start declaration body", "open the declaration body here");
    var members = std.array_list.Managed(syntax.ast.BodyMember).init(self.allocator);

    while (!self.at(.r_brace) and !self.at(.eof)) {
        self.consumeDocComments();
        const annotations_inner = try self.parseAnnotations();
        try members.append(try self.parseBodyMember(annotations_inner));
    }

    const close = try self.expect(.r_brace, "expected '}' to close declaration body", "declaration body should end here");
    const start = if (annotations.len > 0) annotations[0].span.start else decl_token.span.start;
    return .{
        .kind = kind,
        .annotations = annotations,
        .name = name_token.lexeme,
        .parents = try parents.toOwnedSlice(),
        .members = try members.toOwnedSlice(),
        .span = source_pkg.Span.init(start, close.span.end),
    };
}

pub fn parseConstructDeclWithAnnotations(self: *Parser, annotations: []const syntax.ast.Annotation, is_comptime: bool) !syntax.ast.ConstructDecl {
    const construct_token = try self.expect(.kw_construct, "expected 'construct'", "construct declarations start with 'construct'");
    const name_token = try self.expect(.identifier, "expected construct name", "name the construct here");
    var parents = std.array_list.Managed(syntax.ast.QualifiedName).init(self.allocator);
    if (self.match(.kw_extends)) {
        while (true) {
            try parents.append(try self.parseQualifiedName("expected parent construct name after 'extends'"));
            if (!self.match(.comma)) break;
        }
    }
    _ = try self.expect(.l_brace, "expected '{' to start construct body", "open the construct body here");
    var sections = std.array_list.Managed(syntax.ast.ConstructSection).init(self.allocator);
    var members = std.array_list.Managed(syntax.ast.BodyMember).init(self.allocator);

    while (!self.at(.r_brace) and !self.at(.eof)) {
        self.consumeDocComments();
        // Direct, SwiftUI-style members (`@Required let body: Widget`, `let node: Node { ... }`,
        // `@Required function measure(...) -> Size`) start with an annotation or a member keyword.
        // Section-based entries (`requires { ... }`, `content { ... }`, etc.) start with a bare
        // identifier (the section name), so the two surfaces never collide.
        if (self.at(.at_sign) or self.at(.kw_let) or self.at(.kw_var) or self.at(.kw_function) or self.at(.kw_override)) {
            const member_annotations = try self.parseAnnotations();
            try members.append(try parseConstructMember(self, member_annotations));
        } else {
            try sections.append(try self.parseConstructSection());
        }
    }

    const close = try self.expect(.r_brace, "expected '}' to close construct body", "construct body should end here");
    const start = if (annotations.len > 0) annotations[0].span.start else construct_token.span.start;
    return .{
        .annotations = annotations,
        .is_comptime = is_comptime,
        .name = name_token.lexeme,
        .parents = try parents.toOwnedSlice(),
        .sections = try sections.toOwnedSlice(),
        .members = try members.toOwnedSlice(),
        .span = source_pkg.Span.init(start, close.span.end),
    };
}

// Parse a direct construct member: a field (possibly a computed `let node: Node { ... }`) or a
// function. Construct functions may be bodyless signatures (`@Required function f(...) -> T`),
// which is how `@Required` declares a required behavior without an implementation.
fn parseConstructMember(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.BodyMember {
    const is_override = self.match(.kw_override);
    if (self.at(.kw_let) or self.at(.kw_var)) {
        return .{ .field_decl = try self.parseFieldDecl(annotations, is_override) };
    }
    if (self.at(.kw_function)) {
        return .{ .function_decl = try parseConstructMemberFunction(self, annotations, is_override) };
    }
    try self.emitUnexpectedToken(
        "expected construct member",
        self.peek(),
        "construct members are fields or functions",
        "Use `let`/`var` for fields (including computed `let node: Node { ... }`) or `function` for behaviors.",
    );
    return error.DiagnosticsEmitted;
}

// Like `parseFunctionDeclWithAnnotations`, but a construct member function may omit its body:
// a bare signature (terminated by `;`, a closing `}`, or the next member) declares a required
// or abstract behavior rather than an implementation.
fn parseConstructMemberFunction(self: *Parser, annotations: []const syntax.ast.Annotation, is_override: bool) !syntax.ast.FunctionDecl {
    const function_token = try self.expect(.kw_function, "expected 'function'", "function declarations start with 'function'");
    const name_token = try self.expect(.identifier, "expected function name", "name the function here");
    const params = try self.parseParamList();
    const return_type = try self.parseOptionalReturnType();
    var body: ?syntax.ast.Block = null;
    var end = if (return_type) |ty| typeSpan(ty.*).end else paramsEnd(params, name_token.span.end);
    if (self.at(.l_brace)) {
        body = try self.parseBlock();
        end = body.?.span.end;
    } else {
        end = try self.consumeFieldTerminator(end);
    }
    const start = if (annotations.len > 0) annotations[0].span.start else function_token.span.start;
    return .{
        .annotations = annotations,
        .is_override = is_override,
        .name = name_token.lexeme,
        .params = params,
        .return_type = return_type,
        .body = body,
        .span = source_pkg.Span.init(start, end),
    };
}

pub fn parseConstructSection(self: *Parser) !syntax.ast.ConstructSection {
    const name_token = try self.expect(.identifier, "expected construct section name", "name the section here");
    if (self.match(.colon)) {
        const type_expr = try self.parseTypeExpr();
        const end = try self.consumeStatementTerminator(typeSpan(type_expr.*).end, "expected ';' after construct section type", "terminate the typed construct section with ';'");
        const qualified = try self.makeSingleSegmentName(name_token);
        const entries = try self.allocator.alloc(syntax.ast.ConstructSectionEntry, 1);
        entries[0] = .{ .named_rule = .{
            .name = qualified,
            .args = &.{},
            .type_expr = type_expr,
            .value = null,
            .block = null,
            .span = source_pkg.Span.init(name_token.span.start, end),
        } };
        return .{
            .name = name_token.lexeme,
            .kind = sectionKind(name_token.lexeme),
            .entries = entries,
            .span = source_pkg.Span.init(name_token.span.start, end),
        };
    }
    // A construct-body `content { name { accepts T; count R } ... }` block declares named
    // content channels rather than generic section entries.
    if (std.mem.eql(u8, name_token.lexeme, "content")) {
        return self.parseConstructContentSection(name_token);
    }
    _ = try self.expect(.l_brace, "expected '{' after construct section name", "open the construct section here");
    var entries = std.array_list.Managed(syntax.ast.ConstructSectionEntry).init(self.allocator);

    while (!self.at(.r_brace) and !self.at(.eof)) {
        if (sectionKind(name_token.lexeme) == .modifiers and self.at(.identifier) and self.peekNext().kind == .l_brace) {
            const subgroup = self.advance();
            _ = try self.expect(.l_brace, "expected '{' after wrapper group", "open the wrapper group here");
            while (!self.at(.r_brace) and !self.at(.eof)) {
                if (self.at(.at_sign)) {
                    try entries.append(.{ .annotation_spec = try self.parseAnnotationSpec() });
                    continue;
                }
                try self.emitUnexpectedToken(
                    "expected wrapper annotation",
                    self.peek(),
                    "wrapper groups contain annotation specs",
                    "Use entries such as `@State;` inside `member { ... }` or `parameter { ... }`.",
                );
                return error.DiagnosticsEmitted;
            }
            _ = try self.expect(.r_brace, "expected '}' to close wrapper group", "wrapper group should end here");
            _ = subgroup;
            continue;
        }
        if (self.at(.at_sign) and sectionKind(name_token.lexeme) == .annotations) {
            try entries.append(.{ .annotation_spec = try self.parseAnnotationSpec() });
            continue;
        }
        if (self.at(.kw_let)) {
            try entries.append(.{ .field_decl = try self.parseFieldDecl(&.{}, false) });
            continue;
        }
        const entry_annotations = try self.parseAnnotations();
        if (self.at(.kw_function)) {
            const signature = try self.parseFunctionSignature(entry_annotations);
            _ = self.match(.semicolon);
            try entries.append(.{ .function_signature = signature });
            continue;
        }
        if (entry_annotations.len != 0) {
            try self.emitUnexpectedToken(
                "expected annotated construct section entry",
                self.peek(),
                "annotations in construct sections must apply to a function signature",
                "Write `@Required function name(...) -> Type` or remove the annotation.",
            );
            return error.DiagnosticsEmitted;
        }
        if (self.isLifecycleHookStart()) {
            try entries.append(.{ .lifecycle_hook = try self.parseLifecycleHook() });
            continue;
        }
        if (sectionKind(name_token.lexeme) == .properties) {
            try entries.append(.{ .property_schema = try self.parsePropertySchemaField() });
            continue;
        }
        if (self.at(.identifier)) {
            try entries.append(.{ .named_rule = try self.parseNamedRule() });
            continue;
        }

        try self.emitUnexpectedToken(
            "expected construct section entry",
            self.peek(),
            "expected a construct section entry here",
            "Use an annotation spec, field, lifecycle hook, function signature, or named rule inside this section.",
        );
        return error.DiagnosticsEmitted;
    }

    const close = try self.expect(.r_brace, "expected '}' to close construct section", "construct section should end here");
    return .{
        .name = name_token.lexeme,
        .kind = sectionKind(name_token.lexeme),
        .entries = try entries.toOwnedSlice(),
        .span = source_pkg.Span.init(name_token.span.start, close.span.end),
    };
}

// Parse a construct-body `content { channel { accepts T; count min..max } ... }` block into a
// section whose entries are content-channel schemas.
pub fn parseConstructContentSection(self: *Parser, name_token: syntax.Token) !syntax.ast.ConstructSection {
    // `content sealed` / `content passthrough` are bare directives.
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "sealed")) {
        return finishContentDirectiveSection(self, name_token, .sealed);
    }
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "passthrough")) {
        return finishContentDirectiveSection(self, name_token, .passthrough);
    }
    // `content refine { channel { ... } }` narrows inherited channels.
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "refine")) {
        const directive_token = self.advance();
        _ = try self.expect(.l_brace, "expected '{' after 'refine'", "open the refinement block here");
        var entries = std.array_list.Managed(syntax.ast.ConstructSectionEntry).init(self.allocator);
        try entries.append(.{ .content_directive = .{ .mode = .refine, .span = directive_token.span } });
        while (!self.at(.r_brace) and !self.at(.eof)) {
            try entries.append(.{ .content_channel = try self.parseContentChannel() });
        }
        const close = try self.expect(.r_brace, "expected '}' to close refinement block", "refinement block should end here");
        return makeContentSection(self, name_token, try entries.toOwnedSlice(), close.span.end);
    }
    // `content project { local as Parent.channel }` maps declaration sections to inherited channels.
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "project")) {
        const directive_token = self.advance();
        _ = try self.expect(.l_brace, "expected '{' after 'project'", "open the projection block here");
        var entries = std.array_list.Managed(syntax.ast.ConstructSectionEntry).init(self.allocator);
        try entries.append(.{ .content_directive = .{ .mode = .project, .span = directive_token.span } });
        while (!self.at(.r_brace) and !self.at(.eof)) {
            try entries.append(.{ .content_projection = try self.parseContentProjection() });
        }
        const close = try self.expect(.r_brace, "expected '}' to close projection block", "projection block should end here");
        return makeContentSection(self, name_token, try entries.toOwnedSlice(), close.span.end);
    }

    _ = try self.expect(.l_brace, "expected '{' to start content channels", "open the content channel block here");
    var entries = std.array_list.Managed(syntax.ast.ConstructSectionEntry).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        try entries.append(.{ .content_channel = try self.parseContentChannel() });
    }
    const close = try self.expect(.r_brace, "expected '}' to close content channels", "content channel block should end here");
    return makeContentSection(self, name_token, try entries.toOwnedSlice(), close.span.end);
}

fn finishContentDirectiveSection(self: *Parser, name_token: syntax.Token, mode: syntax.ast.ContentDirectiveMode) !syntax.ast.ConstructSection {
    const directive_token = self.advance();
    _ = self.match(.semicolon);
    const entries = try self.allocator.alloc(syntax.ast.ConstructSectionEntry, 1);
    entries[0] = .{ .content_directive = .{ .mode = mode, .span = directive_token.span } };
    return makeContentSection(self, name_token, entries, directive_token.span.end);
}

fn makeContentSection(self: *Parser, name_token: syntax.Token, entries: []syntax.ast.ConstructSectionEntry, end: usize) syntax.ast.ConstructSection {
    _ = self;
    return .{
        .name = name_token.lexeme,
        .kind = .custom,
        .entries = entries,
        .span = source_pkg.Span.init(name_token.span.start, end),
    };
}

// Parse `local as Parent.channel` inside a `content project { ... }` block.
pub fn parseContentProjection(self: *Parser) !syntax.ast.ContentProjection {
    const local_token = try self.expect(.identifier, "expected projection source name", "name the local declaration section to project");
    _ = try self.expect(.kw_as, "expected 'as' in projection", "write projections as `local as Parent.channel`");
    const target = try self.parseQualifiedName("expected `Parent.channel` projection target");
    if (target.segments.len < 2) {
        try self.emitUnexpectedToken(
            "incomplete projection target",
            self.previous(),
            "projection targets are written `Parent.channel`",
            "Name both the parent construct and its channel, for example `WebElement.content`.",
        );
        return error.DiagnosticsEmitted;
    }
    _ = self.match(.semicolon);
    // Split `Parent.channel`: the last segment is the channel, the rest the construct path.
    const channel_segment = target.segments[target.segments.len - 1];
    const construct_segments = target.segments[0 .. target.segments.len - 1];
    return .{
        .local = local_token.lexeme,
        .target_construct = .{ .segments = construct_segments, .span = target.span },
        .target_channel = channel_segment.text,
        .span = source_pkg.Span.init(local_token.span.start, channel_segment.span.end),
    };
}

pub fn parseContentChannel(self: *Parser) !syntax.ast.ContentChannelSchema {
    const name_token = try self.expect(.identifier, "expected content channel name", "name the content channel here");
    _ = try self.expect(.l_brace, "expected '{' after channel name", "open the channel rule block here");
    var accepts: ?syntax.ast.QualifiedName = null;
    var count: ?syntax.ast.CountRange = null;
    while (!self.at(.r_brace) and !self.at(.eof)) {
        const rule_token = try self.expect(.identifier, "expected 'accepts' or 'count'", "channel rules are `accepts Type` and `count min..max`");
        if (std.mem.eql(u8, rule_token.lexeme, "accepts")) {
            accepts = try self.parseQualifiedName("expected accepted type name after 'accepts'");
            _ = self.match(.semicolon);
        } else if (std.mem.eql(u8, rule_token.lexeme, "count")) {
            count = try self.parseCountRange();
            _ = self.match(.semicolon);
        } else {
            try self.emitUnexpectedToken(
                "unknown content channel rule",
                rule_token,
                "expected 'accepts' or 'count'",
                "Channel rules are `accepts Type` and `count min..max`.",
            );
            return error.DiagnosticsEmitted;
        }
    }
    const close = try self.expect(.r_brace, "expected '}' to close channel rules", "channel rule block should end here");
    return .{
        .name = name_token.lexeme,
        .accepts = accepts,
        .count = count,
        .span = source_pkg.Span.init(name_token.span.start, close.span.end),
    };
}

// Parse a `min..max` or `min..` count range. The `..` is a dedicated `dot_dot` token.
pub fn parseCountRange(self: *Parser) !syntax.ast.CountRange {
    const min_token = try self.expect(.integer, "expected lower bound", "count ranges start with an integer lower bound");
    const min = std.fmt.parseInt(u32, min_token.lexeme, 10) catch 0;
    _ = try self.expect(.dot_dot, "expected '..' in count range", "write count ranges as `min..max` or `min..`");
    var max: ?u32 = null;
    var end = self.previous().span.end;
    if (self.at(.integer)) {
        const max_token = self.advance();
        max = std.fmt.parseInt(u32, max_token.lexeme, 10) catch null;
        end = max_token.span.end;
    }
    return .{ .min = min, .max = max, .span = source_pkg.Span.init(min_token.span.start, end) };
}

// Parse one entry of a construct `properties { ... }` schema, e.g. `required path: String`
// or `uuid: UUID = defaultUuid`. The optional `required` marker is recognized contextually:
// it is only a marker when another identifier (the property name) follows it, so a property
// may still be named `required` via `required: Type`.
pub fn parsePropertySchemaField(self: *Parser) !syntax.ast.PropertySchemaField {
    const start = self.peek().span.start;
    var required = false;
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "required") and self.peekNext().kind == .identifier) {
        _ = self.advance();
        required = true;
    }
    const name_token = try self.expect(.identifier, "expected property name", "name the property here");
    _ = try self.expect(.colon, "expected ':' after property name", "declare the property type with `name: Type`");
    const type_expr = try self.parseTypeExpr();
    var end = typeSpan(type_expr.*).end;
    var default_value: ?*syntax.ast.Expr = null;
    if (self.match(.equal)) {
        default_value = try self.parseExpression();
        end = exprSpan(default_value.?.*).end;
    }
    // Property entries are newline-separated; a trailing `;` is allowed but not required,
    // since the `properties { ... }` block is closed by `}` and each entry starts with a name.
    _ = self.match(.semicolon);
    return .{
        .required = required,
        .name = name_token.lexeme,
        .type_expr = type_expr,
        .default_value = default_value,
        .span = source_pkg.Span.init(start, end),
    };
}

// Parse a construct-backed declaration's `properties { name: value ... }` section.
pub fn parseDeclPropertiesSection(self: *Parser) !syntax.ast.DeclPropertiesSection {
    const properties_token = try self.expect(.identifier, "expected 'properties'", "declaration property sections start with 'properties'");
    _ = try self.expect(.l_brace, "expected '{' after 'properties'", "open the properties section here");
    var entries = std.array_list.Managed(syntax.ast.DeclPropertyEntry).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        const name_token = try self.expect(.identifier, "expected property name", "name the property here");
        _ = try self.expect(.colon, "expected ':' after property name", "assign the property with `name: value`");
        const value = try self.parseExpression();
        const end = exprSpan(value.*).end;
        // A trailing `;` is allowed but not required between newline-separated assignments.
        _ = self.match(.semicolon);
        try entries.append(.{
            .name = name_token.lexeme,
            .value = value,
            .span = source_pkg.Span.init(name_token.span.start, end),
        });
    }
    const close = try self.expect(.r_brace, "expected '}' to close properties section", "properties section should end here");
    return .{
        .entries = try entries.toOwnedSlice(),
        .span = source_pkg.Span.init(properties_token.span.start, close.span.end),
    };
}

pub fn parseAnnotationSpec(self: *Parser) !syntax.ast.AnnotationSpec {
    const at_token = try self.expect(.at_sign, "expected '@' in annotation spec", "annotation specs start with '@'");
    const name = try self.parseQualifiedName("expected annotation name in construct section");
    var type_expr: ?*syntax.ast.TypeExpr = null;
    var default_value: ?*syntax.ast.Expr = null;
    var end = name.span.end;
    if (self.match(.colon)) {
        type_expr = try self.parseTypeExpr();
        end = typeSpan(type_expr.?.*).end;
    }
    if (self.match(.equal)) {
        default_value = try self.parseExpression();
        end = exprSpan(default_value.?.*).end;
    }
    _ = try self.expect(.semicolon, "expected ';' after annotation spec", "terminate the annotation spec with ';'");
    return .{
        .name = name,
        .type_expr = type_expr,
        .default_value = default_value,
        .span = source_pkg.Span.init(at_token.span.start, end),
    };
}

pub fn parseConstructFormDeclWithAnnotations(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.ConstructFormDecl {
    const construct_name = try self.parseQualifiedName("expected construct name");
    const name_token = try self.expect(.identifier, "expected declaration name after construct name", "name the construct-defined declaration here");
    // Parameters are optional: `Widget App { ... }` declares a no-parameter form.
    const params = if (self.at(.l_paren)) try self.parseParamList() else try self.allocator.alloc(syntax.ast.ParamDecl, 0);
    const body = try self.parseConstructBody();
    const start = if (annotations.len > 0) annotations[0].span.start else construct_name.span.start;
    return .{
        .annotations = annotations,
        .construct_name = construct_name,
        .name = name_token.lexeme,
        .params = params,
        .body = body,
        .span = source_pkg.Span.init(start, body.span.end),
    };
}

// Parse `extend ConstructName { function modifier(...) -> ReturnType { ... } }`.
fn parseExtendDecl(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.ExtendDecl {
    const extend_token = try self.expect(.kw_extend, "expected 'extend'", "extension declarations start with 'extend'");
    const construct_name = try self.parseQualifiedName("expected the construct name to extend");
    _ = try self.expect(.l_brace, "expected '{' to start extension body", "open the extension body here");
    var members = std.array_list.Managed(syntax.ast.BodyMember).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        self.consumeDocComments();
        const member_annotations = try self.parseAnnotations();
        try members.append(try parseConstructMember(self, member_annotations));
    }
    const close = try self.expect(.r_brace, "expected '}' to close extension body", "extension body should end here");
    const start = if (annotations.len > 0) annotations[0].span.start else extend_token.span.start;
    return .{
        .annotations = annotations,
        .construct_name = construct_name,
        .members = try members.toOwnedSlice(),
        .span = source_pkg.Span.init(start, close.span.end),
    };
}

pub fn parseConstructBody(self: *Parser) !syntax.ast.ConstructBody {
    const open = try self.expect(.l_brace, "expected '{' to start declaration body", "open the declaration body here");
    var members = std.array_list.Managed(syntax.ast.BodyMember).init(self.allocator);
    while (!self.at(.r_brace) and !self.at(.eof)) {
        self.consumeDocComments();
        const annotations = try self.parseAnnotations();
        try members.append(try self.parseBodyMember(annotations));
    }
    const close = try self.expect(.r_brace, "expected '}' to close declaration body", "declaration body should end here");
    return .{
        .members = try members.toOwnedSlice(),
        .span = source_pkg.Span.init(open.span.start, close.span.end),
    };
}

pub fn parseBodyMember(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.BodyMember {
    const is_override = self.match(.kw_override);
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "static")) {
        try self.emitUnexpectedToken(
            "removed static keyword",
            self.peek(),
            "`static` has been removed and is not valid Kira syntax",
            "Use `let` for immutable members and `var` for mutable members.",
        );
        return error.DiagnosticsEmitted;
    }
    if (self.at(.kw_let) or self.at(.kw_var)) return .{ .field_decl = try self.parseFieldDecl(annotations, is_override) };
    if (self.at(.kw_function)) return .{ .function_decl = try self.parseFunctionDeclWithAnnotations(annotations, is_override, false) };
    if (is_override) {
        try self.emitUnexpectedToken(
            "expected override member declaration",
            self.peek(),
            "override must apply to a field or function declaration",
            "Use `override function ...`, `override let ...`, or `override var ...` inside a type body.",
        );
        return error.DiagnosticsEmitted;
    }
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "content") and self.peekNext().kind == .l_brace) {
        return .{ .content_section = try self.parseContentSection(annotations) };
    }
    if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "properties") and self.peekNext().kind == .l_brace) {
        return .{ .properties_section = try self.parseDeclPropertiesSection() };
    }
    if (self.isLifecycleHookStart()) return .{ .lifecycle_hook = try self.parseLifecycleHook() };
    return .{ .named_rule = try self.parseNamedRule() };
}

pub fn parseFieldDecl(self: *Parser, annotations: []const syntax.ast.Annotation, is_override: bool) !syntax.ast.FieldDecl {
    const storage_token = if (self.at(.kw_let) or self.at(.kw_var))
        self.advance()
    else
        try self.expect(.kw_let, "expected field declaration", "field declarations use 'let' or 'var'");
    const name_token = try self.expect(.identifier, "expected field name", "name the field here");
    var type_expr: ?*syntax.ast.TypeExpr = null;
    var value: ?*syntax.ast.Expr = null;
    var body: ?syntax.ast.Block = null;
    var end = name_token.span.end;
    if (self.match(.colon)) {
        type_expr = try self.parseTypeExpr();
        end = typeSpan(type_expr.?.*).end;
    }
    // A block-bodied computed member, `let node: Node { body.node }`, replaces a stored value
    // with a composition body. It needs no field terminator; the block's `}` ends it.
    if (self.at(.l_brace)) {
        body = try self.parseBlock();
        end = body.?.span.end;
    } else {
        if (self.match(.equal)) {
            value = try self.parseExpression();
            end = exprSpan(value.?.*).end;
        }
        end = try self.consumeFieldTerminator(end);
    }
    return .{
        .annotations = annotations,
        .is_override = is_override,
        .storage = switch (storage_token.kind) {
            .kw_let => .immutable,
            .kw_var => .mutable,
            else => unreachable,
        },
        .name = name_token.lexeme,
        .type_expr = type_expr,
        .value = value,
        .body = body,
        .span = source_pkg.Span.init(if (annotations.len > 0) annotations[0].span.start else storage_token.span.start, end),
    };
}

pub fn parseContentSection(self: *Parser, annotations: []const syntax.ast.Annotation) !syntax.ast.ContentSection {
    const content_token = try self.expect(.identifier, "expected 'content'", "content sections start with 'content'");
    const builder = try self.parseBuilderBlock();
    return .{
        .annotations = annotations,
        .builder = builder,
        .span = source_pkg.Span.init(if (annotations.len > 0) annotations[0].span.start else content_token.span.start, builder.span.end),
    };
}

pub fn parseLifecycleHook(self: *Parser) !syntax.ast.LifecycleHook {
    const name_token = try self.expect(.identifier, "expected lifecycle hook name", "write the lifecycle hook name here");
    _ = try self.expect(.l_paren, "expected '(' after lifecycle hook name", "open the lifecycle hook arguments here");
    var args = std.array_list.Managed(syntax.ast.RuleArg).init(self.allocator);
    while (!self.at(.r_paren) and !self.at(.eof)) {
        const start_token = self.peek();
        var label: ?[]const u8 = null;
        var value: ?*syntax.ast.Expr = null;
        if (self.at(.identifier) and self.peekNext().kind == .colon) {
            label = self.advance().lexeme;
            _ = self.advance();
            value = try self.parseExpression();
        } else if (!self.at(.r_paren)) {
            value = try self.parseExpression();
        }
        try args.append(.{
            .label = label,
            .value = value,
            .span = source_pkg.Span.init(start_token.span.start, if (value) |expr| exprSpan(expr.*).end else start_token.span.end),
        });
        if (!self.match(.comma)) break;
    }
    _ = try self.expect(.r_paren, "expected ')' after lifecycle hook arguments", "close the lifecycle hook arguments here");
    const body = try self.parseBlock();
    return .{
        .name = name_token.lexeme,
        .args = try args.toOwnedSlice(),
        .body = body,
        .span = source_pkg.Span.init(name_token.span.start, body.span.end),
    };
}

pub fn parseNamedRule(self: *Parser) !syntax.ast.NamedRule {
    const name = try self.parseQualifiedName("expected rule name");
    var args = std.array_list.Managed(syntax.ast.RuleArg).init(self.allocator);
    var type_expr: ?*syntax.ast.TypeExpr = null;
    var value: ?*syntax.ast.Expr = null;
    var block: ?syntax.ast.Block = null;
    var end = name.span.end;

    if (self.match(.l_paren)) {
        while (!self.at(.r_paren) and !self.at(.eof)) {
            const start_token = self.peek();
            var label: ?[]const u8 = null;
            var arg_value: ?*syntax.ast.Expr = null;
            if (self.at(.identifier) and self.peekNext().kind == .colon) {
                label = self.advance().lexeme;
                _ = self.advance();
                arg_value = try self.parseExpression();
            } else if (!self.at(.r_paren)) {
                arg_value = try self.parseExpression();
            }
            try args.append(.{
                .label = label,
                .value = arg_value,
                .span = source_pkg.Span.init(start_token.span.start, if (arg_value) |expr| exprSpan(expr.*).end else start_token.span.end),
            });
            if (!self.match(.comma)) break;
        }
        const close = try self.expect(.r_paren, "expected ')' after rule arguments", "close the rule arguments here");
        end = close.span.end;
    }

    if (self.match(.colon)) {
        type_expr = try self.parseTypeExpr();
        end = typeSpan(type_expr.?.*).end;
    }

    if (self.match(.equal)) {
        value = try self.parseExpression();
        end = exprSpan(value.?.*).end;
    }

    if (self.at(.l_brace)) {
        block = try self.parseBlock();
        end = block.?.span.end;
    } else {
        _ = try self.expect(.semicolon, "expected ';' after rule", "terminate the rule with ';'");
    }

    return .{
        .name = name,
        .args = try args.toOwnedSlice(),
        .type_expr = type_expr,
        .value = value,
        .block = block,
        .span = source_pkg.Span.init(name.span.start, end),
    };
}
