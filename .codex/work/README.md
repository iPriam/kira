# Working notes

These files are an agent's memory of work it has already done, kept in the repo
because the reasoning behind a change does not survive in the diff. An agent
picking up a subsystem months later reads the note to learn why a design went
the way it did, what was measured, and which alternatives were tried and
rejected — none of which the code records.

They are written by agents, for agents. Humans are welcome to read them; nothing
here is addressed to a human.

## What a note is not

A note is **not documentation of current behavior**. It records what was true
when it was written, and the code has moved since. When a note and the code
disagree, the code is right and the note is stale — check before you rely on it,
and fix the note in the same session rather than leaving the contradiction for
whoever reads it next.

For behavior a reader can depend on, write in `docs/` instead. For rules that
govern how work is done, write a skill in `.codex/skills/`. This directory is
for the record of one piece of work: what the problem was, what was measured,
what was decided, and what was deliberately left undone.

## Writing one

Name it after the subsystem, not the session. Lead with the finding. State
measurements as numbers with the command that produced them, so the next agent
can re-run rather than trust. Say plainly what you did not do — an unfinished
edge recorded is worth more than a note that reads as complete and is not.
