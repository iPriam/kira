//! What can go wrong assembling a wasm module.

use std::path::PathBuf;

/// A failure while building a Kira wasm module.
#[derive(Debug, thiserror::Error)]
pub enum WasmError {
    /// An IR node carried the error type.
    ///
    /// Lowering only ever runs on a program that type-checked, so this is a
    /// compiler bug rather than a program's fault — but it is reported instead
    /// of asserted, because a backend never gets to end its caller's process.
    #[error("the wasm backend was handed an ill-typed program (this is a compiler bug)")]
    ErrorType,
    /// A local slot had type `Void`, which has no storage.
    #[error("`{0}` declares a local with no value type (this is a compiler bug)")]
    VoidLocal(String),
    /// A binary operator reached the wrong lowering path.
    #[error("an operator reached the wrong lowering path (this is a compiler bug)")]
    UnsupportedOperator,
    /// A `break`/`continue` reached lowering with no enclosing loop, which
    /// analysis is supposed to have rejected.
    #[error(
        "a `break`/`continue` reached the wasm backend outside a loop (this is a compiler bug)"
    )]
    JumpOutsideLoop,
    /// A struct id named no declared struct.
    #[error("the wasm backend was handed an unknown struct (this is a compiler bug)")]
    UnknownStruct,
    /// A struct field had type `Void`, which has no storage.
    #[error("a struct field has no value type (this is a compiler bug)")]
    VoidField,
    /// A place walked into something that is not a struct.
    #[error("an assignment walks a field of a value that is not a struct (this is a compiler bug)")]
    NotAStruct,
    /// `print` was handed a struct, which has no pinned rendering.
    ///
    /// Analysis rejects this before a program is lowered; it is reported rather
    /// than rendered as invented text.
    #[error("print cannot format a struct")]
    UnprintableStruct,
    /// `print` was called with other than one argument.
    #[error("`print` takes one argument, but the IR carried {0} (this is a compiler bug)")]
    PrintArity(usize),
    /// A call named a function index outside the program.
    #[error("call to unknown function index {0} (this is a compiler bug)")]
    UnknownFunction(u32),
    /// A function body or export was attached to a handle the module did not
    /// issue.
    #[error("the generated module was wired inconsistently (this is a compiler bug)")]
    Wiring,
    /// An artifact path could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}
