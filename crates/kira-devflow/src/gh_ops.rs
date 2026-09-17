//! GitHub operations for devflow, via the `gh` CLI. Every query uses `--jq` so
//! the extraction happens in gh and this module stays free of JSON parsing.
//! Guards baked in here: PRs open against an explicit base with complete-branch
//! metadata, and merges are squash-with-merge-subject — one flat entry per PR
//! reading "Merge pull request #N from owner/branch".

use crate::context::{Context, owner_of, repo_of};
use crate::error::DevflowError;
use crate::proc;

/// The current-head check tally for a PR.
pub struct CheckStatus {
    /// The raw `bucket\tname\tstate\tdescription` rows.
    pub lines: String,
    /// Total number of checks.
    pub total: u32,
    /// Checks still pending.
    pub pending: u32,
    /// Checks that failed or were cancelled.
    pub failing: u32,
}

impl CheckStatus {
    /// True only when there is at least one check and none pending or failing.
    pub fn green(&self) -> bool {
        self.total != 0 && self.pending == 0 && self.failing == 0
    }
}

/// Number of an open PR whose head branch is `head` on repo `slug` (works for a
/// cross-fork PR too — `--head` filters by head branch name), or `None`.
/// Keeps PR opens idempotent across retry/resume.
pub fn pr_number_on(ctx: &Context, slug: &str, head: &str) -> Result<Option<u32>, DevflowError> {
    let output = proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "list",
            "-R",
            slug,
            "--head",
            head,
            "--json",
            "number",
            "--jq",
            ".[0].number // empty",
        ],
    )?;
    if output.is_empty() {
        return Ok(None);
    }
    Ok(output.parse::<u32>().ok())
}

/// Number of an open PR for `head` against the fork, or `None`.
pub fn pr_number_for_branch(ctx: &Context, head: &str) -> Result<Option<u32>, DevflowError> {
    pr_number_on(ctx, &ctx.fork_slug, head)
}

/// Open a PR on the fork: `base` <- `head`. Returns the new PR number.
pub fn open_fork_pr(
    ctx: &Context,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<u32, DevflowError> {
    let url = proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "create",
            "-R",
            &ctx.fork_slug,
            "--base",
            base,
            "--head",
            head,
            "--title",
            title,
            "--body",
            body,
        ],
    )?;
    number_from_pr_url(&url)
        .ok_or_else(|| DevflowError::msg(format!("could not parse PR number from {url}")))
}

/// Open a PR from the fork's default branch to upstream's default branch.
pub fn open_upstream_pr(
    ctx: &Context,
    head_slug: &str,
    base: &str,
    head: &str,
    title: &str,
    body: &str,
) -> Result<u32, DevflowError> {
    let upstream = ctx
        .upstream_slug
        .as_deref()
        .ok_or_else(|| DevflowError::msg("no `upstream` remote configured"))?;
    let head_ref = format!("{}:{head}", owner_of(head_slug));
    let url = proc::capture(
        &ctx.repo_root,
        &[
            "gh", "pr", "create", "-R", upstream, "--base", base, "--head", &head_ref, "--title",
            title, "--body", body,
        ],
    )?;
    number_from_pr_url(&url)
        .ok_or_else(|| DevflowError::msg(format!("could not parse PR number from {url}")))
}

/// Refresh an existing PR from the same complete-branch metadata used to open
/// it, so retries correct stale or session-scoped descriptions.
pub fn update_pr(
    ctx: &Context,
    slug: &str,
    number: u32,
    title: &str,
    body: &str,
) -> Result<(), DevflowError> {
    let num = number.to_string();
    proc::check(
        &ctx.repo_root,
        &[
            "gh", "pr", "edit", &num, "-R", slug, "--title", title, "--body", body,
        ],
    )
}

/// Post a comment on a PR.
pub fn comment(ctx: &Context, slug: &str, number: u32, body: &str) -> Result<(), DevflowError> {
    let num = number.to_string();
    proc::check(
        &ctx.repo_root,
        &["gh", "pr", "comment", &num, "-R", slug, "--body", body],
    )
}

/// Comma-joined reviewers whose SUBMITTED review is attached to the PR's current
/// head commit. A review of an older pushed head cannot satisfy the landing gate
/// after new fixes have been added; a walkthrough comment is not a review.
pub fn head_reviewer_logins(
    ctx: &Context,
    slug: &str,
    number: u32,
) -> Result<String, DevflowError> {
    let num = number.to_string();
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "view",
            &num,
            "-R",
            slug,
            "--json",
            "headRefOid,reviews",
            "--jq",
            ".headRefOid as $head | [.reviews[] | select(.commit.oid == $head) | .author.login] | unique | join(\",\")",
        ],
    )
}

/// Comma-joined reviewers whose submitted review is attached to an EARLIER
/// pushed head only. A non-empty result while `head_reviewer_logins` is empty
/// means the bots already reviewed once and need a re-request, not more waiting.
pub fn stale_reviewer_logins(
    ctx: &Context,
    slug: &str,
    number: u32,
) -> Result<String, DevflowError> {
    let num = number.to_string();
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "view",
            &num,
            "-R",
            slug,
            "--json",
            "headRefOid,reviews",
            "--jq",
            ".headRefOid as $head | [.reviews[] | select(.commit.oid != $head) | .author.login] | unique | join(\",\")",
        ],
    )
}

/// The PR's exact current head commit id.
pub fn pr_head_oid(ctx: &Context, slug: &str, number: u32) -> Result<String, DevflowError> {
    let num = number.to_string();
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "view",
            &num,
            "-R",
            slug,
            "--json",
            "headRefOid",
            "--jq",
            ".headRefOid",
        ],
    )
}

/// Current-head check state. `gh pr checks` deliberately exits non-zero while
/// checks are pending or failing, so those documented states are parsed rather
/// than mistaken for an invocation failure.
pub fn pr_check_status(ctx: &Context, slug: &str, number: u32) -> Result<CheckStatus, DevflowError> {
    let num = number.to_string();
    let output = proc::run(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "checks",
            &num,
            "-R",
            slug,
            "--json",
            "bucket,name,state,description",
            "--jq",
            ".[] | [.bucket, .name, .state, (.description // \"\")] | @tsv",
        ],
    )?;
    if !accepted_check_exit(output.code) {
        return Err(DevflowError::msg("gh pr checks query failed"));
    }
    let lines = output.stdout.trim().to_string();
    let counts = count_checks(&lines);
    Ok(CheckStatus {
        lines,
        total: counts.0,
        pending: counts.1,
        failing: counts.2,
    })
}

/// `gh pr checks` exits 0 green, 1 pending/failing, 8 no-checks; all carry JSON.
fn accepted_check_exit(code: Option<i32>) -> bool {
    matches!(code, Some(0 | 1 | 8))
}

/// `(total, pending, failing)` from the tab-separated check rows.
fn count_checks(lines: &str) -> (u32, u32, u32) {
    let (mut total, mut pending, mut failing) = (0, 0, 0);
    for row in lines.lines() {
        if row.is_empty() {
            continue;
        }
        let Some((bucket, _)) = row.split_once('\t') else {
            continue;
        };
        total += 1;
        if bucket.eq_ignore_ascii_case("pending") {
            pending += 1;
        }
        if bucket.eq_ignore_ascii_case("fail")
            || bucket.eq_ignore_ascii_case("cancel")
            || bucket.eq_ignore_ascii_case("cancelled")
        {
            failing += 1;
        }
    }
    (total, pending, failing)
}

/// Exact-head inline findings from bot reviews. Without `include_codex`, this
/// reports CodeRabbit; with it, both required bot reviewers are included.
pub fn head_review_findings(
    ctx: &Context,
    slug: &str,
    number: u32,
    include_codex: bool,
) -> Result<String, DevflowError> {
    let head = pr_head_oid(ctx, slug, number)?;
    let filter = if include_codex {
        "((.user.login | ascii_downcase | contains(\"coderabbit\")) or (.user.login | ascii_downcase | contains(\"codex\")))"
    } else {
        "(.user.login | ascii_downcase | contains(\"coderabbit\"))"
    };
    let jq = format!(
        ".[] | select(.original_commit_id == \"{head}\") | select({filter}) | \"[\\(.user.login)] \\(.path):\\(.line // .original_line // 0)\\n\\(.body)\\n\\(.html_url)\""
    );
    let endpoint = format!("repos/{slug}/pulls/{number}/comments");
    proc::capture(
        &ctx.repo_root,
        &["gh", "api", "--paginate", &endpoint, "--jq", &jq],
    )
}

/// Every workflow-run database id attached to `head`, newest first.
pub fn run_ids_for_head(ctx: &Context, slug: &str, head: &str) -> Result<String, DevflowError> {
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "run",
            "list",
            "-R",
            slug,
            "--commit",
            head,
            "--limit",
            "20",
            "--json",
            "databaseId,createdAt",
            "--jq",
            "sort_by(.createdAt) | reverse | .[].databaseId",
        ],
    )
}

/// Completed workflow-run database ids attached to `head`, newest first.
pub fn completed_run_ids_for_head(
    ctx: &Context,
    slug: &str,
    head: &str,
) -> Result<String, DevflowError> {
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "run",
            "list",
            "-R",
            slug,
            "--commit",
            head,
            "--limit",
            "20",
            "--json",
            "databaseId,status,createdAt",
            "--jq",
            "[.[] | select(.status == \"completed\")] | sort_by(.createdAt) | reverse | .[].databaseId",
        ],
    )
}

/// Failed job ids within a workflow run.
pub fn failed_job_ids_for_run(
    ctx: &Context,
    slug: &str,
    run_id: &str,
) -> Result<String, DevflowError> {
    let endpoint = format!("repos/{slug}/actions/runs/{run_id}/jobs");
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "api",
            "--paginate",
            &endpoint,
            "--jq",
            ".jobs[] | select(.conclusion == \"failure\") | .id",
        ],
    )
}

/// The raw log for a failed job.
pub fn failed_job_log(ctx: &Context, slug: &str, job_id: &str) -> Result<String, DevflowError> {
    let endpoint = format!("repos/{slug}/actions/jobs/{job_id}/logs");
    proc::capture(&ctx.repo_root, &["gh", "api", &endpoint])
}

/// Per-job runner identity and labels for a workflow run, tab-separated.
pub fn workflow_runner_details(
    ctx: &Context,
    slug: &str,
    run_id: &str,
) -> Result<String, DevflowError> {
    let endpoint = format!("repos/{slug}/actions/runs/{run_id}/jobs");
    proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "api",
            "--paginate",
            &endpoint,
            "--jq",
            ".jobs[] | [.name, (.status // \"\"), (.runner_name // \"\"), (.runner_group_name // \"\"), ((.labels // []) | join(\",\"))] | @tsv",
        ],
    )
}

/// Rerun a completed workflow.
pub fn rerun_workflow(ctx: &Context, slug: &str, run_id: &str) -> Result<(), DevflowError> {
    proc::check(&ctx.repo_root, &["gh", "run", "rerun", run_id, "-R", slug])
}

/// CodeRabbit may decline oversized PRs through a successful head check rather
/// than a submitted review. That is still a completed response (with an honest
/// skipped reason), so it must not leave `wait-reviews` hanging forever.
pub fn coderabbit_check_responded(
    ctx: &Context,
    slug: &str,
    number: u32,
) -> Result<bool, DevflowError> {
    let num = number.to_string();
    let output = proc::run(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "checks",
            &num,
            "-R",
            slug,
            "--json",
            "name,state",
            "--jq",
            "[.[] | select((.name | ascii_downcase | contains(\"coderabbit\")) and .state == \"SUCCESS\")] | length",
        ],
    )?;
    if !accepted_check_exit(output.code) {
        return Err(DevflowError::msg("gh pr checks query failed"));
    }
    Ok(output.stdout.trim().parse::<u32>().unwrap_or(0) > 0)
}

/// Count of unresolved review threads across ALL pages (0 = all findings
/// resolved). Pages through `reviewThreads` so a PR with >100 threads cannot
/// hide unresolved findings on a later page and falsely read as resolved.
pub fn unresolved_thread_count(
    ctx: &Context,
    slug: &str,
    number: u32,
) -> Result<u32, DevflowError> {
    let owner = owner_of(slug);
    let repo = repo_of(slug);

    let mut total = 0;
    let mut cursor: Option<String> = None;

    loop {
        let after = match &cursor {
            Some(value) => format!("\"{value}\""),
            None => String::from("null"),
        };
        let query = format!(
            "query {{ repository(owner:\"{owner}\", name:\"{repo}\") {{ pullRequest(number:{number}) {{ reviewThreads(first:100, after:{after}) {{ nodes {{ isResolved }} pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
        );
        let query_arg = format!("query={query}");
        // Emit "<unresolved-on-page>\t<hasNextPage>\t<endCursor>".
        let jq = ".data.repository.pullRequest.reviewThreads | \
             \"\\([.nodes[]|select(.isResolved==false)]|length)\\t\\(.pageInfo.hasNextPage)\\t\\(.pageInfo.endCursor // \"\")\"";
        let output = proc::capture(
            &ctx.repo_root,
            &["gh", "api", "graphql", "-f", &query_arg, "--jq", jq],
        )?;

        let mut fields = output.split('\t');
        let count = fields.next().unwrap_or("0").trim();
        let has_next = fields.next().unwrap_or("false").trim();
        let end_cursor = fields.next().unwrap_or("").trim();

        total += count.parse::<u32>().unwrap_or(0);

        if has_next != "true" || end_cursor.is_empty() {
            break;
        }
        cursor = Some(end_cursor.to_string());
    }
    Ok(total)
}

/// Reply to and resolve the unresolved review thread anchored at `path` (and
/// `line` when several threads sit on the same file). Landing requires zero
/// unresolved threads, and bots do not resolve their own threads — a finding
/// that was addressed (or investigated and rejected with evidence) is closed
/// here so the whole PR flow stays inside devflow.
pub fn resolve_thread_at(
    ctx: &Context,
    slug: &str,
    number: u32,
    path: &str,
    line: u32,
    body: &str,
) -> Result<(), DevflowError> {
    let owner = owner_of(slug);
    let repo = repo_of(slug);

    let query = format!(
        "query {{ repository(owner:\"{owner}\", name:\"{repo}\") {{ pullRequest(number:{number}) {{ reviewThreads(first:100) {{ nodes {{ id isResolved path line originalLine }} }} }} }} }}"
    );
    let query_arg = format!("query={query}");
    let listing = proc::capture(
        &ctx.repo_root,
        &[
            "gh", "api", "graphql", "-f", &query_arg, "--jq",
            ".data.repository.pullRequest.reviewThreads.nodes[] | select(.isResolved==false) | \"\\(.id)\\t\\(.path)\\t\\(.line // .originalLine // 0)\"",
        ],
    )?;

    // Prefer the exact path:line anchor; review threads drift lines as new
    // commits land, so a single unresolved thread on the file also matches.
    let mut exact: Option<&str> = None;
    let mut on_path: Option<&str> = None;
    let mut on_path_count = 0;
    for entry in listing.lines() {
        if entry.is_empty() {
            continue;
        }
        let mut fields = entry.split('\t');
        let (Some(id), Some(entry_path), entry_line) =
            (fields.next(), fields.next(), fields.next().unwrap_or("0").trim())
        else {
            continue;
        };
        if entry_path != path {
            continue;
        }
        on_path = Some(id);
        on_path_count += 1;
        if entry_line.parse::<u32>().unwrap_or(0) == line {
            exact = Some(id);
        }
    }
    let thread_id = exact
        .or(if on_path_count == 1 { on_path } else { None })
        .ok_or(DevflowError::ThreadNotFound)?;

    let tid_arg = format!("tid={thread_id}");
    let body_arg = format!("body={body}");
    proc::capture(
        &ctx.repo_root,
        &[
            "gh", "api", "graphql", "-f",
            "query=mutation($tid:ID!,$body:String!){addPullRequestReviewThreadReply(input:{pullRequestReviewThreadId:$tid,body:$body}){comment{id}}}",
            "-f", &tid_arg, "-f", &body_arg,
        ],
    )?;
    proc::capture(
        &ctx.repo_root,
        &[
            "gh", "api", "graphql", "-f",
            "query=mutation($tid:ID!){resolveReviewThread(input:{threadId:$tid}){thread{isResolved}}}",
            "-f", &tid_arg,
        ],
    )?;
    Ok(())
}

/// Land the PR as ONE commit whose subject reads like GitHub's merge line:
/// `Merge pull request #N from <owner>/<branch>`, PR title as body. This is a
/// SQUASH merge with a custom subject — the only way to get a single flat-list
/// entry per PR (squash collapses the children; a real `--merge` commit
/// re-exposes every child commit) while still reading like a merge.
pub fn land_as_pull_request(ctx: &Context, slug: &str, number: u32) -> Result<(), DevflowError> {
    let num = number.to_string();
    let info = proc::capture(
        &ctx.repo_root,
        &[
            "gh",
            "pr",
            "view",
            &num,
            "-R",
            slug,
            "--json",
            "headRefName,headRepositoryOwner,title",
            "--jq",
            "(.headRepositoryOwner.login // \"\") + \"\\t\" + .headRefName + \"\\t\" + .title",
        ],
    )?;
    let mut fields = info.split('\t');
    let (Some(owner), Some(branch)) = (fields.next(), fields.next()) else {
        return Err(DevflowError::msg("could not parse PR head info"));
    };
    let title = fields.next().unwrap_or("");
    let subject = format!("Merge pull request #{number} from {owner}/{branch}");
    proc::check(
        &ctx.repo_root,
        &[
            "gh", "pr", "merge", &num, "-R", slug, "--squash", "--subject", &subject, "--body",
            title,
        ],
    )
}

/// The trailing PR number from a `.../pull/N` URL.
fn number_from_pr_url(url: &str) -> Option<u32> {
    url.trim().rsplit('/').next()?.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_from_pr_url_reads_the_trailing_id() {
        assert_eq!(
            number_from_pr_url("https://github.com/iPriam/kira/pull/11\n"),
            Some(11)
        );
    }

    #[test]
    fn count_checks_classifies_current_buckets() {
        let counts = count_checks(
            "pass\tlinux\tSUCCESS\t\n\
             pending\tmacos\tIN_PROGRESS\t\n\
             fail\twindows\tFAILURE\tbroken\n\
             skipping\tCodeRabbit\tSUCCESS\toversized",
        );
        assert_eq!(counts, (4, 1, 1));
    }
}
