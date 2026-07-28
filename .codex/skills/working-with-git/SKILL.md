---
name: working-with-git
description: "The git contract here: every command that can discard, set aside, or rewrite work is refused — reversible ones included (`stash`, `reset --soft`, `commit --amend`), and read commands whose output is redirected onto a tracked path (`git show <rev>:<path> > <path>`) with them — because the working tree is assumed to hold uncommitted work at all times; plus the non-destructive route to each goal, what this checkout actually is, and commit rules. Load before running or suggesting any git command other than `git diff` and `git status` — including `git log`, `git add`, `git commit`, and any git command buried inside a compound shell command."
---

# Working with git

Assume the working tree **always** holds uncommitted work that exists nowhere
else. Never "if the tree is dirty", never "it looked clean" — always. Check
nothing to earn an exception: this is not a claim about the tree, it is the
rule that decides which commands may run at all.

Everything below follows from it. A command whose safety depends on the tree
being clean is a command that is never safe here.

## Refuse the whole category, not a list of names

Refuse every git command that can discard, set aside, or rewrite work:

- **Discards:** `reset` (any mode, `--hard` included), `restore`,
  `checkout -- <file>`, `clean`, `rm`, `branch -D`, `worktree remove`
- **Sets aside:** `stash` in every form — `push`, `save`, `pop`, `drop`, and a
  bare `git stash`
- **Rewrites:** `commit --amend`, `rebase`, `filter-branch`, `push --force`,
  `push --force-with-lease`
- **Collects:** `gc --prune`, `reflog expire`
- **Overwrites:** any command that puts history's copy of a file back into the
  tree — `git show <rev>:<path> > <path>`, `git cat-file -p <blob> > <path>`,
  `git archive` unpacked over the checkout

`.codex/hooks/refuse-destructive-git.sh` denies these before they run, on every
Bash call. Expect it to catch the named shapes and nothing else: it allows what
it does not recognize, because a hook that guessed would block ordinary work.
Read the list as examples of the category, never as its boundary, and judge an
unnamed command by what it can do to work that exists in one place only.

**Read the whole line, never the subcommand.** `show`, `cat-file` and `archive`
are reads right up until their output is redirected onto a path in the working
tree, where they overwrite it exactly as `checkout -- <path>` would. A safe
subcommand is not a safe command.

Refuse a banned command **even when it is reversible** — `stash` has `pop`,
`reset --soft` keeps the diff, `amend` leaves the old commit in the reflog —
and even when you took a backup yourself a moment earlier. Recovery is a claim
about a step that has not happened yet, or about one whose coverage of what
preceded you is exactly what you did not check.

Extend the refusal to **suggesting** any of them. The hook reads commands, not
prose: a banned command in a code block is one paste from being a run command.

## Reach the goal another way

Recognize the goal behind the banned command and take the route that keeps the
work:

| Goal | Not this | This |
|---|---|---|
| See a test fail without your fix | `stash`, `reset`, `restore` | copy the one file aside (`cp x.rs "$TMP/x.rs.bak"`), edit it, run it, copy it back. One file, no git, no blast radius |
| Measure your change against the code without it | `git show <rev>:<path> > <path>` to put the old version back | build the current tree, copy the *artifact* aside (`cp target/release/x "$TMP/x.before"`), then edit forward to the other version and build again. Compare two artifacts, never two states of the tree |
| Get a clean tree to test | `stash` | commit on a scratch branch, or build a separate worktree |
| Local `main` drifted from remote | `reset --hard origin/main` | `git switch -c backup-<date>` first, then `git merge --ff-only origin/main` — when it refuses, the drift is real content, so read it before deciding |
| Undo a commit already made | `reset --hard HEAD~1` | `git revert <sha>` — keeps history and the work |
| Fix the commit just made | `commit --amend` | a second commit; squash later only on a branch nobody else has |
| Abandon working-tree edits | `checkout -- <file>` / `restore` | leave them; commit them on a scratch branch; or ask. WIP in a worktree is never yours to discard |
| Take one commit from elsewhere | reset + reapply | `git cherry-pick <sha>` |

When none of these reach the goal, say so and stop. Never read "no safe route
exists" as licence to take the unsafe one.

## Respect what this checkout actually is

- Expect one remote: `origin` → `github.com/kira-lang-com/kira.git`. Assume
  **no `upstream` remote** and no fork — never write instructions or commands
  that presume one.
- Treat `devflow` (`crates/kira-devflow`) as a **stub**: every verb prints
  `not yet implemented` and exits 2. Never route work through it or cite it as
  the flow until it is built. Use plain `git`/`gh` here.
- Note that commit signing is unconfigured in this checkout. Never pass
  `--no-gpg-sign` to work around signing, and never claim commits are signed.

## Write the commit

- Omit `Co-Authored-By` and AI-attribution trailers.
- Say what changed and why it matters in the subject, in the imperative,
  matching the existing log (`Emit the native half of a hybrid program`, not
  `feat: add hybrid emit`).
- State the reason in the body for a change to a frozen root crate
  (`kira-core`, `kira-source`, the `*-model` crates) — that is the rule those
  crates carry.
- Commit directly to the checked-out `main` for local iteration.

## Recognize the outward-facing commands

Defer to `AGENTS.md`'s **Scope** rule, which is always loaded and governs
these; it is not repeated here. Read these as the outward-facing ones in git
terms: `push`, `gh pr create`, `gh pr merge`, `gh pr review`, force-push,
branch deletion. Run them only when the user named that step — an agent in
the sibling repo chained a PR off a "commit" instruction and was told *"Didn't
say to open a pr. I said commit"*.
