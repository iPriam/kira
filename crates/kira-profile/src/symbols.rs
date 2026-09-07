//! Recognising Kira in a machine symbol.
//!
//! A native sampler hands back whatever the platform's symbolizer knew: a
//! mangled Rust name, a C entry point, or the name the LLVM backend emitted for
//! a Kira function. This module turns that into the identity a report prints —
//! which is what makes an LLVM profile read like a VM profile instead of like a
//! disassembly of the runtime.
//!
//! The backend spells a Kira function `kira_fn_<index>_<name>`, so a machine
//! frame carries the index that names the function in the program's own tables.
//! Everything else is classified by what it belongs to: Kira's runtime, the
//! program's other machine code, a C library it imported, or the system.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kira_debug::{Backend, DebugInfo};

use crate::model::FrameKind;

/// What one Kira function is called, and where it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionIdentity {
    /// The Kira spelling a report prints.
    pub name: String,
    /// The best known source line of the declaration.
    pub line: u32,
}

/// What a machine symbol turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolIdentity {
    /// The name a report prints.
    pub name: String,
    /// What kind of code it is.
    pub kind: FrameKind,
    /// The Kira function index, when the symbol named one.
    pub function: Option<u32>,
}

/// The Kira identities behind one program's symbols.
#[derive(Debug, Clone)]
pub struct KiraSymbols {
    /// Every function by the ID the program's own tables use.
    ///
    /// Keyed rather than positional. The backend spells a symbol
    /// `kira_fn_<id>_<name>`, and that ID is the program's, not an offset into
    /// whatever subset of functions a debug record happens to list — so a
    /// program whose first listed function has ID 4 resolved nothing, and every
    /// frame in it reported as the raw `kira_fn_4_Grid_step`. The two agree
    /// only when the list happens to start at zero and skip nothing.
    functions: HashMap<u32, FunctionIdentity>,
    /// Every function by the name it was written under.
    ///
    /// A platform symbolizer that reads a program's debug records answers with
    /// the name the *declaration* carries — `fib`, not the `kira_fn_7_fib` the
    /// linker sees — so a machine frame in the program's own image has to be
    /// recognised by that too, or an LLVM profile reports the program's own
    /// functions as anonymous native code.
    by_name: HashMap<String, u32>,
    source: Option<PathBuf>,
    backend: Backend,
}

impl KiraSymbols {
    /// The identities of every function in a compiled program.
    #[must_use]
    pub fn from_debug(info: &DebugInfo) -> Self {
        let functions = info
            .functions
            .iter()
            .map(|function| {
                (
                    function.id,
                    FunctionIdentity {
                        name: function.name.clone(),
                        line: function.line,
                    },
                )
            })
            .collect();
        let by_name = info
            .functions
            .iter()
            .map(|function| (function.name.clone(), function.id))
            .collect();
        Self {
            functions,
            by_name,
            source: info.source.as_ref().map(|source| source.path.clone()),
            backend: info.backend,
        }
    }

    /// The engine the program was recorded on.
    #[must_use]
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// The source file the program was compiled from.
    #[must_use]
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// The identity of Kira function `index`.
    #[must_use]
    pub fn function(&self, index: u32) -> Option<&FunctionIdentity> {
        self.functions.get(&index)
    }

    /// How many functions the program has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.functions.len()
    }

    /// Whether the program has no functions at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.functions.is_empty()
    }

    /// What `symbol`, found in `object`, is.
    ///
    /// `object` is the image the address fell in; it decides the system case,
    /// which no name pattern can, because a system library's symbols look like
    /// anyone else's.
    #[must_use]
    pub fn classify(&self, symbol: &str, object: &str) -> SymbolIdentity {
        if let Some(index) = kira_function_index(symbol) {
            let name = self
                .function(index)
                .map(|identity| identity.name.clone())
                .unwrap_or_else(|| symbol.to_owned());
            return SymbolIdentity {
                name,
                kind: FrameKind::Kira,
                function: Some(index),
            };
        }
        if !is_runtime(symbol)
            && !is_system_object(object)
            && let Some(index) = self.by_name.get(symbol)
        {
            return SymbolIdentity {
                name: symbol.to_owned(),
                kind: FrameKind::Kira,
                function: Some(*index),
            };
        }
        let kind = if is_runtime(symbol) {
            FrameKind::Runtime
        } else if is_system_object(object) {
            FrameKind::System
        } else if symbol.is_empty() {
            FrameKind::Unknown
        } else {
            FrameKind::Native
        };
        SymbolIdentity {
            name: if symbol.is_empty() {
                "[unknown]".to_owned()
            } else {
                symbol.to_owned()
            },
            kind,
            function: None,
        }
    }
}

/// The Kira function index a backend-emitted symbol carries.
///
/// Both spellings the backend produces are accepted: `kira_fn_<index>_<name>`
/// for a Kira body, and `kira_native_fn_<index>` for the native half of a
/// hybrid program. The index is the same one the program's own tables use.
#[must_use]
pub fn kira_function_index(symbol: &str) -> Option<u32> {
    let trimmed = symbol.trim_start_matches('_');
    if let Some(rest) = trimmed.strip_prefix("kira_native_fn_") {
        return rest.parse().ok();
    }
    let rest = trimmed.strip_prefix("kira_fn_")?;
    let digits = rest.split('_').next()?;
    digits.parse().ok()
}

/// Whether a symbol belongs to Kira's own runtime rather than to the program.
///
/// The runtime is the interpreter, the value heap, the native bridge, the glue
/// the backend emits around a Kira function, and the parts of the Rust standard
/// library they call. Time in any of them is time the program caused but did
/// not write, which is the distinction a report exists to draw.
fn is_runtime(symbol: &str) -> bool {
    const PREFIXES: [&str; 7] = [
        "kira_rt_",
        "kira_lib_",
        "kira_vm_",
        "kira_ffi_",
        "kira_foreign_adapter_",
        "kira.elem.",
        "kira.native.state.",
    ];
    const CONTAINS: [&str; 6] = [
        "kira_vm_runtime",
        "kira_native_bridge",
        "kira_runtime_abi",
        "kira_hybrid_runtime",
        "kira_bytecode",
        "kira_main",
    ];
    let trimmed = symbol.trim_start_matches('_');
    PREFIXES.iter().any(|prefix| trimmed.starts_with(prefix))
        || CONTAINS.iter().any(|needle| symbol.contains(needle))
        || is_rust_runtime(symbol)
}

/// Whether a symbol is the language runtime underneath every Kira program.
fn is_rust_runtime(symbol: &str) -> bool {
    const ROOTS: [&str; 5] = ["core::", "alloc::", "std::", "hashbrown::", "__rust_"];
    ROOTS.iter().any(|root| symbol.starts_with(root))
}

/// Whether an image belongs to the operating system rather than to the program.
fn is_system_object(object: &str) -> bool {
    let lowered = object.to_ascii_lowercase();
    let name = Path::new(&lowered)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(lowered.clone());
    const SYSTEM_NAMES: [&str; 12] = [
        "ntdll.dll",
        "kernel32.dll",
        "kernelbase.dll",
        "user32.dll",
        "win32u.dll",
        "gdi32.dll",
        "ucrtbase.dll",
        "msvcrt.dll",
        "libc.so.6",
        "ld-linux-x86-64.so.2",
        "libpthread.so.0",
        "libsystem_kernel.dylib",
    ];
    if SYSTEM_NAMES.contains(&name.as_str()) {
        return true;
    }
    const SYSTEM_ROOTS: [&str; 5] = [
        "c:\\windows\\",
        "/usr/lib/system/",
        "/system/library/",
        "/lib/x86_64-linux-gnu/",
        "[kernel",
    ];
    SYSTEM_ROOTS.iter().any(|root| lowered.starts_with(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_debug::{DebugFunction, DebugSource};

    fn symbols() -> KiraSymbols {
        KiraSymbols::from_debug(&DebugInfo {
            module_name: "hello".to_owned(),
            backend: Backend::Llvm,
            source: Some(DebugSource {
                path: PathBuf::from("src/main.kira"),
            }),
            functions: vec![
                DebugFunction {
                    id: 0,
                    name: "main".to_owned(),
                    backend: Backend::Llvm,
                    symbol: Some("kira_fn_0_main".to_owned()),
                    line: 3,
                },
                DebugFunction {
                    id: 1,
                    name: "Grid.step".to_owned(),
                    backend: Backend::Llvm,
                    symbol: Some("kira_fn_1_Grid_step".to_owned()),
                    line: 11,
                },
            ],
            optimized: false,
        })
    }

    #[test]
    fn a_backend_symbol_resolves_to_the_kira_function_it_was_emitted_for() {
        let symbols = symbols();
        let identity = symbols.classify("kira_fn_1_Grid_step", "hello.exe");
        assert_eq!(identity.kind, FrameKind::Kira);
        assert_eq!(identity.function, Some(1));
        assert_eq!(identity.name, "Grid.step");
    }

    #[test]
    fn the_leading_underscore_a_mach_o_symbol_carries_does_not_hide_the_index() {
        assert_eq!(kira_function_index("_kira_fn_7_update"), Some(7));
        assert_eq!(kira_function_index("kira_native_fn_2"), Some(2));
        assert_eq!(kira_function_index("kira_rt_string_new"), None);
    }

    #[test]
    fn the_runtime_the_system_and_the_program_are_told_apart() {
        let symbols = symbols();
        assert_eq!(
            symbols.classify("kira_rt_string_new", "hello.exe").kind,
            FrameKind::Runtime
        );
        assert_eq!(
            symbols
                .classify("kira_vm_runtime::interp::Vm::step", "kira.exe")
                .kind,
            FrameKind::Runtime
        );
        assert_eq!(
            symbols
                .classify("NtWaitForSingleObject", "C:\\Windows\\System32\\ntdll.dll")
                .kind,
            FrameKind::System
        );
        assert_eq!(
            symbols.classify("sqlite3_step", "libsqlite3.a").kind,
            FrameKind::Native
        );
        assert_eq!(symbols.classify("", "hello.exe").kind, FrameKind::Unknown);
    }

    #[test]
    fn a_program_symbol_under_its_declared_name_is_still_a_kira_function() {
        // What a platform symbolizer reading the program's debug records
        // answers with, rather than the linker-visible `kira_fn_1_Grid_step`.
        let identity = symbols().classify("Grid.step", "hello.exe");
        assert_eq!(identity.kind, FrameKind::Kira);
        assert_eq!(identity.function, Some(1));
    }

    #[test]
    fn a_system_library_symbol_is_not_mistaken_for_a_kira_function_of_that_name() {
        let identity = symbols().classify("main", "C:\\Windows\\System32\\ntdll.dll");
        assert_eq!(identity.kind, FrameKind::System);
        assert_eq!(identity.function, None);
    }

    #[test]
    fn a_function_index_past_the_program_keeps_the_symbol_it_came_from() {
        let symbols = symbols();
        let identity = symbols.classify("kira_fn_9_ghost", "hello.exe");
        assert_eq!(identity.name, "kira_fn_9_ghost");
        assert_eq!(identity.function, Some(9));
    }
}
