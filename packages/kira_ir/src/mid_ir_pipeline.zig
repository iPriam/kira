const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const ir = @import("ir.zig");
const model = @import("kira_semantics_model");
const mid = @import("mid_ir.zig");
const mid_lower = @import("mid_ir_lower.zig");
const mid_check = @import("mid_ir_check.zig");
const low_level = @import("lower_from_hir.zig");

pub const PrepareResult = union(enum) {
    checked: mid.CheckedProgram,
    failed,
};

pub fn prepareProgram(
    allocator: std.mem.Allocator,
    program: model.Program,
    options: low_level.LowerProgramOptions,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !PrepareResult {
    const lowered = try mid_lower.lowerProgram(allocator, program, .{
        .include_tests = options.include_tests,
    });
    const checked = try mid_check.checkProgram(allocator, lowered, out_diagnostics);
    if (checked == null) return .failed;
    return .{ .checked = checked.? };
}

pub fn lowerCheckedProgram(
    allocator: std.mem.Allocator,
    checked: mid.CheckedProgram,
    options: low_level.LowerProgramOptions,
) !ir.Program {
    return low_level.lowerProgramWithOptions(allocator, checked.program.source_program, options);
}

pub fn lowerProgramWithDiagnostics(
    allocator: std.mem.Allocator,
    program: model.Program,
    options: low_level.LowerProgramOptions,
    out_diagnostics: *std.array_list.Managed(diagnostics.Diagnostic),
) !?ir.Program {
    const prepared = try prepareProgram(allocator, program, options, out_diagnostics);
    return switch (prepared) {
        .checked => |checked| try lowerCheckedProgram(allocator, checked, options),
        .failed => null,
    };
}
