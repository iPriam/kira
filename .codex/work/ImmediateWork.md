# Immediate Work

This file is the controller for the sequential Kira milestone queue.

Do not put full task bodies here. Each real task lives in its own Markdown file under:

    .codex/work/tasks/

The agent must read this file first, then execute every task file in sorted order.



## Execution Model

Work sequentially and autonomously.

1. Read this file.
2. Find the first incomplete task in .codex/work/tasks/.
3. Read that task file completely.
4. Execute only that task.
5. Fix failures instead of accepting them.
6. Write a report under .codex/work/reports/.
7. Write a checkpoint under .codex/work/checkpoints/.
8. Mark the task complete inside its own Markdown file only when its completion criteria are truly satisfied.
9. Move to the next sorted incomplete task.

Do not parallelize milestone tasks in the same branch.

Do not stop because a task becomes difficult.

Do not downgrade a failed requirement into an accepted limitation.

Every error is a failure until fixed, or until it is proven to be an external blocker outside the repo and outside this machine's control.

External blockers are not success states. They must be marked as incomplete/blocking evidence, then the agent must continue to the next independent task if useful work remains.