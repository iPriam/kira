# Sibling matrix: resume notes (2026-08-31)

Continues the Codex session "test every sibling on vm/llvm/hybrid, fix every issue".
Earlier steps (graphics harness packaging, KiraUI catalog identity, raw_sokol
`@MainThreadLifecycle` migration) were already done there.

## Fixed this session

- `kira-native-bridge` unix fiber arena: `StackPool::drop` munmapped the arena
  while `exit()` ran TLS destructors on a lifecycle fiber's own stack →
  silent SIGSEGV on every LLVM app that quits from a lifecycle
  (raw_sokol_interop, KiraUI basic app). Drop now leaves the mapping when the
  current stack pointer sits inside it.
- Module-constant dependency analysis rewritten. Old: pre-analysis name-level
  mention closure (method names bridged unrelated types → false KSEM317 on
  kira-ui `app/Catalog.kira`). New: demand-driven initializer analysis during
  collection (`crates/kira-semantics/src/constants.rs`) + post-analysis
  evaluation ordering over the resolved HIR call graph with row permutation
  (`crates/kira-semantics/src/constant_order.rs`). Docs updated in
  `sites/docs/content/docs/language-reference/declarations.mdx`; harness cases
  added in `tests-kik/harness/app/TlxConstants.kira`.
- VM: `ensure_constants` ran constant initializers inside a lifecycle fiber's
  slice budget; a heavy initializer exhausted it inside `enter_values`, which
  cannot suspend → "a run with no instruction budget suspended" trap on
  KiraUI VM. Budget is now taken off while constants fill.

## Environment

- lldb-dap (Xcode 27 beta, lldb-2103.0.25.1) hangs after `launch` because
  **Developer mode is disabled** (`DevToolsSecurity -status`). Every LLDB
  debug path (kira-lldb MCP, `kira debug --backend llvm`, plain `lldb -p`)
  hangs machine-wide. Fix needs `sudo DevToolsSecurity -enable` (blocked for
  agents; user must run it). VM debugging (`kira debug --batch`, no LLDB) works.
- Sibling tests/examples assume cwd = owning package root (cwd-relative
  `Resources/Default.kcui`). Matrix runner: `.codex/tmp/sibling_matrix.sh`,
  log `/tmp/sibling_matrix.log`.

## Remaining

- Sibling matrix run to completion + fix failures.
- Workspace gate (`kira_dev_validate` full) + `backend_parity` suite
  (semantics changed) + `kira_dev_build` workspace (native runtime changed).

## Completed (session end)

- Sibling matrix: 381 cases (5 test suites + every example, vm/llvm/hybrid), 0 failures.
- Workspace gate: fmt, clippy -D warnings, full workspace tests green; kik parity
  count updated 1372 -> 1374 for the two new Tlx constants cases.
- Debugger: unauthorized-host detection (`kira_debug::debugging_unauthorized`),
  CLI warning, MCP timeout hint, test skips, docs.
- Committed to main in five verified commits (ec8cd75..3f35232).
