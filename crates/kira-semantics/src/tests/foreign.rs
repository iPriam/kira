//! The `@FFI.Extern` seam: what an accepted foreign declaration records, what a
//! call to one type-checks against, and every refusal the frontend carries.
//!
//! Seamless C-FFI is new Kira design — the oracle has no foreign-call concept —
//! so these tests are the specification of what the marker means. Every refusal
//! is checked by code and, where the program is otherwise clean, proved to be
//! the *only* diagnostic reported, so a rule is never mistaken for a cascade.

use super::*;
use kira_runtime_abi::{ForeignAbi, ForeignType, ForeignTypeSpec};
use kira_semantics_model::HirProgram;
use kira_semantics_model::hir::{Callee, HirExpr};

/// The analyzed program of a single-file application.
fn program(text: &str) -> HirProgram {
    let db = salsa::DatabaseImpl::new();
    let source =
        SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), Vec::new());
    analyzed(&db, source).clone()
}

/// Whether the program contains a call resolved to a foreign callable.
fn has_foreign_call(program: &HirProgram) -> bool {
    program.exprs.iter().any(|(_, expr)| {
        matches!(
            expr,
            HirExpr::Call {
                callee: Callee::Foreign(_),
                ..
            }
        )
    })
}

/// A declaration whose `@FFI.Extern` block is `block`.
fn extern_add(block: &str) -> String {
    format!(
        "@Main function main() {{ return }}\n\
         @FFI.Extern {{ {block} }} function ffiAdd(a: I32, b: I32) -> I32"
    )
}

/// A well-formed declaration carrying one more annotation.
fn extern_with_marker(marker: &str) -> String {
    format!(
        "@Main function main() {{ return }}\n\
         @FFI.Extern {{ library: l, symbol: s, abi: c }} {marker} \
         function ffiAdd(a: I32) -> I32"
    )
}

/// A well-formed declaration whose one parameter is written `ty`.
fn extern_param(ty: &str) -> String {
    format!(
        "@Main function main() {{ return }}\n\
         @FFI.Extern {{ library: l, symbol: s, abi: c }} function f(a: {ty}) -> I32"
    )
}

mod aggregates;
mod callbacks;
mod calls;
mod declarations;
mod signatures;
