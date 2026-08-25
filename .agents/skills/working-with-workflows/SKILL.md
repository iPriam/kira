---
name: working-with-workflows
description: "How to staff an ultracode workflow with Sol as default implementer and one final Fable review only for broad cross-area work. Read before calling Workflow or spawning more than one Agent."
---

# Working with workflows

## Default to Sol without a verifier

Staff Sol for preparation, implementation, and focused testing. Do not add a
verifier for small or medium work: tests and direct behavioral checks belong
inside the Sol implementation call.

Skip preparation when the work-list and interfaces are known; when
decomposition itself is the risk, use one Sol preparation call. Never fan out
multiple planners.

Use `pipeline()` when work items can move independently. Give every concurrent
item a disjoint substrate: no file, resource, or record may belong to two
implementers. Tell each agent to work only inside its substrate and run no git
commands. If a failure originates in another substrate, leave it to that
substrate's owner.

Use a barrier only when a later stage needs the integrated result from every
prior item. Dependency waves and final integrated review are valid barriers;
conceptual stage boundaries are not.

## Use one final Fable only for broad work

Treat work as broad when it crosses many crates or product areas, changes a
shared contract, or depends on integration behavior no single substrate can
verify. After every Sol implementation finishes, run exactly one read-only
Fable agent over the integrated change. Never run one Fable per implementer.

Require Fable to return only verified findings with file and line, evidence,
impact, owning substrate, and a specific fix. Fable reviews; Fable does not
edit.

Group surviving findings by disjoint substrate and send each to Sol for repair.
Run no second verifier unless the user asks. Omit Fable entirely for local or
single-area work.

## Pick models and effort

Use Sol by default. Use Fable only for the single final review described above.
Use Opus only when Sol is unavailable or the user requests it. Never use
`effort: 'max'`; use `high` only when the task needs it.

Confirm Fable is available before making a workflow depend on it. When it is
unavailable, use one final Opus review for broad work rather than multiplying
reviewers.

## Point agents at repository rules

Tell every subagent that `AGENTS.md` governs it and to load the skills its task
touches. Never paste those rules into prompts; copied rules drift and can
outrank current repository instructions.

Give each agent only its task, substrate, constraints, and acceptance criteria.
Do not send the full work-list or unrelated background it can discover itself.
