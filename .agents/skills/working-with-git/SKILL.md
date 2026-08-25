---
name: working-with-git
description: "Read before running or suggesting any git command except `git diff` and `git status`, including commands inside compound shell lines. Preserve uncommitted work, keep history forward-only, and require explicit user direction for remote actions."
---

Assume all uncommitted work is unique. Refuse commands or suggestions that can discard, hide, rewrite, prune, or overwrite work, even when reversible or backed up.

This includes `reset`, `restore`, `checkout -- <path>`, `clean`, `rm`, every `stash`, amend, rebase, history filtering, force-push, pruning, `branch -D`, `worktree remove`, and redirects that write Git history over tracked files. Judge the whole shell line.

Use scratch branches or worktrees for isolated state, `revert` to undo commits, new commits to correct commits, and `cherry-pick` to import commits. Stop when no preserving route exists.

Commit local iteration directly to `main`. Use an imperative subject matching repository history. Omit AI and co-author trailers. Explain frozen-root changes in the body.

Run `push`, `gh pr create`, `gh pr merge`, `gh pr review`, or branch deletion only when the user explicitly requests that action.