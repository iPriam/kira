const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const model = @import("kira_semantics_model");
const shared = @import("lower_shared.zig");

pub fn validateDirectFfiBoundary(
    ctx: *shared.Context,
    declaration_name: []const u8,
    declaration_header: shared.FunctionHeader,
    body: []const model.Statement,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) !void {
    if (declaration_header.execution == .native) return;
    // The VM executes direct FFI through LibFFI (see kira_vm_runtime/src/vm_ffi.zig),
    // so the @Native requirement is lifted for that target. Other backends still
    // require an @Native boundary to marshal the call.
    if (ctx.allow_runtime_direct_ffi) return;

    for (body) |statement| {
        if (findDirectFfiUseInStatement(statement, function_headers)) |use| {
            try emitDirectFfiRequiresNative(ctx, declaration_name, declaration_header.span, use);
            return error.DiagnosticsEmitted;
        }
    }
}

const DirectFfiUse = struct {
    symbol_name: []const u8,
    span: source_pkg.Span,
};

fn findDirectFfiUseInStatement(
    statement: model.Statement,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?DirectFfiUse {
    switch (statement) {
        .let_stmt => |node| if (node.value) |value| return findDirectFfiUseInExpr(value.*, function_headers),
        .assign_stmt => |node| {
            if (findDirectFfiUseInExpr(node.target.*, function_headers)) |use| return use;
            if (findDirectFfiUseInExpr(node.value.*, function_headers)) |use| return use;
        },
        .expr_stmt => |node| return findDirectFfiUseInExpr(node.expr.*, function_headers),
        .if_stmt => |node| {
            if (findDirectFfiUseInExpr(node.condition.*, function_headers)) |use| return use;
            if (findDirectFfiUseInStatements(node.then_body, function_headers)) |use| return use;
            if (node.else_body) |else_body| if (findDirectFfiUseInStatements(else_body, function_headers)) |use| return use;
        },
        .for_stmt => |node| {
            if (findDirectFfiUseInExpr(node.iterator.*, function_headers)) |use| return use;
            if (findDirectFfiUseInStatements(node.body, function_headers)) |use| return use;
        },
        .while_stmt => |node| {
            if (findDirectFfiUseInExpr(node.condition.*, function_headers)) |use| return use;
            if (findDirectFfiUseInStatements(node.body, function_headers)) |use| return use;
        },
        .match_stmt => |node| {
            if (findDirectFfiUseInExpr(node.subject.*, function_headers)) |use| return use;
            for (node.arms) |arm| {
                if (findDirectFfiUseInMatchPattern(arm.pattern, function_headers)) |use| return use;
                if (arm.guard) |guard| if (findDirectFfiUseInExpr(guard.*, function_headers)) |use| return use;
                if (findDirectFfiUseInStatements(arm.body, function_headers)) |use| return use;
            }
        },
        .switch_stmt => |node| {
            if (findDirectFfiUseInExpr(node.subject.*, function_headers)) |use| return use;
            for (node.cases) |case| {
                if (findDirectFfiUseInExpr(case.pattern.*, function_headers)) |use| return use;
                if (findDirectFfiUseInStatements(case.body, function_headers)) |use| return use;
            }
            if (node.default_body) |default_body| if (findDirectFfiUseInStatements(default_body, function_headers)) |use| return use;
        },
        .return_stmt => |node| if (node.value) |value| return findDirectFfiUseInExpr(value.*, function_headers),
        .break_stmt, .continue_stmt => {},
    }
    return null;
}

fn findDirectFfiUseInStatements(
    statements: []const model.Statement,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?DirectFfiUse {
    for (statements) |statement| {
        if (findDirectFfiUseInStatement(statement, function_headers)) |use| return use;
    }
    return null;
}

fn findDirectFfiUseInExpr(
    expr: model.Expr,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?DirectFfiUse {
    switch (expr) {
        .function_ref => |node| {
            if (externHeaderForFunctionRef(node, function_headers)) |header| {
                return .{
                    .symbol_name = directFfiSymbolName(node.name, header),
                    .span = node.span,
                };
            }
        },
        .c_string_to_string => |node| return findDirectFfiUseInExpr(node.value.*, function_headers),
        .array_len => |node| return findDirectFfiUseInExpr(node.object.*, function_headers),
        .string_len => |node| return findDirectFfiUseInExpr(node.object.*, function_headers),
        .field => |node| return findDirectFfiUseInExpr(node.object.*, function_headers),
        .native_state => |node| return findDirectFfiUseInExpr(node.value.*, function_headers),
        .native_user_data => |node| return findDirectFfiUseInExpr(node.state.*, function_headers),
        .native_recover => |node| return findDirectFfiUseInExpr(node.value.*, function_headers),
        .binary => |node| {
            if (findDirectFfiUseInExpr(node.lhs.*, function_headers)) |use| return use;
            if (findDirectFfiUseInExpr(node.rhs.*, function_headers)) |use| return use;
        },
        .unary => |node| return findDirectFfiUseInExpr(node.operand.*, function_headers),
        .cast => |node| return findDirectFfiUseInExpr(node.operand.*, function_headers),
        .conditional => |node| {
            if (findDirectFfiUseInExpr(node.condition.*, function_headers)) |use| return use;
            if (findDirectFfiUseInExpr(node.then_expr.*, function_headers)) |use| return use;
            if (findDirectFfiUseInExpr(node.else_expr.*, function_headers)) |use| return use;
        },
        .construct => |node| {
            for (node.fields) |field| {
                if (findDirectFfiUseInExpr(field.value.*, function_headers)) |use| return use;
            }
        },
        .construct_enum_variant => |node| {
            if (node.payload) |payload| return findDirectFfiUseInExpr(payload.*, function_headers);
        },
        .call => |node| {
            if (externHeaderForCall(node, function_headers)) |header| {
                return .{
                    .symbol_name = directFfiSymbolName(node.callee_name, header),
                    .span = node.span,
                };
            }
            for (node.args) |arg| {
                if (findDirectFfiUseInExpr(arg.*, function_headers)) |use| return use;
            }
            if (node.trailing_builder) |builder| {
                if (findDirectFfiUseInBuilder(builder, function_headers)) |use| return use;
            }
        },
        .virtual_call => |node| {
            if (findDirectFfiUseInExpr(node.receiver.*, function_headers)) |use| return use;
            for (node.args) |arg| {
                if (findDirectFfiUseInExpr(arg.*, function_headers)) |use| return use;
            }
        },
        .call_value => |node| {
            if (findDirectFfiUseInExpr(node.callee.*, function_headers)) |use| return use;
            for (node.args) |arg| {
                if (findDirectFfiUseInExpr(arg.*, function_headers)) |use| return use;
            }
        },
        .callback => |node| return findDirectFfiUseInStatements(node.body, function_headers),
        .array => |node| {
            for (node.elements) |element| {
                if (findDirectFfiUseInExpr(element.*, function_headers)) |use| return use;
            }
        },
        .builder_array => |node| return findDirectFfiUseInBuilder(node.builder, function_headers),
        .index => |node| {
            if (findDirectFfiUseInExpr(node.object.*, function_headers)) |use| return use;
            if (findDirectFfiUseInExpr(node.index.*, function_headers)) |use| return use;
        },
        .integer,
        .float,
        .string,
        .boolean,
        .null_ptr,
        .local,
        .namespace_ref,
        .parent_view,
        => {},
    }
    return null;
}

fn findDirectFfiUseInMatchPattern(
    pattern: model.MatchPattern,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?DirectFfiUse {
    return switch (pattern) {
        .variant => |node| if (node.inner) |inner| findDirectFfiUseInMatchPattern(inner.*, function_headers) else null,
        .binding => null,
    };
}

fn findDirectFfiUseInBuilder(
    builder: model.BuilderBlock,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?DirectFfiUse {
    for (builder.items) |item| {
        switch (item) {
            .expr => |node| if (findDirectFfiUseInExpr(node.expr.*, function_headers)) |use| return use,
            .if_item => |node| {
                if (findDirectFfiUseInExpr(node.condition.*, function_headers)) |use| return use;
                if (findDirectFfiUseInBuilder(node.then_block, function_headers)) |use| return use;
                if (node.else_block) |else_block| if (findDirectFfiUseInBuilder(else_block, function_headers)) |use| return use;
            },
            .for_item => |node| {
                if (findDirectFfiUseInExpr(node.iterator.*, function_headers)) |use| return use;
                if (findDirectFfiUseInBuilder(node.body, function_headers)) |use| return use;
            },
            .switch_item => |node| {
                if (findDirectFfiUseInExpr(node.subject.*, function_headers)) |use| return use;
                for (node.cases) |case| {
                    if (findDirectFfiUseInExpr(case.pattern.*, function_headers)) |use| return use;
                    if (findDirectFfiUseInBuilder(case.body, function_headers)) |use| return use;
                }
                if (node.default_block) |default_block| if (findDirectFfiUseInBuilder(default_block, function_headers)) |use| return use;
            },
        }
    }
    return null;
}

fn externHeaderForCall(
    call: model.hir.CallExpr,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?shared.FunctionHeader {
    if (externHeaderByName(function_headers, call.callee_name)) |header| return header;
    if (call.function_id) |function_id| {
        if (externHeaderById(function_headers, function_id)) |header| return header;
    }
    return null;
}

fn externHeaderForFunctionRef(
    function_ref: model.hir.FunctionRefExpr,
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
) ?shared.FunctionHeader {
    if (externHeaderByName(function_headers, function_ref.name)) |header| return header;
    return externHeaderById(function_headers, function_ref.function_id);
}

fn externHeaderById(
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    function_id: u32,
) ?shared.FunctionHeader {
    var iterator = function_headers.iterator();
    while (iterator.next()) |entry| {
        if (entry.value_ptr.id == function_id and entry.value_ptr.is_extern) return entry.value_ptr.*;
    }
    return null;
}

fn externHeaderByName(
    function_headers: *const std.StringHashMapUnmanaged(shared.FunctionHeader),
    name: []const u8,
) ?shared.FunctionHeader {
    const leaf = qualifiedLeaf(name);
    const header = function_headers.get(name) orelse function_headers.get(leaf) orelse return null;
    return if (header.is_extern) header else null;
}

fn directFfiSymbolName(callee_name: []const u8, header: shared.FunctionHeader) []const u8 {
    if (header.foreign) |foreign| return foreign.symbol_name;
    return qualifiedLeaf(callee_name);
}

fn qualifiedLeaf(name: []const u8) []const u8 {
    const index = std.mem.lastIndexOfScalar(u8, name, '.') orelse return name;
    return name[index + 1 ..];
}

fn emitDirectFfiRequiresNative(
    ctx: *shared.Context,
    declaration_name: []const u8,
    declaration_span: source_pkg.Span,
    use: DirectFfiUse,
) !void {
    try diagnostics.appendOwned(ctx.allocator, ctx.diagnostics, .{
        .severity = .@"error",
        .code = "KSEM093",
        .title = "direct FFI requires @Native",
        .message = try std.fmt.allocPrint(
            ctx.allocator,
            "The declaration '{s}' directly uses FFI-bound symbol '{s}', but the VM cannot execute FFI directly.",
            .{ declaration_name, use.symbol_name },
        ),
        .labels = &.{
            diagnostics.primaryLabel(use.span, "direct FFI-bound symbol use"),
            diagnostics.secondaryLabel(declaration_span, "this declaration is not marked @Native"),
        },
        .help = "Mark this declaration with @Native, or move the direct FFI use into a small @Native helper and call that helper instead.",
    });
}
