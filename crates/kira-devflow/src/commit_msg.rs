//! Conventional-commit message inference from a staged `--name-status` diff.
//! Used as the fallback when the invoking agent does not pass an explicit `-m`
//! subject: the agent knows intent best, but a sensible auto-message beats a
//! forced hand-typed flag for mechanical commits.

use std::collections::BTreeSet;

/// The commit type inferred from the staged paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Docs,
    Test,
    Chore,
}

/// Infer a Conventional Commit subject from `git diff --cached --name-status`.
pub fn infer(name_status: &str) -> String {
    let mut scopes: BTreeSet<String> = BTreeSet::new();
    let mut paths: Vec<&str> = Vec::new();
    let (mut any_code, mut any_docs, mut any_test) = (false, false, false);

    for line in name_status.lines() {
        let path = last_field(line);
        if path.is_empty() {
            continue;
        }
        paths.push(path);
        if let Some(scope) = scope_of(path) {
            scopes.insert(scope.to_string());
        }
        if path.ends_with(".md") {
            any_docs = true;
        } else if is_test_path(path) {
            any_test = true;
        } else if path.ends_with(".rs") {
            any_code = true;
        }
    }

    let kind = if any_code {
        Kind::Chore
    } else if any_test && !any_docs {
        Kind::Test
    } else if any_docs && !any_test {
        Kind::Docs
    } else {
        Kind::Chore
    };
    let type_str = match kind {
        Kind::Docs => "docs",
        Kind::Test => "test",
        Kind::Chore => "chore",
    };

    let mut out = String::from(type_str);
    let scope_list: Vec<&String> = scopes.iter().collect();
    match scope_list.len() {
        1 => {
            out.push('(');
            out.push_str(scope_list[0]);
            out.push(')');
        }
        n if n > 1 => out.push_str("(repo)"),
        _ => {}
    }
    out.push_str(": ");

    if paths.len() == 1 {
        out.push_str("update ");
        out.push_str(paths[0]);
    } else {
        out.push_str(&format!("update {} files", paths.len()));
        if (1..=3).contains(&scope_list.len()) {
            out.push_str(" (");
            for (index, scope) in scope_list.iter().enumerate() {
                if index != 0 {
                    out.push_str(", ");
                }
                out.push_str(scope);
            }
            out.push(')');
        }
    }
    out
}

/// The last tab-separated field of a name-status line (handles a rename's
/// `R\told\tnew`).
fn last_field(line: &str) -> &str {
    line.split('\t')
        .rfind(|field| !field.is_empty())
        .unwrap_or("")
        .trim()
}

/// Whether a path lives under a test tree.
fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/")
        || path.starts_with("tests-kik/")
        || path.contains("/tests/")
}

/// Derive a short scope from a repo path, or `None` for a rootless one.
fn scope_of(path: &str) -> Option<&str> {
    if let Some(rest) = path.strip_prefix("crates/") {
        let name = rest.split('/').next().unwrap_or(rest);
        // Trim the conventional "kira-" prefix for a tighter scope.
        return Some(name.strip_prefix("kira-").unwrap_or(name));
    }
    if path.starts_with("docs/") {
        return Some("docs");
    }
    if path.starts_with("tests-kik/") || path.starts_with("tests/") {
        return Some("tests");
    }
    if path.starts_with(".codex/") {
        return Some("codex");
    }
    if path.starts_with("examples/") {
        return Some("examples");
    }
    if path == "Cargo.toml" || path == "Cargo.lock" {
        return Some("build");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_single_crate_code_change() {
        assert_eq!(
            infer("M\tcrates/kira-llvm-backend/src/backend.rs"),
            "chore(llvm-backend): update crates/kira-llvm-backend/src/backend.rs"
        );
    }

    #[test]
    fn infer_docs_only_change() {
        assert_eq!(
            infer("M\tdocs/incremental_native_codegen.md"),
            "docs(docs): update docs/incremental_native_codegen.md"
        );
    }

    #[test]
    fn infer_multi_file_multi_scope() {
        assert_eq!(
            infer("M\tcrates/kira-ir/src/ir.rs\nM\tdocs/x.md"),
            "chore(repo): update 2 files (docs, ir)"
        );
    }

    #[test]
    fn infer_reads_the_new_path_of_a_rename() {
        assert_eq!(
            infer("R100\tcrates/kira-ir/old.rs\tcrates/kira-ir/new.rs"),
            "chore(ir): update crates/kira-ir/new.rs"
        );
    }
}
