const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const shared = @import("lower_shared.zig");
const members = @import("lower_construct_members.zig");

// Validate caller-provided content at construct-form construction sites. A concrete declaration
// declares its caller-provided children as `@Content` fields; a trailing `{ ... }` block on a
// construction of that declaration fills those fields. Field names are the channel names — there
// are no string-labeled channels.
//
// Rules (all checked here, before any backend):
//   * A trailing block on a declaration with no `@Content` field is rejected (KSEM142).
//   * A single `@Content let x: Widget` field takes exactly one child; `[Widget]` takes many
//     in source order. A bad count is rejected (KSEM143).
//   * With multiple `@Content` fields, a bare block is ambiguous; children must be supplied as
//     named fills using the field names (`header { ... } content { ... }`) (KSEM144).
//   * A content child must be a widget-producing expression, not a literal value (KSEM145).
//
// This is a validation pass: the construction itself is not lowered to a runtime value yet, so
// it runs over the composition bodies (`let body`/`let node { ... }`) where these calls appear.

const ContentField = struct {
    name: []const u8,
    is_list: bool,
    span: source_pkg.Span,
};

const FormContent = struct {
    fields: []const ContentField,
};

pub fn validateWidgetContent(ctx: *shared.Context, program: syntax.ast.Program) !void {
    var forms = std.StringHashMapUnmanaged(FormContent){};
    defer forms.deinit(ctx.allocator);

    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        var fields = std.array_list.Managed(ContentField).init(ctx.allocator);
        for (form_decl.body.members) |member| {
            if (member != .field_decl) continue;
            const field = member.field_decl;
            if (!hasContent(field.annotations)) continue;
            try fields.append(.{
                .name = field.name,
                .is_list = field.type_expr != null and field.type_expr.?.* == .array,
                .span = field.span,
            });
        }
        try forms.put(ctx.allocator, form_decl.name, .{ .fields = try fields.toOwnedSlice() });
    }

    // Walk every composition body for construction calls that carry a trailing content block:
    // declaration members (`let body`/`let node { ... }`) and fluent modifier bodies (`extend`).
    for (program.decls) |decl| {
        const member_list: []const syntax.ast.BodyMember = switch (decl) {
            .construct_form_decl => |form_decl| form_decl.body.members,
            .extend_decl => |extend_decl| extend_decl.members,
            else => continue,
        };
        for (member_list) |member| {
            switch (member) {
                .field_decl => |field| if (field.body) |body| try walkBlock(ctx, body, &forms),
                .function_decl => |function| if (function.body) |body| try walkBlock(ctx, body, &forms),
                else => {},
            }
        }
    }
}

fn hasContent(annotations: []const syntax.ast.Annotation) bool {
    for (annotations) |annotation| {
        if (members.isAnnotation(annotation, "Content")) return true;
    }
    return false;
}

fn walkBlock(ctx: *shared.Context, block: syntax.ast.Block, forms: *const std.StringHashMapUnmanaged(FormContent)) !void {
    for (block.statements) |statement| try walkStatement(ctx, statement, forms);
}

fn walkStatement(ctx: *shared.Context, statement: syntax.ast.Statement, forms: *const std.StringHashMapUnmanaged(FormContent)) anyerror!void {
    switch (statement) {
        .let_stmt => |stmt| if (stmt.value) |value| try walkExpr(ctx, value, forms),
        .assign_stmt => |stmt| {
            try walkExpr(ctx, stmt.target, forms);
            try walkExpr(ctx, stmt.value, forms);
        },
        .expr_stmt => |stmt| try walkExpr(ctx, stmt.expr, forms),
        .return_stmt => |stmt| if (stmt.value) |value| try walkExpr(ctx, value, forms),
        .if_stmt => |stmt| {
            try walkExpr(ctx, stmt.condition, forms);
            try walkBlock(ctx, stmt.then_block, forms);
            if (stmt.else_block) |else_block| try walkBlock(ctx, else_block, forms);
        },
        .for_stmt => |stmt| try walkBlock(ctx, stmt.body, forms),
        .while_stmt => |stmt| try walkBlock(ctx, stmt.body, forms),
        else => {},
    }
}

fn walkExpr(ctx: *shared.Context, expr: *const syntax.ast.Expr, forms: *const std.StringHashMapUnmanaged(FormContent)) anyerror!void {
    switch (expr.*) {
        .call => |call| {
            if (calleeName(call.callee)) |name| {
                if (forms.get(name)) |content| {
                    if (call.trailing_builder) |block| {
                        try validateContentBlock(ctx, name, content, block, forms);
                    }
                }
            }
            try walkExpr(ctx, call.callee, forms);
            for (call.args) |arg| try walkExpr(ctx, arg.value, forms);
        },
        // An empty `Foo {}` parses as a struct literal, not a trailing-builder call. When `Foo`
        // is a declaration with `@Content` fields, that is an empty content block and must obey
        // the same arity rules (e.g. a single `Widget` field needs exactly one child).
        .struct_literal => |literal| {
            if (literal.fields.len == 0) {
                if (calleeNameFromQualified(literal.type_name)) |name| {
                    if (forms.get(name)) |content| {
                        if (content.fields.len > 0) {
                            try validateContentBlock(ctx, name, content, .{ .items = &.{}, .span = literal.span }, forms);
                        }
                    }
                }
            }
            for (literal.fields) |field| try walkExpr(ctx, field.value, forms);
        },
        .member => |member| try walkExpr(ctx, member.object, forms),
        .binary => |binary| {
            try walkExpr(ctx, binary.lhs, forms);
            try walkExpr(ctx, binary.rhs, forms);
        },
        .unary => |unary| try walkExpr(ctx, unary.operand, forms),
        .index => |index| {
            try walkExpr(ctx, index.object, forms);
            try walkExpr(ctx, index.index, forms);
        },
        .conditional => |conditional| {
            try walkExpr(ctx, conditional.condition, forms);
            try walkExpr(ctx, conditional.then_expr, forms);
            try walkExpr(ctx, conditional.else_expr, forms);
        },
        .array => |array| for (array.elements) |element| try walkExpr(ctx, element, forms),
        else => {},
    }
}

fn validateContentBlock(
    ctx: *shared.Context,
    form_name: []const u8,
    content: FormContent,
    block: syntax.ast.BuilderBlock,
    forms: *const std.StringHashMapUnmanaged(FormContent),
) !void {
    if (content.fields.len == 0) {
        try emit(ctx, "KSEM142", "unexpected content block", block.span, try std.fmt.allocPrint(ctx.allocator, "The declaration '{s}' has no `@Content` field, so it cannot take a trailing content block.", .{form_name}), "Remove the block, or add an `@Content` field to the declaration.");
        return error.DiagnosticsEmitted;
    }

    if (content.fields.len == 1) {
        try validateSingleField(ctx, form_name, content.fields[0], block, forms);
        return;
    }

    // Multiple `@Content` fields require named fills using the field names.
    try validateNamedFills(ctx, form_name, content, block, forms);
}

fn validateSingleField(
    ctx: *shared.Context,
    form_name: []const u8,
    field: ContentField,
    block: syntax.ast.BuilderBlock,
    forms: *const std.StringHashMapUnmanaged(FormContent),
) !void {
    var count: usize = 0;
    for (block.items) |item| {
        if (item != .expr) continue;
        count += 1;
        try requireWidgetChild(ctx, item.expr.expr, forms);
    }
    if (!field.is_list and count != 1) {
        try emit(ctx, "KSEM143", "content count mismatch", block.span, try std.fmt.allocPrint(ctx.allocator, "The content field '{s}' of '{s}' is a single `Widget`, so it takes exactly one child, but the block has {d}.", .{ field.name, form_name, count }), "Provide exactly one child, or make the field a `[Widget]` list.");
        return error.DiagnosticsEmitted;
    }
}

fn validateNamedFills(
    ctx: *shared.Context,
    form_name: []const u8,
    content: FormContent,
    block: syntax.ast.BuilderBlock,
    forms: *const std.StringHashMapUnmanaged(FormContent),
) !void {
    for (block.items) |item| {
        const fill_call: ?syntax.ast.CallExpr = if (item == .expr and item.expr.expr.* == .call) item.expr.expr.*.call else null;
        const fill_name: ?[]const u8 = if (fill_call) |call| calleeName(call.callee) else null;
        if (fill_call == null or fill_name == null or !isContentField(content, fill_name.?)) {
            try emit(ctx, "KSEM144", "ambiguous content block", block.span, try std.fmt.allocPrint(ctx.allocator, "The declaration '{s}' has multiple `@Content` fields, so children must be named fills using the field names.", .{form_name}), "Wrap children in named blocks, for example `header { ... }` and `content { ... }`.");
            return error.DiagnosticsEmitted;
        }
        // Validate the named fill's own children against that field.
        const field = contentField(content, fill_name.?).?;
        if (fill_call.?.trailing_builder) |inner| {
            try validateSingleField(ctx, form_name, field, inner, forms);
        }
    }
}

fn requireWidgetChild(ctx: *shared.Context, expr: *const syntax.ast.Expr, forms: *const std.StringHashMapUnmanaged(FormContent)) !void {
    _ = forms;
    if (!isWidgetProducing(expr)) {
        try emit(ctx, "KSEM145", "non-widget content child", exprSpan(expr.*), "A content child must be a widget-producing expression, not a plain value.", "Provide a widget such as `Text(...)`, not a literal.");
        return error.DiagnosticsEmitted;
    }
}

// A widget-producing expression is a construction/call, a reference (assumed widget-typed), or a
// modifier chain rooted at one. A bare literal value is never a widget.
fn isWidgetProducing(expr: *const syntax.ast.Expr) bool {
    return switch (expr.*) {
        .call, .identifier, .member => true,
        else => false,
    };
}

fn isContentField(content: FormContent, name: []const u8) bool {
    return contentField(content, name) != null;
}

fn contentField(content: FormContent, name: []const u8) ?ContentField {
    for (content.fields) |field| {
        if (std.mem.eql(u8, field.name, name)) return field;
    }
    return null;
}

fn calleeName(callee: *const syntax.ast.Expr) ?[]const u8 {
    return switch (callee.*) {
        .identifier => |ident| if (ident.name.segments.len == 1) ident.name.segments[0].text else null,
        else => null,
    };
}

fn calleeNameFromQualified(name: syntax.ast.QualifiedName) ?[]const u8 {
    return if (name.segments.len == 1) name.segments[0].text else null;
}

fn emit(ctx: *shared.Context, code: []const u8, title: []const u8, span: source_pkg.Span, message: []const u8, help: []const u8) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = code,
        .title = title,
        .message = message,
        .labels = &.{diagnostics.primaryLabel(span, title)},
        .help = help,
    });
}

fn exprSpan(expr: syntax.ast.Expr) source_pkg.Span {
    return switch (expr) {
        inline else => |node| node.span,
    };
}
