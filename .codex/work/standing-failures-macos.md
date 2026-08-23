# Standing test failures on macOS: resolved

The nine failures this note used to track are gone; the workspace gate is
fully green on this host (3581/3581).

One root caused nine of them: the native backend released a call's lent
temporaries oldest-first, and because each temporary's dynamic alloca carries
a nesting stack-pointer save, the first restore popped every younger slot
before its value was released — the release calls that followed scribbled
over those bytes. Any call lending two or more temporaries corrupted or
crashed at cleanup, which is what took down the six in-language-compiler
parity tests and the three inflate/PNG harness cases. The fix is reverse
release order in `lower/call.rs`; `LtxLentTemporaryTests` pins the shape.

The rest: `lldb-dap` now resolves through `xcrun` when `PATH` misses it, the
debugger transcripts accept arm64's `pc =` beside x86-64's `rip =`, and the
`exp(log(37))` pin became the one-ulp band the host libm is entitled to.

A scratch worktree at `/tmp/kira-head-check` (detached at `f420eb8`) proved
the failures pre-dated the FFI-lifetime work; remove it with
`git worktree remove /tmp/kira-head-check`.
