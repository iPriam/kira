# Standing test failures on this macOS host

Nine failures reproduce identically at `f420eb8` (pre-FFI-lifetime work) and
after it, so none is caused by the owned-C-block migration. All are
host-environment shaped and need their own session:

1. `backend_parity compiler::*` (6 tests): the in-language checker
   (`checkPackage` through Foundation's `KiraPackage`) exits 0 on VM and 1 on
   LLVM native with **empty stderr** — the native binary fails silently.
   Likely sibling of the Apple `-isysroot` work in `f420eb8`.
2. `end_to_end debug::vm_lldb_*` and `debug::hybrid_lldb_*`: the LLDB-driving
   pair times out (~366 s each). LLDB attach/entitlements on this machine.
3. `kik_harness the_harness_suite_passes_identically_on_vm_and_native`: three
   pure-Kira cases fail on LLVM only — `InflateFixedTables`,
   `ImagePngPalette4Extent`, `ImagePngPalette4Pixels`. VM passes all 1271.
   Smells like host libm / fast-math divergence in the native build; the
   related `PrimitiveExpAndLogInvertEachOther` pin was libm-dependent and is
   now a one-ulp band (fixed in the FFI-lifetime commit).

A `git worktree` at `/tmp/kira-head-check` (detached at `f420eb8`) was used to
prove pre-existence; remove it when done with `git worktree remove`.
