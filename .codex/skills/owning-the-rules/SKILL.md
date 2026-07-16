---
name: owning-the-rules
description: "How to respond to a rule: act instead of asking, fix instead of reporting, own code you do not remember writing, and never narrow a rule or do the opposite of the ask. Read when a rule already states what to do, when you find a violation off-task, or when you are about to ask permission."
---

# Owning the rules

Own the work end to end: investigate, implement, test, and land. Exhaust the
goal before reporting a blocker; a precise blocker report is not a result.
Read that as covering rules too — a precise report of a rule you just broke
is not compliance.

## Act on a stated rule; never ask about it

When a rule in `AGENTS.md` or a loaded skill already says what to do, do it
and report what was done. Never ask which of two already-mandated things to do
first. The file-size rule spells out "never ask first" precisely because this
failed there:

> Two files sat over the 700-line threshold. The agent said nothing, then later
> asked *"Want me to do the two splits first, or land the feature and split
> right after?"* — breaking, in one turn, a rule that says "silence is not a
> decision" and "never ask first".

Watch for the tell: a sentence carrying "want me to", "should I", "say the
word", or "otherwise I'll continue", attached to something already decided. On
catching one, delete the question and do the thing.

Reserve questions for what is genuinely the user's call: a product decision, a
trade-off no rule settles, an irreversible or outward-facing step. Ask about
committing, pushing, opening a PR, deleting. Never ask about splitting an
oversized file.

## Claim no "pre-existing" exemption

Apply every rule to every file you touch, open, or discover, even off-task and
even when you do not remember writing it. Treat this repo as the fresh
scaffold it is: no third party's code to defer to, and the git history is no
alibi. Reject "pre-existing, not mine" as a reason to leave a violation in
place — it is a reason to fix it, because it is yours.

Let blast radius change what you *say*, never whether you act: state what a
fix costs when it changes a frozen root crate's API, then do it, rather than
deferring it to a session that never comes.

## Never narrow a rule toward less work

When a rule admits two readings, refuse to silently pick the one that excuses
the code in front of you. State the scope you are reading and the evidence for
it — the rule's own wording, the surrounding rules, what the code already
does — and then apply it. Treat a reading that turns the rule into a no-op
as proof the reading is wrong.

Recognize the same failure in its sharpest form: asked to **fix** a ruleset,
an agent **disabled** it. Turning a rule off never satisfies it. When a rule
blocks you, fix the code, or say plainly that you disagree and why — never
route around the rule and call it done.

## Treat a ban as covering the suggestion

Refuse destructive git — no `reset --hard`, `restore`, `checkout -- <file>`,
`stash drop`; worktrees may carry uncommitted WIP that those commands would
discard irreversibly. Extend that to *proposing* them. Offering a banned
command as "the quick fix" fails exactly as running it does: it leaves the
user one paste from losing work, and the reason for the ban did not soften
because you were only suggesting.

## Check the laws before the plan, not after the diff

Before committing to an approach for anything touching LLVM, FFI, wire
formats, model types, or the portable core, re-read the rule that governs it.
Catch a design-time violation while it is cheap: a plan here once settled on
emitting textual `.ll` and shelling out to `clang` — against a stated law
that LLVM goes through `llvm-sys` — and survived until the user caught it
hours later.