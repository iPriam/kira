# FFI / native-bridge catch-all harness

A monolithic Kira suite of harness-owned `Test` declarations that stress the
`@Native` and `@Runtime` bridge in depth. It covers struct, scalar, enum, and
array returns and arguments, native-to-VM closures, native-to-VM-to-native
re-entry, borrow and move across the bridge, and allocation churn. 267 tests
across `app/<purpose>/`:

- `structs/`: native struct return-by-value, struct-borrow into `@Runtime`,
  scalar round-trips, native-to-VM callback (prefix `fsb`/`Fsb`).
- `enums/`: enums returned to native, enum fields in native structs,
  payload/payload-less variants, state machines (`fen`/`Fen`).
- `collections/`: `[Int]` / arrays-of-structs across the bridge, `borrow mut`
  mutation + sync, nested array fields, churn (`far`/`Far`).
- `closures/`: native-invoked VM closures, sandwich re-entry, loop churn
  (`fcb`/`Fcb`).
- `scalars/`: Int/Bool/Float scalars, multi-hop N-to-R-to-N-to-R round-trips, strings
  across the bridge (`fmx`/`Fmx`).

## Commands

This suite runs through the pure-Kira test driver on VM, LLVM, and hybrid. The
driver executes each `Test` at build time, so the `@Native` calls cross the
selected backend's bridge and the verdict is backend-independent:

| Backend | Command |
| --- | --- |
| VM | `kira test --backend vm tests-kik/ffi-harness` |
| LLVM | `kira test --backend llvm tests-kik/ffi-harness` |
| Hybrid | `kira test --backend hybrid tests-kik/ffi-harness` |

The driver reports 267 declared tests, with the same 267 passed and zero failed
or skipped on all three backends.

Every test reduces a bridge exercise to a scalar and asserts it with
`Result.Ok(...)`; the comparison runs in Kira (no Zig override). See `FINDINGS.md`
for bridge bugs this harness surfaced.

## Authoring Notes

- Imports are FILE-scoped: every file that uses `Test`/`Result`/`TestFailure`
  (or any other Foundation symbol) needs its own `import Foundation`; sibling
  files importing the same module do not conflict.
- Every top-level name uses a per-domain prefix so the flat namespace stays clean.
- `test` typically returns an Int/Bool/String; struct results are compared
  structurally by the driver, so a struct result is fine too. End with a clean
  trailing `return`.
- Trap-style expectations are supported by the driver. `FmxTrapExpectBridgesNative`
  covers a native bridge trap path.
