---
name: working-with-agents-instructions
description: "Style rules for writing or editing AGENTS.md, CLAUDE.md, or any .codex/skills/*/SKILL.md in this repo: no giant markdown files, no no-ops, no named persona, identity once then imperative voice, and how a skill gets found without a routing table. STOP and load this skill first, with no exception, before typing a single character into AGENTS.md, CLAUDE.md, or any .codex/skills/*/SKILL.md — editing one of these files without it first is itself the violation."
---

# Working with agent instructions

## Keep instruction files small

Never grow a single instruction file past what a task actually needs loaded
every time. Confine `AGENTS.md`/`CLAUDE.md` to the few rules every task needs,
and move anything situational — a layering map, a done-bar, a protocol —
into its own skill under `.codex/skills/`, loaded only when the task touches
it. Treat a 200-line always-on file as a bug rather than thoroughness: the
rules that matter drown in the rules that don't, and a reader gets through the
whole thing and still misses the one that applied.

## Cut every no-op

Keep a skill to the smallest subset that changes behavior, and delete the
rest. Test each line against one question: would a competent engineer who
never read this line have done something different? Where the answer is no,
the line is a no-op — cut it, however true it is.

Four kinds of no-op recur. Delete general good practice that holds in any repo
("write tests", "prefer clarity"). Delete anything a tool already states in
its own description or error output — restating it wins nothing and rots the
moment the tool changes. Delete background that explains rather than
instructs; a skill is a set of commands, not a briefing. Delete the rule for a
situation this workspace cannot reach.

Judge a draft by what survives deletion, never by what it covers. A skill that
earns twenty lines is finished at twenty lines.

## Let the description find the skill

Expect no routing table. Open every `SKILL.md` with frontmatter whose
`description` names both what the skill covers and when to read it ("Read
before …", "Read when …"). Scan `.codex/skills/*/SKILL.md` descriptions
when a task starts and load the ones whose trigger matches. Recognize that a
description saying only what the skill *is*, without saying when it applies,
describes a skill that never loads — write the trigger into the description
or delete the skill as dead weight.

## State each rule once

Say a rule once. Never restate the same rule across three sections in
different words — that produces surface area for self-contradiction rather
than reinforcement. Reserve one exception: a deliberate top-and-bottom bookend
for the smallest set of irreversible rules (destructive git, fake success),
copied verbatim rather than paraphrased — a reworded "recap" reads as a new
rule to reconcile, not a repeat. Delete an instruction for a path that can't be
reached; keep nothing "for reference."

Reserve a second exception: a skill may restate a core `AGENTS.md` rule it
depends on, because a skill loads independently and cannot assume the core is
still in context. Apply the bookend's discipline — copy the restatement
verbatim (same clause, same wording) from wherever it is canonical rather than
re-deriving it, and keep both copies in sync when either changes.

## Put the scope inside the rule

Write a rule so its scope is unmissable at the point of the rule, never
inferable from a heading. Say in the sentence when a rule applies to one kind
of file; say *everything* when it applies to everything. Never lean on a
section title to carry the scope — a reader arriving at the bullet does not
re-read the heading, and a scope that can be read two ways gets read the wrong
way.

## Name no persona; assign the identity once, then command

Open `AGENTS.md` by telling the reader what it is — "You are an autonomous
senior compiler/runtime engineer…" — and treat that as the only
second-person sentence in the system. Keep it: a file that opens by describing
some named third party never tells the reader it is them, so every rule that
follows reads as being about somebody else.

Invent no name for the reader. A persona ("Kai does X", "Harmony doesn't Y")
buys nothing a role description doesn't, and it costs a layer of indirection
on every single rule.

After that opening sentence, write every rule as a command, and reach for a
precise verb rather than a bare "do"/"don't": **Analyze. Create. Focus.
Respect. Refuse. Reject. Prove. Verify. Preserve. Confine. Escalate. Consult.
Split. Own. Exhaust. Route. Delete.** Prefer "Refuse destructive git" over
"you must not run destructive git"; prefer "Respect the ladder" over "don't
ignore file size". Let the verb carry the rule.