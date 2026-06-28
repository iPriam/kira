//! Pure-Kira test driver synthesis.
//!
//! In test mode the compiler can synthesize a Kira entry function that runs
//! every `Test` declaration's `expect`/`test` sections, compares the result in
//! Kira (`==`), and prints a `PASS`/`FAIL`/`SKIP` line per test. Running that
//! driver on a backend is how `kira test` executes the suite without any
//! backend-specific Zig comparison override — so the same Test runs on vm,
//! llvm, and hybrid (and FFI works on hybrid because the driver is ordinary
//! Kira that bridges into @Native through the normal path).
//!
//! Trap-expectation tests (`expect` returns `Result.Error(...)`) cannot be run
//! from pure Kira yet — a hard abort (array OOB / divide-by-zero) is not
//! catchable via `attempt`/`handle` — so the driver SKIPs them by only calling
//! `test()` inside the `Ok` arm. Making those catchable is a separate runtime
//! change (phase 2).

const std = @import("std");
const syntax = @import("kira_syntax_model");
const diagnostics = @import("kira_diagnostics");
const parser = @import("kira_parser");

/// The synthesized driver function's name. The test runner invokes it by name.
pub const driver_function_name = "__kira_test_main";

fn isTestForm(form: syntax.ast.ConstructFormDecl) bool {
    const segments = form.construct_name.segments;
    return segments.len != 0 and std.mem.eql(u8, segments[segments.len - 1].text, "Test");
}

/// Returns `program` augmented with the synthesized driver function, or the
/// original program unchanged when it declares no `Test`s.
pub fn injectTestDriver(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    diags: *std.array_list.Managed(diagnostics.Diagnostic),
) !syntax.ast.Program {
    var names = std.array_list.Managed([]const u8).init(allocator);
    defer names.deinit();
    for (program.decls) |decl| {
        if (decl != .construct_form_decl) continue;
        const form = decl.construct_form_decl;
        if (!isTestForm(form)) continue;
        try names.append(form.name);
    }
    if (names.items.len == 0) return program;

    var src: std.Io.Writer.Allocating = .init(allocator);
    defer src.deinit();
    const writer = &src.writer;
    try writer.print("function {s}() {{\n", .{driver_function_name});
    for (names.items) |name| {
        // The string literals bake the test name in at synthesis time (Kira has
        // no string concatenation), so each line is self-describing.
        //
        // Each marker line is prefixed with a NUL (`\0`) sentinel. The driver's
        // PASS/FAIL/KTRAP output is captured on the same stdout stream as any
        // output the tests themselves print, so without a sentinel a test that
        // printed e.g. "PASS x" would be miscounted as a test result. The runner
        // only treats NUL-prefixed lines as driver markers; ordinary test output
        // never begins with a NUL.
        try writer.print(
            "    match {s}__expect() {{\n" ++
                "        Ok(__expected) -> {{\n" ++
                "            let __actual = {s}__test()\n" ++
                "            if __actual == __expected {{ print(\"\\0PASS {s}\") }} else {{ print(\"\\0FAIL {s} (value mismatch)\") }}\n" ++
                "        }}\n" ++
                // A trap-expectation test (expect = Result.Error): the driver must
                // NOT call test() here (a hard abort would kill the whole driver).
                // Emit a marker so the runner re-runs test() in isolation and
                // checks that it traps.
                "        Error(__failure) -> {{ print(\"\\0KTRAP {s}\") }}\n" ++
                "    }}\n",
            .{ name, name, name, name, name },
        );
    }
    try writer.writeAll("    return\n}\n");

    const driver_program = try parser.parseSource(allocator, src.written(), diags);
    if (diagnostics.hasErrors(diags.items)) return program;

    // The parser records a top-level function in BOTH `decls` (as a
    // `.function_decl`) and `functions`; semantics reads `decls`. Extend both,
    // keeping the per-list origin arrays aligned (a root origin per driver item).
    var decls = std.array_list.Managed(syntax.ast.Decl).init(allocator);
    try decls.appendSlice(program.decls);
    try decls.appendSlice(driver_program.decls);
    var decl_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    try decl_origins.appendSlice(program.decl_origins);
    while (decl_origins.items.len < program.decls.len) try decl_origins.append(.{});
    for (driver_program.decls) |_| try decl_origins.append(.{});

    var functions = std.array_list.Managed(syntax.ast.FunctionDecl).init(allocator);
    try functions.appendSlice(program.functions);
    try functions.appendSlice(driver_program.functions);
    var function_origins = std.array_list.Managed(syntax.ast.DeclOrigin).init(allocator);
    try function_origins.appendSlice(program.function_origins);
    while (function_origins.items.len < program.functions.len) try function_origins.append(.{});
    for (driver_program.functions) |_| try function_origins.append(.{});

    return .{
        .imports = program.imports,
        .decls = try decls.toOwnedSlice(),
        .functions = try functions.toOwnedSlice(),
        .import_origins = program.import_origins,
        .decl_origins = try decl_origins.toOwnedSlice(),
        .function_origins = try function_origins.toOwnedSlice(),
    };
}
