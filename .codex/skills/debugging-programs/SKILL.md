---
name: debugging-programs
description: "Read before debugging a Kira program, inspecting a stop, or reaching for print statements to find a runtime fault. Drive the `kira-lldb` MCP server's sessions rather than re-running the program, and read Kira state rather than the interpreter's."
---

# Debugging a Kira program

The `kira-lldb` MCP server holds a stopped program open across calls. Use it instead of adding prints and re-running: a stop answers as many questions as you ask it.

Every tool is `kira_lldb_…` and takes an optional `session`, required only once a second session is open.

## Starting

`launch` with `source` (a `.kira` file or package directory) and `breakpoints`, a list of `function` or `function:instruction` spellings. It builds through `kira debug --prepare` and reports the first stop plus the program's function table.

Without `breakpoints` the program runs to completion and the report carries its output and exit code. Pass `backend` as `vm` (default), `llvm`, or `hybrid`; pass `executable` instead of `source` for a binary built elsewhere.

`close` when finished. It kills the target and deletes the artifacts the build kept. A session left open holds a debugger process and those files.

## Reading a stop

On `vm` and `hybrid`, `state` is the answer: the Kira function and instruction, the frame's locals, the operand stack, and the Kira call stack. `backtrace`, `variables`, and `registers` describe the interpreter running the program, not the program — reach for them only when the fault is in the runtime itself.

On `llvm` that inverts: there is no `state`, and `backtrace`, `variables`, and `disassemble` describe the program directly.

`source` and `functions` map either back to what the author wrote.

## Moving

`step` takes `mode` — `into`, `over`, `out`, `instruction` — and `count`. On bytecode a step resumes once per interpreted instruction, so `over` and `out` report `vm_stops` and stop at `budget` rather than running forever. A step that reports `budget_exhausted` did not arrive; raise `budget` or use `continue` with `until`.

`continue` with `until` runs to a Kira location at full speed. Prefer it to a long `step` when you know where you are going.

`finish` runs to the end and reports output and exit status. It removes every breakpoint and tells the VM to stop publishing state, so use it rather than resuming in a loop.

## Limits

Set memory through `write_memory`, never through a raw `memory write` you compose yourself, and never through DAP's own request: the LLDB the Swift toolchains ship exits on `writeMemory` and takes the session with it.

Evaluating a target function at a stop is unreliable on the same LLDB. `evaluate` on data is fine; do not call program functions from it.

A Kira breakpoint is a condition on the VM probe, so it costs nothing until reached. A session with no breakpoints installs no probe at all, which is why an unbroken run is as fast as an ordinary one.

`KIRA_EXECUTABLE` selects the compiler the server builds with, and `KIRA_LLDB_DAP` the adapter it drives. `sites/docs/content/docs/appendix/debugging.mdx` covers the mechanism, and `kira debug --prepare` is the contract underneath it.
