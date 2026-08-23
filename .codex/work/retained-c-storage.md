# Retained C storage: bounding the deliberate leak

## State

`kira-runtime-abi/src/c_storage.rs` `retain_text`/`retain_bytes` hand C
process-lifetime storage and never reclaim it. The VM surfaces both as ordinary
instructions (`CStringNew`, `CLayoutAddress`, `ArrayElements`), so a program
converting one string per frame grows without bound for the process life.
`HeapStats` does not see the growth.

## Why not patched in place

The safe fix needs an owner for every retained block that outlives any single
call:

- VM side: `Heap` would hold `Vec<RetainedBlock>` released at `Instance`
  teardown. Safe because no foreign call is in flight at teardown.
- Native-bridge side (`kira-native-bridge/src/array.rs`, `runtime.rs`) builds
  retained blocks too, but a loaded native library has no teardown moment that
  provably postdates every C reader of those pointers.

A process-global registry freed "at exit" risks freeing under C callbacks that
fire late. Both engines must agree on one owner before anything is freed.

## Design to implement

1. `c_storage` returns typed owners (`RetainedCString`, `RetainedBytes`) whose
   addresses borrow from the owner.
2. `Heap` owns them; `Drop for Heap` releases. Never during execution.
3. Native bridge gets an explicit embedder-driven `release_retained` called only
   after its host proves no callee can still hold a pointer (hybrid session
   close).
4. Wire `HeapStats` or a sibling counter so leaks are observable.

Found by the 2026-08 bug hunt (VM M2). Do not "simplify" by freeing earlier.
