//! Attribute (`@Name`) and derive (`@Derive(Name, ...)`) procedural macro invocation. Runs the
//! compile-time evaluator (macro_eval.zig) over the decorated declaration, strips the macro
//! annotation, and re-parses the macro's `Syntax` output into declarations to splice. Split from
//! macro_expand.zig (Core Law #5).

const std = @import("std");
const syntax = @import("kira_syntax_model");
const source_pkg = @import("kira_source");
const parser = @import("kira_parser");
const expand = @import("macro_expand.zig");
const eval = @import("macro_eval.zig");

const ast = syntax.ast;
const Span = source_pkg.Span;
const Expander = expand.Expander;

pub const DeclMacroResult = struct {
    decl: ast.Decl,
    generated: []ast.Decl,
};

/// Run attribute (`@Name`) and derive (`@Derive(Name, ...)`) macros attached to `decl`. Returns the
/// declaration with macro annotations stripped, plus the declarations the macros generated (each
/// re-parsed from the macro's `Syntax` text via `parser.parseSource`).
pub fn applyDeclMacros(exp: *Expander, decl: ast.Decl) !DeclMacroResult {
    const annotations = declAnnotations(decl);
    if (annotations.len == 0 or exp.proc_macros.count() == 0) return .{ .decl = decl, .generated = &.{} };

    const target = try buildDeclaration(exp, decl);
    const target_kind = declTargetKind(decl);
    var kept = std.array_list.Managed(ast.Annotation).init(exp.allocator);
    var generated = std.array_list.Managed(ast.Decl).init(exp.allocator);
    var stripped_any = false;

    for (annotations) |annotation| {
        const name = annotationName(annotation);
        if (std.mem.eql(u8, name, "Derive")) {
            for (annotation.args) |arg| {
                const derive_name = identifierArg(arg.value) orelse continue;
                const macro = exp.proc_macros.get(derive_name);
                if (macro != null and macro.?.kind == .proc_derive) {
                    if (!try checkAppliesTo(exp, macro.?, target_kind, annotation.span)) continue;
                    try runProcMacro(exp, macro.?, target, annotation.span, &generated);
                } else {
                    try exp.err("KMAC011", "not a derive macro", "Only a `comptime macro { kind { derive } }` may appear in `@Derive(...)`.", annotation.span, "this is not a derive macro", "Declare the macro with `kind { derive }`, or use it as a function/attribute macro.");
                }
            }
            stripped_any = true;
            continue;
        }
        const macro = exp.proc_macros.get(name);
        if (macro != null and macro.?.kind == .proc_attribute) {
            if (try checkAppliesTo(exp, macro.?, target_kind, annotation.span)) {
                try runProcMacro(exp, macro.?, target, annotation.span, &generated);
            }
            stripped_any = true;
            continue;
        }
        try kept.append(annotation);
    }

    if (!stripped_any) return .{ .decl = decl, .generated = try generated.toOwnedSlice() };
    return .{
        .decl = setDeclAnnotations(decl, try kept.toOwnedSlice()),
        .generated = try generated.toOwnedSlice(),
    };
}

/// Look up a function-position macro by call name, or null if `name` is not a `kind { function }`
/// macro in scope. Callers in expression/statement position use this to decide whether a
/// `name!(args)` call is a procedural function macro (vs an unknown declarative macro -> KMAC001).
pub fn lookupFuncMacro(exp: *Expander, call: ast.CallExpr) ?ast.MacroDecl {
    if (call.callee.* != .identifier or call.callee.identifier.name.segments.len != 1) return null;
    return exp.func_macros.get(call.callee.identifier.name.segments[0].text);
}

/// Render a function macro's call arguments to source text and run `expand(input: Syntax)`,
/// returning the generated source text (or null if the macro emitted a diagnostic).
fn runFuncMacroText(exp: *Expander, macro: ast.MacroDecl, call: ast.CallExpr) !?[]const u8 {
    var input = std.array_list.Managed(u8).init(exp.allocator);
    for (call.args, 0..) |arg, i| {
        if (i != 0) try input.appendSlice(", ");
        try input.appendSlice(try eval.exprToText(exp.allocator, arg.value.*));
    }
    var evaluator = eval.Evaluator{ .allocator = exp.allocator, .diags = exp.diags };
    return evaluator.runOnInput(macro, try input.toOwnedSlice()) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.MacroEvalError => return null,
    };
}

/// Expand a top-level `name!(args)` function-macro invocation: render the arguments to source,
/// run the macro's `expand(input: Syntax)`, and re-parse the generated declarations.
pub fn expandMacroInvocation(exp: *Expander, call: ast.CallExpr) ![]ast.Decl {
    const macro = lookupFuncMacro(exp, call) orelse {
        const name = if (call.callee.* == .identifier and call.callee.identifier.name.segments.len == 1)
            call.callee.identifier.name.segments[0].text
        else
            "";
        const message = try std.fmt.allocPrint(exp.allocator, "no function macro named '{s}' is in scope", .{name});
        try exp.err("KMAC001", "unknown macro", message, call.span, "this macro is not declared", "Declare it with `comptime macro <name> { kind { function } ... }`.");
        return &.{};
    };
    const text = (try runFuncMacroText(exp, macro, call)) orelse return &.{};
    const generated_program = parser.parseSource(exp.allocator, text, exp.diags) catch return &.{};
    return generated_program.decls;
}

const frag_wrapper_name = "__kira_macro_frag__";

/// Parse the generated source of a function macro by wrapping it in a throwaway function and
/// returning that function's body statements (or null if the wrapper did not parse).
fn parseFuncMacroBody(exp: *Expander, macro: ast.MacroDecl, call: ast.CallExpr) !?[]ast.Statement {
    const text = (try runFuncMacroText(exp, macro, call)) orelse return null;
    const wrapped = try std.fmt.allocPrint(exp.allocator, "function {s}() {{\n{s}\n}}", .{ frag_wrapper_name, text });
    const program = parser.parseSource(exp.allocator, wrapped, exp.diags) catch return null;
    const body = wrapperBody(program) orelse return null;
    return body.statements;
}

/// A function macro used in *statement* position: its expansion's statements are spliced in place
/// of the `name!(...)` call.
pub fn expandFuncMacroStatements(exp: *Expander, macro: ast.MacroDecl, call: ast.CallExpr) ![]ast.Statement {
    return (try parseFuncMacroBody(exp, macro, call)) orelse {
        try exp.err("KMAC016", "macro output is not a statement list", "This function macro was used in statement position, but its expansion did not parse as Kira statements.", call.span, "expansion used as statements here", "Return statements/expressions from the macro, or use it in declaration position at top level.");
        return &.{};
    };
}

/// A function macro used in *expression* position: its expansion must be a single expression
/// statement, whose expression becomes the value. Anything else (a `let`, multiple statements) has
/// no value and is rejected with KMAC017.
pub fn expandFuncMacroExpr(exp: *Expander, macro: ast.MacroDecl, call: ast.CallExpr) !?*ast.Expr {
    const statements = (try parseFuncMacroBody(exp, macro, call)) orelse return null;
    if (statements.len != 1 or statements[0] != .expr_stmt) {
        try exp.err("KMAC017", "macro output is not an expression", "This function macro was used in expression position, but its expansion did not parse as a single Kira expression.", call.span, "expansion used as a value here", "Return one expression from the macro, or use it in statement position.");
        return null;
    }
    return statements[0].expr_stmt.expr;
}

/// Find the throwaway wrapper function's body in a parsed program.
fn wrapperBody(program: ast.Program) ?ast.Block {
    for (program.decls) |decl| {
        if (decl == .function_decl and std.mem.eql(u8, decl.function_decl.name, frag_wrapper_name)) {
            return decl.function_decl.body;
        }
    }
    return null;
}

fn runProcMacro(exp: *Expander, macro: ast.MacroDecl, target: ?eval.Declaration, span: Span, generated: *std.array_list.Managed(ast.Decl)) !void {
    const decl_target = target orelse {
        try exp.err("KMAC007", "macro target not supported", "This macro can only be applied to a struct or class declaration.", span, "unsupported macro target", "Apply the macro to a `struct` or `class` declaration.");
        return;
    };
    var evaluator = eval.Evaluator{ .allocator = exp.allocator, .diags = exp.diags };
    const text_opt = evaluator.runOnDeclaration(macro, decl_target) catch |err| switch (err) {
        error.OutOfMemory => return error.OutOfMemory,
        error.MacroEvalError => return, // diagnostic already emitted
    };
    const text = text_opt orelse return;

    const generated_program = parser.parseSource(exp.allocator, text, exp.diags) catch return;
    for (generated_program.decls) |gen_decl| try generated.append(gen_decl);
}

fn buildDeclaration(exp: *Expander, decl: ast.Decl) !?eval.Declaration {
    switch (decl) {
        .type_decl => |type_decl| {
            var fields = std.array_list.Managed(eval.Field).init(exp.allocator);
            for (type_decl.members) |member| {
                if (member == .field_decl) {
                    const field = member.field_decl;
                    const type_text = if (field.type_expr) |type_expr|
                        try eval.typeToText(exp.allocator, type_expr.*)
                    else
                        "";
                    try fields.append(.{ .name = field.name, .type_text = type_text });
                }
            }
            return eval.Declaration{
                .name = type_decl.name,
                .fields = try fields.toOwnedSlice(),
                .syntax = type_decl.name,
                .span = type_decl.span,
            };
        },
        .enum_decl => |enum_decl| {
            // Enum variants surface through the same `fields` reflection: `field.name` is the
            // variant name, `field.type` its associated payload type (empty if the variant is bare).
            var fields = std.array_list.Managed(eval.Field).init(exp.allocator);
            for (enum_decl.variants) |variant| {
                const type_text = if (variant.associated_type) |type_expr|
                    try eval.typeToText(exp.allocator, type_expr.*)
                else
                    "";
                try fields.append(.{ .name = variant.name, .type_text = type_text });
            }
            return eval.Declaration{
                .name = enum_decl.name,
                .fields = try fields.toOwnedSlice(),
                .syntax = enum_decl.name,
                .span = enum_decl.span,
            };
        },
        else => return null,
    }
}

/// The `appliesTo` target kind of a declaration, or null for kinds that can't carry derive/attribute
/// macros (a class/struct distinction comes from `TypeDecl.kind`).
fn declTargetKind(decl: ast.Decl) ?ast.MacroTargetKind {
    return switch (decl) {
        .type_decl => |d| switch (d.kind) {
            .class => .class_target,
            .struct_decl => .struct_target,
        },
        .enum_decl => .enum_target,
        else => null,
    };
}

/// Verify a derive/attribute macro's `appliesTo` list admits this declaration kind. An empty list
/// admits everything (back-compat with macros that omit `appliesTo`). Emits KMAC007 and returns
/// false on a mismatch.
fn checkAppliesTo(exp: *Expander, macro: ast.MacroDecl, target_kind: ?ast.MacroTargetKind, span: Span) !bool {
    if (macro.applies_to.len == 0) return true;
    const kind = target_kind orelse {
        try exp.err("KMAC007", "macro target not supported", "This macro can only be applied to a struct, class, or enum declaration.", span, "unsupported macro target", "Apply the macro to a `struct`, `class`, or `enum` declaration.");
        return false;
    };
    for (macro.applies_to) |allowed| {
        if (allowed == kind) return true;
    }
    const kind_name = switch (kind) {
        .struct_target => "struct",
        .class_target => "class",
        .enum_target => "enum",
    };
    const message = try std.fmt.allocPrint(exp.allocator, "macro '{s}' does not apply to a {s} declaration", .{ macro.name, kind_name });
    try exp.err("KMAC007", "macro target not in appliesTo", message, span, "this declaration kind is not in the macro's `appliesTo`", "Add this declaration kind to the macro's `appliesTo`, or remove the annotation.");
    return false;
}

fn declAnnotations(decl: ast.Decl) []const ast.Annotation {
    return switch (decl) {
        .type_decl => |d| d.annotations,
        .enum_decl => |d| d.annotations,
        .construct_decl => |d| d.annotations,
        .construct_form_decl => |d| d.annotations,
        .extend_decl => |d| d.annotations,
        .function_decl => |d| d.annotations,
        else => &.{},
    };
}

fn setDeclAnnotations(decl: ast.Decl, annotations: []const ast.Annotation) ast.Decl {
    switch (decl) {
        .type_decl => |d| {
            var nd = d;
            nd.annotations = annotations;
            return .{ .type_decl = nd };
        },
        .enum_decl => |d| {
            var nd = d;
            nd.annotations = annotations;
            return .{ .enum_decl = nd };
        },
        else => return decl,
    }
}

fn annotationName(annotation: ast.Annotation) []const u8 {
    const segments = annotation.name.segments;
    if (segments.len == 0) return "";
    return segments[segments.len - 1].text;
}

fn identifierArg(expr: *ast.Expr) ?[]const u8 {
    if (expr.* == .identifier and expr.identifier.name.segments.len == 1) {
        return expr.identifier.name.segments[0].text;
    }
    return null;
}
