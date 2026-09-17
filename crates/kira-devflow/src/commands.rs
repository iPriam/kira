//! The devflow verb set and the fork/upstream/PR/AI-review implementations.
//!
//! Each function is a thin orchestration of `git_ops`/`gh_ops` with the flow
//! guards made structural: push is always SSH, land is always squash-as-PR (one
//! flat entry, merge-style subject) + resync, status never trusts ahead/behind
//! counts, and upstream PRs are refused until the fork PR has actually merged.

use std::thread::sleep;
use std::time::Duration;

use crate::context::{Context, owner_of};
use crate::error::DevflowError;
use crate::git_ops::{self, ResyncResult};
use crate::{commit_msg, gh_ops, land_gates, pr_scope};

/// How long between polls in the wait verbs.
const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// A wait verb returns after this long instead of polling forever: a bounded
/// wait surfaces stuck gates (review never re-requested, hung workflow) to the
/// caller, who restarts the same verb to keep waiting.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Every verb devflow accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Status,
    Commit,
    Push,
    PrScope,
    OpenForkPr,
    RequestReviews,
    WaitCi,
    CiFailures,
    CiRunners,
    RerunCi,
    ReviewFindings,
    WaitReviews,
    ResolveThread,
    Land,
    Sync,
    OpenUpstreamPr,
    ReleaseWindow,
    NextVersion,
    ReleasePrep,
    Release,
}

/// All verbs, in help order.
pub const ALL: [Verb; 20] = [
    Verb::Status,
    Verb::Commit,
    Verb::Push,
    Verb::PrScope,
    Verb::OpenForkPr,
    Verb::RequestReviews,
    Verb::WaitCi,
    Verb::CiFailures,
    Verb::CiRunners,
    Verb::RerunCi,
    Verb::ReviewFindings,
    Verb::WaitReviews,
    Verb::ResolveThread,
    Verb::Land,
    Verb::Sync,
    Verb::OpenUpstreamPr,
    Verb::ReleaseWindow,
    Verb::NextVersion,
    Verb::ReleasePrep,
    Verb::Release,
];

impl Verb {
    /// Parse a verb from its command-line spelling.
    pub fn parse(text: &str) -> Option<Self> {
        ALL.iter().copied().find(|verb| verb.label() == text)
    }

    /// The verb's command-line spelling.
    pub fn label(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::PrScope => "pr-scope",
            Self::OpenForkPr => "open-fork-pr",
            Self::RequestReviews => "request-reviews",
            Self::WaitCi => "wait-ci",
            Self::CiFailures => "ci-failures",
            Self::CiRunners => "ci-runners",
            Self::RerunCi => "rerun-ci",
            Self::ReviewFindings => "review-findings",
            Self::WaitReviews => "wait-reviews",
            Self::ResolveThread => "resolve-thread",
            Self::Land => "land",
            Self::Sync => "sync",
            Self::OpenUpstreamPr => "open-upstream-pr",
            Self::ReleaseWindow => "release-window",
            Self::NextVersion => "next-version",
            Self::ReleasePrep => "release-prep",
            Self::Release => "release",
        }
    }
}

/// `status`: honest divergence via content diff, never commit counts.
pub fn status(ctx: &Context) -> Result<(), DevflowError> {
    git_ops::fetch_remote(ctx, "origin")?;
    if ctx.has_upstream() {
        git_ops::fetch_remote(ctx, "upstream")?;
    }

    let fork_ref = format!("origin/{}", ctx.default_branch);
    println!("devflow status (content diff — commit counts are ignored on purpose)");

    if ctx.has_upstream() {
        let up_ref = format!("upstream/{}", ctx.default_branch);
        report_pair(ctx, "fork vs upstream", &fork_ref, &up_ref)?;
    }
    report_pair(ctx, "local vs fork", &ctx.default_branch, &fork_ref)?;

    let branch = git_ops::current_branch(ctx)?;
    let head = git_ops::head_oid(ctx)?;
    println!("  active head: {branch} {head}");
    let working_tree = git_ops::working_tree_summary(ctx)?;
    if working_tree.is_empty() {
        println!("  working tree: CLEAN");
    } else {
        println!("  working tree: CHANGES\n{working_tree}");
    }
    if branch != ctx.default_branch && branch != "HEAD" {
        let fork_branch_ref = format!("origin/{branch}");
        report_pair(ctx, "active branch vs fork", "HEAD", &fork_branch_ref)?;
    }
    Ok(())
}

/// Print whether two refs share a tree, and the diff stat when they do not.
fn report_pair(ctx: &Context, label: &str, a: &str, b: &str) -> Result<(), DevflowError> {
    let stat = git_ops::content_diff_stat(ctx, a, b)?;
    if stat.is_empty() {
        println!("  {label}: IDENTICAL ({a} == {b})");
    } else {
        println!("  {label}: DIFFERS ({a} vs {b})\n{stat}");
    }
    Ok(())
}

/// `commit [-m subject]`: stage everything, commit signed. Auto-infers a
/// Conventional Commit subject when `-m` is not supplied.
pub fn commit(ctx: &Context, explicit_message: Option<&str>) -> Result<(), DevflowError> {
    git_ops::stage_all(ctx)?;
    if !git_ops::has_staged_changes(ctx)? {
        println!("devflow: nothing to commit");
        return Ok(());
    }
    let message = match explicit_message {
        Some(message) => message.to_string(),
        None => commit_msg::infer(&git_ops::staged_name_status(ctx)?),
    };
    git_ops::commit(ctx, &message)?;
    println!("devflow: committed \"{message}\"");
    Ok(())
}

/// `push`: push the current branch to the fork over SSH (workflow-scope proof).
pub fn push(ctx: &Context) -> Result<(), DevflowError> {
    let branch = git_ops::current_branch(ctx)?;
    if branch == "HEAD" {
        return Err(DevflowError::msg("cannot push a detached HEAD"));
    }
    git_ops::push_fork_branch(ctx, &branch)?;
    println!("devflow: pushed {branch} to {}", ctx.fork_ssh_url);
    Ok(())
}

/// `pr-scope`: print metadata computed from the complete branch.
pub fn pr_scope(ctx: &Context) -> Result<(), DevflowError> {
    let metadata = pr_scope::generate(ctx)?;
    print!("{}\n\n{}", metadata.title, metadata.body);
    Ok(())
}

/// `open-fork-pr`: open ONE PR against upstream (single-stage) from the current
/// branch, or refresh an existing PR. Title and body always come from the
/// complete `base...HEAD` branch inventory.
pub fn open_fork_pr(ctx: &Context) -> Result<(), DevflowError> {
    let branch = git_ops::current_branch(ctx)?;
    let metadata = pr_scope::generate(ctx)?;

    if let Some(upstream) = ctx.upstream_slug.as_deref() {
        // Idempotent: if the upstream PR for this branch already exists (retry/
        // resume), report it instead of erroring on `gh pr create`.
        if let Some(existing) = gh_ops::pr_number_on(ctx, upstream, &branch)? {
            gh_ops::update_pr(ctx, upstream, existing, &metadata.title, &metadata.body)?;
            println!("devflow: refreshed PR #{existing} on {upstream} from complete branch scope");
            return Ok(());
        }
        let number = gh_ops::open_upstream_pr(
            ctx,
            &ctx.fork_slug,
            &ctx.default_branch,
            &branch,
            &metadata.title,
            &metadata.body,
        )?;
        println!(
            "devflow: opened PR #{number} on {upstream} ({}:{branch} -> {})",
            owner_of(&ctx.fork_slug),
            ctx.default_branch
        );
        return Ok(());
    }

    if let Some(existing) = gh_ops::pr_number_for_branch(ctx, &branch)? {
        gh_ops::update_pr(ctx, &ctx.fork_slug, existing, &metadata.title, &metadata.body)?;
        println!("devflow: refreshed fork PR #{existing} from complete branch scope");
        return Ok(());
    }
    let number = gh_ops::open_fork_pr(
        ctx,
        &ctx.default_branch,
        &branch,
        &metadata.title,
        &metadata.body,
    )?;
    println!(
        "devflow: opened fork PR #{number} ({branch} -> {})",
        ctx.default_branch
    );
    Ok(())
}

/// `request-reviews <pr> [--codex]`: always ping CodeRabbit; Codex only on demand.
pub fn request_reviews(ctx: &Context, number: u32, ping_codex: bool) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    gh_ops::comment(ctx, slug, number, "@coderabbitai review")?;
    println!("devflow: requested CodeRabbit review on #{number}");
    if ping_codex {
        gh_ops::comment(ctx, slug, number, "@codex review")?;
        println!("devflow: requested Codex review on #{number}");
    }
    Ok(())
}

/// `wait-ci <pr>`: block on the checks attached to the PR's exact current head.
pub fn wait_ci(ctx: &Context, number: u32) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    let head = gh_ops::pr_head_oid(ctx, slug, number)?;
    println!("devflow: waiting for CI on #{number} exact head {head}");

    let mut waited = Duration::ZERO;
    loop {
        let current_head = gh_ops::pr_head_oid(ctx, slug, number)?;
        if head != current_head {
            return Err(DevflowError::msg(format!(
                "PR #{number} head changed while waiting ({head} -> {current_head}); restart the exact-head gate"
            )));
        }
        let checks = gh_ops::pr_check_status(ctx, slug, number)?;
        if checks.failing != 0 {
            return Err(DevflowError::msg(format!(
                "CI failed on #{number} ({} failing check(s))\n{}",
                checks.failing, checks.lines
            )));
        }
        if checks.green() {
            println!(
                "devflow: CI green on #{number} exact head {head} ({} checks)",
                checks.total
            );
            return Ok(());
        }
        if checks.total == 0 {
            println!("devflow: #{number} has no checks on exact head yet; waiting...");
        } else {
            println!(
                "devflow: #{number} has {} pending check(s)\n{}",
                checks.pending, checks.lines
            );
        }
        if waited >= WAIT_TIMEOUT {
            return Err(DevflowError::WaitTimedOut(format!(
                "wait-ci timed out after 5m with the gate still pending; re-run `devflow wait-ci {number}` to keep polling"
            )));
        }
        sleep(POLL_INTERVAL);
        waited += POLL_INTERVAL;
    }
}

/// `ci-failures <pr>`: print failed job logs for workflow runs attached to the
/// PR's exact current head.
pub fn ci_failures(ctx: &Context, number: u32) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    let head = gh_ops::pr_head_oid(ctx, slug, number)?;
    let run_ids = gh_ops::run_ids_for_head(ctx, slug, &head)?;
    if run_ids.is_empty() {
        println!("devflow: no workflow runs on #{number} exact head {head}");
        return Ok(());
    }

    let mut found = false;
    for run_id in run_ids.lines().filter(|line| !line.is_empty()) {
        let job_ids = gh_ops::failed_job_ids_for_run(ctx, slug, run_id)?;
        for job_id in job_ids.lines().filter(|line| !line.is_empty()) {
            found = true;
            let log = gh_ops::failed_job_log(ctx, slug, job_id)?;
            let excerpt = failure_excerpt(&log);
            println!(
                "devflow: failed CI excerpt for #{number} exact head {head}, run {run_id}, job {job_id}\n{excerpt}"
            );
        }
    }
    if !found {
        println!("devflow: no failed jobs on #{number} exact head {head}");
    }
    Ok(())
}

/// Keep only the lines of a CI log that name a failure, plus context after a
/// backend parity failure, capped so a giant log cannot flood the terminal.
fn failure_excerpt(log: &str) -> String {
    let mut result = String::new();
    let mut emitted = 0;
    let mut parity_context = 0;
    for line in log.lines() {
        let relevant = failure_relevant(line);
        if line.contains("FAIL <parity") {
            parity_context = 40;
        }
        if !relevant && parity_context == 0 {
            continue;
        }
        result.push_str(line);
        result.push('\n');
        if !relevant && parity_context != 0 {
            parity_context -= 1;
        }
        emitted += 1;
        if emitted == 400 {
            result.push_str("... failure excerpt capped at 400 matching lines ...\n");
            break;
        }
    }
    if emitted == 0 {
        result.push_str(
            "(job failed without a matching error line; inspect the job URL from ci-runners)\n",
        );
    }
    result
}

/// Whether a log line names a failure worth keeping in the excerpt.
fn failure_relevant(line: &str) -> bool {
    const NEEDLES: [&str; 16] = [
        "##[error]",
        " error:",
        "error[",
        "failed",
        "FAIL ",
        "panicked",
        "undefined reference",
        "linker",
        "clang:",
        "lld:",
        "kira llvm backend",
        "/usr/bin/x86_64-linux-gnu-ld:",
        "Process completed with exit code",
        "A connection attempt failed",
        "dial tcp",
        "LNK",
    ];
    NEEDLES.iter().any(|needle| line.contains(needle))
}

/// `ci-runners <pr>`: report the actual runner assigned to every job attached
/// to the PR's exact current head, including the requested runner labels.
pub fn ci_runners(ctx: &Context, number: u32) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    let head = gh_ops::pr_head_oid(ctx, slug, number)?;
    let run_ids = gh_ops::run_ids_for_head(ctx, slug, &head)?;
    if run_ids.is_empty() {
        println!("devflow: no workflow runs on #{number} exact head {head}");
        return Ok(());
    }

    println!("devflow: CI runners on #{number} exact head {head}");
    for run_id in run_ids.lines().filter(|line| !line.is_empty()) {
        let details = gh_ops::workflow_runner_details(ctx, slug, run_id)?;
        if details.is_empty() {
            println!("  run {run_id}: jobs have not been created yet");
        } else {
            println!("  run {run_id}\n{details}");
        }
    }
    Ok(())
}

/// `rerun-ci <pr>`: rerun every completed workflow attached to the PR's exact
/// head. Useful after changing repository runner-provider configuration.
pub fn rerun_ci(ctx: &Context, number: u32) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    let head = gh_ops::pr_head_oid(ctx, slug, number)?;
    let run_ids = gh_ops::completed_run_ids_for_head(ctx, slug, &head)?;
    if run_ids.is_empty() {
        println!("devflow: no completed workflow runs to rerun on #{number} exact head {head}");
        return Ok(());
    }
    for run_id in run_ids.lines().filter(|line| !line.is_empty()) {
        gh_ops::rerun_workflow(ctx, slug, run_id)?;
        println!("devflow: reran workflow {run_id} on #{number} exact head {head}");
    }
    Ok(())
}

/// `review-findings <pr> [--codex]`: print exact-head inline findings without
/// bypassing devflow for ad-hoc GitHub reads.
pub fn review_findings(ctx: &Context, number: u32, include_codex: bool) -> Result<(), DevflowError> {
    let findings = gh_ops::head_review_findings(ctx, ctx.pr_slug(), number, include_codex)?;
    if findings.is_empty() {
        println!("devflow: no exact-head inline findings on #{number}");
    } else {
        println!("devflow: exact-head inline findings on #{number}\n{findings}");
    }
    Ok(())
}

/// `wait-reviews <pr> [--codex]`: block until required reviewers have posted and
/// no unresolved review threads remain. This is the gate that stops the flow
/// advancing while findings are still open.
pub fn wait_reviews(ctx: &Context, number: u32, require_codex: bool) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    let mut waited = Duration::ZERO;
    loop {
        // Gate on SUBMITTED reviews, not comments: a bot walkthrough comment or
        // a rate-limited/incomplete review must not read as "reviewed".
        let logins = gh_ops::head_reviewer_logins(ctx, slug, number)?;
        let has_rabbit =
            logins.contains("coderabbit") || gh_ops::coderabbit_check_responded(ctx, slug, number)?;
        let has_codex = logins.contains("codex");
        let reviewers_seen = has_rabbit && (!require_codex || has_codex);

        if reviewers_seen {
            let unresolved = gh_ops::unresolved_thread_count(ctx, slug, number)?;
            if unresolved == 0 {
                println!("devflow: reviews complete on #{number}, no unresolved threads");
                return Ok(());
            }
            println!("devflow: #{number} has {unresolved} unresolved review thread(s); waiting...");
        } else {
            // A REQUIRED bot whose review sits on an EARLIER head has already
            // responded once and will not re-review on its own — waiting cannot
            // succeed until reviews are re-requested, so surface that now
            // instead of burning the whole wait window. Stale HUMAN reviews are
            // ignored: they never satisfy the bot gate in the first place.
            let stale = gh_ops::stale_reviewer_logins(ctx, slug, number)?;
            let stale_rabbit = !has_rabbit && stale.contains("coderabbit");
            let stale_codex = require_codex && !has_codex && stale.contains("codex");
            if stale_rabbit || stale_codex {
                return Err(DevflowError::msg(format!(
                    "#{number} has required reviews only on an EARLIER head (stale: {stale}); run `devflow request-reviews {number}` for the current head, then wait again"
                )));
            }
            println!("devflow: waiting for reviews on #{number} (seen: {logins})");
        }

        if waited >= WAIT_TIMEOUT {
            return Err(DevflowError::WaitTimedOut(format!(
                "wait-reviews timed out after 5m with the gate still pending; re-run `devflow wait-reviews {number}` to keep polling"
            )));
        }
        sleep(POLL_INTERVAL);
        waited += POLL_INTERVAL;
    }
}

/// `resolve-thread <pr> <path>:<line> -m "reason"`: reply to and resolve the
/// unresolved review thread at that anchor. Only for findings actually
/// addressed in a pushed commit or investigated and rejected with evidence —
/// the reason must say which.
pub fn resolve_thread(
    ctx: &Context,
    number: u32,
    target: &str,
    body: &str,
) -> Result<(), DevflowError> {
    let (path, line_text) = target
        .rsplit_once(':')
        .ok_or_else(|| DevflowError::msg("resolve-thread target must be <path>:<line>"))?;
    let line = line_text
        .parse::<u32>()
        .map_err(|_| DevflowError::msg(format!("invalid line in thread target \"{target}\"")))?;

    match gh_ops::resolve_thread_at(ctx, ctx.pr_slug(), number, path, line, body) {
        Ok(()) => {
            println!("devflow: replied to and resolved thread {target} on #{number}");
            Ok(())
        }
        Err(DevflowError::ThreadNotFound) => Err(DevflowError::msg(format!(
            "no unresolved thread at {target} on #{number} (already resolved, or line drifted — check `review-findings {number}`)"
        ))),
        Err(other) => Err(other),
    }
}

/// `land <pr> [--codex] [--force]`: refuse unless the required reviewers have
/// SUBMITTED a review and no threads are unresolved, then land as one squash
/// commit with a "Merge pull request #N from ..." subject and resync the
/// local default branch. Checking only unresolved-thread-count is unsafe: it
/// is 0 before reviews post, so land applies the same participant gate as
/// wait-reviews. `--force` lands anyway, printing the bypassed gates.
pub fn land(
    ctx: &Context,
    number: u32,
    require_codex: bool,
    force: bool,
) -> Result<(), DevflowError> {
    let slug = ctx.pr_slug();
    let checks = gh_ops::pr_check_status(ctx, slug, number)?;
    let logins = gh_ops::head_reviewer_logins(ctx, slug, number)?;
    let has_rabbit =
        logins.contains("coderabbit") || gh_ops::coderabbit_check_responded(ctx, slug, number)?;
    let has_codex = logins.contains("codex");
    let unresolved = gh_ops::unresolved_thread_count(ctx, slug, number)?;
    match land_gates::refusal(
        &checks,
        &logins,
        has_rabbit,
        has_codex,
        require_codex,
        unresolved,
    ) {
        Some(reason) if force => {
            println!("devflow: --force: landing #{number} despite failing gates: {reason}")
        }
        Some(reason) => {
            return Err(DevflowError::msg(format!(
                "refusing to land #{number}: {reason} (override with `devflow land {number} --force`)"
            )));
        }
        None => {}
    }

    gh_ops::land_as_pull_request(ctx, slug, number)?;
    println!("devflow: landed PR #{number} on {slug} (squash, 'Merge pull request' subject)");

    // Single-stage: keep the fork a pure mirror of upstream so it never diverges.
    if ctx.has_upstream() {
        git_ops::mirror_fork_to_upstream(ctx)?;
        println!(
            "devflow: mirrored fork {branch} to upstream/{branch}",
            branch = ctx.default_branch
        );
    }

    let backup = format!("devflow/prelanded-{}", ctx.default_branch);
    report_resync(
        git_ops::resync_local_default_branch(ctx, "origin", &backup)?,
        ctx,
        &backup,
        true,
    );
    Ok(())
}

/// `sync`: resync the local default branch to the fork remote (standalone, for
/// when a land happened elsewhere and local main drifted).
pub fn sync(ctx: &Context) -> Result<(), DevflowError> {
    let backup = format!("devflow/presync-{}", ctx.default_branch);
    report_resync(
        git_ops::resync_local_default_branch(ctx, "origin", &backup)?,
        ctx,
        &backup,
        false,
    );
    Ok(())
}

/// Print the outcome of a resync, worded for `land` or `sync`.
fn report_resync(result: ResyncResult, ctx: &Context, backup: &str, after_land: bool) {
    let branch = &ctx.default_branch;
    match (result, after_land) {
        (ResyncResult::AlreadySynced, true) => {
            println!("devflow: local branch already in sync with fork");
        }
        (ResyncResult::AlreadySynced, false) => println!("devflow: already in sync"),
        (ResyncResult::FastForwarded, true) => {
            println!("devflow: local {branch} fast-forwarded to origin");
        }
        (ResyncResult::FastForwarded, false) => {
            println!("devflow: fast-forwarded {branch} to origin");
        }
        (ResyncResult::ResetWithBackup, true) => println!(
            "devflow: local {branch} reset to origin (post-merge); prior tip backed up on {backup}"
        ),
        (ResyncResult::ResetWithBackup, false) => {
            println!("devflow: reset {branch} to origin; prior tip on {backup}");
        }
        (ResyncResult::SkippedDirty, true) => println!(
            "devflow: WARNING working tree dirty — local branch NOT resynced (commit/stash then re-run `devflow sync`)"
        ),
        (ResyncResult::SkippedDirty, false) => {
            println!("devflow: working tree dirty — not touched");
        }
    }
}

/// `open-upstream-pr`: only valid with an upstream remote. Opens the fork
/// default branch against the upstream default branch with complete metadata.
pub fn open_upstream_pr(ctx: &Context) -> Result<(), DevflowError> {
    let Some(upstream) = ctx.upstream_slug.as_deref() else {
        return Err(DevflowError::msg("no `upstream` remote configured"));
    };
    git_ops::fetch_remote(ctx, "origin")?;
    let metadata = pr_scope::generate(ctx)?;
    let number = gh_ops::open_upstream_pr(
        ctx,
        &ctx.fork_slug,
        &ctx.default_branch,
        &ctx.default_branch,
        &metadata.title,
        &metadata.body,
    )?;
    println!(
        "devflow: opened upstream PR #{number} ({}:{branch} -> {upstream}:{branch})",
        ctx.fork_slug,
        branch = ctx.default_branch
    );
    Ok(())
}
