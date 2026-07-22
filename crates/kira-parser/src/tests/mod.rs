//! Parser tests: the shapes the grammar must produce, and the recovery that
//! keeps one bad construct from derailing a file.
//!
//! Split out of `lib.rs` on the file-size ladder: the parser's own surface —
//! `parse`, `ParseResult`, and the token cursor — is ~250 lines, and the tests
//! had grown to twice that. They stay a `#[cfg(test)]` module of this crate,
//! beside the code they test.

mod aliases;
mod arrays;
mod classes;
mod closures;
mod constructs;
mod declarations;
mod enums;
mod exports;
mod expressions;
mod foreign;
mod generics;
mod imports;
mod native_state;
mod ownership;
mod precedence;

use crate::*;
use kira_runtime_abi::Execution;
use kira_syntax_model::ast::{Function, Item, TypeRef, TypeRefId};

fn parse_text(text: &str) -> ParseResult {
    parse(SourceId::new(0), text)
}

/// The one function in `text`, for tests that parse a single declaration.
fn only_function(result: &ParseResult) -> &Function {
    match result.tree.items() {
        [Item::Function(function)] => function,
        items => panic!("expected exactly one function, got {items:?}"),
    }
}

/// The one struct in `text`, for tests that parse a single declaration.
fn only_struct(result: &ParseResult) -> &kira_syntax_model::ast::StructDecl {
    match result.tree.items() {
        [Item::Struct(declaration)] => declaration,
        items => panic!("expected exactly one struct, got {items:?}"),
    }
}

/// Renders a written type back to its source spelling, so a test can assert
/// the *shape* the parser built rather than an arena index.
fn type_spelling(result: &ParseResult, id: TypeRefId) -> String {
    match result.tree.type_ref(id) {
        TypeRef::Named { name, .. } => result.interner.resolve(*name).to_owned(),
        TypeRef::AnyConstruct { family, .. } => {
            format!("Any {}", result.interner.resolve(*family))
        }
        TypeRef::Generic { name, args, .. } => {
            let written: Vec<String> = args.iter().map(|&arg| type_spelling(result, arg)).collect();
            format!("{}<{}>", result.interner.resolve(*name), written.join(", "))
        }
        TypeRef::Array { element, .. } => format!("[{}]", type_spelling(result, *element)),
        TypeRef::Function {
            params,
            result: ret,
            ..
        } => {
            let written: Vec<String> = params
                .iter()
                .map(|&param| type_spelling(result, param))
                .collect();
            format!(
                "({}) -> {}",
                written.join(", "),
                type_spelling(result, *ret)
            )
        }
        TypeRef::Error { .. } => "<error>".to_owned(),
    }
}

/// The first statement of the first function in `text`.
fn first_stmt(result: &ParseResult) -> &kira_syntax_model::ast::Stmt {
    let function = match &result.tree.items()[0] {
        Item::Function(function) => function,
        other => panic!("expected function, got {other:?}"),
    };
    result.tree.stmt(function.body.stmts[0])
}
