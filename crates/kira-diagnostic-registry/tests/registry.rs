//! The gate that keeps the diagnostic code table and its artifacts honest.
//!
//! Four claims, each one a way the registry drifted before it was generated:
//! the table lists every code the toolchain emits, it lists nothing the
//! toolchain does not emit, the generated files are what the table renders, and
//! the documentation names no code the table has never heard of.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use kira_diagnostic_messages::registry::{self, FAMILIES};
use kira_diagnostic_registry::{artifacts, emitted_codes};

/// The repository root, taken from this crate's place inside it.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two directories below the repository root")
        .to_path_buf()
}

/// How to make a failing claim pass again.
const REGENERATE: &str = "edit crates/kira-diagnostic-messages/diagnostic-codes.tsv, then run \
     `cargo run -p kira-diagnostic-registry -- write`";

#[test]
fn every_code_the_toolchain_emits_is_registered() {
    let emitted = emitted_codes(&repository_root()).expect("the source tree reads");
    let missing: Vec<String> = emitted
        .iter()
        .filter(|(code, _)| !registry::contains(code))
        .map(|(code, origin)| format!("{code} ({})", origin.display()))
        .collect();
    assert!(
        missing.is_empty(),
        "these codes are emitted but not registered: {}\n{REGENERATE}",
        missing.join(", ")
    );
}

#[test]
fn every_registered_code_is_one_the_toolchain_emits() {
    let emitted = emitted_codes(&repository_root()).expect("the source tree reads");
    let extra: Vec<&str> = registry::all()
        .iter()
        .map(|entry| entry.code)
        .filter(|code| !emitted.contains_key(*code))
        .collect();
    assert!(
        extra.is_empty(),
        "these codes are registered but nothing emits them: {}\n{REGENERATE}",
        extra.join(", ")
    );
}

#[test]
fn every_generated_artifact_is_current() {
    let repo = repository_root();
    let stale: Vec<String> = artifacts()
        .into_iter()
        .filter(|artifact| !artifact.is_current(&repo).unwrap_or(false))
        .map(|artifact| artifact.path.display().to_string())
        .collect();
    assert!(
        stale.is_empty(),
        "these generated files no longer match the code table: {}\n{REGENERATE}",
        stale.join(", ")
    );
}

#[test]
fn the_documentation_names_no_unregistered_code() {
    let mut unknown = BTreeSet::new();
    let mut pages = Vec::new();
    collect_pages(&repository_root().join("sites/docs/content"), &mut pages);
    assert!(!pages.is_empty(), "the documentation was not found");
    for page in &pages {
        let text = fs::read_to_string(page).expect("a documentation page reads");
        for word in text.split(|character: char| !character.is_ascii_alphanumeric()) {
            if looks_like_a_code(word) && !registry::contains(word) {
                unknown.insert(format!("{word} ({})", page.display()));
            }
        }
    }
    assert!(
        unknown.is_empty(),
        "the documentation names codes the table does not list: {}",
        unknown.into_iter().collect::<Vec<_>>().join(", ")
    );
}

#[test]
fn the_appendix_names_every_family() {
    let index = repository_root().join("sites/docs/content/docs/appendix/diagnostics/index.mdx");
    let text = fs::read_to_string(index).expect("the diagnostics appendix reads");
    for family in FAMILIES {
        assert!(
            text.contains(&format!("`{}`", family.prefix())),
            "{} is missing from the prefix table",
            family.prefix()
        );
    }
}

/// Every `.mdx` page under `root`.
fn collect_pages(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_pages(&path, out);
        } else if path.extension().is_some_and(|found| found == "mdx") {
            out.push(path);
        }
    }
}

/// Whether a word is spelled like a diagnostic code.
fn looks_like_a_code(word: &str) -> bool {
    let bytes = word.as_bytes();
    if bytes.len() < 6 {
        return false;
    }
    let (letters, digits) = bytes.split_at(bytes.len() - 3);
    letters[0] == b'K'
        && letters.iter().all(u8::is_ascii_uppercase)
        && digits.iter().all(u8::is_ascii_digit)
}
