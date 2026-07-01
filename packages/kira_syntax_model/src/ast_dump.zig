const std = @import("std");
const ast = @import("ast.zig");

pub fn dumpProgram(writer: anytype, program: ast.Program) !void {
    try writer.writeAll("Program\n");
    for (program.imports) |import_decl| {
        try indent(writer, 1);
        try writer.print("Import {s}\n", .{qualifiedNameText(import_decl.module_name)});
    }
    for (program.decls) |decl| {
        try dumpDecl(writer, decl, 1);
    }
}

fn dumpDecl(writer: anytype, decl: ast.Decl, depth: usize) anyerror!void {
    switch (decl) {
        .annotation_decl => |annotation_decl| {
            try indent(writer, depth);
            try writer.print("Annotation {s}\n", .{annotation_decl.name});
        },
        .capability_decl => |capability_decl| {
            try indent(writer, depth);
            try writer.print("Capability {s}\n", .{capability_decl.name});
        },
        .enum_decl => |enum_decl| {
            try indent(writer, depth);
            try writer.print("Enum {s}\n", .{enum_decl.name});
            for (enum_decl.variants) |variant_decl| {
                try indent(writer, depth + 1);
                try writer.print("Variant {s}\n", .{variant_decl.name});
            }
        },
        .function_decl => |function_decl| {
            try indent(writer, depth);
            try writer.print("Function {s}\n", .{function_decl.name});
            if (function_decl.body) |body| {
                try dumpBlock(writer, body, depth + 1);
            }
        },
        .type_decl => |type_decl| {
            try indent(writer, depth);
            try writer.print("{s} {s}\n", .{ typeKindLabel(type_decl.kind), type_decl.name });
            for (type_decl.members) |member| try dumpBodyMember(writer, member, depth + 1);
        },
        .construct_decl => |construct_decl| {
            try indent(writer, depth);
            try writer.print("Construct {s}\n", .{construct_decl.name});
            for (construct_decl.sections) |section| {
                try indent(writer, depth + 1);
                try writer.print("Section {s}\n", .{section.name});
            }
            for (construct_decl.members) |member| try dumpBodyMember(writer, member, depth + 1);
        },
        .construct_form_decl => |form_decl| {
            try indent(writer, depth);
            try writer.print("ConstructDecl {s} {s}\n", .{ qualifiedNameText(form_decl.construct_name), form_decl.name });
            for (form_decl.body.members) |member| try dumpBodyMember(writer, member, depth + 1);
        },
        .extend_decl => |extend_decl| {
            try indent(writer, depth);
            try writer.print("Extend {s}\n", .{qualifiedNameText(extend_decl.construct_name)});
            for (extend_decl.members) |member| try dumpBodyMember(writer, member, depth + 1);
        },
        .macro_decl => |macro_decl| {
            try indent(writer, depth);
            try writer.print("Macro {s}\n", .{macro_decl.name});
            if (macro_decl.expand_block) |block| try dumpBlock(writer, block, depth + 1);
        },
        .macro_invocation => |call| {
            try indent(writer, depth);
            try writer.writeAll("MacroInvocation\n");
            try dumpExpr(writer, .{ .call = call }, depth + 1);
        },
    }
}

fn dumpBodyMember(writer: anytype, member: ast.BodyMember, depth: usize) anyerror!void {
    switch (member) {
        .field_decl => |field_decl| {
            try indent(writer, depth);
            try writer.print("Field {s} {s}\n", .{
                @tagName(field_decl.storage),
                field_decl.name,
            });
        },
        .function_decl => |function_decl| {
            try indent(writer, depth);
            try writer.print("Function {s}\n", .{function_decl.name});
            if (function_decl.body) |body| {
                try dumpBlock(writer, body, depth + 1);
            }
        },
        .content_section => |content| {
            try indent(writer, depth);
            try writer.writeAll("Content\n");
            try dumpBuilderBlock(writer, content.builder, depth + 1);
        },
        .properties_section => |properties| {
            try indent(writer, depth);
            try writer.writeAll("Properties\n");
            for (properties.entries) |entry| {
                try indent(writer, depth + 1);
                try writer.print("{s}\n", .{entry.name});
            }
        },
        .lifecycle_hook => |hook| {
            try indent(writer, depth);
            try writer.print("Lifecycle {s}\n", .{hook.name});
            try dumpBlock(writer, hook.body, depth + 1);
        },
        .named_rule => |rule| {
            try indent(writer, depth);
            try writer.print("Rule {s}\n", .{qualifiedNameText(rule.name)});
        },
    }
}

fn indent(writer: anytype, depth: usize) !void {
    for (0..depth) |_| try writer.writeAll("  ");
}

fn typeKindLabel(kind: ast.TypeKind) []const u8 {
    return switch (kind) {
        .class => "Class",
        .struct_decl => "Struct",
    };
}

fn dumpBlock(writer: anytype, block: ast.Block, depth: usize) anyerror!void {
    try indent(writer, depth);
    try writer.writeAll("Block\n");
    for (block.statements) |statement| try dumpStatement(writer, statement, depth + 1);
}

fn dumpStatement(writer: anytype, statement: ast.Statement, depth: usize) anyerror!void {
    switch (statement) {
        .let_stmt => |let_stmt| {
            try indent(writer, depth);
            try writer.print("Let {s}\n", .{let_stmt.name});
            if (let_stmt.value) |value| try dumpExpr(writer, value.*, depth + 1);
        },
        .assign_stmt => |assign_stmt| {
            try indent(writer, depth);
            try writer.writeAll("Assign\n");
            try dumpExpr(writer, assign_stmt.target.*, depth + 1);
            try dumpExpr(writer, assign_stmt.value.*, depth + 1);
        },
        .expr_stmt => |expr_stmt| {
            try indent(writer, depth);
            try writer.writeAll("ExprStmt\n");
            try dumpExpr(writer, expr_stmt.expr.*, depth + 1);
        },
        .return_stmt => |return_stmt| {
            try indent(writer, depth);
            try writer.writeAll("Return\n");
            if (return_stmt.value) |value| try dumpExpr(writer, value.*, depth + 1);
        },
        .if_stmt => |if_stmt| {
            try indent(writer, depth);
            try writer.writeAll("If\n");
            try dumpExpr(writer, if_stmt.condition.*, depth + 1);
            try dumpBlock(writer, if_stmt.then_block, depth + 1);
        },
        .for_stmt => |for_stmt| {
            try indent(writer, depth);
            try writer.print("For {s}\n", .{for_stmt.binding_name});
            try dumpExpr(writer, for_stmt.iterator.*, depth + 1);
            try dumpBlock(writer, for_stmt.body, depth + 1);
        },
        .while_stmt => |while_stmt| {
            try indent(writer, depth);
            try writer.writeAll("While\n");
            try dumpExpr(writer, while_stmt.condition.*, depth + 1);
            try dumpBlock(writer, while_stmt.body, depth + 1);
        },
        .break_stmt => {
            try indent(writer, depth);
            try writer.writeAll("Break\n");
        },
        .continue_stmt => {
            try indent(writer, depth);
            try writer.writeAll("Continue\n");
        },
        .match_stmt => |match_stmt| {
            try indent(writer, depth);
            try writer.writeAll("Match\n");
            try dumpExpr(writer, match_stmt.subject.*, depth + 1);
        },
        .switch_stmt => |switch_stmt| {
            try indent(writer, depth);
            try writer.writeAll("Switch\n");
            try dumpExpr(writer, switch_stmt.subject.*, depth + 1);
        },
        .attempt_stmt => |attempt_stmt| {
            try indent(writer, depth);
            try writer.writeAll("Attempt\n");
            for (attempt_stmt.body) |body_stmt| try dumpStatement(writer, body_stmt, depth + 1);
            for (attempt_stmt.handlers) |handler| {
                try indent(writer, depth + 1);
                try writer.print("Handle {s}\n", .{handler.variant_name});
                try dumpBlock(writer, handler.body, depth + 2);
            }
        },
    }
}

fn dumpBuilderBlock(writer: anytype, block: ast.BuilderBlock, depth: usize) anyerror!void {
    try indent(writer, depth);
    try writer.writeAll("Builder\n");
    for (block.items) |item| {
        switch (item) {
            .expr => |value| {
                try indent(writer, depth + 1);
                try writer.writeAll("BuilderExpr\n");
                try dumpExpr(writer, value.expr.*, depth + 2);
            },
            .if_item => {
                try indent(writer, depth + 1);
                try writer.writeAll("BuilderIf\n");
            },
            .for_item => {
                try indent(writer, depth + 1);
                try writer.writeAll("BuilderFor\n");
            },
            .switch_item => {
                try indent(writer, depth + 1);
                try writer.writeAll("BuilderSwitch\n");
            },
        }
    }
}

fn dumpExpr(writer: anytype, expr: ast.Expr, depth: usize) anyerror!void {
    switch (expr) {
        .integer => |value| {
            try indent(writer, depth);
            try writer.print("Int {d}\n", .{value.value});
        },
        .float => |value| {
            try indent(writer, depth);
            try writer.print("Float {d}\n", .{value.value});
        },
        .string => |value| {
            try indent(writer, depth);
            try writer.print("String \"{s}\"\n", .{value.value});
        },
        .builder_array => |value| {
            try indent(writer, depth);
            try writer.writeAll("BuilderArray\n");
            try dumpBuilderBlock(writer, value.builder, depth + 1);
        },
        .bool => |value| {
            try indent(writer, depth);
            try writer.print("Bool {}\n", .{value.value});
        },
        .identifier => |value| {
            try indent(writer, depth);
            try writer.print("Identifier {s}\n", .{qualifiedNameText(value.name)});
        },
        .array => |value| {
            try indent(writer, depth);
            try writer.writeAll("Array\n");
            for (value.elements) |element| try dumpExpr(writer, element.*, depth + 1);
        },
        .callback => |value| {
            try indent(writer, depth);
            try writer.writeAll("CallbackLiteral\n");
            if (value.params.len != 0) {
                try indent(writer, depth + 1);
                try writer.writeAll("Params\n");
                for (value.params) |param| {
                    try indent(writer, depth + 2);
                    try writer.print("{s}\n", .{param.name});
                }
            }
            try dumpBlock(writer, value.body, depth + 1);
        },
        .struct_literal => |value| {
            try indent(writer, depth);
            try writer.print("StructLiteral {s}\n", .{qualifiedNameText(value.type_name)});
            for (value.fields) |field| {
                try indent(writer, depth + 1);
                try writer.print("Field {s}\n", .{field.name});
                try dumpExpr(writer, field.value.*, depth + 2);
            }
        },
        .native_state => |value| {
            try indent(writer, depth);
            try writer.writeAll("NativeState\n");
            try dumpExpr(writer, value.value.*, depth + 1);
        },
        .native_user_data => |value| {
            try indent(writer, depth);
            try writer.writeAll("NativeUserData\n");
            try dumpExpr(writer, value.state.*, depth + 1);
        },
        .native_recover => |value| {
            try indent(writer, depth);
            try writer.writeAll("NativeRecover\n");
            try indent(writer, depth + 1);
            try writer.writeAll("Type ");
            try dumpTypeExprInline(writer, value.state_type.*);
            try writer.writeAll("\n");
            try dumpExpr(writer, value.value.*, depth + 1);
        },
        .ownership => |value| {
            try indent(writer, depth);
            try writer.print("Ownership {s}\n", .{@tagName(value.op)});
            try dumpExpr(writer, value.operand.*, depth + 1);
        },
        .unary => |value| {
            try indent(writer, depth);
            try writer.print("Unary {s}\n", .{@tagName(value.op)});
            try dumpExpr(writer, value.operand.*, depth + 1);
        },
        .binary => |value| {
            try indent(writer, depth);
            try writer.print("Binary {s}\n", .{@tagName(value.op)});
            try dumpExpr(writer, value.lhs.*, depth + 1);
            try dumpExpr(writer, value.rhs.*, depth + 1);
        },
        .conditional => |value| {
            try indent(writer, depth);
            try writer.writeAll("Conditional\n");
            try dumpExpr(writer, value.condition.*, depth + 1);
            try dumpExpr(writer, value.then_expr.*, depth + 1);
            try dumpExpr(writer, value.else_expr.*, depth + 1);
        },
        .member => |value| {
            try indent(writer, depth);
            try writer.print("Member {s}\n", .{value.member});
            try dumpExpr(writer, value.object.*, depth + 1);
        },
        .index => |value| {
            try indent(writer, depth);
            try writer.writeAll("Index\n");
            try dumpExpr(writer, value.object.*, depth + 1);
            try dumpExpr(writer, value.index.*, depth + 1);
        },
        .call => |value| {
            try indent(writer, depth);
            try writer.writeAll("Call\n");
            try dumpExpr(writer, value.callee.*, depth + 1);
            for (value.args) |arg| try dumpExpr(writer, arg.value.*, depth + 1);
            if (value.trailing_builder) |builder| try dumpBuilderBlock(writer, builder, depth + 1);
            if (value.trailing_callback) |callback| {
                try indent(writer, depth + 1);
                try writer.writeAll("Callback\n");
                try dumpBlock(writer, callback.body, depth + 2);
            }
        },
        .try_expr => |value| {
            try indent(writer, depth);
            try writer.writeAll("Try\n");
            try dumpExpr(writer, value.operand.*, depth + 1);
        },
        .quote => |value| {
            try indent(writer, depth);
            try writer.print("Quote parts={d}\n", .{value.parts.len});
        },
    }
}

fn dumpTypeExprInline(writer: anytype, ty: ast.TypeExpr) anyerror!void {
    switch (ty) {
        .named => |value| try writer.writeAll(qualifiedNameText(value)),
        .generic => |value| {
            try writer.writeAll(qualifiedNameText(value.base));
            try writer.writeAll("<");
            for (value.args, 0..) |arg, index| {
                if (index != 0) try writer.writeAll(", ");
                try dumpTypeExprInline(writer, arg.*);
            }
            try writer.writeAll(">");
        },
        .ownership => |value| {
            switch (value.mode) {
                .owned => {},
                .borrow_read => try writer.writeAll("borrow "),
                .borrow_mut => try writer.writeAll("borrow mut "),
                .move => try writer.writeAll("move "),
                .copy => try writer.writeAll("copy "),
            }
            try dumpTypeExprInline(writer, value.target.*);
        },
        .any => |value| {
            try writer.writeAll(if (value.existential) "some " else "any ");
            try dumpTypeExprInline(writer, value.target.*);
        },
        .array => |value| {
            try writer.writeAll("[");
            try dumpTypeExprInline(writer, value.element_type.*);
            try writer.writeAll("]");
        },
        .function => |value| {
            try writer.writeAll("(");
            for (value.params, 0..) |param, index| {
                if (index != 0) try writer.writeAll(", ");
                try dumpTypeExprInline(writer, param.*);
            }
            try writer.writeAll(") -> ");
            try dumpTypeExprInline(writer, value.result.*);
        },
    }
}

fn qualifiedNameText(name: ast.QualifiedName) []const u8 {
    if (name.segments.len == 0) return "";
    return name.segments[0].text;
}

test "ast dump smoke" {
    _ = std.testing;
}
