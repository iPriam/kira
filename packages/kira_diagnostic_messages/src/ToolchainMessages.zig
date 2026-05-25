const std = @import("std");
const diagnostics = @import("kira_diagnostics");
const DiagnosticDomain = @import("DiagnosticDomain.zig").DiagnosticDomain;
const CompilerPhase = @import("CompilerPhase.zig").CompilerPhase;
const message = @import("DiagnosticMessage.zig");

pub fn missingLlvmToolchain() diagnostics.Diagnostic {
    return message.build(.{
        .code = .KTC001_MissingLlvmToolchain,
        .domain = .toolchain,
        .phase = .toolchain_activation,
        .title = "LLVM backend is unavailable",
        .message = "Kira could not start the native toolchain because LLVM is not available in this build.",
        .help = "Set KIRA_LLVM_HOME or run `kira fetch-llvm` to install the pinned LLVM toolchain.",
    });
}

pub fn unsupportedHostTarget(allocator: std.mem.Allocator, triple: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KTC003_UnsupportedTargetTriple,
        .domain = .toolchain,
        .phase = .toolchain_activation,
        .title = "unsupported host target",
        .message = try std.fmt.allocPrint(
            allocator,
            "The current host target `{s}` is not supported by this project or one of its native libraries.",
            .{triple},
        ),
        .help = "Add a matching target section to the relevant NativeLibs manifest, or build on a supported host.",
    });
}

pub fn invalidToolchainActivation(allocator: std.mem.Allocator, err_name: []const u8) !diagnostics.Diagnostic {
    return message.build(.{
        .code = .KTC007_InvalidToolchainActivation,
        .domain = .toolchain,
        .phase = .toolchain_activation,
        .title = "toolchain build failed",
        .message = try std.fmt.allocPrint(
            allocator,
            "Kira hit a toolchain failure while preparing this program ({s}).",
            .{err_name},
        ),
        .help = "Check the managed toolchain setup and try the command again.",
    });
}
