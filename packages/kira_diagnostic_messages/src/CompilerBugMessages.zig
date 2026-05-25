const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;
const message = @import("DiagnosticMessage.zig");

pub fn genericInternalCompilerError(allocator: std.mem.Allocator, err_name: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KIC001_GenericInternalCompilerError,
        .domain = .compiler_internal,
        .phase = .crash_boundary,
        .title = "internal compiler error",
        .message = "Kira hit an unexpected internal failure and stopped before it could finish the command.",
        .notes = try allocator.dupe([]const u8, &.{
            "This is a compiler bug, not a source error.",
            try std.fmt.allocPrint(allocator, "internal error = {s}", .{err_name}),
        }),
        .help = "Please report this bug with the command you ran and the source file that triggered it.",
    });
}

pub fn stageFailedWithoutDiagnostic(
    allocator: std.mem.Allocator,
    phase: CompilerPhase,
) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KIC014_InvalidCompilerPhaseState,
        .domain = .compiler_internal,
        .phase = phase,
        .title = "compiler phase failed without a diagnostic",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira stopped during `{s}` without reporting a normal diagnostic.",
            .{@tagName(phase)},
        ),
        .notes = &.{"This is a compiler bug, not a source error."},
        .help = "Please report the command, backend, and source file that triggered this failure.",
    });
}
