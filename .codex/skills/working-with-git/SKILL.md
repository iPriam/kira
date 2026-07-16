---
name: working-with-git
description: "Git rules for this repo: the banned destructive commands and the non-destructive way to reach the same goal, what this checkout actually has (no upstream remote, devflow unimplemented), commit message rules, and the ask-before-outward-facing-steps line. Read before any git operation, and before proposing a git command to the user."
---

# Working with git

Refuse destructive git — no `reset --hard`, `restore`, `checkout -- <file>`,
`stash drop`; worktrees may carry uncommitted WIP that those commands would
discard irreversibly.

Extend that ban to **suggesting** them. In the sibling repo an agent wrote out
`git switch main && git reset --hard upstream/main` as "one command" to fix
branch drift, caught itself only afterward, and that repo's user had already
said *"You removed my work 6 times this week, enough with the dangerous git
operations!"*. Treat a banned command in a code block as one paste from being
a run command.

## Reach the goal another way

Recognize the goal behind `reset --hard`: almost always "make my branch look
like theirs" or "drop this change". Prefer these routes:

| Goal | Not this | This |
|---|---|---|
| Local `main` drifted from remote | `reset --hard origin/main` | `git switch -c backup-<date>` first, then fast-forward: `git merge --ff-only origin/main` — when it refuses, the drift is real content, so read it before deciding |
| Undo a commit already made | `reset --hard HEAD~1` | `git revert <sha>` — keeps history and the work |
| Abandon working-tree edits | `checkout -- <file>` / `restore` | leave them; commit them on a scratch branch; or ask. WIP in a worktree is never yours to discard |
| Get a clean tree to test | `stash` + `stash drop` | commit on a branch, or use a separate worktree |
| Take one commit from elsewhere | reset + reapply | `git cherry-pick <sha>` |

When none of these reach the goal, say so — never treat it as licence to
reach for the banned command.

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