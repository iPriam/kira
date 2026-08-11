//! The source/function identity shared by every debugger adapter.

use std::path::{Path, PathBuf};

use kira_ir::IrProgram;

/// The engine a function executes on in a debug session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Backend {
    /// Kira bytecode interpreted by the VM.
    Vm,
    /// A program split between bytecode and native code.
    Hybrid,
    /// LLVM-generated host machine code.
    Llvm,
}

impl Backend {
    /// The command-line spelling of this backend.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Vm => "vm",
            Self::Hybrid => "hybrid",
            Self::Llvm => "llvm",
        }
    }
}

/// A source file known to the debugger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugSource {
    /// The path LLDB and an editor should open.
    pub path: PathBuf,
}

/// One function's stable debugger identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugFunction {
    /// The function's index in IR/module tables.
    pub id: u32,
    /// The Kira spelling shown in a backtrace and breakpoint list.
    pub name: String,
    /// The engine that owns this function body.
    pub backend: Backend,
    /// The native symbol, when this function has a machine-code body.
    pub symbol: Option<String>,
    /// The best source line for the function declaration.
    pub line: u32,
}

/// Debug information shared by VM, hybrid, and LLVM builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugInfo {
    /// The artifact/module name shown in debugger output.
    pub module_name: String,
    /// The backend whose functions are described.
    pub backend: Backend,
    /// The source file attached to DWARF and debugger output.
    pub source: Option<DebugSource>,
    /// Function identities in IR order.
    pub functions: Vec<DebugFunction>,
    /// Whether the native backend should mark this unit optimized.
    pub optimized: bool,
}

impl DebugInfo {
    /// Builds debugger identities from the same IR every backend consumes.
    ///
    /// Declaration lines are recovered from the source when it is readable;
    /// the stable function index supplies a deterministic line for generated or
    /// unavailable source files.
    #[must_use]
    pub fn from_ir(
        program: &IrProgram,
        module_name: impl Into<String>,
        backend: Backend,
        source: Option<&Path>,
    ) -> Self {
        let source = source.map(|path| DebugSource {
            path: path.to_path_buf(),
        });
        let lines = source
            .as_ref()
            .and_then(|source| std::fs::read_to_string(&source.path).ok())
            .map(|text| text.lines().map(str::to_owned).collect::<Vec<_>>());
        let functions = program
            .functions
            .iter()
            .enumerate()
            .map(|(index, function)| {
                let line = declaration_line(lines.as_deref(), &function.name, index);
                let symbol = matches!(backend, Backend::Llvm | Backend::Hybrid)
                    .then(|| function_symbol(index, &function.name));
                DebugFunction {
                    id: index as u32,
                    name: function.name.clone(),
                    backend,
                    symbol,
                    line,
                }
            })
            .collect();
        Self {
            module_name: module_name.into(),
            backend,
            source,
            functions,
            optimized: false,
        }
    }

    /// Marks the native unit as optimized while preserving its debug data.
    #[must_use]
    pub fn optimized(mut self, optimized: bool) -> Self {
        self.optimized = optimized;
        self
    }

    /// Returns the identity for function `id`.
    #[must_use]
    pub fn function(&self, id: usize) -> Option<&DebugFunction> {
        self.functions.get(id)
    }
}

/// The native body symbol shared by DWARF, LLDB, and object inspection.
#[must_use]
pub fn function_symbol(index: usize, name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("kira_fn_{index}_{sanitized}")
}

fn declaration_line(lines: Option<&[String]>, name: &str, fallback: usize) -> u32 {
    let Some(lines) = lines else {
        return fallback.saturating_add(1) as u32;
    };
    lines
        .iter()
        .position(|line| {
            line.split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .any(|word| word == name)
        })
        .map_or(fallback.saturating_add(1) as u32, |line| line as u32 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::Execution;
    use kira_semantics_model::Type;
    use la_arena::Arena;

    #[test]
    fn native_symbols_are_stable_and_safe_for_linkers() {
        assert_eq!(function_symbol(2, "draw!"), "kira_fn_2_draw_");
        assert_ne!(function_symbol(1, "same"), function_symbol(2, "same"));
    }

    #[test]
    fn identities_follow_ir_order_and_source_declarations() {
        let function = kira_ir::IrFunction {
            name: "main".to_owned(),
            param_count: 0,
            locals: Vec::<Type>::new(),
            native_state_locals: Vec::new(),
            return_type: Type::Void,
            execution: Execution::Runtime,
            by_reference_params: Vec::new(),
            by_pointer_params: Vec::new(),
            body: Vec::new(),
        };
        let program = IrProgram {
            functions: vec![function],
            types: Default::default(),
            main: Some(0),
            exports: Vec::new(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            exprs: Arena::new(),
        };
        let path =
            std::env::temp_dir().join(format!("kira-debug-model-{}.kira", std::process::id()));
        std::fs::write(&path, "let x = 1\nfunction main() {}\n").expect("write source");
        let info = DebugInfo::from_ir(&program, "demo", Backend::Llvm, Some(&path));
        let _ = std::fs::remove_file(&path);
        assert_eq!(info.functions[0].line, 2);
        assert_eq!(info.functions[0].symbol.as_deref(), Some("kira_fn_0_main"));
    }
}
