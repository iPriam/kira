# Raw system-call harness

`@FFI.Syscall` exercised against a real Linux kernel. 18 tests in
`app/SkxSyscallTests.kira`, all with the `skx`/`Skx` prefix, depending on
`packages/linux` for the typed wrappers and on Foundation for `Result`.

## One command

```sh
kira test --backend llvm tests-kik/syscall-harness
```

Linux-only and LLVM-only, and neither is a choice this suite makes:

- A `@FFI.Syscall` is refused at compile time on a target that cannot reach the
  Linux kernel. On macOS or Windows there is no program here to run, which is why
  the cargo gate (`crates/kira-cli/tests/kik_harness.rs`) is
  `#[cfg(target_os = "linux")]` rather than a skip inside the suite.
- The VM and hybrid engines refuse a program that calls the kernel, by name,
  before it starts. A system call is an instruction and the interpreter has none
  of its own to put one in; reaching the kernel from it would mean the *host's*
  numbers on the *host's* architecture, which is a different call from the one
  the program named.

## What the cases assert, and why they are deterministic

Nothing here writes bytes anybody sees: the success path is a `write` of an empty
string, which is a real round trip through the entry sequence with no output to
land in the middle of the driver's report.

Every failing case is chosen so its `errno` is the same on every machine — a bad
file descriptor is `EBADF` whoever runs it, a path that does not exist is
`ENOENT` whether or not the process is privileged, and a process with no children
gets `ECHILD`. `mount` of a real device is deliberately absent: its answer depends
on who is running the suite.

Coverage is by arity, because the arity is what the emitted constraint string
differs by: `sync` takes none, `umount2` two, `write`/`read`/`execve` three,
`wait4` four, `mount` five. The two remaining cases pin the width rules — a
declaration written `I32` puts a sign-extended 32-bit value in its register, and
a raw entry point answers the kernel's own `-errno` with no decode in the way.
