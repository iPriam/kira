---
name: working-with-workflows
description: "How to staff an ultracode workflow: which model implements, which verifies, what each model is good at, and how to point a subagent at the rules instead of recopying them. Read before calling the Workflow tool and before spawning more than one Agent."
---

# Working with workflows

## Stage a workflow: prepare once, then implement/verify per unit

A workflow has two stages, not three: **prepare**, then a single
**implement → verify** stage that repeats per work item. Do not add a
standalone, workflow-wide verify pass after all implementation finishes:
that serializes the whole fan-out behind one late gate and does not scale.
Pair each implementation with its own verify as soon as that unit is done,
via `pipeline()`, so slow items don't block fast ones and mistakes surface
per-item instead of in one late batch.

Optimize the orchestrator for speed. Default to `pipeline()` over `parallel()`
so no item waits on the slowest sibling; reserve a barrier for the rare case
where a later stage genuinely needs every prior result at once. Fan out as
wide as the work-list allows rather than batching it down.

Assign each item in a `pipeline()` its own substrate, whatever scope of
state (files, resources, records) no other concurrent item touches, so
parallel implement calls never overlap. Tell every agent staffed inside a
`pipeline()` that it runs alongside sibling agents on other items, must work
only within its assigned substrate, and must not run any git command:
concurrent commits, adds, or stages from parallel agents corrupt shared repo
state. Confine each agent to fixing only what it owns: if verify or a build
surfaces an error in another item's substrate, leave that fix to the owning
item's own implement/verify pair, waiting on it if blocked, or otherwise
keep writing within its own scoped substrate. An agent editing outside its
substrate reintroduces the overlap the substrate split exists to prevent.

**Prepare**, optional, and worth it only where the shape of the work is still
open. Staff the most taste and intelligence on offer and ignore efficiency
outright: one expensive call that settles a plan, an interface, or a
decomposition pays for itself across every downstream implementation stage.
Skip the stage entirely once the shape is already decided. Do not reach for
prepare by default; most work-lists arrive already decomposed and go
straight to implement/verify.

**Implement → verify (per item)**, for each unit of work, staff
intelligence and efficiency on the implement call, not taste. Follow it
immediately with exactly one verify call on that same item, staffed with high
taste and high intelligence, from a different model family than the
implementer when a different family is available. A model reviewing its own
output rationalizes rather than refutes; a same-family sibling carries the
same blind spots that produced the bug. Never spend a second or third
verifier on one unit; one crossed-family pass is the point of this
structure, not a floor.

## Pick the model per stage

Set `opts.model` on the `agent()` call. Omit it and the stage inherits the
session model, prefer that when nothing about the stage argues otherwise.
Sonnet (Claude) and Terra (GPT) are omitted because their performance does
not justify their cost. Sonnet, in particular, is inefficient, while more
capable models, despite having higher nominal prices, complete tasks more
effectively and at a lower overall cost per task.

Taste is subjective. It defines the model's ability to design efficient,
correct code, well designed APIs, and high-quality documentation.

| Model | Taste | Intelligence | Efficiency |
| --- | --- | --- | --- |
| Sol (5.6) | 6/10 | 8/10 | 10/10 |
| Fable (5) | 9/10 | 9/10 | 4/10 |
| Opus (4.8) | 7/10 | 6/10 | 6/10 |

Read the ranking off the mechanics behind it. Fable owns the taste column
outright and edges Sol on raw intelligence, which is what makes its verdict
worth more than a second pass from the model that wrote the code. Read its
efficiency score as a price, not a habit: Fable works cleanly, and a rate per
token steep enough to demolish that lands it near $16 (DeepSWE) a task to reach a score
Sol beats at a quarter of the spend. Sol is the opposite trade: the highest
score in the table at the lowest cost per task. Reach for Opus on
availability, never on merit: no column here argues for it over Sol, and the
reason it gets staffed at all is a harness that does not offer the GPT models.
Codex runs Sol. Where Claude Code may offer Sol and Fable, staff those and
leave Opus alone unless GPT is unavailable. Sonnet fails the other way round:
a low rate spent on far more tokens than the work needs, which lands it at the
highest cost per finished task of anything listed.

Never read a matching benchmark score as matching capability. Terra scores
level with Fable on the coding-agent index at a fraction of the price and is
still a far smaller model with a far narrower reach; staff it on volume,
never on the stage where the answer has to be right the first time.

Confirm Fable is available to the user before a script depends on it; treat
the fallback as the normal path, not a failure. In most cases GPT is not
available in Claude Code, so treat Claude as the default. In Codex,
treat GPT as the default.

## Point subagents at the rules; never recopy them

Tell each subagent that `AGENTS.md` governs it and that it must load the
skills its task touches. Never paste the rules themselves into a prompt; a
copy drifts the moment `AGENTS.md` changes, and a stale copy outranks the real
file in the reader's context.

Scope each pipeline agent's prompt to only what that item needs: its own
task, its own substrate, its constraints. Never hand it the full work-list,
other items' prompts, or background it can derive itself: bloat that
context and the agent spends its budget reading instead of working. Trust it
to discover the rest of what it needs from the repo on its own.
