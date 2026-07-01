const std = @import("std");
const source_pkg = @import("kira_source");
const syntax = @import("kira_syntax_model");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");
const type_impl = @import("lower_program_types.zig");
const parent = @import("lower_program.zig");

const lowerFunction = parent.lowerFunction;

pub fn registerExtendFunctionHeaders(
    ctx: *shared.Context,
    extend_decl: syntax.ast.ExtendDecl,
    function_headers: *std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    const family = extend_decl.construct_name.segments[extend_decl.construct_name.segments.len - 1].text;
    for (extend_decl.members) |member| {
        if (member != .function_decl) continue;
        const function_decl = member.function_decl;
        var param_types = std.array_list.Managed(model.ResolvedType).init(ctx.allocator);
        var param_ownership = std.array_list.Managed(model.OwnershipMode).init(ctx.allocator);
        var param_defaults = std.array_list.Managed(?*syntax.ast.Expr).init(ctx.allocator);
        try param_types.append(try constructAnyResolvedType(ctx, family));
        try param_ownership.append(.borrow_read);
        try param_defaults.append(null);
        for (function_decl.params) |param| {
            try param_ownership.append(shared.ownershipModeFromSyntax(param.type_expr));
            try param_defaults.append(param.default_value);
            if (param.type_expr) |type_expr| {
                try param_types.append(try shared.typeFromSyntaxChecked(ctx, type_expr.*));
            } else {
                try param_types.append(.{ .kind = .unknown });
            }
        }
        const full_name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ family, function_decl.name });
        try function_headers.put(ctx.allocator, full_name, .{
            .id = @as(u32, @intCast(function_headers.count())),
            .params = try param_types.toOwnedSlice(),
            .param_ownership = try param_ownership.toOwnedSlice(),
            .param_defaults = try param_defaults.toOwnedSlice(),
            .execution = .inherited,
            .return_type = if (function_decl.return_type) |return_type| try shared.typeFromSyntaxChecked(ctx, return_type.*) else .{ .kind = .unknown },
            .return_ownership = shared.ownershipModeFromSyntax(function_decl.return_type),
            .span = function_decl.span,
        });
    }
}

pub fn registerExtendMethods(
    ctx: *shared.Context,
    extend_decl: syntax.ast.ExtendDecl,
    type_headers: *std.StringHashMapUnmanaged(shared.TypeHeader),
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    const family = extend_decl.construct_name.segments[extend_decl.construct_name.segments.len - 1].text;
    const families = ctx.form_families orelse return;
    var iterator = families.iterator();
    while (iterator.next()) |entry| {
        if (!familyListContains(entry.value_ptr.*, family)) continue;
        var header = type_headers.get(entry.key_ptr.*) orelse continue;
        var methods = std.array_list.Managed(shared.MethodMember).init(ctx.allocator);
        try methods.appendSlice(header.methods);
        for (extend_decl.members) |member| {
            if (member != .function_decl) continue;
            const function_decl = member.function_decl;
            if (type_impl.methodNameExists(methods.items, function_decl.name)) continue;
            const full_name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ family, function_decl.name });
            const function_header = function_headers.get(full_name) orelse continue;
            try methods.append(.{
                .name = try ctx.allocator.dupe(u8, function_decl.name),
                .full_name = full_name,
                .receiver_type_name = try ctx.allocator.dupe(u8, entry.key_ptr.*),
                .receiver_offset = 0,
                .params = if (function_header.params.len > 0) function_header.params[1..] else &.{},
                .param_ownership = if (function_header.param_ownership.len > 0) function_header.param_ownership[1..] else &.{},
                .return_type = function_header.return_type,
                .return_ownership = function_header.return_ownership,
                .span = function_decl.span,
            });
        }
        header.methods = try methods.toOwnedSlice();
        try type_headers.put(ctx.allocator, entry.key_ptr.*, header);
    }
}

pub fn lowerExtendFunctions(
    ctx: *shared.Context,
    extend_decl: syntax.ast.ExtendDecl,
    imports: []const model.Import,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ![]model.Function {
    const family = extend_decl.construct_name.segments[extend_decl.construct_name.segments.len - 1].text;
    var functions = std.array_list.Managed(model.Function).init(ctx.allocator);
    for (extend_decl.members) |member| {
        if (member != .function_decl) continue;
        const function_decl = member.function_decl;
        var params = std.array_list.Managed(syntax.ast.ParamDecl).init(ctx.allocator);
        try params.append(.{
            .annotations = &.{},
            .name = "self",
            .type_expr = try constructExistentialTypeExpr(ctx, family, extend_decl.construct_name.span),
            .span = function_decl.span,
        });
        try params.appendSlice(function_decl.params);
        try functions.append(try lowerFunction(ctx, .{
            .annotations = function_decl.annotations,
            .name = try std.fmt.allocPrint(ctx.allocator, "{s}.{s}", .{ family, function_decl.name }),
            .params = try params.toOwnedSlice(),
            .return_type = function_decl.return_type,
            .body = function_decl.body,
            .span = function_decl.span,
        }, imports, function_headers));
    }
    return functions.toOwnedSlice();
}

fn constructAnyResolvedType(ctx: *shared.Context, family: []const u8) !model.ResolvedType {
    return .{
        .kind = .construct_any,
        .name = try std.fmt.allocPrint(ctx.allocator, "any {s}", .{family}),
        .construct_constraint = .{ .construct_name = try ctx.allocator.dupe(u8, family) },
    };
}

fn constructExistentialTypeExpr(ctx: *shared.Context, family: []const u8, span: source_pkg.Span) !*syntax.ast.TypeExpr {
    // The synthesized `self` of an `extend Family` modifier dispatches dynamically over the whole
    // construct family, so it is an existential (`some Family`) — mark it so a later phase that
    // gives `any` monomorphized-generic meaning does not misclassify it.
    const target = try ctx.allocator.create(syntax.ast.TypeExpr);
    const segments = try ctx.allocator.alloc(syntax.ast.NameSegment, 1);
    segments[0] = .{ .text = family, .span = span };
    target.* = .{ .named = .{ .segments = segments, .span = span } };
    const existential_ty = try ctx.allocator.create(syntax.ast.TypeExpr);
    existential_ty.* = .{ .any = .{ .target = target, .span = span, .existential = true } };
    return existential_ty;
}

fn familyListContains(families: []const []const u8, family: []const u8) bool {
    for (families) |candidate| {
        if (std.mem.eql(u8, candidate, family)) return true;
    }
    return false;
}
