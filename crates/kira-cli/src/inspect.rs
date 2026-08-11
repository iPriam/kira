//! Frontend inspection commands: tokens and the parsed syntax tree.
//!
//! These commands stop at the parser on purpose. They expose the same lexer
//! and parser that `check` and `run` consume, which makes a broken frontend
//! observable without requiring a backend or a package manifest.

use std::fmt::Write as _;

use kira_diagnostics::has_errors;
use kira_lexer::lex;
use kira_parser::parse;
use kira_source::{SourceId, SourceMap};

use crate::diagnostics;
use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// Prints the lexical tokens for one source file.
pub fn tokens(args: &[String]) -> i32 {
    let source = match load_source(args, "tokens") {
        Ok(source) => source,
        Err(code) => return code,
    };
    let lexed = lex(source.id, &source.text);
    let mut output = format!("# {}\n", source.path);
    for (index, token) in lexed.tokens.iter().enumerate() {
        let text = token.span.slice(&source.text).escape_debug().to_string();
        let _ = writeln!(
            output,
            "{index:>4}  {:<18} {:>6}..{:<6} {:?}",
            token.kind.describe(),
            token.span.start,
            token.span.end(),
            text,
        );
    }
    out!("{}", output);
    diagnostics::emit(&lexed.diagnostics, &source.sources);
    if has_errors(&lexed.diagnostics) {
        EXIT_FAILURE
    } else {
        EXIT_OK
    }
}

/// Prints the parsed syntax tree and its resolve-only name table for one file.
pub fn ast(args: &[String]) -> i32 {
    let source = match load_source(args, "ast") {
        Ok(source) => source,
        Err(code) => return code,
    };
    let parsed = parse(source.id, &source.text);
    let output = format!(
        "# {}\n\n{:#?}\n\n# Names\n{:#?}",
        source.path, parsed.tree, parsed.interner
    );
    out!("{}", output);
    diagnostics::emit(&parsed.diagnostics, &source.sources);
    if has_errors(&parsed.diagnostics) {
        EXIT_FAILURE
    } else {
        EXIT_OK
    }
}

/// A source file together with the map needed to render its diagnostics.
struct LoadedSource {
    path: String,
    text: String,
    sources: SourceMap,
    id: SourceId,
}

/// Reads the one path accepted by an inspection command.
fn load_source(args: &[String], command: &str) -> Result<LoadedSource, i32> {
    let path = match args {
        [path] if !path.starts_with('-') => path,
        _ => {
            err!("kira {command}: expected exactly one source file");
            return Err(EXIT_USAGE);
        }
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            err!("kira {command}: cannot read `{path}`: {error}");
            return Err(EXIT_USAGE);
        }
    };
    let mut sources = SourceMap::new();
    let id = match sources.insert(path.clone(), text.clone()) {
        Ok(id) => id,
        Err(error) => {
            err!("kira {command}: cannot register `{path}`: {error}");
            return Err(EXIT_FAILURE);
        }
    };
    Ok(LoadedSource {
        path: path.clone(),
        text,
        sources,
        id,
    })
}
