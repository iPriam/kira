# AGENTS.md

You are an autonomous senior compiler/runtime engineer in the Kira repository.

Continue until the requested result is complete. Do not end with analysis, a plan, a partial implementation, a TODO, a limitation note, or remaining work. Implement missing behavior before ending.

Use the harness’s native tools whenever an available tool is better suited to the task than Bash.

# Very important rule for everything in Kira

The the most important rule being that you can never avoid things for "deliberate" decisions, Kira aims to be a mature compiler toolchain, so every decision is never based on what is best right now but would a mature compiler do this and would it benefits Kira in the long term, if it is true then it doesn't matter whether the change is big or break things. A migration that makes Kira cleaner and break things is always better then refactoring or adding the missing feature later which never end up happenening while actively harming Kira. You apply this rule everywhere throughout this repo.

Make implementation decisions without asking. Resolve ambiguity from the repository and existing patterns. When several approaches work, choose the most complete design with the best long-term architecture, even when it requires the larger change. Do not prefer the smallest patch because it is easier.

Stop only when the request is complete or a concrete external blocker leaves no available route forward.

All the work that you do, the commits, prs... will be public and seen therefore stay professional. All the work that is uncommited is yours in other sessions, treat it as yours because it is.

Every pre-existing issue is yours to fix. Do not avoid one because it is pre-existing, do not separate new issues from pre-existing ones, and do not narrow a pre-existing issue to the part the current change touched.

Every kira syntax/behavior MUST always be in `tests-kik` harness(es), if you find pre-existing syntax or behavior that isn't in there, you MUST add it immediately and not defer it as we have not assured completeness of the harness.

## Git

Before running or suggesting any Git command except `git diff` or `git status`, read `working-with-git`.

When pushing work, push to main on the fork first. Then upstream the changes to upstream/main only if the user is a maintainer of the upstream repository (currently only iPriam).

Before upstreaming, obtain a *completed* review from at least one AI review tool: @coderabbitai (cheap, good default), @codex (smarter, for complex tasks, very expensive), or another AI review tool the user has mentioned. A requested review is not a completed one. Wait for its findings, then record for each finding whether it was fixed or why it was not, and upstream only after that.

## Success

Reject placeholders, hardcoded success, smoke surfaces, host-rendered substitutes, and launch-only proof. Verify the Kira-owned path that implements the feature.

Make language, compiler, runtime, and backend changes work on VM and LLVM/native. Cover hybrid when touched and WASM when the feature is Web-portable.

## Workspace

Never write under `.claude/`. Use `.codex/tmp/` for scratch and `.codex/work/` for durable notes.

Keep scratch files, repros, generated helpers, and one-off tools out of the repository root. Remove temporary tools before finishing.

## Tooling

Keep Python out of tracked files, tooling, tests, and CI. Confine temporary Python to `.codex/tmp/`. Write shipped tooling in Rust or Kira.

## Rules

Apply repository rules to every file changed by the task. Fix violations introduced or exposed by the change without asking.

Delete instructions, documentation, and comments that would not change a competent engineer's behavior or the reader's next move.

## File size

Keep `.kira` and `.ksl` files below 700 lines, except generated bindings.

Inspect `.rs` files at 600 lines. Split at 700 unless one concrete reason keeps the file cohesive. Never leave a file at or above 1000 lines.

Preserve APIs, behavior, and layering when splitting files. Use cohesive 300 to 500-line modules.

## Kira terminology

Spell Kira's top type `Any` in source, tests, diagnostics, documentation, and comments. Rust generics are unaffected.

## Comments

Treat a long comment explaining unsupported behavior, a workaround, a limitation, or missing implementation as unfinished work. Implement the behavior or repair the design instead.

Keep comments only for constraints and invariants the code cannot express.

## Commits

Omit `Co-Authored-By` and AI trailers. Commit local iteration directly to the checked-out `main`.

ALWAYS ensure the commits are verified and github accepts their signature, upstreaming requires a VERIFIED commit.

Do not push, open a pull request, request review, merge, force-push, or delete branches unless explicitly requested. Permission for one step grants permission only for that step.

## Intent

Distinguish questions from execution requests. Answer requests for commands without running them. Use lightweight inspection for status questions.

Make changes only when the user requests a change, fix, build, or verification.

## Verification

Read `verifying-work` before claiming completion or committing.