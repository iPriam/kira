# Linux

The Linux kernel, called directly. `import Linux` and a path dependency:

```kira
let dependencies = [
    Dependency { name: "Linux", path: "../kira/packages/linux" }
]
```

Every function here reaches the kernel through `@FFI.Syscall`, which lowers to
`svc #0` on AArch64 and `syscall` on x86-64 — no libc, no `dlopen`, no symbol to
resolve at startup. That is what makes a Kira program able to be PID 1 in an
initramfs: build it with

```sh
kira build --target aarch64-linux-gnu --relocation-model static --linkage static <package>
```

and the calls in it are instructions in the image rather than jumps through a
loader that is not there.

## Why this is a package and not part of Foundation

`import Foundation` loads every `.kira` file under `foundation/app/` into one
flat scope, on every platform, whether the program names anything in it or not.
A `@FFI.Syscall` declaration is refused at compile time on a target that cannot
reach the Linux kernel — that refusal is the point of the feature — so a Linux
syscall module inside Foundation would refuse every Windows and macOS build of
every Kira program, including ones that had never heard of it.

A Linux-only capability being an explicit dependency is also the honest shape:
a package that names this one is saying it does not run anywhere else.

## The error convention

A Linux system call answers a non-negative value or a small negative number that
is `-errno`. That is one integer carrying two meanings, and reading it wrong
looks like success: `-2` from `read` is `ENOENT`, and a caller that treated it as
a byte count would advance a buffer backwards. So nothing here hands the raw
answer back. Every call returns `Result<Int, LinuxError>`, decoded once, in
`linuxDecode`.

## Not everything, deliberately

The calls here are the ones a userland needs to start: `read`, `write`, `mount`,
`umount2`, `reboot`, `execve`, `wait4`, `exit_group`, `sync`. The compiler owns
the numbers — they differ per architecture — so adding a call means adding a row
to `kira_runtime_abi::syscall`, and a name this package does not declare is
refused by name rather than mis-numbered.
