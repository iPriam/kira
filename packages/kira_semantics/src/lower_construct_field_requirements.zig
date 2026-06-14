const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const requirements = @import("lower_construct_requirements.zig");

// Validate that every construct-backed declaration satisfies the `@Required` *fields* of its
// construct family, applying the terminal-`node` rule: a required field need not be provided
// directly when the declaration overrides every default member that reads it. This is what lets
// a primitive widget supply `let node: Node { ... }` instead of `let body: Widget` while a
// composite widget supplies `body` and inherits the default `let node: Node { body.node }`.
//
// A self-referential required field (one whose type is the construct family itself, e.g.
// `body: Widget` in `construct Widget`) is also checked for direct infinite expansion: a
// declaration that provides `body` as a construction of itself, without overriding the default
// member that consumes it, would resolve forever and is rejected (KSEM141).

const FormMembers = struct {
    name: []const u8,
    parent_leaf: []const u8,
    provided: std.StringHashMapUnmanaged(void),
    fields: []const syntax.ast.FieldDecl,
    span: source_pkg.Span,
};

pub fn validateConstructFormFieldRequirements(
    ctx: *shared.Context,
    program: syntax.ast.Program,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    form_parent: *const std.StringHashMapUnmanaged([]const u8),
) !void {
    var forms = std.StringHashMapUnmanaged(FormMembers){};
    defer {
        var dit = forms.iterator();
        while (dit.next()) |entry| entry.value_ptr.provided.deinit(ctx.allocator);
        forms.deinit(ctx.allocator);
    }

    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        var provided = std.StringHashMapUnmanaged(void){};
        var fields = std.array_list.Managed(syntax.ast.FieldDecl).init(ctx.allocator);
        for (form_decl.body.members) |member| {
            switch (member) {
                .field_decl => |field| {
                    try provided.put(ctx.allocator, field.name, {});
                    try fields.append(field);
                },
                .function_decl => |function| try provided.put(ctx.allocator, function.name, {}),
                else => {},
            }
        }
        try forms.put(ctx.allocator, form_decl.name, .{
            .name = form_decl.name,
            .parent_leaf = form_decl.construct_name.segments[form_decl.construct_name.segments.len - 1].text,
            .provided = provided,
            .fields = try fields.toOwnedSlice(),
            .span = form_decl.span,
        });
    }

    var it = forms.iterator();
    while (it.next()) |entry| {
        try validateForm(ctx, entry.value_ptr.*, &forms, constructs, construct_headers, form_parent);
    }
}

fn validateForm(
    ctx: *shared.Context,
    form: FormMembers,
    forms: *const std.StringHashMapUnmanaged(FormMembers),
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    form_parent: *const std.StringHashMapUnmanaged([]const u8),
) !void {
    const family = requirements.resolveFamilyConstructModel(constructs, construct_headers, form_parent, form.parent_leaf) orelse return;

    var required = std.StringArrayHashMapUnmanaged(model.RequiredField){};
    defer required.deinit(ctx.allocator);
    var defaults = std.StringArrayHashMapUnmanaged(model.ConstructDefaultMember){};
    defer defaults.deinit(ctx.allocator);
    try collectRequiredFieldsAndDefaults(ctx, family, constructs, construct_headers, &required, &defaults);
    if (required.count() == 0) return;

    // Names provided by this declaration or any ancestor declaration in its chain.
    var provided = std.StringHashMapUnmanaged(void){};
    defer provided.deinit(ctx.allocator);
    try collectChainProvided(ctx, form.name, forms, construct_headers, &provided);

    for (required.values()) |req| {
        if (provided.contains(req.name)) {
            try checkSelfRecursion(ctx, form, family, req, defaults, &provided);
            continue;
        }
        if (!dischargedByOverride(req.name, defaults, &provided)) {
            try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
                .severity = .@"error",
                .code = "KSEM140",
                .title = "missing required member",
                .message = try std.fmt.allocPrint(ctx.allocator, "The declaration '{s}' must provide the required member '{s}' of construct '{s}', or override every member that produces it.", .{ form.name, req.name, family.name }),
                .labels = &.{diagnostics.primaryLabel(form.span, "required member is not provided")},
                .help = try terminalHelp(ctx, req.name, defaults),
            });
            return error.DiagnosticsEmitted;
        }
    }
}

// A required field is discharged without being provided when the declaration overrides every
// default member that reads it (and at least one such default member exists).
fn dischargedByOverride(
    req_name: []const u8,
    defaults: std.StringArrayHashMapUnmanaged(model.ConstructDefaultMember),
    provided: *const std.StringHashMapUnmanaged(void),
) bool {
    var any = false;
    for (defaults.values()) |member| {
        if (!references(member, req_name)) continue;
        any = true;
        if (!provided.contains(member.name)) return false;
    }
    return any;
}

fn references(member: model.ConstructDefaultMember, name: []const u8) bool {
    for (member.references) |ref| {
        if (std.mem.eql(u8, ref, name)) return true;
    }
    return false;
}

// A declaration that provides a self-referential required field (`body: Widget`) but relies on
// the default member that consumes it (does not override `node`) and defines that field as a
// construction of itself expands forever.
fn checkSelfRecursion(
    ctx: *shared.Context,
    form: FormMembers,
    family: model.Construct,
    req: model.RequiredField,
    defaults: std.StringArrayHashMapUnmanaged(model.ConstructDefaultMember),
    provided: *const std.StringHashMapUnmanaged(void),
) !void {
    if (!std.mem.eql(u8, req.type_text, family.name)) return;
    if (dischargedByOverride(req.name, defaults, provided)) return; // consumer overridden -> body unused
    const field = findField(form, req.name) orelse return;
    if (!fieldConstructs(field, form.name)) return;
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM141",
        .title = "recursive declaration expansion",
        .message = try std.fmt.allocPrint(ctx.allocator, "The declaration '{s}' defines '{s}' as itself, so resolving it through the construct's default member never terminates.", .{ form.name, req.name }),
        .labels = &.{diagnostics.primaryLabel(field.span, "this member expands to the declaration itself")},
        .help = "Provide a terminal member (such as an explicit `node`) or compose a different declaration.",
    });
    return error.DiagnosticsEmitted;
}

fn findField(form: FormMembers, name: []const u8) ?syntax.ast.FieldDecl {
    for (form.fields) |field| {
        if (std.mem.eql(u8, field.name, name)) return field;
    }
    return null;
}

fn fieldConstructs(field: syntax.ast.FieldDecl, form_name: []const u8) bool {
    if (field.value) |value| {
        if (exprConstructs(value, form_name)) return true;
    }
    if (field.body) |body| {
        if (blockConstructs(body, form_name)) return true;
    }
    return false;
}

fn blockConstructs(block: syntax.ast.Block, form_name: []const u8) bool {
    for (block.statements) |statement| {
        switch (statement) {
            .expr_stmt => |stmt| if (exprConstructs(stmt.expr, form_name)) return true,
            .return_stmt => |stmt| if (stmt.value) |value| {
                if (exprConstructs(value, form_name)) return true;
            },
            .let_stmt => |stmt| if (stmt.value) |value| {
                if (exprConstructs(value, form_name)) return true;
            },
            else => {},
        }
    }
    return false;
}

// True when an expression is, or is rooted at, a construction of `form_name` (a call or bare
// identifier with that name), looking through trailing modifiers (`Foo().padding(...)`).
fn exprConstructs(expr: *const syntax.ast.Expr, form_name: []const u8) bool {
    switch (expr.*) {
        .identifier => |ident| return ident.name.segments.len == 1 and std.mem.eql(u8, ident.name.segments[0].text, form_name),
        .call => |call| {
            if (calleeRoot(call.callee)) |root| {
                if (std.mem.eql(u8, root, form_name)) return true;
            }
            return exprConstructs(call.callee, form_name);
        },
        .member => |member| return exprConstructs(member.object, form_name),
        else => return false,
    }
}

fn calleeRoot(callee: *const syntax.ast.Expr) ?[]const u8 {
    return switch (callee.*) {
        .identifier => |ident| if (ident.name.segments.len == 1) ident.name.segments[0].text else null,
        else => null,
    };
}

fn terminalHelp(
    ctx: *shared.Context,
    req_name: []const u8,
    defaults: std.StringArrayHashMapUnmanaged(model.ConstructDefaultMember),
) ![]const u8 {
    for (defaults.values()) |member| {
        if (references(member, req_name)) {
            return std.fmt.allocPrint(ctx.allocator, "Provide '{s}', or override '{s}' to supply the value directly.", .{ req_name, member.name });
        }
    }
    return std.fmt.allocPrint(ctx.allocator, "Provide the required member '{s}'.", .{req_name});
}

// Collect a construct's transitive required fields and default members (its own plus all
// `extends` ancestors'). A nearer declaration's member overrides an inherited one by name.
fn collectRequiredFieldsAndDefaults(
    ctx: *shared.Context,
    construct_model: model.Construct,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    required: *std.StringArrayHashMapUnmanaged(model.RequiredField),
    defaults: *std.StringArrayHashMapUnmanaged(model.ConstructDefaultMember),
) !void {
    for (construct_model.parents) |parent_link| {
        if (construct_headers.get(parent_link.name)) |header| {
            try collectRequiredFieldsAndDefaults(ctx, constructs[header.index], constructs, construct_headers, required, defaults);
        }
    }
    for (construct_model.required_fields) |field| {
        try required.put(ctx.allocator, field.name, field);
    }
    for (construct_model.default_members) |member| {
        try defaults.put(ctx.allocator, member.name, member);
    }
}

fn collectChainProvided(
    ctx: *shared.Context,
    form_name: []const u8,
    forms: *const std.StringHashMapUnmanaged(FormMembers),
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    out: *std.StringHashMapUnmanaged(void),
) !void {
    var current = form_name;
    var hops: usize = 0;
    while (hops < 4096) : (hops += 1) {
        const info = forms.get(current) orelse return;
        var it = info.provided.iterator();
        while (it.next()) |entry| try out.put(ctx.allocator, entry.key_ptr.*, {});
        if (construct_headers.get(info.parent_leaf) != null) return;
        current = info.parent_leaf;
    }
}
