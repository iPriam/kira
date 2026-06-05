const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const model = @import("kira_semantics_model");
const syntax = @import("kira_syntax_model");
const analyzer = @import("analyzer.zig");

fn analyzeSource(allocator: std.mem.Allocator, text: []const u8, diags: *std.array_list.Managed(diagnostics.Diagnostic)) !model.Program {
    const lexer = @import("kira_lexer");
    const parser = @import("kira_parser");
    const source_pkg = @import("kira_source");

    const source = try source_pkg.SourceFile.initOwned(allocator, "test.kira", text);
    const tokens = try lexer.tokenize(allocator, &source, diags);
    const program = try parser.parse(allocator, tokens, diags);
    return analyzer.analyze(allocator, program, diags);
}

test "reports missing @Main entrypoint" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(allocator, "function helper() { return; }", &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expect(diags.items.len > 0);
    try std.testing.expectEqualStrings("missing @Main entrypoint", diags.items[0].title);
}

test "reports multiple @Main entrypoints" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "@Main\nfunction first() { return; }\n@Main\nfunction second() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expect(diags.items.len > 0);
    try std.testing.expectEqualStrings("multiple @Main entrypoints", diags.items[0].title);
}

test "preserves explicit @Native and @Runtime execution semantics" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "@Main\n" ++
            "@Native\n" ++
            "function entry() { helper(); return; }\n" ++
            "@Runtime\n" ++
            "function helper() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), analyzed.functions.len);
    try std.testing.expectEqual(model.FunctionExecution.native, analyzed.functions[0].execution);
    try std.testing.expectEqual(model.FunctionExecution.runtime, analyzed.functions[1].execution);
}

test "preserves type execution annotations and class annotation flexibility" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "annotation Tagged { targets: class }\n" ++
            "@Native struct NativeValue {}\n" ++
            "@Runtime struct RuntimeValue {}\n" ++
            "@Tagged @Native class NativeBox {}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    var saw_native = false;
    var saw_runtime = false;
    for (analyzed.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, "NativeValue")) {
            saw_native = true;
            try std.testing.expectEqual(model.FunctionExecution.native, type_decl.execution);
        }
        if (std.mem.eql(u8, type_decl.name, "RuntimeValue")) {
            saw_runtime = true;
            try std.testing.expectEqual(model.FunctionExecution.runtime, type_decl.execution);
        }
    }
    try std.testing.expect(saw_native);
    try std.testing.expect(saw_runtime);
}

test "rejects non execution struct annotations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "annotation Tagged { targets: struct }\n" ++
            "@Tagged struct Value {}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "invalid struct annotation");
}

test "requires @Native only for direct FFI use" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    const extern_decl =
        "@FFI.Extern { library: testlib; symbol: ffi_value; abi: c; }\n" ++
        "function ffi_value(): I64;\n";

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            extern_decl ++
                "@Main\n" ++
                "function entry() {\n" ++
                "    ffi_value();\n" ++
                "    return;\n" ++
                "}",
            &diags,
        );

        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "direct FFI requires @Native");
        try std.testing.expectEqualStrings("KSEM093", diags.items[0].code.?);
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        _ = try analyzeSource(
            allocator,
            extern_decl ++
                "@Main\n" ++
                "function entry() {\n" ++
                "    let value = wrapper();\n" ++
                "    print(value);\n" ++
                "    return;\n" ++
                "}\n" ++
                "\n" ++
                "@Native\n" ++
                "function nativeHelper(): I64 {\n" ++
                "    return ffi_value();\n" ++
                "}\n" ++
                "\n" ++
                "function wrapper(): I64 {\n" ++
                "    return nativeHelper();\n" ++
                "}",
            &diags,
        );

        try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            extern_decl ++
                "@Main\n" ++
                "function entry() {\n" ++
                "    let callback: () -> I64 = ffi_value;\n" ++
                "    return;\n" ++
                "}",
            &diags,
        );

        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "direct FFI requires @Native");
    }
}

test "reports conflicting execution annotations" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "@Main\n@Native\n@Runtime\nfunction entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqual(@as(usize, 1), diags.items.len);
    try std.testing.expectEqualStrings("conflicting execution annotations", diags.items[0].title);
}

test "requires explicit parameter types" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "@Main\nfunction entry(value) { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqualStrings("parameter type is required", diags.items[0].title);
}

test "enforces declaration typing and initialization rules" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        _ = try analyzeSource(
            allocator,
            "@Main\nfunction entry() { var text: String; var value: Float = 12.0; value = 13.0; return; }",
            &diags,
        );
        try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "@Main\nfunction entry() { var value: Float = 12; return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("initializer does not match declared type", diags.items[0].title);
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "@Main\nfunction entry() { var value: String; print(value); return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("local is not initialized", diags.items[0].title);
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "@Main\nfunction entry(): Float { let value = 12; return value; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try std.testing.expectEqualStrings("type mismatch", diags.items[0].title);
    }
}

test "validates construct-driven requirements" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "annotation State { }\n" ++
            "construct Widget { annotations { @State; } requires { content; } lifecycle { onAppear() {} } }\n" ++
            "Widget Button() { @State let count: Int = 0; }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try std.testing.expectEqualStrings("missing required content block", diags.items[0].title);
}

test "allows struct methods and constant members" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "struct Point {\n" ++
            "    let x: Float = 0.0;\n" ++
            "    let y: Float = 0.0;\n" ++
            "    let zero: Point = Point(x: 0.0, y: 0.0);\n" ++
            "    function distanceTo(other: borrow Point) -> Float { return x + other.x; }\n" ++
            "}\n" ++
            "@Main function entry() { let start = Point.zero; let end = Point { x: 2.0, y: 3.0 }; print(end.distanceTo(other: start)); return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), analyzed.functions.len);
    try std.testing.expectEqualStrings("Point.distanceTo", analyzed.functions[0].name);
}

test "lowers sparse FFI struct construction as zero-filled construction" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "@FFI.Struct { layout: c; }\n" ++
            "struct Example {\n" ++
            "    var a: U8\n" ++
            "    var b: U8\n" ++
            "    var c: U8\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let first = Example();\n" ++
            "    let second = Example { b: 7 };\n" ++
            "    return;\n" ++
            "}",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const entry = analyzed.functions[0];
    try std.testing.expect(entry.body[0].let_stmt.value.?.* == .construct);
    try std.testing.expectEqual(model.ConstructFillMode.zeroed_ffi_c_layout, entry.body[0].let_stmt.value.?.construct.fill_mode);
    try std.testing.expectEqual(@as(usize, 0), entry.body[0].let_stmt.value.?.construct.fields.len);
    try std.testing.expect(entry.body[1].let_stmt.value.?.* == .construct);
    try std.testing.expectEqual(model.ConstructFillMode.zeroed_ffi_c_layout, entry.body[1].let_stmt.value.?.construct.fill_mode);
    try std.testing.expectEqual(@as(usize, 1), entry.body[1].let_stmt.value.?.construct.fields.len);
    try std.testing.expectEqual(@as(u32, 1), entry.body[1].let_stmt.value.?.construct.fields[0].field_index.?);
}

test "reports loop control outside loops" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "@Main function entry() { break; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "break requires a loop");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "@Main function entry() { continue; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "continue requires a loop");
    }
}

test "validates annotation declarations and fills defaults" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "annotation State { }\n" ++
            "annotation Attribute { parameters { index: Int } }\n" ++
            "annotation InputMapping { parameters { priority: Int = 0 blocksLowerPriorityMappings: Bool = false } }\n" ++
            "construct Widget { annotations { @State; @Attribute; @InputMapping; } requires { content; } }\n" ++
            "Widget Button() {\n" ++
            "    @State let isPressed: Bool = false;\n" ++
            "    @Attribute(0) let position: Float = 0.0;\n" ++
            "    @InputMapping let mapping: Int = 1;\n" ++
            "    content { }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 3), analyzed.annotations.len);
    try std.testing.expectEqual(@as(usize, 1), analyzed.forms.len);
    try std.testing.expectEqual(@as(usize, 3), analyzed.forms[0].fields.len);
    try std.testing.expectEqual(@as(usize, 1), analyzed.forms[0].fields[1].annotations.len);
    try std.testing.expectEqual(@as(usize, 1), analyzed.forms[0].fields[1].annotations[0].arguments.len);
    try std.testing.expectEqual(@as(i64, 0), analyzed.forms[0].fields[1].annotations[0].arguments[0].value.integer);
    try std.testing.expectEqual(@as(usize, 2), analyzed.forms[0].fields[2].annotations[0].arguments.len);
    try std.testing.expectEqual(@as(i64, 0), analyzed.forms[0].fields[2].annotations[0].arguments[0].value.integer);
    try std.testing.expectEqual(false, analyzed.forms[0].fields[2].annotations[0].arguments[1].value.boolean);
}

test "reports annotation schema errors" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation Attribute { parameters { index: Int } }\n" ++
                "construct Widget { annotations { @Attribute; } requires { content; } }\n" ++
                "Widget Button() { @Attribute let position: Float = 0.0; content { } }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "missing annotation parameter");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation Attribute { parameters { index: Int } }\n" ++
                "construct Widget { annotations { @Attribute; } requires { content; } }\n" ++
                "Widget Button() { @Attribute(\"zero\") let position: Float = 0.0; content { } }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "annotation parameter type mismatch");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation State { }\n" ++
                "construct Widget { annotations { @State; } requires { content; } }\n" ++
                "Widget Button() { @State(1) let isPressed: Bool = false; content { } }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "annotation does not accept parameters");
    }
}

test "reports undeclared annotations in construct allowlists" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "construct Widget { annotations { @State; } }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "unknown annotation");
}

test "reports duplicate annotation declarations and parameters" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation State { }\n" ++
                "annotation State { }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "duplicate annotation declaration");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation Attribute { parameters { index: Int index: Int } }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "duplicate annotation parameter");
    }
}

test "reports invalid annotation parameter defaults" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "annotation Attribute { parameters { index: Int = \"zero\" } }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "annotation parameter type mismatch");
}

test "validates annotation targets capabilities and generated overrides" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const analyzed = try analyzeSource(
            allocator,
            "capability Labeling { generated { overridable function label(): Int { return 1; } } }\n" ++
                "annotation Tagged { targets: class uses Labeling }\n" ++
                "@Tagged class Item { override function label(): Int { return 2; } }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectEqual(@as(usize, 0), diags.items.len);
        try std.testing.expectEqual(@as(usize, 1), analyzed.capabilities.len);
        try std.testing.expectEqual(@as(usize, 1), analyzed.annotations[0].generated_functions.len);
    }
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation StructOnly { targets: struct }\n" ++
                "@StructOnly class Item {}\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "invalid annotation target");
    }
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "annotation Tagged { targets: class generated { function label(): Int { return 1; } } }\n" ++
                "@Tagged class Item { override function label(): Int { return 2; } }\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "generated member is not overridable");
    }
    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "capability A { generated { function label(): Int { return 1; } } }\n" ++
                "capability B { generated { function label(): Int { return 2; } } }\n" ++
                "annotation Tagged { targets: class uses A, B }\n" ++
                "@Tagged class Item {}\n" ++
                "@Main function entry() { return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "duplicate generated member");
    }
}

test "struct methods lower like value-oriented instance behavior" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "struct Size { let width: I64 = 2 function doubled() -> I64 { return width * 2; } }\n" ++
            "@Main function entry() { let size = Size(); print(size.doubled()); return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqualStrings("Size.doubled", analyzed.functions[0].name);
}

test "allows imported construct and callable names in the global namespace" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzer.analyzeWithImports(
        allocator,
        try parseSource(
            allocator,
            "import UI\n" ++
                "Widget DashboardShell() {\n" ++
                "    content {\n" ++
                "        Card(\"Operations\")\n" ++
                "    }\n" ++
                "    onAppear() { return; }\n" ++
                "}\n" ++
                "@Main function entry() { return; }",
            &diags,
        ),
        .{
            .constructs = &.{"Widget"},
            .callables = &.{"Card"},
        },
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 1), analyzed.forms.len);
    try std.testing.expectEqualStrings("Widget", analyzed.forms[0].construct.construct_name);
}

test "root declarations shadow dependency declarations without duplicate leakage" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    var program = try parseSource(
        allocator,
        "struct Color { let r: Int = 0; }\n" ++
            "struct Color { let value: Int = 7; }\n" ++
            "@Main function entry() { let color = Color(); print(color.value); return; }",
        &diags,
    );
    const origins = try allocator.alloc(syntax.ast.DeclOrigin, program.decls.len);
    for (origins) |*origin| origin.* = .{};
    origins[0] = .{ .package_name = "KiraGraphics", .source_path = "KiraGraphics/Color.kira" };
    program.decl_origins = origins;

    const analyzed = try analyzer.analyze(allocator, program, &diags);

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const color = findTypeDeclByName(analyzed, "Color") orelse return error.TestUnexpectedResult;
    try std.testing.expectEqual(@as(usize, 1), color.fields.len);
    try std.testing.expectEqualStrings("value", color.fields[0].name);
}

test "duplicate declarations still fail within one dependency namespace" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    var program = try parseSource(
        allocator,
        "struct Color { let r: Int = 0; }\n" ++
            "struct Color { let g: Int = 0; }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );
    const origins = try allocator.alloc(syntax.ast.DeclOrigin, program.decls.len);
    for (origins) |*origin| origin.* = .{ .package_name = "KiraGraphics", .source_path = "KiraGraphics/Color.kira" };
    origins[2] = .{};
    program.decl_origins = origins;

    const result = analyzer.analyze(allocator, program, &diags);

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "duplicate top-level name");
}

test "lowers any construct parameters as structured construct constraints" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "construct Widget {}\n" ++
            "@Runtime function accept(value: any Widget) { return; }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(model.Type.construct_any, analyzed.functions[0].params[0].ty.kind);
    try std.testing.expectEqualStrings("any Widget", analyzed.functions[0].params[0].ty.name.?);
    try std.testing.expectEqualStrings("Widget", analyzed.functions[0].params[0].ty.construct_constraint.?.construct_name);
}

test "class methods can read fields through implicit self" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "class Counter { let value: Int = 1; function current(): Int { return value; } }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), analyzed.functions.len);
}

test "class methods can call sibling methods through implicit self" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "class Counter { let value: Int = 1; function current(): Int { return value; } function mirror(): Int { return current(); } }\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 3), analyzed.functions.len);
}

test "reports inheritance cycles" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "class Left extends Right {}\n" ++
            "class Right extends Left {}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "inheritance cycle");
}

test "reports duplicate direct parents" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "class Base {}\n" ++
            "class Child extends Base, Base {}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "duplicate parent type");
}

test "reports ambiguous inherited field lookups" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "class Left { let value: I64 = 1 }\n" ++
            "class Right { let value: I64 = 2 }\n" ++
            "class Child extends Left, Right {\n" ++
            "    function read(): I64 { return value; }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "ambiguous inherited field lookup");
}

test "reports ambiguous inherited method lookups" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "class Left { function ping(): I64 { return 1; } }\n" ++
            "class Right { function ping(): I64 { return 2; } }\n" ++
            "class Child extends Left, Right {\n" ++
            "    function read(): I64 { return ping(); }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "ambiguous inherited method lookup");
}

test "allows type-qualified constant member lookup outside inheritance" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "class Left { let value: I64 = 1 }\n" ++
            "class Right { let value: I64 = 2 }\n" ++
            "class Child extends Left {\n" ++
            "    function read(): I64 { return Right.value; }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    try std.testing.expectEqual(@as(usize, 2), analyzed.functions.len);
}

test "supports function types trailing callbacks and callable values" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "function sink(value: Int) { print(value); return; }\n" ++
            "function register(handler: (Int) -> Void) { return; }\n" ++
            "@Main function entry() {\n" ++
            "    let callback: (Int) -> Void = sink;\n" ++
            "    register { value in sink(value); }\n" ++
            "    callback(1);\n" ++
            "    return;\n" ++
            "}",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const entry = analyzed.functions[2];
    try std.testing.expect(entry.body[0].let_stmt.value.?.* == .function_ref);
    try std.testing.expect(entry.body[1].expr_stmt.expr.* == .call);
    try std.testing.expect(entry.body[1].expr_stmt.expr.call.args[0].* == .callback);
    try std.testing.expect(entry.body[2].expr_stmt.expr.* == .call_value);
}

test "callable values preserve borrow mut parameter ownership" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "struct Counter { var value: Int }\n" ++
            "function mutate(counter: borrow mut Counter) { counter.value = counter.value + 1; return; }\n" ++
            "@Main function entry() {\n" ++
            "    let callback: (borrow mut Counter) -> Void = mutate;\n" ++
            "    var counter = Counter { value: 0 };\n" ++
            "    callback(counter);\n" ++
            "    return;\n" ++
            "}",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const entry = analyzed.functions[analyzed.functions.len - 1];
    try std.testing.expect(entry.body[2].expr_stmt.expr.* == .call_value);
}

test "supports native callback state handles and recovered access" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "struct CounterState { var count: Int }\n" ++
            "@Native function onTick(data: RawPtr) { var state = nativeRecover<CounterState>(data); state.count = state.count + 1; return; }\n" ++
            "@Main function entry() { var state = nativeState(CounterState { count: 0 }); var token = nativeUserData(state); var recovered = nativeRecover<CounterState>(token); recovered.count = recovered.count + 1; return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);

    const callback_recover = analyzed.functions[0].body[0].let_stmt.value.?.native_recover;
    try std.testing.expectEqual(model.Type.native_state_view, callback_recover.ty.kind);
    try std.testing.expectEqualStrings("CounterState", callback_recover.ty.name.?);

    const entry = analyzed.functions[1];
    try std.testing.expectEqual(model.Type.native_state, entry.body[0].let_stmt.ty.kind);
    try std.testing.expect(entry.body[1].let_stmt.value.?.* == .native_user_data);
    try std.testing.expectEqual(model.Type.native_state_view, entry.body[2].let_stmt.ty.kind);
    try std.testing.expect(entry.body[2].let_stmt.value.?.* == .native_recover);
}

test "rejects native callback state misuse" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "struct CounterState { var count: Int }\n" ++
                "@Main function entry() { var value = CounterState { count: 0 }; var token = nativeUserData(value); return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "nativeUserData requires native state");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "struct CounterState { var count: Int }\n" ++
                "@Main function entry() { var state = nativeRecover<CounterState>(0); return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "nativeRecover requires RawPtr");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "struct LeftState { var count: Int }\n" ++
                "struct RightState { var total: Int }\n" ++
                "@Main function entry() { var state = nativeState(LeftState { count: 0 }); var value = nativeRecover<RightState>(nativeUserData(state)); return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "native state type mismatch");
    }

    {
        var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
        const result = analyzeSource(
            allocator,
            "@FFI.Struct { layout: c; }\n" ++
                "struct CState { var count: Int }\n" ++
                "@Main function entry() { var state = nativeState(CState { count: 0 }); return; }",
            &diags,
        );
        try std.testing.expectError(error.DiagnosticsEmitted, result);
        try expectFirstDiagnosticTitle(diags.items, "native state requires a Kira-owned type");
    }
}

test "allows indirect FFI usage through native functions" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    _ = try analyzeSource(
        allocator,
        "@FFI.Extern { library: testlib; symbol: ffi_value; abi: c; }\n" ++
            "function ffi_value(): I64;\n" ++
            "@Native function readViaNative(): I64 { return ffi_value(); }\n" ++
            "@Main function entry() { readViaNative(); return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
}

test "preserves trailing builder trees on call expressions" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzer.analyzeWithImports(
        allocator,
        try parseSource(
            allocator,
            "import UI\n" ++
                "Widget Screen() {\n" ++
                "    content {\n" ++
                "        Column(\"root\") {\n" ++
                "            Text(\"hello\")\n" ++
                "        }\n" ++
                "    }\n" ++
                "}\n" ++
                "@Main function entry() { return; }",
            &diags,
        ),
        .{
            .constructs = &.{"Widget"},
            .callables = &.{ "Column", "Text" },
        },
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const content = analyzed.forms[0].content orelse return error.TestUnexpectedResult;
    try std.testing.expectEqual(@as(usize, 1), content.items.len);
    try std.testing.expect(content.items[0] == .expr);
    try std.testing.expect(content.items[0].expr.expr.* == .call);
    try std.testing.expectEqual(@as(usize, 1), content.items[0].expr.expr.call.args.len);
    try std.testing.expect(content.items[0].expr.expr.call.trailing_builder != null);
    try std.testing.expectEqual(@as(usize, 1), content.items[0].expr.expr.call.trailing_builder.?.items.len);
}

test "reports override signature mismatches" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const result = analyzeSource(
        allocator,
        "class Base { function ping(value: I64): I64 { return value; } }\n" ++
            "class Child extends Base {\n" ++
            "    override function ping(): I64 { return 1; }\n" ++
            "}\n" ++
            "@Main function entry() { return; }",
        &diags,
    );

    try std.testing.expectError(error.DiagnosticsEmitted, result);
    try expectFirstDiagnosticTitle(diags.items, "override signature mismatch");
}

test "field default overrides reuse the inherited slot" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "class Base { let value: I64 = 1 }\n" ++
            "class Child extends Base {\n" ++
            "    override let value = 2;\n" ++
            "}\n" ++
            "@Main function entry() { let child: Child = Child(); return; }",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const child = findTypeDeclByName(analyzed, "Child") orelse return error.TestUnexpectedResult;
    try std.testing.expectEqual(@as(usize, 1), child.fields.len);
    try std.testing.expectEqual(@as(u32, 0), child.fields[0].slot_index);
    try std.testing.expect(child.fields[0].default_value != null);
    try std.testing.expectEqualStrings("value", child.fields[0].name);
    try std.testing.expect(child.fields[0].default_value.?.* == .integer);
    try std.testing.expectEqual(@as(i64, 2), child.fields[0].default_value.?.integer.value);
}

test "array count lowers as a dedicated expression" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "struct Item { let value: I64 = 1 }\n" ++
            "@Main function entry() {\n" ++
            "    let items = [Item()]\n" ++
            "    let count = items.count\n" ++
            "    return;\n" ++
            "}",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const count_stmt = analyzed.functions[analyzed.entry_index].body[1].let_stmt;
    try std.testing.expect(count_stmt.value != null);
    try std.testing.expect(count_stmt.value.?.* == .array_len);
    try std.testing.expectEqual(model.Type.integer, count_stmt.value.?.array_len.ty.kind);
}

test "string count lowers as a dedicated expression" {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);
    const analyzed = try analyzeSource(
        allocator,
        "@Main function entry() {\n" ++
            "    let title = \"KIRA\"\n" ++
            "    let count = title.count\n" ++
            "    return;\n" ++
            "}",
        &diags,
    );

    try std.testing.expectEqual(@as(usize, 0), diags.items.len);
    const count_stmt = analyzed.functions[analyzed.entry_index].body[1].let_stmt;
    try std.testing.expect(count_stmt.value != null);
    try std.testing.expect(count_stmt.value.?.* == .string_len);
    try std.testing.expectEqual(model.Type.integer, count_stmt.value.?.string_len.ty.kind);
}

fn parseSource(
    allocator: std.mem.Allocator,
    text: []const u8,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    const lexer = @import("kira_lexer");
    const parser = @import("kira_parser");
    const source_pkg = @import("kira_source");

    const source = try source_pkg.SourceFile.initOwned(allocator, "test.kira", text);
    const tokens = try lexer.tokenize(allocator, &source, diags);
    return parser.parse(allocator, tokens, diags);
}

fn expectFirstDiagnosticTitle(items: []const diagnostics.Diagnostic, expected_title: []const u8) !void {
    try std.testing.expect(items.len > 0);
    try std.testing.expectEqualStrings(expected_title, items[0].title);
}

fn findTypeDeclByName(program: model.Program, name: []const u8) ?model.TypeDecl {
    for (program.types) |type_decl| {
        if (std.mem.eql(u8, type_decl.name, name)) return type_decl;
    }
    return null;
}
