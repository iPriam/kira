const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;
const message = @import("DiagnosticMessage.zig");

pub fn nativeCodeRequiresNativeBackend(allocator: std.mem.Allocator, source_path: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KBE001_UnsupportedBackendFeature,
        .domain = .backend,
        .phase = .backend_prepare,
        .title = "native code requires a native-capable backend",
        .message = "This program contains @Native functions, but the selected backend only supports runtime execution.",
        .help = try std.fmt.allocPrint(
            allocator,
            "Use `kira build --backend hybrid {s}` for mixed @Runtime/@Native programs, or `kira build --backend llvm {s}` for fully native output.",
            .{ source_path, source_path },
        ),
    });
}

pub fn nativeFfiPackageRequiresNativeBackend(allocator: std.mem.Allocator, source_path: []const u8, package_name: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KBE001_UnsupportedBackendFeature,
        .domain = .backend,
        .phase = .backend_prepare,
        .title = "native FFI package requires a native-capable backend",
        .message = try std.fmt.allocPrint(
            allocator,
            "package `{s}` requires native FFI but was selected for VM execution",
            .{package_name},
        ),
        .help = try std.fmt.allocPrint(
            allocator,
            "Use `kira check --backend llvm {s}` or `kira check --backend hybrid {s}` so native FFI packages are compiled through a native-capable backend.",
            .{ source_path, source_path },
        ),
    });
}

pub fn runtimeEntrypointInNativeBuild() diagnostics.Diagnostic {
    return message.build(.{
        .code = .KBE002_RuntimeOnlyValueInNativeBackend,
        .domain = .backend,
        .phase = .backend_prepare,
        .title = "native build cannot start from a runtime entrypoint",
        .message = "The selected native backend needs a native entrypoint, but @Main resolves to runtime execution.",
        .help = "Use the VM or hybrid backend, or mark the entry function with @Native.",
    });
}

pub fn runtimeCallInNativeBuild() diagnostics.Diagnostic {
    return message.build(.{
        .code = .KBE002_RuntimeOnlyValueInNativeBackend,
        .domain = .backend,
        .phase = .backend_prepare,
        .title = "native build depends on runtime-only code",
        .message = "The selected native backend encountered a call that still requires the runtime.",
        .help = "Use the hybrid backend for mixed execution, or move the called function to @Native.",
    });
}

pub fn hybridBuildRequiresExplicitExecution() diagnostics.Diagnostic {
    return message.build(.{
        .code = .KBE012_HybridSyncUnsupported,
        .domain = .backend,
        .phase = .backend_prepare,
        .title = "hybrid build needs explicit execution annotations",
        .message = "A hybrid build can only package functions that are explicitly marked with @Runtime or @Native.",
        .help = "Annotate each reachable function with @Runtime or @Native.",
    });
}

/// Raised when the program-phase verifier finds an executable obligation undischarged
/// (e.g. a call target that was never lowered, an aggregate with no known layout). This is
/// a lowering/compiler integrity gap surfaced as an early compile error, instead of letting
/// the half-lowered program reach codegen/runtime and detonate there. `summary`,
/// `function_name`, and `detail` come from the verifier's `VerifyFailure`.
pub fn executableObligationUnmet(
    allocator: std.mem.Allocator,
    summary: []const u8,
    function_name: []const u8,
    detail: []const u8,
) !diagnostics.Diagnostic {
    const location = if (function_name.len != 0)
        try std.fmt.allocPrint(allocator, " in function `{s}`", .{function_name})
    else
        try allocator.dupe(u8, "");
    const named = if (detail.len != 0)
        try std.fmt.allocPrint(allocator, ": `{s}`", .{detail})
    else
        try allocator.dupe(u8, "");
    return message.build(.{
        .code = .KIR001_InvalidLoweredNode,
        .domain = .lowering,
        .phase = .backend_prepare,
        .title = "program is not executable: a lowering obligation was left undischarged",
        .message = try std.fmt.allocPrint(allocator, "{s}{s}{s}", .{ summary, location, named }),
        .help = "The program reached backend emission without complete lowering. This is a compiler/lowering gap, not a runtime fault; it is rejected here so it cannot fail later at runtime. Run `kira check` to validate the frontend shape.",
    });
}

pub fn unsupportedExecutableFeature() diagnostics.Diagnostic {
    return message.build(.{
        .code = .KIR001_InvalidLoweredNode,
        .domain = .lowering,
        .phase = .lowering,
        .title = "feature is not executable in the current backend pipeline",
        .message = "This program uses language constructs that are not yet lowered into the shared executable IR.",
        .help = "Use `kira check` to validate the frontend shape, or stay within the currently executable subset for `run` and `build`.",
    });
}
