const build_target = @import("build_target.zig");
const native = @import("kira_native_lib_definition");

pub const BuildRequest = struct {
    source_path: []const u8,
    output_path: []const u8,
    target: build_target.BuildTarget = .{},
    native_libraries: []const native.ResolvedNativeLibrary = &.{},
    /// Compile in test mode: keep `Test` sections reachable and do not require a
    /// `@Main`. Set by `kira test` when it needs runnable test artifacts.
    test_mode: bool = false,
    /// Synthesize the pure-Kira test driver (`__kira_test_main`) entry that runs
    /// every Test and reports PASS/FAIL/SKIP. Implies `test_mode`.
    synthesize_test_driver: bool = false,
};
