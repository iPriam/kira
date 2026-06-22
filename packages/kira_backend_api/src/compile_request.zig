const ir = @import("kira_ir");
const native = @import("kira_native_lib_definition");

pub const BackendMode = enum {
    vm_bytecode,
    llvm_native,
    hybrid,
};

pub const NativeEmitOptions = struct {
    object_path: []const u8,
    executable_path: ?[]const u8 = null,
    shared_library_path: ?[]const u8 = null,
    ir_path: ?[]const u8 = null,
};

pub const CompileRequest = struct {
    mode: BackendMode,
    /// Native/LLVM emission accepts only a verified-executable program. A backend cannot be
    /// built around a raw `ir.Program`; obtain a `VerifiedProgram` from `ir.verify` (or, for
    /// trusted/test IR, `ir.VerifiedProgram.assumeVerified`).
    program: *const ir.VerifiedProgram,
    module_name: []const u8,
    emit: NativeEmitOptions,
    target_selector: ?native.TargetSelector = null,
    resolved_native_libraries: []const native.ResolvedNativeLibrary = &.{},
};
