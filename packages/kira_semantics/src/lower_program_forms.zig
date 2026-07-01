//! Construct-form family lowering: building the form→family ancestry map, collecting `@Content`
//! field references, synthesizing the backing struct for a construct form, and rewriting content
//! field types to their existential (`some`) storage form. Extracted from `lower_program.zig` to
//! keep that file within the repository's file-size budget (Core Law #5). These helpers depend
//! only on the shared context, the semantics model, and sibling construct helpers — never on
//! `lower_program.zig` itself — so the dependency direction stays one-way.

const std = @import("std");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const requirements = @import("lower_construct_requirements.zig");
const form_surface = @import("construct_form_surface.zig");
const construct_members = @import("lower_construct_members.zig");

pub fn buildFormFamilies(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    form_parent: *const std.StringHashMapUnmanaged([]const u8),
    out: *std.StringHashMapUnmanaged([]const []const u8),
) !void {
    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        const parent_leaf = form_decl.construct_name.segments[form_decl.construct_name.segments.len - 1].text;
        const family = requirements.resolveFamilyConstructModel(constructs, construct_headers, form_parent, parent_leaf) orelse continue;
        var names = std.array_list.Managed([]const u8).init(allocator);
        try collectConstructAncestry(allocator, family, constructs, construct_headers, &names);
        try out.put(allocator, form_decl.name, try names.toOwnedSlice());
    }
}

fn collectConstructAncestry(
    allocator: std.mem.Allocator,
    construct_model: model.Construct,
    constructs: []const model.Construct,
    construct_headers: *const std.StringHashMapUnmanaged(shared.ConstructHeader),
    out: *std.array_list.Managed([]const u8),
) !void {
    for (out.items) |existing| {
        if (std.mem.eql(u8, existing, construct_model.name)) return;
    }
    try out.append(construct_model.name);
    for (construct_model.parents) |parent_link| {
        if (construct_headers.get(parent_link.name)) |header| {
            try collectConstructAncestry(allocator, constructs[header.index], constructs, construct_headers, out);
        }
    }
}

pub fn buildFormContentFields(
    ctx: *shared.Context,
    program: syntax.ast.Program,
    out: *std.StringHashMapUnmanaged([]const shared.ContentFieldRef),
) !void {
    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form_decl = decl.construct_form_decl;
        const body_members = try form_surface.effectiveMembers(ctx, form_decl);
        var fields = std.array_list.Managed(shared.ContentFieldRef).init(ctx.allocator);
        for (body_members) |member| {
            if (member != .field_decl) continue;
            const field = member.field_decl;
            if (!construct_members.hasContentAnnotation(field.annotations)) continue;
            try fields.append(.{
                .name = field.name,
                .is_list = field.type_expr != null and field.type_expr.?.* == .array,
            });
        }
        if (fields.items.len > 0) try out.put(ctx.allocator, form_decl.name, try fields.toOwnedSlice());
    }
}

pub fn synthesizeFormStruct(ctx: *shared.Context, form_decl: syntax.ast.ConstructFormDecl) !syntax.ast.TypeDecl {
    const body_members = try form_surface.effectiveMembers(ctx, form_decl);
    var members = std.array_list.Managed(syntax.ast.BodyMember).init(ctx.allocator);
    for (body_members) |member| {
        if (member != .field_decl) continue;
        const field = member.field_decl;
        // A computed `let node: T { ... }` is the bridge accessor, never stored state.
        if (field.body != null) continue;
        if (construct_members.hasContentAnnotation(field.annotations)) {
            // Caller-provided children are stored as `some Family` slots so heterogeneous widgets
            // coexist. Rewrite the family-typed field (`[Widget]`/`Widget`) to `some` form.
            var content_field = field;
            content_field.type_expr = if (field.type_expr) |type_expr| try existentializeContentType(ctx.allocator, type_expr) else null;
            try members.append(.{ .field_decl = content_field });
            continue;
        }
        try members.append(.{ .field_decl = field });
    }
    return .{
        .kind = .struct_decl,
        .annotations = &.{},
        .name = form_decl.name,
        .parents = &.{},
        .members = try members.toOwnedSlice(),
        .span = form_decl.span,
    };
}

// Rewrite an `@Content` field's family type to its existential `some` form:
// `[Widget]` -> `[some Widget]`, `Widget` -> `some Widget`. Content fields hold heterogeneous
// concrete constructs dispatched dynamically, so they use the existential `some` qualifier (not
// `any`, which is a monomorphized generic). A type already wrapped is left as-is.
fn existentializeContentType(allocator: std.mem.Allocator, type_expr: *syntax.ast.TypeExpr) !*syntax.ast.TypeExpr {
    switch (type_expr.*) {
        .array => |array| {
            const element = try existentializeContentType(allocator, array.element_type);
            const rewritten = try allocator.create(syntax.ast.TypeExpr);
            rewritten.* = .{ .array = .{ .element_type = element, .span = array.span } };
            return rewritten;
        },
        .named => |named| {
            const target = try allocator.create(syntax.ast.TypeExpr);
            target.* = .{ .named = named };
            const rewritten = try allocator.create(syntax.ast.TypeExpr);
            rewritten.* = .{ .any = .{ .target = target, .span = named.span, .existential = true } };
            return rewritten;
        },
        .any => |any_type| {
            // An author who already wrote `any Widget` / `some Widget` on an @Content field still
            // gets existential storage — force the flag so heterogeneous content never keeps
            // non-existential `any` (which a later phase would treat as a monomorphized generic).
            // Keep the inner target as-is; re-wrapping it would produce a nested `some some Widget`.
            if (any_type.existential) return type_expr;
            const rewritten = try allocator.create(syntax.ast.TypeExpr);
            rewritten.* = .{ .any = .{ .target = any_type.target, .span = any_type.span, .existential = true } };
            return rewritten;
        },
        else => return type_expr,
    }
}

pub fn formFamiliesFor(ctx: *shared.Context, form_name: []const u8) ![]const []const u8 {
    const families = ctx.form_families orelse return &.{};
    const list = families.get(form_name) orelse return &.{};
    const owned = try ctx.allocator.alloc([]const u8, list.len);
    for (list, 0..) |family, index| owned[index] = try ctx.allocator.dupe(u8, family);
    return owned;
}
