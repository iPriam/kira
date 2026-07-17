# The hybrid seam — ownership rules and traps

The hybrid backend is landed; read `.codex/work/llvm-and-hybrid.md` first for
how the pieces fit together. This note carries the part that is expensive to
re-derive: who frees what at the boundary, and the traps found by reading the
codegen. Getting one of these wrong is a double free or a leak, not a compile
error.

These rules are enforced nowhere. They were read out of the codegen, and the
code that depends on them is `kira-hybrid-runtime/src/marshal.rs` — which
documents each one at the point it acts on it. Change the codegen's free
behavior and that file changes in the same commit, or the seam corrupts memory.

## The ownership rules

**A native callee frees its string arguments.** `emit_return` in
`codegen/lower/stmt.rs` frees every `String` local at return, and parameters
occupy locals `0..param_count`. So the host builds each string argument with the
library's `kira_rt_str_new` and does **not** free it — the callee did.

**A trampoline's result is owned by the host.** Read the bytes with
`kira_rt_str_data`/`kira_rt_str_len`, copy them out, then `kira_rt_str_free`.

**Native→VM args are transferred too.** `lower_runtime_call` in
`codegen/lower/call.rs` writes handles into the `BridgeValue` array and never
frees them after the call. So the invoker takes ownership: read the bytes, free
the handle. A `String` it returns must be a fresh handle from the library's
`kira_rt_str_new` — the native caller frees it as an ordinary expression value.

**v0 emits `Ownership::Owned` for every parameter.** The IR carries no per-param
mode (no borrow syntax yet). The codegen always frees, so a `Borrow`/`BorrowMut`
**string** param would double-free; `kira-hybrid-runtime`'s `validate.rs` rejects
one at manifest load rather than accepting a mode nothing implements. Ownership
is immaterial for `Int`/`Float`/`Bool` (Copy, nothing to free), which is why
those pass in any mode.

**The empty string is a null handle.** Every `kira_rt_*` helper accepts null as
`""`, so a zero-initialized slot is already a valid, free-able value. A host that
special-cases null before calling in would be re-deriving that for no reason.

## Traps

**The dylib does not export what the host resolves, by default.** Confirmed, and
fixed in `link_shared_library` — see the Pitfalls section of
`.codex/work/llvm-and-hybrid.md` for what was observed and what the fix is.
`HYBRID_HOST_SYMBOLS` in `kira-runtime-abi` is the single list the link forces in
and the host looks up; adding a symbol the host resolves means adding it there,
not in two places.

**Resolve every symbol out of the loaded library, never `kirac`'s own copy.**
`kira-cli` links `kira-native-bridge` as an rlib, so the host process carries its
own `kira_rt_*` and `kira_hybrid_*` symbols. Allocating a handle in one copy and
freeing it in the other is a cross-allocator free. `libloading` defaults to
`RTLD_LOCAL`, so `dlsym(handle, …)` gets the library's own — keep it that way,
and **do not** make `kira-hybrid-runtime` depend on `kira-native-bridge`.

**The invoker is a bare `extern "C" fn` with no user-data pointer.** It cannot
close over the session, so it finds it in a thread-local set by a guard for the
run's duration and cleared on drop. Native code calling back from a thread the
host never entered is out of scope for v0: the invoker finds no session, says so,
and exits, rather than running against a null pointer.

**Do not alias `&mut dyn HostCapabilities`.** The outer VM holds the host mutably
while native code calls back and needs a host again. The host therefore has no
mutable state — it is a handle over a shared `Session`, constructed fresh per
nesting level. `write_line` goes straight to stdout, so there is nothing to
synchronize.

**Stdout ordering is already fine.** Both halves write through Rust's
`LineWriter`, which flushes on newline, so `print` from the VM and from
`kira_rt_print_*` interleave correctly on fd 1. No extra flushing needed.

**A trap inside a callback must not unwind across the C frame.** `extern "C"`
aborts on unwind, so no `catch_unwind` is needed — but a `VmError` inside the
invoker has nowhere to go. It prints and exits 1, matching
`kira_rt_trap_div_zero`, so a trap reached through native code and one reached
directly look the same to a user. `backend_parity/` pins both directions.

## Known gaps

- **One session per thread at a time.** Two `Session::run`s nested on one thread
  work only because each library has its own invoker slot; the same library
  loaded twice would share one (`dlopen` refcounts to one image) and the inner
  session's cleanup would clear it out from under the outer one. Nothing needs
  this yet.
- **No leak accounting on the native side.** The VM proves `heap.current == 0` at
  exit; the native half has no equivalent, so a leaked handle at the seam is
  silent. A double free crashes and the parity tests catch it; a leak would not.
