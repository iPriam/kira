//! Semantic analysis for KSL modules.
//!
//! Layer 2 of the Kira package graph.
//!
//! Resolves and type-checks a parsed KSL file, together with everything it
//! imports, into one [`CheckedModule`] where every name is resolved and every
//! expression carries its type. Lowering that to the backend-facing IR is
//! `kira-shader-ir`'s job, one layer up — the same split Kira's own frontend
//! makes between `kira-semantics` and `kira-ir`.
//!
//! Total: a rejected construct is reported and replaced with a placeholder, so
//! one bad line never hides the rest of the file.

pub mod builtins;
pub mod model;

mod check;
mod diagnostics;
#[cfg(test)]
mod tests;

use kira_diagnostics::Diagnostic;

pub use check::Module;
pub use model::CheckedModule;

/// What checking one KSL file produced.
#[derive(Debug)]
pub struct Checked {
    /// The checked module, always present even when something was rejected.
    pub module: CheckedModule,
    /// Everything the check reported.
    pub diagnostics: Vec<Diagnostic>,
}

impl Checked {
    /// Whether the check reported nothing.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Checks `main` together with the modules it imports.
///
/// `imports` pairs each alias with the module it names — resolving an
/// `import` path to a file is the pipeline's job, because this crate has no
/// filesystem. An alias that no import mentions is simply unused; an import
/// with no matching entry is reported.
#[must_use]
pub fn check(main: &Module, imports: &[(String, Module)]) -> Checked {
    let mut checker = check::Checker::new(main, diagnostics::Reporter::new(main.source));

    // Imported declarations land first so the main file can name them, and the
    // imports' own bodies are checked in the same pass.
    for (alias, module) in imports {
        checker.switch_to(module, alias);
        checker.declare();
    }
    for (alias, module) in imports {
        checker.switch_to(module, alias);
        checker.check_items();
    }

    checker.switch_to(main, "");
    checker.declare();
    checker.report_unresolved_imports(imports);
    checker.check_items();

    let (module, diagnostics) = checker.finish();
    Checked {
        module,
        diagnostics,
    }
}
