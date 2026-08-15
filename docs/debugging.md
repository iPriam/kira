# Debugging

`kira debug` stops a running Kira program and shows what it is doing. It works
on all three backends, and what a stop looks like depends on what the backend
actually runs.

```sh
kira debug app.kira --break priceCart          # the VM's own debugger
kira debug app.kira --backend llvm --lldb      # LLDB over native code
kira debug app.kira --lldb-dap --dap-continues 4
kira debug app.kira --prepare                  # build it, describe it, run nothing
```

## What a stop is on each backend

| Backend | What runs | What a breakpoint is |
| --- | --- | --- |
| `vm` | interpreted bytecode | a Kira function and instruction index |
| `hybrid` | bytecode plus native functions | either, in one session |
| `llvm` | machine code with DWARF | a native symbol or a source line |

Native code has machine instructions and a line table, so LLDB debugs it the
way it debugs anything else. Bytecode has neither. The VM instead calls one
native probe, `kira_vm_debug_probe`, before every interpreted instruction, and
publishes the decoded stop beside it:

| Symbol | What it holds |
| --- | --- |
| `KIRA_VM_DEBUG_STATE` | the stop as a C struct |
| `KIRA_VM_DEBUG_TEXT` | the same stop as readable text |
| `KIRA_VM_DEBUG_ACTIVE` | whether a debugger still wants stops |

A debugger breaks on the probe and reads the text, which needs no call into the
debugged process — the LLDB some toolchains ship is unreliable when a target
function is evaluated repeatedly at a stop.

`KIRA_VM_DEBUG_ACTIVE` is the way back. Once the VM has stopped once it
publishes state before every instruction, because the debugger may resume into
any of them. A debugger that has finished stepping writes `0` there and the
interpreter runs at full speed again; writing `1` brings the stops back. A
recursive program takes minutes to end with the stops on and no time at all
with them off.

## `--prepare`

`kira debug --prepare` builds the program, keeps the artifacts, and prints one
JSON object describing what it built — then runs nothing. It is the contract an
editor or an agent builds through when it owns the debugger session itself.

```json
{
  "backend": "vm",
  "executable": "…/kira.exe",
  "arguments": ["__vm-debug-host", "--module", "…kbc", "--"],
  "functions": [{ "id": 4, "name": "discountAmount", "line": 75, "execution": "bytecode" }],
  "probe": { "symbol": "kira_vm_debug_probe", "function_register": "$rcx", "pc_register": "$rdx" },
  "artifacts": ["…kbc"]
}
```

`executable` and `arguments` are what a debugger launches; `functions` is what a
breakpoint resolves against; `probe` names the symbols above and the registers
the probe's function identifier and instruction index arrive in, which is what
makes a Kira breakpoint expressible as an LLDB condition. `artifacts` belongs to
whoever holds the session: nothing else will delete those files.

## The debug-session MCP server

`kira-lldb-mcp` holds debug sessions open so an agent can ask more than one
question about the same stopped program. It is configured in this repository's
`.mcp.json` as `kira-lldb`, and builds through `kira debug --prepare`.

It offers 25 tools, in five groups:

| Group | Tools |
| --- | --- |
| sessions | `launch`, `sessions`, `status`, `close` |
| breakpoints | `break_set`, `break_list`, `break_delete`, `watch` |
| execution | `continue`, `step`, `pause`, `finish` |
| inspection | `backtrace`, `variables`, `evaluate`, `registers`, `read_memory`, `write_memory`, `threads` |
| Kira | `state`, `functions`, `source` |
| machine code | `disassemble`, `modules`, `command` |

Each is spelled `kira_lldb_…`. Three of them answer things a native debugger
cannot: `state` decodes the stopped VM — the Kira function, the instruction, the
frame's locals, the operand stack, and the Kira call stack — while `functions`
and `source` map that back to what the program's author wrote.

Stepping means different things on the two engines, and the server does both.
A native frame is stepped by the debugger. A bytecode frame is resumed to the
next probe stop, and `over` and `out` keep resuming until the Kira call depth
comes back down, which is why a step reports how many VM stops it passed
through and stops at a budget rather than running forever.

Two environment variables select what it drives: `KIRA_EXECUTABLE` is the
compiler it builds with, and `KIRA_LLDB_DAP` is the debug adapter it runs.
