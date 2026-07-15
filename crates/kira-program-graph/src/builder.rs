//! Program graph construction.
//!
//! Ported from kira-zig `kira_program_graph/src/builder.zig`. Owns
//! `buildProgramGraph` / `buildProgramGraphFromFiles` (walk the root program's
//! imports, resolve them via [`crate::imports`], parse each module once, and
//! splice the parsed programs into one whole-program AST),
//! `collectPackageModuleFiles`, `parseModuleProgram`, and the
//! timings/progress instrumentation hooks.

/// The whole-program module graph (Zig `ProgramGraph`).
///
/// TODO(port): the Zig struct wraps `program: syntax.ast.Program`; wire the
/// field once `kira-syntax-model` defines its AST (empty skeleton at scaffold
/// time — kept field-less here so the build never depends on a sibling
/// agent's output).
#[derive(Debug, Clone, Default)]
pub struct ProgramGraph {}

// TODO(port): `build_program_graph`, `build_program_graph_from_files`,
// `collect_package_module_files`, `parse_module_program`,
// `set_timings_enabled`, `set_progress_callback`.
