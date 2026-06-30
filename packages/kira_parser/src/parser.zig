const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");

pub fn parse(allocator: std.mem.Allocator, tokens: []const syntax.Token, out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic)) !syntax.ast.Program {
    const default_source_path = if (tokens.len > 0) tokens[0].span.source_path else null;
    const previous_source_path = source_pkg.Span.setDefaultSourcePath(default_source_path);
    defer _ = source_pkg.Span.setDefaultSourcePath(previous_source_path);
    var parser = Parser{
        .allocator = allocator,
        .tokens = tokens,
        .diagnostics = out_diagnostics,
    };
    return parser.parseProgram();
}

pub const Parser = struct {
    allocator: std.mem.Allocator,
    tokens: []const syntax.Token,
    index: usize = 0,
    allow_trailing_block_call: bool = true,
    diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
    // Bounds expression-parsing recursion. The recursive-descent parser otherwise
    // overflows the native stack and SIGSEGVs on pathologically deep nesting
    // (e.g. ~1100 nested parens / calls / arrays / closures); past this depth we
    // emit a clean diagnostic instead. Far above any realistic expression nesting.
    expr_depth: u32 = 0,
    pub const max_expr_depth: u32 = 256;
    const decl_impl = @import("parser_decls.zig");
    const statement_impl = @import("parser_statements.zig");
    const block_impl = @import("parser_blocks.zig");
    const param_impl = @import("parser_params.zig");
    const type_expr_impl = @import("parser_types_exprs.zig");
    const macro_impl = @import("parser_macros.zig");

    pub const parseTopLevelDecl = decl_impl.parseTopLevelDecl;
    pub const parseAnnotationDecl = decl_impl.parseAnnotationDecl;
    pub const parseAnnotationDeclWithAnnotations = decl_impl.parseAnnotationDeclWithAnnotations;
    pub const parseCapabilityDecl = decl_impl.parseCapabilityDecl;
    pub const parseEnumDecl = decl_impl.parseEnumDecl;
    pub const parseEnumDeclWithAnnotations = decl_impl.parseEnumDeclWithAnnotations;
    pub const parseAnnotationTarget = decl_impl.parseAnnotationTarget;
    pub const parseGeneratedBlock = decl_impl.parseGeneratedBlock;
    pub const parseGeneratedMember = decl_impl.parseGeneratedMember;
    pub const parseAnnotationParameterDecl = decl_impl.parseAnnotationParameterDecl;
    pub const parseAnnotations = decl_impl.parseAnnotations;
    pub const parseAnnotationBlock = decl_impl.parseAnnotationBlock;
    pub const parseFunctionDeclWithAnnotations = decl_impl.parseFunctionDeclWithAnnotations;
    pub const parseFunctionSignature = decl_impl.parseFunctionSignature;
    pub const parseOptionalReturnType = decl_impl.parseOptionalReturnType;
    pub const parseParamList = param_impl.parseParamList;
    pub const parseTypeDeclWithAnnotations = decl_impl.parseTypeDeclWithAnnotations;
    pub const parseConstructDeclWithAnnotations = decl_impl.parseConstructDeclWithAnnotations;
    pub const parseConstructSection = decl_impl.parseConstructSection;
    pub const parseConstructContentSection = decl_impl.parseConstructContentSection;
    pub const parseContentChannel = decl_impl.parseContentChannel;
    pub const parseContentProjection = decl_impl.parseContentProjection;
    pub const parseCountRange = decl_impl.parseCountRange;
    pub const parsePropertySchemaField = decl_impl.parsePropertySchemaField;
    pub const parseDeclPropertiesSection = decl_impl.parseDeclPropertiesSection;
    pub const parseAnnotationSpec = decl_impl.parseAnnotationSpec;
    pub const parseConstructFormDeclWithAnnotations = decl_impl.parseConstructFormDeclWithAnnotations;
    pub const parseDeclarativeMacroDecl = macro_impl.parseDeclarativeMacroDecl;
    pub const parseProceduralMacroDecl = macro_impl.parseProceduralMacroDecl;
    pub const parseQuoteExpr = macro_impl.parseQuoteExpr;
    pub const parseConstructBody = decl_impl.parseConstructBody;
    pub const parseBodyMember = decl_impl.parseBodyMember;
    pub const parseFieldDecl = decl_impl.parseFieldDecl;
    pub const parseContentSection = decl_impl.parseContentSection;
    pub const parseLifecycleHook = decl_impl.parseLifecycleHook;
    pub const parseNamedRule = decl_impl.parseNamedRule;

    pub const parseBlock = statement_impl.parseBlock;
    pub const parseStatement = statement_impl.parseStatement;
    pub const finishIfStatement = statement_impl.finishIfStatement;
    pub const finishForStatement = statement_impl.finishForStatement;
    pub const finishWhileStatement = statement_impl.finishWhileStatement;
    pub const finishMatchStatement = statement_impl.finishMatchStatement;
    pub const finishSwitchStatement = statement_impl.finishSwitchStatement;
    pub const finishAttemptStatement = statement_impl.finishAttemptStatement;

    pub const parseBuilderBlock = block_impl.parseBuilderBlock;
    pub const looksLikeCallbackBlock = block_impl.looksLikeCallbackBlock;
    pub const looksLikeCallbackBlockMissingIn = block_impl.looksLikeCallbackBlockMissingIn;
    pub const parseCallbackBlock = block_impl.parseCallbackBlock;
    pub const parseBuilderItem = block_impl.parseBuilderItem;
    pub const parseBuilderIfItem = block_impl.parseBuilderIfItem;

    pub const parseTypeExpr = type_expr_impl.parseTypeExpr;
    pub const parseExpression = type_expr_impl.parseExpression;
    pub const parseExpressionWithoutTrailingBlockCall = type_expr_impl.parseExpressionWithoutTrailingBlockCall;
    pub const makeIdentifierExpr = type_expr_impl.makeIdentifierExpr;
    pub const looksLikeStructLiteral = type_expr_impl.looksLikeStructLiteral;
    pub const parseStructLiteral = type_expr_impl.parseStructLiteral;
    pub const qualifiedNameFromExpr = type_expr_impl.qualifiedNameFromExpr;
    pub const parseConditional = type_expr_impl.parseConditional;
    pub const parseLogicalOr = type_expr_impl.parseLogicalOr;
    pub const parseLogicalAnd = type_expr_impl.parseLogicalAnd;
    pub const parseEquality = type_expr_impl.parseEquality;
    pub const parseComparison = type_expr_impl.parseComparison;
    pub const parseTerm = type_expr_impl.parseTerm;
    pub const parseFactor = type_expr_impl.parseFactor;
    pub const parseUnary = type_expr_impl.parseUnary;
    pub const parsePostfix = type_expr_impl.parsePostfix;
    pub const parsePrimary = type_expr_impl.parsePrimary;

    pub fn parseProgram(self: *Parser) !syntax.ast.Program {
        var imports = std.array_list.Managed(syntax.ast.ImportDecl).init(self.allocator);
        var decl_list = std.array_list.Managed(syntax.ast.Decl).init(self.allocator);
        var functions = std.array_list.Managed(syntax.ast.FunctionDecl).init(self.allocator);
        var had_errors = false;

        while (!self.at(.eof)) {
            if (self.at(.kw_import)) {
                const import_decl = self.parseImportDecl() catch |err| switch (err) {
                    error.DiagnosticsEmitted => blk: {
                        had_errors = true;
                        self.recoverToTopLevel();
                        break :blk null;
                    },
                    else => return err,
                };
                if (import_decl) |value| try imports.append(value);
                continue;
            }

            self.consumeDocComments();
            const annotations = self.parseAnnotations() catch |err| switch (err) {
                error.DiagnosticsEmitted => blk: {
                    had_errors = true;
                    self.recoverToTopLevel();
                    break :blk &.{};
                },
                else => return err,
            };

            const decl = self.parseTopLevelDecl(annotations) catch |err| switch (err) {
                error.DiagnosticsEmitted => blk: {
                    had_errors = true;
                    self.recoverToTopLevel();
                    break :blk null;
                },
                else => return err,
            };

            if (decl) |value| {
                switch (value) {
                    .function_decl => |function_decl| try functions.append(function_decl),
                    else => {},
                }
                try decl_list.append(value);
            }
        }

        if (had_errors) return error.DiagnosticsEmitted;
        return .{
            .imports = try imports.toOwnedSlice(),
            .decls = try decl_list.toOwnedSlice(),
            .functions = try functions.toOwnedSlice(),
        };
    }

    pub fn parseImportDecl(self: *Parser) !?syntax.ast.ImportDecl {
        const import_token = try self.expect(.kw_import, "expected 'import'", "imports start with 'import'");
        const module_name = try self.parseQualifiedName("expected module name after 'import'");
        var alias: ?[]const u8 = null;
        var end = module_name.span.end;
        if (self.match(.kw_as)) {
            const alias_token = try self.expect(.identifier, "expected alias after 'as'", "write the import alias here");
            alias = alias_token.lexeme;
            end = alias_token.span.end;
        }
        return .{
            .module_name = module_name,
            .alias = alias,
            .span = source_pkg.Span.init(import_token.span.start, end),
        };
    }

    pub fn parseQualifiedName(self: *Parser, title: []const u8) !syntax.ast.QualifiedName {
        const first = try self.expect(.identifier, title, "write an identifier here");
        var segments = std.array_list.Managed(syntax.ast.NameSegment).init(self.allocator);
        try segments.append(.{ .text = first.lexeme, .span = first.span });
        var end = first.span.end;
        while (self.match(.dot)) {
            const next = try self.expect(.identifier, "expected identifier after '.'", "write an identifier here");
            try segments.append(.{ .text = next.lexeme, .span = next.span });
            end = next.span.end;
        }
        return .{
            .segments = try segments.toOwnedSlice(),
            .span = source_pkg.Span.init(first.span.start, end),
        };
    }

    pub fn makeSingleSegmentName(self: *Parser, token: syntax.Token) !syntax.ast.QualifiedName {
        const segments = try self.allocator.alloc(syntax.ast.NameSegment, 1);
        segments[0] = .{ .text = token.lexeme, .span = token.span };
        return .{
            .segments = segments,
            .span = token.span,
        };
    }

    pub fn cloneQualifiedName(allocator: std.mem.Allocator, name: syntax.ast.QualifiedName) !syntax.ast.QualifiedName {
        const segments = try allocator.alloc(syntax.ast.NameSegment, name.segments.len);
        @memcpy(segments, name.segments);
        return .{
            .segments = segments,
            .span = name.span,
        };
    }

    pub fn makeBinaryExpr(self: *Parser, operator: syntax.Token, lhs: *syntax.ast.Expr, rhs: *syntax.ast.Expr) !*syntax.ast.Expr {
        const node = try self.allocator.create(syntax.ast.Expr);
        node.* = .{ .binary = .{
            .op = switch (operator.kind) {
                .plus => .add,
                .minus => .subtract,
                .star => .multiply,
                .slash => .divide,
                .percent => .modulo,
                .equal_equal => .equal,
                .bang_equal => .not_equal,
                .less => .less,
                .less_equal => .less_equal,
                .greater => .greater,
                .greater_equal => .greater_equal,
                .amp_amp => .logical_and,
                .pipe_pipe => .logical_or,
                else => unreachable,
            },
            .lhs = lhs,
            .rhs = rhs,
            .span = source_pkg.Span.init(exprSpan(lhs.*).start, exprSpan(rhs.*).end),
        } };
        return node;
    }

    pub fn consumeFieldTerminator(self: *Parser, fallback_end: usize) !usize {
        if (self.match(.semicolon)) return self.previous().span.end;
        if (self.at(.r_brace) or self.at(.eof) or self.at(.doc_comment) or self.at(.at_sign) or self.at(.kw_function) or self.at(.kw_let) or self.at(.kw_var) or self.at(.kw_override) or self.isLifecycleHookStart()) {
            return fallback_end;
        }
        if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "content") and self.peekNext().kind == .l_brace) return fallback_end;
        if (self.at(.identifier) and std.mem.eql(u8, self.peek().lexeme, "body") and self.peekNext().kind == .l_brace) return fallback_end;
        return (try self.expect(.semicolon, "expected ';' after field declaration", "terminate the field declaration with ';'")).span.end;
    }

    pub fn consumeStatementTerminator(self: *Parser, fallback_end: usize, title: []const u8, label_message: []const u8) !usize {
        if (self.match(.semicolon)) return self.previous().span.end;
        if (self.isStatementBoundary()) return fallback_end;
        return (try self.expect(.semicolon, title, label_message)).span.end;
    }

    pub fn isStatementBoundary(self: *Parser) bool {
        return self.at(.r_brace) or self.at(.eof) or self.at(.at_sign) or self.at(.kw_let) or self.at(.kw_var) or self.at(.kw_return) or self.at(.kw_if) or self.at(.kw_for) or self.at(.kw_while) or self.at(.kw_match) or self.at(.kw_switch) or
            self.at(.identifier) or self.at(.integer) or self.at(.float) or self.at(.string) or self.at(.kw_true) or self.at(.kw_false) or self.at(.l_paren) or self.at(.l_bracket) or self.at(.bang) or self.at(.minus);
    }

    pub fn looksLikeConstructFormDecl(self: *Parser) bool {
        if (!self.at(.identifier)) return false;
        var cursor = self.index;
        cursor += 1;
        while (cursor + 1 < self.tokens.len and self.tokens[cursor].kind == .dot and self.tokens[cursor + 1].kind == .identifier) {
            cursor += 2;
        }
        // A construct-defined declaration form is `ConstructName DeclName ( ... ) { ... }`
        // or, when it takes no parameters, `ConstructName DeclName { ... }`.
        return cursor + 1 < self.tokens.len and self.tokens[cursor].kind == .identifier and
            (self.tokens[cursor + 1].kind == .l_paren or self.tokens[cursor + 1].kind == .l_brace);
    }

    pub fn isLifecycleHookStart(self: *Parser) bool {
        return self.at(.identifier) and self.peekNext().kind == .l_paren and
            (std.mem.eql(u8, self.peek().lexeme, "onAppear") or
                std.mem.eql(u8, self.peek().lexeme, "onDisappear") or
                std.mem.eql(u8, self.peek().lexeme, "onChange"));
    }

    pub fn expect(self: *Parser, kind: syntax.TokenKind, title: []const u8, label_message: []const u8) !syntax.Token {
        if (self.at(kind)) return self.advance();
        const actual = self.peek();
        const detail = try std.fmt.allocPrint(
            self.allocator,
            "Kira expected {s}, but found {s}.",
            .{ tokenDescription(kind), tokenDescription(actual.kind) },
        );
        try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
            .severity = .@"error",
            .code = "KPAR001",
            .title = title,
            .message = detail,
            .labels = &.{
                diagnostics.primaryLabel(actual.span, label_message),
            },
            .help = expectedTokenHelp(kind),
        });
        return error.DiagnosticsEmitted;
    }

    pub fn emitUnexpectedToken(self: *Parser, title: []const u8, token: syntax.Token, label_message: []const u8, help: ?[]const u8) !void {
        const detail = try std.fmt.allocPrint(
            self.allocator,
            "Kira expected different syntax here, but found {s}.",
            .{tokenDescription(token.kind)},
        );
        try diagnostics.appendOwned(self.allocator, self.diagnostics, .{
            .severity = .@"error",
            .code = "KPAR005",
            .title = title,
            .message = detail,
            .labels = &.{
                diagnostics.primaryLabel(token.span, label_message),
            },
            .help = help,
        });
    }

    pub fn recoverToStatementBoundary(self: *Parser) void {
        if (self.at(.semicolon)) {
            _ = self.advance();
            return;
        }
        while (!self.at(.semicolon) and !self.at(.r_brace) and !self.at(.eof)) _ = self.advance();
        if (self.at(.semicolon)) _ = self.advance();
    }

    pub fn recoverToTopLevel(self: *Parser) void {
        if (!self.at(.eof)) _ = self.advance();
        while (!self.at(.eof) and !self.at(.kw_import) and !self.at(.doc_comment) and !self.at(.kw_annotation) and !self.at(.kw_capability) and !self.at(.kw_class) and !self.at(.kw_comptime) and !self.at(.kw_enum) and !self.at(.kw_struct) and !self.at(.kw_function) and !self.at(.kw_type) and !self.at(.kw_construct) and !self.at(.at_sign) and !self.looksLikeConstructFormDecl()) {
            _ = self.advance();
        }
    }

    pub fn consumeDocComments(self: *Parser) void {
        while (self.at(.doc_comment)) _ = self.advance();
    }

    pub fn match(self: *Parser, kind: syntax.TokenKind) bool {
        if (!self.at(kind)) return false;
        _ = self.advance();
        return true;
    }

    pub fn at(self: *Parser, kind: syntax.TokenKind) bool {
        return self.peek().kind == kind;
    }

    pub fn peek(self: *Parser) syntax.Token {
        return self.tokens[self.index];
    }

    pub fn peekNext(self: *Parser) syntax.Token {
        if (self.index + 1 >= self.tokens.len) return self.tokens[self.tokens.len - 1];
        return self.tokens[self.index + 1];
    }

    pub fn peekAhead(self: *Parser, offset: usize) syntax.Token {
        const target = self.index + offset;
        if (target >= self.tokens.len) return self.tokens[self.tokens.len - 1];
        return self.tokens[target];
    }

    pub fn previous(self: *Parser) syntax.Token {
        return self.tokens[self.index - 1];
    }

    pub fn advance(self: *Parser) syntax.Token {
        const token = self.tokens[self.index];
        if (self.index < self.tokens.len - 1) self.index += 1;
        return token;
    }
};

pub fn exprSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        .integer => |node| node.span,
        .float => |node| node.span,
        .string => |node| node.span,
        .bool => |node| node.span,
        .identifier => |node| node.span,
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
        .member => |node| node.span,
        .index => |node| node.span,
        .call => |node| node.span,
        .quote => |node| node.span,
        .try_expr => |node| node.span,
    };
}

pub fn typeSpan(ty: syntax.ast.TypeExpr) source_pkg.Span {
    return switch (ty) {
        .named => |node| node.span,
        .generic => |node| node.span,
        .ownership => |node| node.span,
        .any => |node| node.span,
        .array => |node| node.span,
        .function => |node| node.span,
    };
}

pub fn paramsEnd(params: []const syntax.ast.ParamDecl, fallback: usize) usize {
    if (params.len == 0) return fallback + 2;
    return params[params.len - 1].span.end + 1;
}

pub fn sectionKind(name: []const u8) syntax.ast.ConstructSectionKind {
    if (std.mem.eql(u8, name, "annotations")) return .annotations;
    if (std.mem.eql(u8, name, "wrappers")) return .modifiers;
    if (std.mem.eql(u8, name, "modifiers")) return .modifiers;
    if (std.mem.eql(u8, name, "requires")) return .requires;
    if (std.mem.eql(u8, name, "lifecycle")) return .lifecycle;
    if (std.mem.eql(u8, name, "builder")) return .builder;
    if (std.mem.eql(u8, name, "representation")) return .representation;
    if (std.mem.eql(u8, name, "properties")) return .properties;
    return .custom;
}

pub fn tokenDescription(kind: syntax.TokenKind) []const u8 {
    return switch (kind) {
        .eof => "the end of the file",
        .identifier => "an identifier",
        .integer => "an integer literal",
        .float => "a float literal",
        .string => "a string literal",
        .doc_comment => "a documentation comment",
        .kw_annotation => "'annotation'",
        .kw_capability => "'capability'",
        .kw_class => "'class'",
        .kw_comptime => "'comptime'",
        .kw_macro => "'macro'",
        .kw_quote => "'quote'",
        .kw_construct => "'construct'",
        .kw_enum => "'enum'",
        .kw_struct => "'struct'",
        .kw_type => "'type'",
        .kw_extends => "'extends'",
        .kw_extend => "'extend'",
        .kw_attempt => "'attempt'",
        .kw_try => "'try'",
        .kw_self_type => "'Self'",
        .kw_function => "'function'",
        .kw_generated => "'generated'",
        .kw_override => "'override'",
        .kw_overridable => "'overridable'",
        .kw_targets => "'targets'",
        .kw_uses => "'uses'",
        .kw_let => "'let'",
        .kw_var => "'var'",
        .kw_return => "'return'",
        .kw_import => "'import'",
        .kw_as => "'as'",
        .kw_if => "'if'",
        .kw_else => "'else'",
        .kw_for => "'for'",
        .kw_in => "'in'",
        .kw_while => "'while'",
        .kw_break => "'break'",
        .kw_continue => "'continue'",
        .kw_match => "'match'",
        .kw_switch => "'switch'",
        .kw_case => "'case'",
        .kw_default => "'default'",
        .kw_true => "'true'",
        .kw_false => "'false'",
        .at_sign => "'@'",
        .dollar => "'$'",
        .hash_brace => "'#{'",
        .l_paren => "'('",
        .r_paren => "')'",
        .l_brace => "'{'",
        .r_brace => "'}'",
        .l_bracket => "'['",
        .r_bracket => "']'",
        .semicolon => "';'",
        .comma => "','",
        .colon => "':'",
        .question => "'?'",
        .equal => "'='",
        .equal_equal => "'=='",
        .bang => "'!'",
        .bang_equal => "'!='",
        .amp_amp => "'&&'",
        .pipe_pipe => "'||'",
        .plus => "'+'",
        .minus => "'-'",
        .arrow => "'->'",
        .star => "'*'",
        .slash => "'/'",
        .percent => "'%'",
        .dot => "'.'",
        .dot_dot => "'..'",
        .less => "'<'",
        .less_equal => "'<='",
        .greater => "'>'",
        .greater_equal => "'>='",
    };
}

pub fn unexpectedTokenLabel(kind: syntax.TokenKind) []const u8 {
    return switch (kind) {
        .eof => "the file ends here",
        else => "unexpected token here",
    };
}

pub fn expectedTokenHelp(kind: syntax.TokenKind) ?[]const u8 {
    return switch (kind) {
        .semicolon => "Add ';' to end the current construct.",
        .r_brace => "Close the current block with '}'.",
        .r_paren => "Close the current list with ')'.",
        .r_bracket => "Close the array or array type with ']'.",
        .l_brace => "Start the block or body with '{'.",
        .l_paren => "Open the parameter or argument list with '('.",
        .identifier => "Insert a valid Kira identifier here.",
        else => null,
    };
}

pub fn parseSource(
    allocator: std.mem.Allocator,
    text: []const u8,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    const lexer = @import("kira_lexer");
    const source = try source_pkg.SourceFile.initOwned(allocator, "test.kira", text);
    const tokens = try lexer.tokenize(allocator, &source, diags);
    return parse(allocator, tokens, diags);
}

pub fn readRepoFileForTest(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    const repo_root = try findRepoRootForTest(allocator) orelse return error.FileNotFound;
    defer allocator.free(repo_root);
    const full_path = try std.fs.path.join(allocator, &.{ repo_root, path });
    defer allocator.free(full_path);
    return std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, full_path, allocator, .limited(std.math.maxInt(usize)));
}

pub fn findRepoRootForTest(allocator: std.mem.Allocator) !?[]u8 {
    const exe_path = try std.process.executablePathAlloc(std.Options.debug_io, allocator);
    defer allocator.free(exe_path);
    var current = try allocator.dupe(u8, std.fs.path.dirname(exe_path) orelse ".");
    errdefer allocator.free(current);

    while (true) {
        const build_path = try std.fs.path.join(allocator, &.{ current, "build.zig" });
        defer allocator.free(build_path);
        if (fileExistsForTest(build_path)) return current;

        const parent = std.fs.path.dirname(current) orelse break;
        if (std.mem.eql(u8, parent, current)) break;
        const copy = try allocator.dupe(u8, parent);
        allocator.free(current);
        current = copy;
    }

    allocator.free(current);
    return null;
}

pub fn fileExistsForTest(path: []const u8) bool {
    var file = std.Io.Dir.openFileAbsolute(std.Options.debug_io, path, .{}) catch std.Io.Dir.cwd().openFile(std.Options.debug_io, path, .{}) catch return false;
    file.close(std.Options.debug_io);
    return true;
}

test "parses imports functions and construct declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "import UI as Kit\n" ++
            "/// demo\n" ++
            "construct Widget { annotations { @State; } requires { function render() } lifecycle { onAppear() {} } }\n" ++
            "Widget Button(title: String) { @State let count: Int = 0; content { Text(title) } }\n" ++
            "@Main function entry(): Int { let x: Float = 12; print(x); return 0; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 1), program.imports.len);
    try std.testing.expectEqual(@as(usize, 3), program.decls.len);
    try std.testing.expectEqual(@as(usize, 1), program.functions.len);
}

test "reports removed declaration syntax diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = parseSource(allocator, "type OldShape { let value: Int = 0 }", &diags);
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("removed type declaration syntax", diags.items[0].title);
    }
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = parseSource(allocator, "@Doc(\"old\")\nstruct Shape { let value: Int = 0 }", &diags);
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("removed @Doc annotation", diags.items[0].title);
    }
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = parseSource(allocator, "struct Shape { static let zero: Int = 0 }", &diags);
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("removed static keyword", diags.items[0].title);
    }
}

test "parses annotation declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "annotation State { }\n" ++
            "annotation Attribute { parameters { index: Int } }\n" ++
            "annotation InputMapping { parameters { priority: Int = 0 blocksLowerPriorityMappings: Bool = false } }\n",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 3), program.decls.len);
    try std.testing.expectEqualStrings("State", program.decls[0].annotation_decl.name);
    try std.testing.expectEqual(@as(usize, 0), program.decls[0].annotation_decl.parameters.len);
    try std.testing.expectEqualStrings("Attribute", program.decls[1].annotation_decl.name);
    try std.testing.expectEqual(@as(usize, 1), program.decls[1].annotation_decl.parameters.len);
    try std.testing.expectEqualStrings("index", program.decls[1].annotation_decl.parameters[0].name);
    try std.testing.expectEqual(@as(usize, 2), program.decls[2].annotation_decl.parameters.len);
    try std.testing.expect(program.decls[2].annotation_decl.parameters[0].default_value != null);
    try std.testing.expect(program.decls[2].annotation_decl.parameters[1].default_value != null);
}

test {
    _ = @import("parser_tests.zig");
    _ = @import("parser_app_surface_tests.zig");
}
