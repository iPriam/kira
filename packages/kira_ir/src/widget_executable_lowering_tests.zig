const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const lexer = @import("kira_lexer");
const parser = @import("kira_parser");
const semantics = @import("kira_semantics");
const lower_from_hir = @import("lower_from_hir.zig");

fn lowerSource(source_text: []const u8) !void {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();

    const allocator = arena.allocator();
    var diags = std.array_list.Managed(diagnostics.Diagnostic).init(allocator);

    const source = try source_pkg.SourceFile.initOwned(allocator, "test.kira", source_text);
    const tokens = try lexer.tokenize(allocator, &source, &diags);
    const parsed = try parser.parse(allocator, tokens, &diags);
    const analyzed = try semantics.analyze(allocator, parsed, &diags);
    try std.testing.expectEqual(@as(usize, 0), diags.items.len);

    _ = try lower_from_hir.lowerProgram(allocator, analyzed);
}

test "lowers widget body content and dynamic lower dispatch into executable IR" {
    try lowerSource(
        "struct FoundationUiContext {}\n" ++
            "struct FoundationView { let id: Int = 0 }\n" ++
            "construct Widget {\n" ++
            "    @Required let body: Widget\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        let ignored = context.id\n" ++
            "        return body.lower(context)\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget Text(text: String) {\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        let ignored = text.count + context.id\n" ++
            "        return FoundationView { id: 1 }\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget AppSurface() {\n" ++
            "    @Content let content: Widget\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        return content.lower(context)\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let root = AppSurface() {\n" ++
            "        Text(text: \"hello\")\n" ++
            "    }\n" ++
            "    let context = FoundationUiContext {}\n" ++
            "    let view = root.lower(context)\n" ++
            "    let ignored = view.id\n" ++
            "    return\n" ++
            "}\n",
    );
}

test "lowers widget content arrays built from For blocks into executable IR" {
    try lowerSource(
        "struct FoundationUiContext { let id: Int = 0 }\n" ++
            "struct FoundationView { let id: Int = 0 }\n" ++
            "construct Widget {\n" ++
            "    @Required let body: Widget\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        return body.lower(context)\n" ++
            "    }\n" ++
            "}\n" ++
            "function lowerAll(context: borrow FoundationUiContext, children: [any Widget]) -> FoundationView {\n" ++
            "    var index = 0\n" ++
            "    var last = FoundationView {}\n" ++
            "    while index < children.count {\n" ++
            "        last = children[index].lower(context)\n" ++
            "        index = index + 1\n" ++
            "    }\n" ++
            "    return last\n" ++
            "}\n" ++
            "Widget Text(text: String) {\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        let ignored = text.count + context.id\n" ++
            "        return FoundationView { id: 2 }\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget VStack() {\n" ++
            "    @Content let children: [Widget]\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        return lowerAll(context, children)\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let values = [1, 2, 3]\n" ++
            "    let root = VStack() {\n" ++
            "        For(value in values) {\n" ++
            "            Text(text: \"row\")\n" ++
            "        }\n" ++
            "    }\n" ++
            "    let context = FoundationUiContext {}\n" ++
            "    let view = root.lower(context)\n" ++
            "    let ignored = view.id\n" ++
            "    return\n" ++
            "}\n",
    );
}

test "lowers widget extension modifiers and static computed accessors into executable IR" {
    try lowerSource(
        "struct FoundationUiContext { let id: Int = 0 }\n" ++
            "struct FoundationView { let id: Int = 0 }\n" ++
            "struct Color {\n" ++
            "    let r: Float = 0.0\n" ++
            "    let Blue: Color {\n" ++
            "        return Color { r: 1.0 }\n" ++
            "    }\n" ++
            "}\n" ++
            "construct Widget {\n" ++
            "    @Required let body: Widget\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        return body.lower(context)\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget Text(text: String) {\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        let ignored = text.count + context.id\n" ++
            "        return FoundationView { id: 3 }\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget FillLayer(color: Color) {\n" ++
            "    @Content let content: Widget\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        let ignored = color.r + intAsFloat(value: context.id)\n" ++
            "        return content.lower(context)\n" ++
            "    }\n" ++
            "}\n" ++
            "extend Widget {\n" ++
            "    function fill(color: Color) -> Widget {\n" ++
            "        FillLayer(color: color) { self }\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let root = Text(text: \"hello\").fill(Color.Blue)\n" ++
            "    let context = FoundationUiContext {}\n" ++
            "    let view = root.lower(context)\n" ++
            "    let ignored = view.id\n" ++
            "    return\n" ++
            "}\n",
    );
}

test "lowers derived widget body accessors that capture named struct fields" {
    try lowerSource(
        "struct FoundationUiContext { let id: Int = 0 }\n" ++
            "struct FoundationView { let id: Int = 0 }\n" ++
            "struct Color {\n" ++
            "    let r: Float = 0.0\n" ++
            "    let Blue: Color {\n" ++
            "        return Color { r: 1.0 }\n" ++
            "    }\n" ++
            "}\n" ++
            "construct Widget {\n" ++
            "    @Required let body: Widget\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        return body.lower(context)\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget Text(text: String) {\n" ++
            "    function lower(context: borrow FoundationUiContext) -> FoundationView {\n" ++
            "        let ignored = text.count + context.id\n" ++
            "        return FoundationView { id: 4 }\n" ++
            "    }\n" ++
            "}\n" ++
            "Widget AccentCard(title: String, accent: Color) {\n" ++
            "    body {\n" ++
            "        Text(text: title)\n" ++
            "    }\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let root = AccentCard(title: \"hello\", accent: Color.Blue)\n" ++
            "    let context = FoundationUiContext {}\n" ++
            "    let view = root.lower(context)\n" ++
            "    let ignored = view.id\n" ++
            "    return\n" ++
            "}\n",
    );
}

test "lowers enum namespace references used by widget helper code into executable IR" {
    try lowerSource(
        "enum ProjectStatus {\n" ++
            "    Active\n" ++
            "    Waiting\n" ++
            "}\n" ++
            "struct Color {\n" ++
            "    let r: Float = 0.0\n" ++
            "    let Green: Color {\n" ++
            "        return Color { r: 1.0 }\n" ++
            "    }\n" ++
            "    let Yellow: Color {\n" ++
            "        return Color { r: 2.0 }\n" ++
            "    }\n" ++
            "}\n" ++
            "function statusColor(status: ProjectStatus) -> Color {\n" ++
            "    if status == ProjectStatus.Active {\n" ++
            "        return Color.Green\n" ++
            "    }\n" ++
            "    return Color.Yellow\n" ++
            "}\n" ++
            "@Main function entry() {\n" ++
            "    let color = statusColor(ProjectStatus.Waiting)\n" ++
            "    let ignored = color.r\n" ++
            "    return\n" ++
            "}\n",
    );
}
