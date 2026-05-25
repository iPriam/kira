const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;
const message = @import("DiagnosticMessage.zig");

pub fn missingProjectManifest(allocator: std.mem.Allocator, path: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KPK001_MissingProjectManifest,
        .domain = .package,
        .phase = .project_discovery,
        .title = "project manifest not found",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira could not find `kira.toml` or `project.toml` under `{s}`.",
            .{path},
        ),
        .help = "Run the command from a project root, or pass an explicit manifest path.",
    });
}

pub fn missingSourceFile(allocator: std.mem.Allocator, root: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KPK007_MissingSourceFile,
        .domain = .package,
        .phase = .project_discovery,
        .title = "target entrypoint is missing",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira expected `app/main.kira` under `{s}`, but that source file does not exist.",
            .{root},
        ),
        .help = "Add `app/main.kira`, or point the command at a library root for `check`/`build` only.",
    });
}

pub fn noBuildableTarget(allocator: std.mem.Allocator, source_root: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KPK010_NoBuildableTarget,
        .domain = .package,
        .phase = .project_discovery,
        .title = "library has no source files",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira could not find any `.kira` source files under `{s}`.",
            .{source_root},
        ),
        .help = "Add library source files under the package `app/` directory.",
    });
}
