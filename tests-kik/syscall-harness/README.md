# Raw system-call harness

`@FFI.Syscall` exercised against a real Linux kernel. 18 tests in
`app/SkxSyscallTests.kira`, all with the `skx`/`Skx` prefix, depending on
`packages/linux` for the typed wrappers and on Foundation for `Result`.

## Two commands

```sh
kira test --backend llvm   tests-kik/syscall-harness
kira test --backend hybrid tests-kik/syscall-harness
```

Both, because they reach the kernel by different routes and only running both
proves the second. On `llvm` the call site is machine code in the program. On
`hybrid` the bodies holding the calls are the native half and a `Test` reaches
them across the bridge — the same crossing the FFI harness exercises — so a
change that broke the native half's lowering while leaving a whole-program build
working would fail there and nowhere else.

Linux-only, and that is not a choice this suite makes: a `@FFI.Syscall` is
refused at compile time on a target that cannot reach the Linux kernel, so on
macOS or Windows there is no program here to run. That is why the cargo gate
(`crates/kira-cli/tests/kik_harness.rs`) is `#[cfg(target_os = "linux")]` rather
than a skip inside the suite.

The pure VM refuses such a program by name before it starts, and that refusal is
gated too: it has no instruction stream of its own to put `svc` in.

## What the cases assert, and why they are deterministic

Nothing here writes bytes anybody sees: the success path is a `write` of an empty
string, which is a real round trip through the entry sequence with no output to
land in the middle of the driver's report.

Every failing case asserts something that does not depend on who ran the suite. A
bad file descriptor is `EBADF` whoever asks, a program that is not there is
`ENOENT`, and a process with no children gets `ECHILD` — those assert the number.
`mount` and `umount2` need `CAP_SYS_ADMIN` and the kernel checks that before it
looks at the path, so their number is `EPERM` in CI and `ENOENT` as root; those
assert that the kernel refused the call, which is true either way. `mount` of a
*real* device is deliberately absent for the same reason in the other direction:
as root it would succeed.

Coverage is by arity, because the arity is what the emitted constraint string
differs by: `sync` takes none, `umount2` two, `write`/`read`/`execve` three,
`wait4` four, `mount` five. The two remaining cases pin the width rules — a
declaration written `I32` puts a sign-extended 32-bit value in its register, and
a raw entry point answers the kernel's own `-errno` with no decode in the way.
