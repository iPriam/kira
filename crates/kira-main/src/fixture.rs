//! A hand-built stand-in for the motivating library, for this crate's tests.
//!
//! The real one is authored in Kira and compiled by `kira`, which lives far
//! above this crate — so a test here that wanted a `.kbc` would either depend
//! upward or ship a committed binary nobody can read a diff of. Building the
//! module directly keeps the fixture in Rust, in the open, and cheap to bend
//! into the shapes these tests need: a wrong arity, a swapped class, a missing
//! export.
//!
//! The end-to-end proof that a *compiled* Kira library reaches this surface
//! belongs to the consumer test crate the generator step brings with it. This
//! fixture proves the embedding surface, not the compiler.

use kira_bytecode::exports::{ExportTable, ExportType, ModuleExport};
use kira_bytecode::module::{FrameRelease, FuncProto, Module};
use kira_bytecode::op::{FieldPath, Instruction as I};
use kira_runtime_abi::Execution;

/// One function, spelled the way every fixture here needs it.
fn func(name: &str, params: u64, locals: u64, code: Vec<I>) -> FuncProto {
    FuncProto {
        name: name.to_owned(),
        param_count: params,
        local_count: locals,
        execution: Execution::Runtime,
        code,
        releases: FrameRelease::EveryLocal,
    }
}

/// One exported function, with its Kira spelling derived the obvious way.
fn export(name: &str, function: u32, params: Vec<ExportType>, result: ExportType) -> ModuleExport {
    ModuleExport {
        name: name.to_owned(),
        kira_name: name.to_owned(),
        function,
        params,
        result,
    }
}

/// A `Button` handle, the only class this fixture exports.
pub(crate) const BUTTON: ExportType = ExportType::Handle { class: 0 };

/// The library: a one-field `Button` class and four exports over it.
///
/// - 0 `make_button(title: String) -> Button` — a handle out
/// - 1 `button_label(b: Button) -> String` — an owned string out
/// - 2 `click_at(b: Button, x: Int) -> Bool` — Rust re-entering Kira
/// - 3 `greet(name: String)` — a void export that reaches the host
pub(crate) fn library() -> Module {
    Module {
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        exports: ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![
                export("make_button", 0, vec![ExportType::String], BUTTON),
                export("button_label", 1, vec![BUTTON], ExportType::String),
                export(
                    "click_at",
                    2,
                    vec![BUTTON, ExportType::Int],
                    ExportType::Bool,
                ),
                export("greet", 3, vec![ExportType::String], ExportType::Void),
            ],
        },
        functions: vec![
            func(
                "make_button",
                1,
                1,
                vec![I::LoadLocal(0), I::NewStruct(1), I::Return],
            ),
            func(
                "button_label",
                1,
                1,
                vec![I::LoadLocal(0), I::GetField(0), I::Return],
            ),
            // Overwrites its own copy of the title — deliberately unobservable
            // to the caller — then answers about the argument it was given.
            func(
                "click_at",
                2,
                2,
                vec![
                    I::ConstStr(1),
                    I::StoreField {
                        slot: 0,
                        path: FieldPath::new(vec![0]),
                    },
                    I::LoadLocal(1),
                    I::ConstInt(0),
                    I::GeInt,
                    I::Return,
                ],
            ),
            func(
                "greet",
                1,
                1,
                vec![I::LoadLocal(0), I::Print, I::ReturnVoid],
            ),
        ],
        // A library has no entrypoint. That is the whole distinction, and every
        // test here runs against a module carrying it.
        main: None,
        strings: vec!["ok".to_owned(), "clicked".to_owned()],
    }
}

/// The library's bytes, which is what an embedder actually receives.
pub(crate) fn artifact() -> Vec<u8> {
    library().to_bytes()
}
