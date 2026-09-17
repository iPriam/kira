//! Pull-request metadata derived from the complete branch diff. PR scope is a
//! property of `base...HEAD`, never of the current process, conversation, or
//! editing session.

use crate::context::Context;
use crate::error::DevflowError;
use crate::git_ops;

/// The broad area a changed path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    CompilerRuntime,
    PackagesManifests,
    Tests,
    DeveloperTooling,
    PlatformsWeb,
    DocsWorkflow,
}

/// Every area, in report order.
const AREAS: [Area; 6] = [
    Area::CompilerRuntime,
    Area::PackagesManifests,
    Area::Tests,
    Area::DeveloperTooling,
    Area::PlatformsWeb,
    Area::DocsWorkflow,
];

/// The generated title and body for a PR.
pub struct Metadata {
    /// The PR title.
    pub title: String,
    /// The PR body, in Markdown.
    pub body: String,
}

/// Generate PR metadata from the complete branch against its landing base.
pub fn generate(ctx: &Context) -> Result<Metadata, DevflowError> {
    let base_ref = format!(
        "{}/{}",
        if ctx.has_upstream() { "upstream" } else { "origin" },
        ctx.default_branch
    );
    let files = git_ops::branch_changed_files(ctx, &base_ref)?;
    let commits = git_ops::branch_commit_subjects(ctx, &base_ref)?;
    Ok(render(&base_ref, &files, &commits))
}

/// Build the title and body from the file list and commit subjects.
fn render(base_ref: &str, files: &str, commits: &str) -> Metadata {
    let mut counts = [0usize; AREAS.len()];
    let mut file_count = 0;
    for path in files.lines().filter(|line| !line.is_empty()) {
        file_count += 1;
        counts[classify(path) as usize] += 1;
    }
    let active = counts.iter().filter(|count| **count != 0).count();

    let title = if active >= 4 {
        String::from("Advance Kira compiler/runtime, packages, tests, and developer tooling")
    } else {
        focused_title(&counts)
    };

    let mut body = format!(
        "## Summary\n\nThis description is generated from the complete `{base_ref}...HEAD` branch diff ({file_count} changed files), not from the current session.\n"
    );
    for area in AREAS {
        let count = counts[area as usize];
        if count != 0 {
            body.push_str(&format!("\n- {} ({count} files)", area_description(area)));
        }
    }

    body.push_str("\n\n## Branch commits\n");
    let mut commit_count = 0;
    for subject in commits.lines().filter(|line| !line.is_empty()) {
        commit_count += 1;
        body.push_str(&format!("\n- {subject}"));
    }
    if commit_count == 0 {
        body.push_str("\n- No commits found beyond the base ref");
    }
    body.push('\n');

    Metadata { title, body }
}

/// A title naming only the areas that this branch actually touched.
fn focused_title(counts: &[usize; AREAS.len()]) -> String {
    let mut title = String::from("Advance Kira ");
    let mut written = 0;
    for area in AREAS {
        if counts[area as usize] == 0 {
            continue;
        }
        if written != 0 {
            title.push_str(if written == 1 { " and " } else { ", " });
        }
        title.push_str(area_title(area));
        written += 1;
    }
    if written == 0 {
        title.push_str("development");
    }
    title
}

/// Bucket a changed path into one area.
fn classify(path: &str) -> Area {
    if has_any(path, &["test", "tests", "corpus", "FailTest"]) {
        return Area::Tests;
    }
    if has_any(path, &["manifest", "package.kira", "package-manager", "dependency"]) {
        return Area::PackagesManifests;
    }
    if has_any(path, &["wasm", "web", "shader", "runner", "graphics"]) {
        return Area::PlatformsWeb;
    }
    if has_any(path, &[".codex/", "AGENTS.md", "CHANGELOG", "README", "docs/", ".github/"]) {
        return Area::DocsWorkflow;
    }
    if has_any(path, &["kira-cli", "kira-devflow", "toolchain", "debug", "Cargo.toml"]) {
        return Area::DeveloperTooling;
    }
    Area::CompilerRuntime
}

/// Whether `path` contains any of `needles`.
fn has_any(path: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| path.contains(needle))
}

/// The short area name used in a focused title.
fn area_title(area: Area) -> &'static str {
    match area {
        Area::CompilerRuntime => "compiler/runtime",
        Area::PackagesManifests => "packages",
        Area::Tests => "tests",
        Area::DeveloperTooling => "developer tooling",
        Area::PlatformsWeb => "platforms and Web",
        Area::DocsWorkflow => "documentation and workflow",
    }
}

/// The full area description used in a body bullet.
fn area_description(area: Area) -> &'static str {
    match area {
        Area::CompilerRuntime => {
            "advances compiler, IR, VM, LLVM, hybrid, FFI, and runtime implementation"
        }
        Area::PackagesManifests => {
            "evolves declarative packages, manifests, dependencies, and package management"
        }
        Area::Tests => "expands Kira-native tests, backend parity, fixtures, and validation infrastructure",
        Area::DeveloperTooling => {
            "improves the CLI, build system, debugger, toolchain, and developer workflow"
        }
        Area::PlatformsWeb => "advances Web, WASM, shaders, graphics, and platform runners",
        Area::DocsWorkflow => "updates documentation, repository policy, CI, and agent workflow",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_metadata_from_the_complete_branch_inventory() {
        let rendered = render(
            "upstream/main",
            "crates/kira-compiler-bridge/src/root.rs\n\
             crates/kira-manifest/src/parser.rs\n\
             tests-kik/app/package.kira\n\
             crates/kira-cli/src/main.rs\n\
             .codex/skills/working-with-git/SKILL.md",
            "Implement compiler work\nMigrate packages",
        );
        assert_eq!(
            rendered.title,
            "Advance Kira compiler/runtime, packages, tests, and developer tooling"
        );
        assert!(rendered.body.contains(
            "complete `upstream/main...HEAD` branch diff (5 changed files)"
        ));
        assert!(rendered.body.contains("Implement compiler work"));
    }
}
