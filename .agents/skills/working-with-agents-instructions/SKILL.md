---
name: working-with-agents-instructions
description: "Read before editing AGENTS.md, AGENTS.md, or any .codex/skills/*/SKILL.md. Follow caveman:caveman and working-with-markdown first. Delete no-ops, state rules once, scope rules inline, use imperative voice, keep skills within 80 lines."
---

# Agent instructions

Follow `caveman:caveman` in `full` mode and `working-with-markdown` before editing covered files.

## No-ops

Delete lines that would not change competent engineer behavior:

- generic practice
- behavior or errors already stated by tools
- explanation without instruction
- unreachable rules
- rule justification

State rule directly. Add one reason clause only to block likely shortcut.

## Placement

Keep `AGENTS.md` and `AGENTS.md` for rules needed by nearly every task. Move situational rules into skills.

## Descriptions

Put scope and trigger in every skill `description`. Use `Read before...` or `Read when...`.

## Repetition

State each rule once.

Repeat only:

- irreversible rules as verbatim bookends
- required core `AGENTS.md` rules inside dependent skills, verbatim

Keep copies synchronized.

## Scope

Put scope inside each rule. Write `everything` for universal scope. Never rely on headings for scope.

## Voice

Open `AGENTS.md` with one purpose sentence. Use no other second-person sentence.

Write rules as commands with precise verbs. Use no personas.

## Size

Keep each `.codex/skills/*/SKILL.md` at or below 80 lines.

Run `wc -l .codex/skills/*/SKILL.md` before finishing.