//! Shared devflow context: repo root and the fork/upstream identity derived
//! from `git remote` URLs. Nothing here is hardcoded to a particular GitHub
//! account — the slugs and the SSH push URL are parsed from the actual
//! `origin` and `upstream` remotes so the tool works for any fork.

use crate::error::DevflowError;
use crate::proc;

/// The repository identity every flow verb runs against.
pub struct Context {
    /// Absolute path to the repository root.
    pub repo_root: String,
    /// e.g. `iPriam/kira` — the fork (origin).
    pub fork_slug: String,
    /// e.g. `kira-lang-com/kira` — upstream, or `None` with no upstream remote.
    pub upstream_slug: Option<String>,
    /// SSH push URL for the fork, e.g. `git@github.com:iPriam/kira.git`.
    ///
    /// Used for every push so an OAuth token missing the `workflow` scope
    /// cannot reject pushes that touch `.github/workflows/*`.
    pub fork_ssh_url: String,
    /// Default branch name, e.g. `main`.
    pub default_branch: String,
}

impl Context {
    /// Whether an `upstream` remote is configured.
    pub fn has_upstream(&self) -> bool {
        self.upstream_slug.is_some()
    }

    /// Single-stage: the PR lives on upstream when there is an upstream remote
    /// (the owner is a maintainer, so there is one landing — on upstream).
    /// Falls back to the fork only when no upstream remote is configured.
    pub fn pr_slug(&self) -> &str {
        self.upstream_slug.as_deref().unwrap_or(&self.fork_slug)
    }

    /// Discover the context from the surrounding git clone.
    pub fn discover() -> Result<Self, DevflowError> {
        let repo_root = proc::capture(".", &["git", "rev-parse", "--show-toplevel"])?;
        let default_branch = detect_default_branch(&repo_root);

        let origin_url = proc::capture(&repo_root, &["git", "remote", "get-url", "origin"])
            .map_err(|_| {
                DevflowError::msg("no `origin` remote found; run inside the fork clone")
            })?;
        let fork_slug = slug_from_url(&origin_url)?;
        let fork_ssh_url = ssh_url_from_slug(&fork_slug);

        let upstream_slug = proc::capture(&repo_root, &["git", "remote", "get-url", "upstream"])
            .ok()
            .and_then(|url| slug_from_url(&url).ok());

        Ok(Self {
            repo_root,
            fork_slug,
            upstream_slug,
            fork_ssh_url,
            default_branch,
        })
    }
}

/// The default branch from `origin/HEAD`, or `main` when it cannot be read.
fn detect_default_branch(repo_root: &str) -> String {
    if let Ok(reference) =
        proc::capture(repo_root, &["git", "rev-parse", "--abbrev-ref", "origin/HEAD"])
        && let Some((_, name)) = reference.rsplit_once('/')
    {
        return name.to_string();
    }
    String::from("main")
}

/// Extract `owner/repo` from an https or ssh GitHub remote URL.
///
/// Handles `https://github.com/owner/repo.git`, `git@github.com:owner/repo.git`,
/// and `ssh://git@github.com/owner/repo.git` (trailing `.git` optional).
pub fn slug_from_url(url: &str) -> Result<String, DevflowError> {
    let mut slug = url.trim();

    // Strip scheme / host prefixes down to `owner/repo`.
    if let Some(index) = slug.find("github.com") {
        slug = &slug[index + "github.com".len()..];
    }
    // After the host there is either ':' (scp-like) or '/' (url path).
    slug = slug.trim_start_matches([':', '/']);
    slug = slug.strip_suffix(".git").unwrap_or(slug);
    slug = slug.trim_matches('/');

    if !slug.contains('/') {
        return Err(DevflowError::msg(format!("unparseable remote URL: {url}")));
    }
    Ok(slug.to_string())
}

/// The SSH push URL for a `owner/repo` slug.
pub fn ssh_url_from_slug(slug: &str) -> String {
    format!("git@github.com:{slug}.git")
}

/// The `owner` half of a `owner/repo` slug.
pub fn owner_of(slug: &str) -> &str {
    slug.split_once('/').map_or(slug, |(owner, _)| owner)
}

/// The `repo` half of a `owner/repo` slug.
pub fn repo_of(slug: &str) -> &str {
    slug.split_once('/').map_or(slug, |(_, repo)| repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_from_url_parses_https_and_ssh_forms() {
        for url in [
            "https://github.com/iPriam/kira.git",
            "git@github.com:iPriam/kira.git",
            "ssh://git@github.com/iPriam/kira.git",
            "https://github.com/iPriam/kira",
        ] {
            assert_eq!(slug_from_url(url).expect("slug"), "iPriam/kira");
        }
    }

    #[test]
    fn ssh_url_from_slug_builds_push_url() {
        assert_eq!(
            ssh_url_from_slug("iPriam/kira"),
            "git@github.com:iPriam/kira.git"
        );
    }

    #[test]
    fn owner_and_repo_split() {
        assert_eq!(owner_of("iPriam/kira"), "iPriam");
        assert_eq!(repo_of("iPriam/kira"), "kira");
    }
}
