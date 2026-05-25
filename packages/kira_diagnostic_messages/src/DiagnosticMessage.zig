const diagnostics = @import("kira_diagnostics");
const source_pkg = @import("kira_source");
const code_file = @import("DiagnosticCode.zig");
const DiagnosticCode = code_file.DiagnosticCode;
const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;

pub const NoteList = []const []const u8;

pub const Args = struct {
    code: DiagnosticCode,
    severity: diagnostics.Severity = .@"error",
    domain: DiagnosticDomain,
    phase: ?CompilerPhase = null,
    title: []const u8,
    message: []const u8,
    span: ?source_pkg.Span = null,
    label: ?[]const u8 = null,
    notes: NoteList = &.{},
    help: ?[]const u8 = null,
};

pub fn build(args: Args) diagnostics.Diagnostic {
    const labels = if (args.span) |span|
        &.{diagnostics.primaryLabel(span, args.label orelse args.title)}
    else
        &.{};
    return .{
        .severity = args.severity,
        .code = code_file.text(args.code),
        .domain = @tagName(args.domain),
        .phase = if (args.phase) |phase| @tagName(phase) else null,
        .title = args.title,
        .message = args.message,
        .labels = labels,
        .notes = args.notes,
        .help = args.help,
    };
}
