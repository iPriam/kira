const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const model = @import("kira_semantics_model");
const syntax = @import("kira_syntax_model");
pub const ImportedGlobals = @import("imported_globals.zig").ImportedGlobals;
const lowering = @import("lower_to_hir.zig");

pub const AnalysisOptions = lowering.AnalysisOptions;

pub fn analyze(allocator: std.mem.Allocator, program: syntax.ast.Program, out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic)) !model.Program {
    return analyzeWithImports(allocator, program, .{}, out_diagnostics);
}

pub fn analyzeWithImports(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    imported_globals: ImportedGlobals,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !model.Program {
    return lowering.lowerProgram(allocator, program, imported_globals, out_diagnostics);
}

pub fn analyzeWithImportsOptions(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    imported_globals: ImportedGlobals,
    options: AnalysisOptions,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !model.Program {
    return lowering.lowerProgramWithOptions(allocator, program, imported_globals, options, out_diagnostics);
}

pub fn analyzeLibrary(
    allocator: std.mem.Allocator,
    program: syntax.ast.Program,
    imported_globals: ImportedGlobals,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !model.Program {
    return lowering.lowerProgramWithOptions(allocator, program, imported_globals, .{ .require_main = false }, out_diagnostics);
}


test {
    _ = @import("analyzer_tests.zig");
}
