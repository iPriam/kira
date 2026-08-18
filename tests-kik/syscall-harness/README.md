# Raw system-call harness

`@FFI.Syscall` exercised against a real Linux kernel, for the calls only an
emitted instruction can make. 19 tests in `app/SkxSyscallTests.kira`, all with
the `skx`/`Skx` prefix, depending on `packages/linux` for the typed wrappers and
on Foundation for `Result`.

`tests-kik/syscall-parity-harness` is the other half: the four calls a host can
make on an interpreted program's behalf, run on all three engines. Read its
README for what each package covers and why they cannot be one.

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

The pure VM refuses this package by name before it starts, and that refusal is
gated too. Not because an interpreter cannot enter the kernel — it asks its
host, and `tests-kik/syscall-parity-harness` is the package that does — but
because *these* calls act on the interpreter's own process or on the machine it
is running on. `execve` would replace the VM's image, `exit_group` would end it
mid-suite, `wait4` would reap its children, and `mount` and `umount2` would act
on the filesystem of whoever ran `kira run`. The six of them are why this package
and that one are two packages: the refusal is decided from the calls a program
*names*, before a single case is collected.

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
