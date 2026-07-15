//! Build requests.
//!
//! Ported from kira-zig `kira_build_definition/src/build_request.zig`.

use kira_native_lib_definition::ResolvedNativeLibrary;

use crate::build_target::BuildTarget;

/// A request to build one source root (Zig `BuildRequest`).
#[derive(Debug, Clone, Default)]
pub struct BuildRequest {
    /// Zig `source_path: []const u8`.
    pub source_path: String,
    /// Zig `output_path: []const u8`.
    pub output_path: String,
    /// Zig `target: BuildTarget = .{}`.
    pub target: BuildTarget,
    /// Zig `native_libraries: []const ResolvedNativeLibrary`.
    pub native_libraries: Vec<ResolvedNativeLibrary>,
    /// Zig `test_mode` — keep `Test` sections reachable and do not require a
    /// `@Main`. Set by `kira test`.
    pub test_mode: bool,
    /// Zig `synthesize_test_driver` — synthesize the pure-Kira test driver
    /// (`__kira_test_main`). Implies `test_mode`.
    pub synthesize_test_driver: bool,
}
