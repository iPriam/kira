const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const parent = @import("lower_program.zig");

const lowerFunction = parent.lowerFunction;

pub fn isTestForm(form_decl: syntax.ast.ConstructFormDecl) bool {
    const segments = form_decl.construct_name.segments;
    return segments.len != 0 and std.mem.eql(u8, segments[segments.len - 1].text, "Test");
}

pub fn registerTestSectionHeaders(
    ctx: *shared.Context,
    form_decl: syntax.ast.ConstructFormDecl,
    function_headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    if (!isTestForm(form_decl)) return;
    const sections = try findSections(ctx, form_decl);
    if (sections.test_rule) |rule| {
        try putHeader(ctx, function_headers, form_decl.name, "test", &.{}, .{ .kind = .unknown }, rule.span);
    }
    if (sections.expect_rule) |rule| {
        try putHeader(ctx, function_headers, form_decl.name, "expect", &.{}, .{ .kind = .unknown }, rule.span);
    }
}

pub fn lowerTestSections(
    ctx: *shared.Context,
    form_decl: syntax.ast.ConstructFormDecl,
    imports: []const model.Import,
    function_headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
    tests: *std.array_list.Managed(model.TestCase),
) ![]model.Function {
    if (!isTestForm(form_decl)) return &.{};
    const sections = try findSections(ctx, form_decl);
    const test_rule = sections.test_rule orelse {
        try emit(ctx, "KSEM148", "missing required test section", form_decl.span, "test section is missing", "Add a `test { ... }` section to this Test declaration.");
        return error.DiagnosticsEmitted;
    };
    const expect_rule = sections.expect_rule orelse {
        try emit(ctx, "KSEM149", "missing required expect section", form_decl.span, "expect section is missing", "Add an `expect { ... }` section to this Test declaration.");
        return error.DiagnosticsEmitted;
    };
    if (test_rule.block == null) {
        try emit(ctx, "KSEM150", "test section requires a body", test_rule.span, "test section has no body", "Write `test { return ... }`.");
        return error.DiagnosticsEmitted;
    }
    if (expect_rule.block == null) {
        try emit(ctx, "KSEM151", "expect section requires a body", expect_rule.span, "expect section has no body", "Write `expect { return result == ... }`.");
        return error.DiagnosticsEmitted;
    }

    var lowered = std.array_list.Managed(model.Function).init(ctx.allocator);
    const test_name = try memberName(ctx.allocator, form_decl.name, "test");
    const expect_name = try memberName(ctx.allocator, form_decl.name, "expect");

    const test_fn = try lowerFunction(ctx, .{
        .annotations = &.{},
        .name = test_name,
        .params = &.{},
        .return_type = null,
        .body = test_rule.block,
        .span = test_rule.span,
    }, imports, function_headers);
    if (function_headers.getPtr(test_name)) |header| header.return_type = test_fn.return_type;
    try lowered.append(test_fn);

    if (function_headers.getPtr(expect_name)) |header| {
        header.params = &.{};
        header.param_ownership = &.{};
        header.return_type = resultTypeFromResolved(ctx, test_fn.return_type);
    }
    const result_type = try resultTypeExpr(ctx, test_fn.return_type, expect_rule.span);
    const expect_fn = try lowerFunction(ctx, .{
        .annotations = &.{},
        .name = expect_name,
        .params = &.{},
        .return_type = result_type,
        .body = expect_rule.block,
        .span = expect_rule.span,
    }, imports, function_headers);
    try lowered.append(expect_fn);
    try tests.append(.{
        .name = try ctx.allocator.dupe(u8, form_decl.name),
        .test_function = test_name,
        .expect_function = expect_name,
        .result_type = test_fn.return_type,
        .span = form_decl.span,
    });
    return lowered.toOwnedSlice();
}

const Sections = struct {
    test_rule: ?syntax.ast.NamedRule = null,
    expect_rule: ?syntax.ast.NamedRule = null,
};

fn findSections(ctx: *shared.Context, form_decl: syntax.ast.ConstructFormDecl) !Sections {
    var found: Sections = .{};
    for (form_decl.body.members) |member| {
        if (member != .named_rule) continue;
        const rule = member.named_rule;
        if (rule.name.segments.len != 1) continue;
        const name = rule.name.segments[0].text;
        if (std.mem.eql(u8, name, "test")) {
            if (found.test_rule != null) {
                try emit(ctx, "KSEM152", "duplicate test section", rule.span, "duplicate test section", "Keep exactly one `test { ... }` section.");
                return error.DiagnosticsEmitted;
            }
            found.test_rule = rule;
        } else if (std.mem.eql(u8, name, "expect")) {
            if (found.expect_rule != null) {
                try emit(ctx, "KSEM153", "duplicate expect section", rule.span, "duplicate expect section", "Keep exactly one `expect { ... }` section.");
                return error.DiagnosticsEmitted;
            }
            found.expect_rule = rule;
        }
    }
    return found;
}

fn putHeader(
    ctx: *shared.Context,
    function_headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
    form_name: []const u8,
    section_name: []const u8,
    params: []const model.ResolvedType,
    return_type: model.ResolvedType,
    span: source_pkg.Span,
) !void {
    const name = try memberName(ctx.allocator, form_name, section_name);
    try function_headers.put(ctx.allocator, name, .{
        .id = @as(u32, @intCast(function_headers.count())),
        .params = params,
        .param_ownership = &.{},
        .execution = .inherited,
        .return_type = return_type,
        .span = span,
    });
}

fn typeExprFromResolved(ctx: *shared.Context, ty: model.ResolvedType, span: source_pkg.Span) !?*syntax.ast.TypeExpr {
    const text = try shared.typeTextFromResolved(ctx.allocator, ty);
    const segments = try ctx.allocator.alloc(syntax.ast.NameSegment, 1);
    segments[0] = .{ .text = text, .span = span };
    const expr = try ctx.allocator.create(syntax.ast.TypeExpr);
    expr.* = .{ .named = .{ .segments = segments, .span = span } };
    return expr;
}

fn resultTypeExpr(ctx: *shared.Context, value_type: model.ResolvedType, span: source_pkg.Span) !?*syntax.ast.TypeExpr {
    const value_expr = (try typeExprFromResolved(ctx, value_type, span)).?;
    const failure_expr = try namedTypeExpr(ctx, "TestFailure", span);
    const args = try ctx.allocator.alloc(*syntax.ast.TypeExpr, 2);
    args[0] = value_expr;
    args[1] = failure_expr;
    const base_segments = try ctx.allocator.alloc(syntax.ast.NameSegment, 1);
    base_segments[0] = .{ .text = "Result", .span = span };
    const expr = try ctx.allocator.create(syntax.ast.TypeExpr);
    expr.* = .{ .generic = .{
        .base = .{ .segments = base_segments, .span = span },
        .args = args,
        .span = span,
    } };
    return expr;
}

fn namedTypeExpr(ctx: *shared.Context, name: []const u8, span: source_pkg.Span) !*syntax.ast.TypeExpr {
    const segments = try ctx.allocator.alloc(syntax.ast.NameSegment, 1);
    segments[0] = .{ .text = name, .span = span };
    const expr = try ctx.allocator.create(syntax.ast.TypeExpr);
    expr.* = .{ .named = .{ .segments = segments, .span = span } };
    return expr;
}

fn resultTypeFromResolved(ctx: *shared.Context, value_type: model.ResolvedType) model.ResolvedType {
    return .{
        .kind = .enum_instance,
        .name = std.fmt.allocPrint(ctx.allocator, "Result__{s}__TestFailure", .{
            shared.typeTextFromResolved(ctx.allocator, value_type) catch "Unknown",
        }) catch "Result__Unknown__TestFailure",
    };
}

fn memberName(allocator: std.mem.Allocator, form_name: []const u8, member: []const u8) ![]const u8 {
    // `Foo__test` / `Foo__expect` are valid Kira identifiers (no `.`), so the
    // synthesized pure-Kira test driver can call them directly. The Zig test
    // runner reconstructs the same `{name}__{member}` form.
    return std.fmt.allocPrint(allocator, "{s}__{s}", .{ form_name, member });
}

fn emit(ctx: *shared.Context, code: []const u8, title: []const u8, span: source_pkg.Span, label: []const u8, help: []const u8) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = code,
        .title = title,
        .message = title,
        .labels = &.{diagnostics.primaryLabel(span, label)},
        .help = help,
    });
}
