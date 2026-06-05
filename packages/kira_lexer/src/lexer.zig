const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");

pub fn tokenize(allocator: std.mem.Allocator, source: *const source_pkg.SourceFile, out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic)) ![]syntax.Token {
    const previous_source_path = source_pkg.Span.setDefaultSourcePath(source.path);
    defer _ = source_pkg.Span.setDefaultSourcePath(previous_source_path);
    var tokens = std.array_list.Managed(syntax.Token).init(allocator);
    var index: usize = 0;

    while (index < source.text.len) {
        const byte = source.text[index];
        switch (byte) {
            ' ', '\t', '\r', '\n' => index += 1,
            '/' => {
                if (index + 1 < source.text.len and source.text[index + 1] == '/') {
                    const start = index;
                    const is_doc_comment = index + 2 < source.text.len and source.text[index + 2] == '/';
                    index += if (is_doc_comment) 3 else 2;
                    const content_start = index;
                    while (index < source.text.len and source.text[index] != '\n') : (index += 1) {}
                    if (is_doc_comment) {
                        var content = source.text[content_start..index];
                        if (content.len > 0 and content[0] == ' ') content = content[1..];
                        try tokens.append(makeToken(.doc_comment, content, start, index));
                    }
                } else {
                    try tokens.append(makeToken(.slash, source.text[index .. index + 1], index, index + 1));
                    index += 1;
                }
            },
            '@' => {
                try tokens.append(makeToken(.at_sign, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '$' => {
                try tokens.append(makeToken(.dollar, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '(' => {
                try tokens.append(makeToken(.l_paren, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            ')' => {
                try tokens.append(makeToken(.r_paren, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '{' => {
                try tokens.append(makeToken(.l_brace, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '}' => {
                try tokens.append(makeToken(.r_brace, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '[' => {
                try tokens.append(makeToken(.l_bracket, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            ']' => {
                try tokens.append(makeToken(.r_bracket, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            ';' => {
                try tokens.append(makeToken(.semicolon, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            ',' => {
                try tokens.append(makeToken(.comma, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            ':' => {
                try tokens.append(makeToken(.colon, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '?' => {
                try tokens.append(makeToken(.question, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '.' => {
                try tokens.append(makeToken(.dot, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '+' => {
                try tokens.append(makeToken(.plus, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '-' => {
                if (peekByte(source.text, index + 1) == '>') {
                    try tokens.append(makeToken(.arrow, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try tokens.append(makeToken(.minus, source.text[index .. index + 1], index, index + 1));
                    index += 1;
                }
            },
            '*' => {
                try tokens.append(makeToken(.star, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '&' => {
                if (peekByte(source.text, index + 1) == '&') {
                    try tokens.append(makeToken(.amp_amp, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try diagnostics.appendOwned(allocator, out_diagnostics, .{
                        .severity = .@"error",
                        .code = "KLEX001",
                        .title = "unexpected character",
                        .message = "Kira found a character that does not belong to the current grammar.",
                        .labels = &.{
                            diagnostics.primaryLabel(source_pkg.Span.init(index, index + 1), "this character is not valid here"),
                        },
                        .help = "Remove the character or replace it with valid Kira syntax.",
                    });
                    return error.DiagnosticsEmitted;
                }
            },
            '|' => {
                if (peekByte(source.text, index + 1) == '|') {
                    try tokens.append(makeToken(.pipe_pipe, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try diagnostics.appendOwned(allocator, out_diagnostics, .{
                        .severity = .@"error",
                        .code = "KLEX001",
                        .title = "unexpected character",
                        .message = "Kira found a character that does not belong to the current grammar.",
                        .labels = &.{
                            diagnostics.primaryLabel(source_pkg.Span.init(index, index + 1), "this character is not valid here"),
                        },
                        .help = "Remove the character or replace it with valid Kira syntax.",
                    });
                    return error.DiagnosticsEmitted;
                }
            },
            '%' => {
                try tokens.append(makeToken(.percent, source.text[index .. index + 1], index, index + 1));
                index += 1;
            },
            '=' => {
                if (peekByte(source.text, index + 1) == '=') {
                    try tokens.append(makeToken(.equal_equal, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try tokens.append(makeToken(.equal, source.text[index .. index + 1], index, index + 1));
                    index += 1;
                }
            },
            '!' => {
                if (peekByte(source.text, index + 1) == '=') {
                    try tokens.append(makeToken(.bang_equal, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try tokens.append(makeToken(.bang, source.text[index .. index + 1], index, index + 1));
                    index += 1;
                }
            },
            '<' => {
                if (peekByte(source.text, index + 1) == '=') {
                    try tokens.append(makeToken(.less_equal, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try tokens.append(makeToken(.less, source.text[index .. index + 1], index, index + 1));
                    index += 1;
                }
            },
            '>' => {
                if (peekByte(source.text, index + 1) == '=') {
                    try tokens.append(makeToken(.greater_equal, source.text[index .. index + 2], index, index + 2));
                    index += 2;
                } else {
                    try tokens.append(makeToken(.greater, source.text[index .. index + 1], index, index + 1));
                    index += 1;
                }
            },
            '"' => {
                const start = index;
                index += 1;
                while (index < source.text.len) : (index += 1) {
                    if (source.text[index] == '\\') {
                        if (index + 1 < source.text.len) {
                            index += 1;
                            continue;
                        }
                    }
                    if (source.text[index] == '"') break;
                }
                if (index >= source.text.len or source.text[index] != '"') {
                    try diagnostics.appendOwned(allocator, out_diagnostics, .{
                        .severity = .@"error",
                        .code = "KLEX002",
                        .title = "unterminated string literal",
                        .message = "Kira reached the end of the file before this string literal was closed.",
                        .labels = &.{
                            diagnostics.primaryLabel(source_pkg.Span.init(start, source.text.len), "string literal starts here"),
                        },
                        .help = "Close the string with a matching '\"'.",
                    });
                    return error.DiagnosticsEmitted;
                }
                const contents = try unescapeStringLiteral(allocator, source.text[start + 1 .. index]);
                index += 1;
                try tokens.append(makeToken(.string, contents, start, index));
            },
            '0'...'9' => {
                const start = index;
                while (index < source.text.len and std.ascii.isDigit(source.text[index])) : (index += 1) {}
                if (index + 1 <= source.text.len and peekByte(source.text, index) == '.' and std.ascii.isDigit(peekByte(source.text, index + 1))) {
                    index += 1;
                    while (index < source.text.len and std.ascii.isDigit(source.text[index])) : (index += 1) {}
                    try tokens.append(makeToken(.float, source.text[start..index], start, index));
                } else {
                    try tokens.append(makeToken(.integer, source.text[start..index], start, index));
                }
            },
            'A'...'Z', 'a'...'z', '_' => {
                const start = index;
                while (index < source.text.len and isIdentifierContinue(source.text[index])) : (index += 1) {}
                const lexeme = source.text[start..index];
                const kind = keywordKind(lexeme);
                try tokens.append(makeToken(kind, lexeme, start, index));
            },
            else => {
                try diagnostics.appendOwned(allocator, out_diagnostics, .{
                    .severity = .@"error",
                    .code = "KLEX001",
                    .title = "unexpected character",
                    .message = "Kira found a character that does not belong to the current grammar.",
                    .labels = &.{
                        diagnostics.primaryLabel(source_pkg.Span.init(index, index + 1), "this character is not valid here"),
                    },
                    .help = "Remove the character or replace it with valid Kira syntax.",
                });
                return error.DiagnosticsEmitted;
            },
        }
    }

    try tokens.append(makeToken(.eof, "", source.text.len, source.text.len));
    return tokens.toOwnedSlice();
}

fn peekByte(text: []const u8, index: usize) u8 {
    if (index >= text.len) return 0;
    return text[index];
}

fn isIdentifierContinue(byte: u8) bool {
    return std.ascii.isAlphanumeric(byte) or byte == '_';
}

fn keywordKind(lexeme: []const u8) syntax.TokenKind {
    if (std.mem.eql(u8, lexeme, "annotation")) return .kw_annotation;
    if (std.mem.eql(u8, lexeme, "capability")) return .kw_capability;
    if (std.mem.eql(u8, lexeme, "class")) return .kw_class;
    if (std.mem.eql(u8, lexeme, "construct")) return .kw_construct;
    if (std.mem.eql(u8, lexeme, "enum")) return .kw_enum;
    if (std.mem.eql(u8, lexeme, "struct")) return .kw_struct;
    if (std.mem.eql(u8, lexeme, "type")) return .kw_type;
    if (std.mem.eql(u8, lexeme, "extends")) return .kw_extends;
    if (std.mem.eql(u8, lexeme, "function")) return .kw_function;
    if (std.mem.eql(u8, lexeme, "generated")) return .kw_generated;
    if (std.mem.eql(u8, lexeme, "override")) return .kw_override;
    if (std.mem.eql(u8, lexeme, "overridable")) return .kw_overridable;
    if (std.mem.eql(u8, lexeme, "targets")) return .kw_targets;
    if (std.mem.eql(u8, lexeme, "uses")) return .kw_uses;
    if (std.mem.eql(u8, lexeme, "let")) return .kw_let;
    if (std.mem.eql(u8, lexeme, "var")) return .kw_var;
    if (std.mem.eql(u8, lexeme, "return")) return .kw_return;
    if (std.mem.eql(u8, lexeme, "import")) return .kw_import;
    if (std.mem.eql(u8, lexeme, "as")) return .kw_as;
    if (std.mem.eql(u8, lexeme, "if")) return .kw_if;
    if (std.mem.eql(u8, lexeme, "else")) return .kw_else;
    if (std.mem.eql(u8, lexeme, "for")) return .kw_for;
    if (std.mem.eql(u8, lexeme, "in")) return .kw_in;
    if (std.mem.eql(u8, lexeme, "while")) return .kw_while;
    if (std.mem.eql(u8, lexeme, "break")) return .kw_break;
    if (std.mem.eql(u8, lexeme, "continue")) return .kw_continue;
    if (std.mem.eql(u8, lexeme, "match")) return .kw_match;
    if (std.mem.eql(u8, lexeme, "switch")) return .kw_switch;
    if (std.mem.eql(u8, lexeme, "case")) return .kw_case;
    if (std.mem.eql(u8, lexeme, "default")) return .kw_default;
    if (std.mem.eql(u8, lexeme, "true")) return .kw_true;
    if (std.mem.eql(u8, lexeme, "false")) return .kw_false;
    return .identifier;
}

fn makeToken(kind: syntax.TokenKind, lexeme: []const u8, start: usize, end: usize) syntax.Token {
    return .{
        .kind = kind,
        .lexeme = lexeme,
        .span = source_pkg.Span.init(start, end),
    };
}

fn unescapeStringLiteral(allocator: std.mem.Allocator, raw: []const u8) ![]const u8 {
    if (std.mem.indexOfScalar(u8, raw, '\\') == null) return raw;

    var out = std.array_list.Managed(u8).init(allocator);
    var index: usize = 0;
    while (index < raw.len) : (index += 1) {
        const byte = raw[index];
        if (byte != '\\' or index + 1 >= raw.len) {
            try out.append(byte);
            continue;
        }

        index += 1;
        try out.append(switch (raw[index]) {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => 0,
            '"' => '"',
            '\\' => '\\',
            else => raw[index],
        });
    }
    return out.toOwnedSlice();
}

fn readRepoFileForTest(allocator: std.mem.Allocator, path: []const u8) ![]const u8 {
    return std.Io.Dir.cwd().readFileAlloc(std.Options.debug_io, path, allocator, .limited(std.math.maxInt(usize)));
}

test "tokenizes expanded declaration grammar" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(
        allocator,
        "test.kira",
        "import UI as Kit\n/// entry\n@Main\nfunction entry(value: Float): Float { let x: Float = 12; return x; }",
    );
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = try tokenize(allocator, &source, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(syntax.TokenKind.kw_import, tokens[0].kind);
    try std.testing.expectEqual(syntax.TokenKind.kw_as, tokens[2].kind);
    try std.testing.expectEqual(syntax.TokenKind.doc_comment, tokens[4].kind);
    try std.testing.expectEqual(syntax.TokenKind.kw_function, tokens[7].kind);
    try std.testing.expectEqual(syntax.TokenKind.colon, tokens[10].kind);
    try std.testing.expectEqual(syntax.TokenKind.float, tokens[20].kind);
}

test "tokenizes modern expression and member syntax" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(
        allocator,
        "modern.kira",
        "class Rect { let zero: Rect = Rect(x: 0.0) function contains(point: Point) -> Bool { return point.x >= 0.0 && point.y >= 0.0 ? true : false } }",
    );
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = try tokenize(allocator, &source, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(syntax.TokenKind.kw_let, tokens[4].kind);
    try std.testing.expectEqual(syntax.TokenKind.arrow, tokens[20].kind);
    try std.testing.expectEqual(syntax.TokenKind.amp_amp, tokens[33].kind);
    try std.testing.expectEqual(syntax.TokenKind.question, tokens[39].kind);
    try std.testing.expectEqual(syntax.TokenKind.colon, tokens[41].kind);
}

test "tokenizes inheritance keywords" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(
        allocator,
        "inheritance.kira",
        "class Dog extends Animal, Pet { override function run() { return; } }",
    );
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = try tokenize(allocator, &source, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(syntax.TokenKind.kw_extends, tokens[2].kind);
    try std.testing.expectEqual(syntax.TokenKind.kw_override, tokens[7].kind);
}

test "tokenizes the checked-in Kira corpus" {
    const corpus = [_][]const u8{
        "examples/hello/main.kira",
        "examples/arithmetic/main.kira",
        "examples/hybrid_roundtrip/main.kira",
        "generated/BootstrapApp/src/main.kira",
        "generated/DemoApp/src/main.kira",
        "templates/app/src/main.kira",
    };

    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    for (corpus) |path| {
        const contents = try readRepoFileForTest(allocator, path);
        const source = try source_pkg.SourceFile.initOwned(allocator, path, contents);
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const tokens = try tokenize(allocator, &source, &diags);

        try std.testing.expectEqual(@as(usize, 0), diags.items.len);
        try std.testing.expect(tokens.len > 1);
        try std.testing.expectEqual(syntax.TokenKind.eof, tokens[tokens.len - 1].kind);
    }
}

test "reports unterminated string literals as diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(allocator, "broken.kira", "@Main\nfunction main() { print(\"hello); return; }");
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = tokenize(allocator, &source, &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("unterminated string literal", diags.items[0].title);
}

test "unescapes common string literal escapes" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(allocator, "strings.kira", "\"a\\n\\t\\\"b\\\\\"");
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const tokens = try tokenize(allocator, &source, &diags);

    try std.testing.expectEqualStrings("a\n\t\"b\\", tokens[0].lexeme);
}

test "reports unexpected characters as diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const source = try source_pkg.SourceFile.initOwned(allocator, "broken.kira", "@Main\nfunction main() { let value = 1 # 2; return; }");
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = tokenize(allocator, &source, &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("unexpected character", diags.items[0].title);
}
