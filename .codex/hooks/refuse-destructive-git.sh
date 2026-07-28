#!/bin/sh
# Refuse, before it runs, every git command `working-with-git` bans.
#
# The skill is instruction, and instruction has now lost to reflex three times
# in this repo — twice on commands the skill named, once on `git show <rev>:<p>
# > <p>`, which it did not. This is the part that does not depend on the rule
# being consulted at the moment of typing.
#
# Reads the PreToolUse payload on stdin and denies by printing a permission
# decision. Anything it does not recognize is allowed: a hook that guesses
# would block ordinary work, and the guessing is what the skill is for.
#
# Disable with /hooks, or delete this file's entry from .codex/settings.json.
set -eu

command=$(jq -r '.tool_input.command // empty' 2>/dev/null || true)
[ -n "$command" ] || exit 0

# `git` in *command position* — starting the line, or following a separator.
# Without this, a `grep "git stash"` over these very rules would be refused.
git='(^|[;&|(]|&&|\|\|)[[:space:]]*git[[:space:]]+'

refuse() {
	reason=$(printf '%s' "$1" | jq -Rs .)
	printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":%s}}\n' "$reason"
	exit 0
}

matches() {
	printf '%s' "$command" | grep -Eq "$1"
}

# Discards, sets-aside and rewrites, in the skill's own grouping.
if matches "${git}(reset|restore|clean|stash|rm)([[:space:]]|\$)"; then
	refuse "working-with-git refuses this: it can discard or set aside work that exists in one place only. Reach the goal another way — the skill's table has a row for each of these. Ask the user if none of them fit."
fi
if matches "${git}checkout[[:space:]].*--([[:space:]]|\$)"; then
	refuse "working-with-git refuses \`git checkout -- <path>\`: it discards uncommitted work, and the tree is assumed to hold some at all times. Leave the edits, or commit them on a scratch branch."
fi
if matches "${git}branch[[:space:]]+-D"; then
	refuse "working-with-git refuses \`git branch -D\`: it deletes unmerged work. Use \`-d\`, which refuses when the branch is unmerged, or ask."
fi
if matches "${git}worktree[[:space:]]+remove"; then
	refuse "working-with-git refuses \`git worktree remove\`: WIP in a worktree is never yours to discard."
fi
if matches "${git}commit[[:space:]].*--amend"; then
	refuse "working-with-git refuses \`git commit --amend\`: it rewrites a commit. Make a second commit instead."
fi
if matches "${git}(rebase|filter-branch)([[:space:]]|\$)"; then
	refuse "working-with-git refuses history rewrites. Use \`git revert\` to undo a commit, or \`git merge --ff-only\` to catch up."
fi
if matches "${git}push[[:space:]].*(--force|--force-with-lease|-f([[:space:]]|\$))"; then
	refuse "working-with-git refuses a force push, and a push at all is the user's call to name. Say what you would push and wait."
fi
if matches "${git}gc[[:space:]].*--prune" || matches "${git}reflog[[:space:]]+expire"; then
	refuse "working-with-git refuses this: it collects the objects an accident would have been recovered from."
fi

# The one no list named: a read command whose output lands on a tracked path,
# which overwrites it exactly as `checkout -- <path>` would.
if matches "${git}(show|cat-file|archive)[^;&|]*>"; then
	refuse "working-with-git refuses putting history's copy of a file back into the tree. A safe subcommand is not a safe command — the redirect overwrites whatever was there. To compare a change against the code without it, build, copy the *artifact* aside, then edit forward."
fi

exit 0
