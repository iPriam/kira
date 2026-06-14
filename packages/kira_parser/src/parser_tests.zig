const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const parent = @import("parser.zig");
const Parser = parent.Parser;
const parseSource = parent.parseSource;
const readRepoFileForTest = parent.readRepoFileForTest;
test "parses builder control flow" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "Widget Screen() { content { if ready { Button() { Text(\"ok\") } } else { Text(\"wait\") } for item in items { Row(item) } switch mode { case current { Text(\"a\") } default { Text(\"b\") } } } }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 1), program.decls.len);
}

test "parses struct literals indexing and loop control flow" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "struct Pair { let left: Int = 0; let right: Int = 0; function sum() -> Int { return left + right; } }\n" ++
            "@Main function entry() { var values = [1, 2, 3]; let pair = Pair { left: 4, right: 5 }; while true { values[0] = pair.sum(); if values[0] == 9 { break; } else if values[0] == 10 { continue; } } return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const entry = program.functions[0];
    try std.testing.expectEqual(@as(usize, 4), entry.body.?.statements.len);
    try std.testing.expect(entry.body.?.statements[1].let_stmt.value.?.* == .struct_literal);
    try std.testing.expect(entry.body.?.statements[2] == .while_stmt);
}

test "parses function types and trailing callback blocks" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "function register(handler: (Int) -> Void) { return; }\n" ++
            "@Main function entry() { register { value in print(value); } return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expect(program.functions[0].params[0].type_expr != null);
    try std.testing.expect(program.functions[0].params[0].type_expr.?.* == .function);
    const call_expr = program.functions[1].body.?.statements[0].expr_stmt.expr;
    try std.testing.expect(call_expr.* == .call);
    try std.testing.expect(call_expr.call.trailing_callback != null);
    try std.testing.expectEqual(@as(usize, 1), call_expr.call.trailing_callback.?.params.len);
}

test "parses builder calls with arguments and trailing content" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "@Main function entry() { VStack(\"root\") { Text(\"hello\"); } return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const call_expr = program.functions[0].body.?.statements[0].expr_stmt.expr;
    try std.testing.expect(call_expr.* == .call);
    try std.testing.expectEqual(@as(usize, 1), call_expr.call.args.len);
    try std.testing.expect(call_expr.call.trailing_builder != null);
    try std.testing.expectEqual(@as(usize, 1), call_expr.call.trailing_builder.?.items.len);
}

test "parses inferred and explicit local declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "@Main function entry() { var text = \"abc\"; var pending: String; var amount: Float = 0.0; return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const statements = program.functions[0].body.?.statements;
    try std.testing.expectEqual(@as(usize, 4), statements.len);
    try std.testing.expect(statements[0].let_stmt.type_expr == null);
    try std.testing.expect(statements[0].let_stmt.value.?.* == .string);
    try std.testing.expect(statements[1].let_stmt.type_expr != null);
    try std.testing.expect(statements[1].let_stmt.value == null);
    try std.testing.expect(statements[2].let_stmt.type_expr != null);
    try std.testing.expect(statements[2].let_stmt.value.?.* == .float);
}

test "parses native callback state builtins" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "struct CounterState { var count: Int }\n" ++
            "@Native function onTick(data: RawPtr) { var state = nativeRecover<CounterState>(data); return; }\n" ++
            "@Main function entry() { var state = nativeState(CounterState { count: 0 }); var token = nativeUserData(state); return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const callback_statements = program.functions[0].body.?.statements;
    try std.testing.expect(callback_statements[0].let_stmt.value.?.* == .native_recover);
    const entry_statements = program.functions[1].body.?.statements;
    try std.testing.expect(entry_statements[0].let_stmt.value.?.* == .native_state);
    try std.testing.expect(entry_statements[1].let_stmt.value.?.* == .native_user_data);
}

test "parses hex integer literals" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "@Main function entry() { let low: Int = 0x1f; let high: Int = 0XCAFE; return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const statements = program.functions[0].body.?.statements;
    try std.testing.expectEqual(@as(i64, 31), statements[0].let_stmt.value.?.integer.value);
    try std.testing.expectEqual(@as(i64, 51966), statements[1].let_stmt.value.?.integer.value);
}

test "parses enum declarations generic type references and match statements" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "enum ParseError {\n" ++
            "    InvalidFormat: String = \"bad\"\n" ++
            "    UnexpectedEnd\n" ++
            "}\n" ++
            "enum Result<Value, Failure> {\n" ++
            "    Ok(Value)\n" ++
            "    Error(Failure)\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let value: Result<String, ParseError> = Result.Ok(\"ok\");\n" ++
            "    match value {\n" ++
            "        Ok(text) -> print(text);\n" ++
            "        Error(inner) as whole -> { print(\"bad\"); print(whole); }\n" ++
            "    }\n" ++
            "    return;\n" ++
            "}",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 3), program.decls.len);
    try std.testing.expect(program.decls[0] == .enum_decl);
    try std.testing.expectEqual(@as(usize, 2), program.decls[1].enum_decl.type_params.len);
    try std.testing.expect(program.functions[0].body.?.statements[0].let_stmt.type_expr.?.* == .generic);
    try std.testing.expect(program.functions[0].body.?.statements[1] == .match_stmt);
    try std.testing.expectEqual(@as(usize, 2), program.functions[0].body.?.statements[1].match_stmt.arms.len);
}

test "parses inheritance declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "class Dog extends Animal, Pet {\n" ++
            "    override function run(): Int { return 1; }\n" ++
            "    override let name = \"Dog\";\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), program.decls.len);
    try std.testing.expectEqual(@as(usize, 2), program.decls[0].type_decl.parents.len);
    try std.testing.expect(program.decls[0].type_decl.members[0].function_decl.is_override);
    try std.testing.expect(program.decls[0].type_decl.members[1].field_decl.is_override);
}

test "parses qualified extends lists and override members" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "class WorkingDog extends animals.Dog, behavior.Runner {\n" ++
            "    override function pace(steps: I64): I64 { return steps; }\n" ++
            "    override var energy: I64 = 9;\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), program.decls[0].type_decl.parents.len);
    try std.testing.expectEqual(@as(usize, 2), program.decls[0].type_decl.parents[0].segments.len);
    try std.testing.expectEqualStrings("animals", program.decls[0].type_decl.parents[0].segments[0].text);
    try std.testing.expectEqualStrings("Dog", program.decls[0].type_decl.parents[0].segments[1].text);
    try std.testing.expectEqual(@as(usize, 2), program.decls[0].type_decl.parents[1].segments.len);
    try std.testing.expectEqualStrings("behavior", program.decls[0].type_decl.parents[1].segments[0].text);
    try std.testing.expectEqualStrings("Runner", program.decls[0].type_decl.parents[1].segments[1].text);
    try std.testing.expect(program.decls[0].type_decl.members[0].function_decl.is_override);
    try std.testing.expect(program.decls[0].type_decl.members[1].field_decl.is_override);
    try std.testing.expectEqual(syntax.ast.FieldStorage.mutable, program.decls[0].type_decl.members[1].field_decl.storage);
}

test "parses construct extends parent lists" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "construct WebElement { }\n" ++
            "construct Drawable { }\n" ++
            "construct Surface extends WebElement, Drawable { }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 0), program.decls[0].construct_decl.parents.len);
    try std.testing.expectEqual(@as(usize, 2), program.decls[2].construct_decl.parents.len);
    try std.testing.expectEqualStrings("WebElement", program.decls[2].construct_decl.parents[0].segments[0].text);
    try std.testing.expectEqualStrings("Drawable", program.decls[2].construct_decl.parents[1].segments[0].text);
}

test "parses construct direct members with @Required and a computed node field" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "construct Widget {\n" ++
            "    @Required let body: Widget\n" ++
            "    let node: Node {\n" ++
            "        body.node\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const construct_decl = program.decls[0].construct_decl;
    try std.testing.expectEqual(@as(usize, 2), construct_decl.members.len);

    const required_body = construct_decl.members[0].field_decl;
    try std.testing.expectEqualStrings("body", required_body.name);
    try std.testing.expectEqual(@as(usize, 1), required_body.annotations.len);
    try std.testing.expectEqualStrings("Required", required_body.annotations[0].name.segments[0].text);
    try std.testing.expect(required_body.body == null);

    const computed_node = construct_decl.members[1].field_decl;
    try std.testing.expectEqualStrings("node", computed_node.name);
    try std.testing.expect(computed_node.value == null);
    try std.testing.expect(computed_node.body != null);
}

test "parses bodyless @Required construct member functions" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "construct Node {\n" ++
            "    @Required function measure() -> Int\n" ++
            "    @Required function place() -> Int\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const construct_decl = program.decls[0].construct_decl;
    try std.testing.expectEqual(@as(usize, 2), construct_decl.members.len);
    const measure = construct_decl.members[0].function_decl;
    try std.testing.expectEqualStrings("measure", measure.name);
    try std.testing.expect(measure.body == null);
    try std.testing.expectEqualStrings("Required", measure.annotations[0].name.segments[0].text);
}

test "parses extend declaration with modifier functions" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "construct Widget { @Required let body: Widget }\n" ++
            "extend Widget {\n" ++
            "    function padding(amount: Float) -> Widget {\n" ++
            "        return self\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const extend_decl = program.decls[1].extend_decl;
    try std.testing.expectEqualStrings("Widget", extend_decl.construct_name.segments[0].text);
    try std.testing.expectEqual(@as(usize, 1), extend_decl.members.len);
    try std.testing.expectEqualStrings("padding", extend_decl.members[0].function_decl.name);
}

test "parses construct property schema and declaration properties section" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "construct Route {\n" ++
            "    properties {\n" ++
            "        required path: String\n" ++
            "        title: String\n" ++
            "    }\n" ++
            "}\n" ++
            "Route Home {\n" ++
            "    properties {\n" ++
            "        path: \"/\"\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const section = program.decls[0].construct_decl.sections[0];
    try std.testing.expectEqual(syntax.ast.ConstructSectionKind.properties, section.kind);
    try std.testing.expectEqual(@as(usize, 2), section.entries.len);
    try std.testing.expect(section.entries[0].property_schema.required);
    try std.testing.expectEqualStrings("path", section.entries[0].property_schema.name);
    try std.testing.expect(!section.entries[1].property_schema.required);

    const form_member = program.decls[1].construct_form_decl.body.members[0];
    try std.testing.expectEqual(@as(usize, 1), form_member.properties_section.entries.len);
    try std.testing.expectEqualStrings("path", form_member.properties_section.entries[0].name);
}

test "parses construct content channels with accepts and count ranges" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "construct WebElement {\n" ++
            "    content {\n" ++
            "        head { accepts Title count 0..1 }\n" ++
            "        body { accepts Title count 1.. }\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const section = program.decls[0].construct_decl.sections[0];
    try std.testing.expectEqual(@as(usize, 2), section.entries.len);
    const head = section.entries[0].content_channel;
    try std.testing.expectEqualStrings("head", head.name);
    try std.testing.expectEqualStrings("Title", head.accepts.?.segments[0].text);
    try std.testing.expectEqual(@as(u32, 0), head.count.?.min);
    try std.testing.expectEqual(@as(u32, 1), head.count.?.max.?);
    const body = section.entries[1].content_channel;
    try std.testing.expectEqual(@as(u32, 1), body.count.?.min);
    try std.testing.expect(body.count.?.max == null);
}

test "parses class struct capability and generated annotation declarations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const program = try parseSource(
        allocator,
        "capability Serializable { generated { overridable function toText(): String { return \"ok\" } } }\n" ++
            "annotation GameItem { targets: class uses Serializable generated { function stableId(): Int { return 1 } } }\n" ++
            "struct Position { let x: Float = 0.0 }\n" ++
            "@GameItem class Player extends Base { override function toText(): String { return \"player\" } }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 5), program.decls.len);
    try std.testing.expectEqualStrings("Serializable", program.decls[0].capability_decl.name);
    try std.testing.expectEqual(@as(usize, 1), program.decls[0].capability_decl.generated_members.len);
    try std.testing.expectEqual(@as(usize, 1), program.decls[1].annotation_decl.targets.len);
    try std.testing.expectEqual(@as(usize, 1), program.decls[1].annotation_decl.uses.len);
    try std.testing.expectEqual(syntax.ast.TypeKind.struct_decl, program.decls[2].type_decl.kind);
    try std.testing.expectEqual(syntax.ast.TypeKind.class, program.decls[3].type_decl.kind);
}

test "reports outdated func and type syntax with migration hints" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = parseSource(allocator, "func main() { return; }", &diags);
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("outdated function declaration syntax", diags.items[0].title);
        try std.testing.expect(diags.items[0].help != null);
    }
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = parseSource(allocator, "type Old { }", &diags);
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("removed type declaration syntax", diags.items[0].title);
        try std.testing.expect(diags.items[0].help != null);
    }
}

test "reports struct inheritance as invalid syntax" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = parseSource(allocator, "struct Point extends Base { }", &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqualStrings("struct cannot inherit", diags.items[0].title);
}

test "reports invalid bare override members" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = parseSource(
        allocator,
        "class Dog {\n" ++
            "    override ready;\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("expected override member declaration", diags.items[0].title);
}

test "parses the hybrid example corpus shape" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const source_text = try readRepoFileForTest(allocator, "examples/hybrid_roundtrip/app/main.kira");
    const program = try parseSource(allocator, source_text, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 3), program.functions.len);
    try std.testing.expectEqual(@as(usize, 3), program.decls.len);
}

test "parses the restored hello example with canonical class/struct syntax" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const source_text = try readRepoFileForTest(allocator, "examples/hello/app/main.kira");
    const program = try parseSource(allocator, source_text, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 3), program.decls.len);
    try std.testing.expectEqual(@as(usize, 3), program.functions.len);
}

test "reports malformed annotations as diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = parseSource(allocator, "@\nfunction main() { return; }", &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expect(diags.items.len >= 1);
    try std.testing.expectEqualStrings("expected annotation name after '@'", diags.items[0].title);
}

test "reports malformed function headers as diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = parseSource(allocator, "@Main\nfunction () { return; }", &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("expected function name", diags.items[0].title);
}

test "reports missing block delimiters as diagnostics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = parseSource(allocator, "@Main\nfunction main() { return;", &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("expected '}' to close block", diags.items[0].title);
}
