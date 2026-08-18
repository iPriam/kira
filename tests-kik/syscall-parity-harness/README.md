# Servable system calls, on all three engines

`@FFI.Syscall` restricted to the four calls an interpreter can serve — `read`,
`write`, `sync`, `ppoll` — so that the same source runs on `vm`, `llvm` and
`hybrid` and can be compared. 10 tests in `app/SkpSyscallTests.kira`, all with
the `skp`/`Skp` prefix, plus a `@Main` whose printed checksums must match byte
for byte across the three.

## Four commands

```sh
kira test --backend vm     tests-kik/syscall-parity-harness
kira test --backend llvm   tests-kik/syscall-parity-harness
kira test --backend hybrid tests-kik/syscall-parity-harness
kira run  --backend vm|llvm|hybrid tests-kik/syscall-parity-harness
```

## Why this is a second package instead of a split file

The VM refuses a program that *names* a call no interpreter can serve, and it
refuses it before the program starts — so the refusal is a property of the
package, not of the case that would have made the call. A file of `mount` cases
sitting beside a file of `write` cases would take the whole package down with
it, and there is no per-case escape because there is nothing to escape from: the
program is refused at the command line, before a single `Test` is collected.

`tests-kik/syscall-harness` is therefore the other half of this suite and not a
superset of it. It keeps the six calls that only the emitted lowering can make —
`mount`, `umount2`, `reboot`, `execve`, `wait4`, `exit_group` — and runs on
`llvm` and `hybrid`. Between the two packages every call in the table is
exercised against a real kernel, and every route into the kernel is:

| Route | Where it is covered |
|---|---|
| Emitted `svc`/`syscall` in a whole-program build | `syscall-harness`, `--backend llvm` |
| Emitted instruction in a hybrid native half | `syscall-harness`, `--backend hybrid` (its wrappers are `@Native`) |
| The interpreter asking its host | this package, `--backend vm` |
| A hybrid bytecode half asking its host | this package, `--backend hybrid` (its wrappers are not `@Native`) |

The declarations here are the package's own rather than `packages/linux`'s for
the same reason the split exists: that package declares the whole kernel surface
Kira knows, including `mount` and `reboot`, and a program importing it names
those whether or not it calls them.

## What the cases assert, and why they are deterministic

Every failing case asserts a number that does not depend on who ran the suite. A
bad file descriptor is `EBADF` for root and for CI alike, a descriptor array the
kernel cannot read is `EFAULT`, and `sync` needs no privilege. The success path
is a `write` of an empty string, which is a real round trip into the kernel with
no output to land in the middle of the driver's report.

Coverage is by arity, because the arity is what the register sequence differs
by: `sync` takes none, `write` and `read` three, `ppoll` four. Two further cases
pin the width rules — a declaration written `I32` puts a sign-extended 32-bit
value in its register, and the raw entry point answers the kernel's own `-errno`
with no decode in the way. That last one is what a host serving the call could
most easily get wrong: decoding the answer itself would make the interpreted
call disagree with the emitted one here and nowhere else.

`ppoll`'s blocking case is deliberately absent. A `ppoll` on no descriptors with
no timeout never returns, which is exactly what an idle PID 1 wants of it and
exactly why no test can assert it.

Linux-only, and not by choice: a `@FFI.Syscall` is refused at compile time on a
target that cannot reach the Linux kernel, so on macOS or Windows there is no
program here to run. That is why the cargo gate
(`crates/kira-cli/tests/kik_harness.rs`) is `#[cfg(target_os = "linux")]` rather
than a skip inside the suite.
