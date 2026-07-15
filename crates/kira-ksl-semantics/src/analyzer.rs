//! The KSL semantic analyzer: walks the parsed KSL AST, resolves imports,
//! type-checks declarations and function bodies, assigns logical binding
//! indices, computes uniform/storage layouts, and produces a
//! `kira_shader_ir::Program` (with reflection) for backend lowering.
//!
//! Ported from kira-zig `packages/kira_ksl_semantics/src/analyzer.zig`.

use kira_shader_ir as shader_ir;

/// A KSL module imported under an alias.
///
/// Zig: `ImportedModule` (`alias`, `module_name`, `module: syntax.ast.Module`).
/// The parsed-module payload is omitted until kira-ksl-syntax-model scaffolds
/// its AST types; TODO(port): add `module: kira_ksl_syntax_model::ast::Module`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedModule {
    pub alias: String,
    pub module_name: String,
}

/// Analyzer state threaded through one `analyze` run.
///
/// Zig: `Analyzer` struct in `analyzer.zig` (root module, imports, diagnostics
/// sink, accumulated type/function/shader tables). Behavior lands with the
/// port; this is the container scaffold only.
#[derive(Debug, Default)]
pub struct Analyzer {
    /// Modules imported by the root module. Zig: `imports`.
    pub imports: Vec<ImportedModule>,
    /// Declarations accumulated so far, in declaration order.
    /// Zig: intermediate lists that become `shader_ir.Program` fields.
    pub types: Vec<shader_ir::TypeDecl>,
    pub functions: Vec<shader_ir::FunctionDecl>,
    pub shaders: Vec<shader_ir::ShaderDecl>,
    // TODO(port): root_module (kira-ksl-syntax-model AST) and the diagnostics
    // sink (kira-diagnostics) once those crates scaffold their types.
}
