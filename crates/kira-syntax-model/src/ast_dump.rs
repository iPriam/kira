//! Debug dump of a parsed program (indented node-per-line text).
//!
//! Mirrors kira-zig `packages/kira_syntax_model/src/ast_dump.zig`.
//! TODO(port): the full recursive dumper lands during migration; this is a
//! placeholder surface used by `kira check --dump-ast`-style tooling.

use crate::ast::Program;
use kira_core::Interner;

/// Renders a program's AST as indented text.
///
/// TODO(port): currently returns an empty string; the real dumper is ported
/// from `ast_dump.zig` during migration.
pub fn dump_program(_program: &Program, _interner: &Interner) -> String {
    String::new()
}
