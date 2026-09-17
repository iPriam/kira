//! Git-level operations for devflow. Every guard that keeps the fork/upstream
//! flow honest lives here: content-diff instead of ahead/behind counts, always
//! push via the SSH remote, and a post-land resync that refuses to run on a
//! dirty tree so it can never destroy uncommitted work.

use crate::context::Context;
use crate::error::DevflowError;
use crate::proc;

/// The branch `HEAD` currently points at, or `HEAD` when detached.
pub fn current_branch(ctx: &Context) -> Result<String, DevflowError> {
    proc::capture(&ctx.repo_root, &["git", "rev-parse", "--abbrev-ref", "HEAD"])
}

/// The full commit id at `HEAD`.
pub fn head_oid(ctx: &Context) -> Result<String, DevflowError> {
    proc::capture(&ctx.repo_root, &["git", "rev-parse", "HEAD"])
}

/// `git status --short` for the working-tree summary line.
pub fn working_tree_summary(ctx: &Context) -> Result<String, DevflowError> {
    proc::capture(&ctx.repo_root, &["git", "status", "--short"])
}

/// Fetch a remote quietly.
pub fn fetch_remote(ctx: &Context, remote: &str) -> Result<(), DevflowError> {
    proc::check(&ctx.repo_root, &["git", "fetch", "--quiet", remote])
}

/// True when the working tree has no uncommitted changes (tracked or staged).
///
/// Untracked files are ignored so scratch under `.codex/tmp` does not block flow.
pub fn working_tree_clean(ctx: &Context) -> Result<bool, DevflowError> {
    let output = proc::capture(
        &ctx.repo_root,
        &["git", "status", "--porcelain", "--untracked-files=no"],
    )?;
    Ok(output.is_empty())
}

/// Stage everything.
pub fn stage_all(ctx: &Context) -> Result<(), DevflowError> {
    proc::check(&ctx.repo_root, &["git", "add", "-A"])
}

/// True when there is anything staged to commit.
pub fn has_staged_changes(ctx: &Context) -> Result<bool, DevflowError> {
    let output = proc::run(&ctx.repo_root, &["git", "diff", "--cached", "--quiet"])?;
    // `--quiet` exits 0 when clean and exactly 1 when there ARE staged changes.
    // Any other exit code is a real git error (corrupt index, lock, ...) and
    // must not be silently reported as "has staged changes".
    match output.code {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(DevflowError::msg("git diff --cached --quiet failed")),
    }
}

/// `git diff --cached --name-status`, used to infer a commit message.
pub fn staged_name_status(ctx: &Context) -> Result<String, DevflowError> {
    proc::capture(&ctx.repo_root, &["git", "diff", "--cached", "--name-status"])
}

/// Commit staged changes. Signing is left to the repo/user git config and is
/// never bypassed here (no `--no-gpg-sign`). Fails loudly if the commit fails.
pub fn commit(ctx: &Context, message: &str) -> Result<(), DevflowError> {
    proc::check(&ctx.repo_root, &["git", "commit", "-m", message])
}

/// Push `branch` to the fork over SSH. Using the SSH URL (not the https origin)
/// means a token lacking the `workflow` scope cannot reject pushes that touch
/// `.github/workflows/*` — the recurring "refusing to allow an OAuth App" error.
pub fn push_fork_branch(ctx: &Context, branch: &str) -> Result<(), DevflowError> {
    proc::check(
        &ctx.repo_root,
        &["git", "push", "-u", &ctx.fork_ssh_url, branch],
    )
}

/// The content difference between two refs, as `git diff --stat`. This is the
/// ONLY honest divergence signal: ahead/behind commit counts lie after a merge
/// (a squash rewrites SHAs; a merge commit adds new ones) whereas an empty diff
/// proves identical trees.
pub fn content_diff_stat(ctx: &Context, a: &str, b: &str) -> Result<String, DevflowError> {
    proc::capture(&ctx.repo_root, &["git", "diff", "--stat", a, b])
}

/// Every path changed by the complete branch, relative to its merge base with
/// `base`. This is PR scope; the working tree and current session are
/// deliberately irrelevant.
pub fn branch_changed_files(ctx: &Context, base: &str) -> Result<String, DevflowError> {
    let range = format!("{base}...HEAD");
    proc::capture(
        &ctx.repo_root,
        &[
            "git",
            "diff",
            "--name-only",
            "--diff-filter=ACDMRTUXB",
            &range,
        ],
    )
}

/// Subjects of every commit in the complete branch, oldest first.
pub fn branch_commit_subjects(ctx: &Context, base: &str) -> Result<String, DevflowError> {
    let range = format!("{base}..HEAD");
    proc::capture(
        &ctx.repo_root,
        &["git", "log", "--reverse", "--format=%s", &range],
    )
}

/// True when refs `a` and `b` point at identical trees (empty content diff).
pub fn trees_identical(ctx: &Context, a: &str, b: &str) -> Result<bool, DevflowError> {
    Ok(content_diff_stat(ctx, a, b)?.is_empty())
}

/// Move a branch pointer (create or force-update) without checking it out.
pub fn set_branch(ctx: &Context, name: &str, target: &str) -> Result<(), DevflowError> {
    proc::check(&ctx.repo_root, &["git", "branch", "-f", name, target])
}

/// Single-stage flow: after a change lands on upstream, force the fork's default
/// branch to `upstream/<default>` so the fork stays a pure 0-ahead/0-behind
/// mirror. Fork `main` is unprotected and its content is already on upstream, so
/// this loses nothing. Pushed over SSH (workflow-scope proof).
pub fn mirror_fork_to_upstream(ctx: &Context) -> Result<(), DevflowError> {
    fetch_remote(ctx, "upstream")?;
    let refspec = format!(
        "upstream/{branch}:{branch}",
        branch = ctx.default_branch
    );
    proc::check(
        &ctx.repo_root,
        &["git", "push", &ctx.fork_ssh_url, &refspec, "--force"],
    )
}

/// The outcome of a resync attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResyncResult {
    /// Already identical — nothing to do.
    AlreadySynced,
    /// Fast-forwarded cleanly.
    FastForwarded,
    /// Diverged (post-merge); reset local branch to the remote after backing
    /// up the previous tip on the backup ref.
    ResetWithBackup,
    /// Skipped because the working tree was dirty — never destroy WIP.
    SkippedDirty,
}

/// Bring the local default branch in line with `remote/<default>` after a land.
///
/// This is the step whose absence leaves local main stranded on pre-merge
/// commits. It refuses to touch a dirty tree, and always backs up the prior tip
/// on `backup_ref` before any reset so no committed work can be lost.
pub fn resync_local_default_branch(
    ctx: &Context,
    remote: &str,
    backup_ref: &str,
) -> Result<ResyncResult, DevflowError> {
    fetch_remote(ctx, remote)?;

    let remote_ref = format!("{remote}/{}", ctx.default_branch);
    if trees_identical(ctx, &ctx.default_branch, &remote_ref)? {
        return Ok(ResyncResult::AlreadySynced);
    }
    if !working_tree_clean(ctx)? {
        return Ok(ResyncResult::SkippedDirty);
    }

    // Preserve the current local tip before moving it.
    set_branch(ctx, backup_ref, &ctx.default_branch)?;

    // Switch onto the default branch if we are not already there.
    let branch = current_branch(ctx)?;
    if branch != ctx.default_branch {
        proc::check(&ctx.repo_root, &["git", "switch", &ctx.default_branch])?;
    }

    // Try a fast-forward first; fall back to a hard reset onto the remote.
    let ff = proc::run(&ctx.repo_root, &["git", "merge", "--ff-only", &remote_ref])?;
    if ff.success {
        return Ok(ResyncResult::FastForwarded);
    }

    // Diverged (post-merge). Reset to the remote; the prior tip is on backup_ref.
    proc::check(&ctx.repo_root, &["git", "reset", "--hard", &remote_ref])?;
    Ok(ResyncResult::ResetWithBackup)
}
