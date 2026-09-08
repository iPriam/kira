//! The Linux system-call ABI: the calls Kira can name, their numbers on each
//! architecture, and the registers the kernel reads them out of.
//!
//! # Why the compiler owns the numbers
//!
//! A system call is identified by a number, and the number for one call is not
//! the same on two architectures: `write` is 64 on AArch64 and 1 on x86-64.
//! Kira has no conditional compilation and no way for a program to ask what
//! architecture it is being built for, so a number written in Kira source could
//! not be selected per machine — one spelling would have to be wrong on the
//! other. A declaration therefore names the call the way `man 2` names it and
//! this table supplies the number, which is also what lets an unsupported call
//! or an unsupported machine be refused *by name* at compile time instead of
//! entering the kernel at runtime with a number that means something else there.
//!
//! # Why the whole ABI is one file
//!
//! The numbers and the registers are two halves of one contract, and only one
//! caller needs each: the frontend validates a declaration against the names,
//! and the code generator emits the registers. Keeping them apart is how the
//! two would come to disagree — an architecture added to the name table but not
//! to the register table would be accepted by the frontend and then emitted
//! with another machine's registers. So both live here, and an architecture Kira
//! lowers on is one [`SyscallArch`] value carrying both.
//!
//! Nothing here is LLVM's vocabulary: the registers are named the way the
//! architecture manuals name them, and assembling them into a particular
//! backend's inline-assembly notation belongs to that backend.
//!
//! [`host`] is the one thing beside the table, and it is here for the same
//! reason both halves of the contract are: a host serving a program it is
//! *interpreting* enters the kernel with the numbers and the registers this
//! file already owns, so it reads them here rather than carrying a second copy.

pub mod host;

use thiserror::Error;

pub use host::{call, perform};

/// A Linux system call a `@FFI.Syscall` declaration may name.
///
/// A closed, Kira-owned set rather than a number the author writes. Every entry
/// is one the compiler has a number for on every architecture it lowers on, so
/// a declaration that passes the frontend cannot fail to have a number later.
/// The set grows by adding a variant with its numbers; nothing else changes.
///
/// The serialized tags are append-only, because an import table travels into a
/// `.kbc` module.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinuxSyscall {
    /// `read(fd, buffer, count)` — bytes from a file descriptor.
    Read = 0,
    /// `write(fd, buffer, count)` — bytes to a file descriptor.
    Write = 1,
    /// `mount(source, target, type, flags, data)` — attach a filesystem.
    Mount = 2,
    /// `umount2(target, flags)` — detach a filesystem.
    Umount2 = 3,
    /// `reboot(magic, magic2, command, argument)` — restart, halt, or power off.
    Reboot = 4,
    /// `execve(path, argv, envp)` — replace this process's image.
    Execve = 5,
    /// `wait4(pid, status, options, rusage)` — reap a child.
    Wait4 = 6,
    /// `exit_group(status)` — end every thread in this process.
    ExitGroup = 7,
    /// `sync()` — flush the filesystem caches.
    Sync = 8,
    /// `ppoll(fds, nfds, timeout, sigmask)` — wait for file descriptors.
    ///
    /// This is how a program waits for nothing in particular. AArch64 has no
    /// `pause` at all — the generic system-call table every architecture added
    /// since 2012 carries only the newer forms — so `ppoll` is the one call
    /// that blocks on both machines Kira lowers on. Given four null arguments
    /// it waits on no descriptor, without a timeout, which is what PID 1 does
    /// once it has nothing left to start; unlike a `wait4` loop it costs no
    /// processor time while doing it.
    Ppoll = 9,
    /// `chdir(path)` — change this process's working directory.
    Chdir = 10,
    /// `chroot(path)` — change this process's idea of `/`.
    ///
    /// The pair with `chdir` is what moves a userland out of the initramfs it
    /// booted in and into the volume holding the system: rootfs cannot be
    /// unmounted or pivoted away from, so the system volume is moved onto `/`
    /// and the process is re-anchored into it. Either one alone leaves a process
    /// whose root and whose working directory disagree about which filesystem it
    /// is on, which is why they arrive together.
    Chroot = 11,
    /// `openat(dirfd, path, flags, mode)` — open a file, relative to a directory.
    ///
    /// `openat` rather than `open`: the generic system-call table every
    /// architecture added since 2012 carries only this one, so AArch64 has no
    /// `open` to name. A caller with an absolute path passes `AT_FDCWD` and the
    /// two are the same call.
    Openat = 12,
    /// `close(fd)` — release a file descriptor.
    Close = 13,
    /// `ioctl(fd, request, argument)` — the call every driver answers on.
    ///
    /// Untyped by design: the request number says what the third argument
    /// points at, and the kernel fills it in. That makes it the whole of
    /// modesetting, of a GPU's resource management, and of most of what a
    /// device does that reading and writing cannot express.
    Ioctl = 14,
    /// `mmap(addr, length, prot, flags, fd, offset)` — map memory.
    Mmap = 15,
    /// `munmap(addr, length)` — release a mapping.
    Munmap = 16,
    /// `mprotect(addr, length, prot)` — change a mapping's permissions.
    Mprotect = 17,
    /// `lseek(fd, offset, whence)` — move a descriptor's file offset.
    Lseek = 18,
    /// `pread64(fd, buffer, count, offset)` — read at an offset, without moving the descriptor's own.
    Pread64 = 19,
    /// `pwrite64(fd, buffer, count, offset)` — write at an offset, leaving the descriptor's own where it was.
    Pwrite64 = 20,
    /// `fstat(fd, statbuf)` — what a descriptor refers to, and how large it is.
    Fstat = 21,
    /// `statx(dirfd, path, flags, mask, statxbuf)` — the same, by path and with a say in what is asked for.
    Statx = 22,
    /// `clock_gettime(clock, timespec)` — what time it is, on the clock named.
    ///
    /// An interpreter serves it because a program asking the time is asking
    /// the machine, and the machine is the same one either way.
    ClockGettime = 23,
    /// `nanosleep(request, remain)` — wait, without spending a processor doing it.
    Nanosleep = 24,
    /// `epoll_create1(flags)` — a descriptor that watches other descriptors.
    EpollCreate1 = 25,
    /// `epoll_ctl(epfd, op, fd, event)` — add, change, or remove one of the watched.
    EpollCtl = 26,
    /// `epoll_pwait(epfd, events, maxevents, timeout, sigmask)` — wait for any of them.
    EpollPwait = 27,
    /// `clone(flags, stack, parent_tid, tls, child_tid)` — a second process.
    ///
    /// The call an init needs before it can start anything: `execve` replaces
    /// the process that makes it, so a program that must still be there
    /// afterwards has to split first.
    Clone = 28,
    /// `getpid()` — this process's identifier.
    Getpid = 29,
    /// `kill(pid, signal)` — send a signal.
    Kill = 30,
    /// `setsid()` — a new session, detached from any terminal.
    Setsid = 31,
    /// `pipe2(fds, flags)` — a pair of descriptors, one writing into the other.
    Pipe2 = 32,
    /// `dup3(oldfd, newfd, flags)` — make one descriptor number refer to another's file.
    Dup3 = 33,
    /// `fcntl(fd, cmd, argument)` — a descriptor's own flags and locks.
    Fcntl = 34,
    /// `rt_sigaction(signal, action, old, size)` — what happens when a signal arrives.
    RtSigaction = 35,
    /// `rt_sigprocmask(how, set, old, size)` — which signals are blocked.
    RtSigprocmask = 36,
    /// `rt_sigreturn()` — return from a signal handler, which no caller writes by hand.
    RtSigreturn = 37,
    /// `signalfd4(fd, mask, size, flags)` — signals as a descriptor to read.
    Signalfd4 = 38,
    /// `mkdirat(dirfd, path, mode)` — create a directory.
    Mkdirat = 39,
    /// `unlinkat(dirfd, path, flags)` — remove a name.
    Unlinkat = 40,
    /// `renameat2(olddirfd, old, newdirfd, new, flags)` — move a name, atomically.
    ///
    /// The call an installer finishes with: a rename either happened or did
    /// not, so what it replaces is never half-written.
    Renameat2 = 41,
    /// `ftruncate(fd, length)` — set a file's size.
    Ftruncate = 42,
    /// `fdatasync(fd)` — flush one file's contents, rather than every filesystem the machine has.
    Fdatasync = 43,
    /// `getdents64(fd, buffer, count)` — what is in a directory.
    Getdents64 = 44,
    /// `fchmodat(dirfd, path, mode, flags)` — a file's permissions.
    Fchmodat = 45,
    /// `syncfs(fd)` — flush the filesystem a descriptor lives on, and no other.
    Syncfs = 46,
    /// `statfs(path, buf)` — how much room a filesystem has.
    Statfs = 47,
    /// `clone3(args, size)` — a second process, described by a structure.
    ///
    /// The one to reach for rather than `clone`. Legacy `clone` takes its
    /// arguments positionally and the last two are in a different order on
    /// aarch64 than on x86-64, so the same call is a different call per
    /// architecture and the mistake is silent. `clone3` takes a
    /// `struct clone_args` and a size, identically everywhere.
    ///
    /// It also carries `CLONE_PIDFD`, which answers with a descriptor for the
    /// child. A supervisor can then wait for a process to end in the same
    /// `ppoll` as everything else it waits on, and can signal it without the
    /// race that a recycled process identifier is.
    Clone3 = 48,
    /// `waitid(idtype, id, infop, options, rusage)` — reap a child, by pidfd.
    ///
    /// `wait4` reaps by process identifier, which is the number `CLONE_PIDFD`
    /// exists to stop anyone using. `waitid` takes `P_PIDFD`, so the thing that
    /// is waited for is the thing that was started rather than whatever now
    /// holds that number.
    Waitid = 49,
    /// `pidfd_send_signal(pidfd, signal, info, flags)` — signal a descriptor.
    ///
    /// `kill` names its target by a number the kernel is free to reuse the
    /// moment the process ends, so a supervisor that signals a stopped service
    /// can signal whatever started next. A descriptor refers to one process for
    /// as long as it is open, and to nothing at all afterwards.
    PidfdSendSignal = 50,
}

/// Every system call this table knows, in tag order.
///
/// A total list rather than a search: the frontend prints it when it refuses an
/// unknown name, and a name that is in the enum but missing from here would be
/// a call the author cannot discover.
pub const LINUX_SYSCALLS: [LinuxSyscall; 51] = [
    LinuxSyscall::Read,
    LinuxSyscall::Write,
    LinuxSyscall::Mount,
    LinuxSyscall::Umount2,
    LinuxSyscall::Reboot,
    LinuxSyscall::Execve,
    LinuxSyscall::Wait4,
    LinuxSyscall::ExitGroup,
    LinuxSyscall::Sync,
    LinuxSyscall::Ppoll,
    LinuxSyscall::Chdir,
    LinuxSyscall::Chroot,
    LinuxSyscall::Openat,
    LinuxSyscall::Close,
    LinuxSyscall::Ioctl,
    LinuxSyscall::Mmap,
    LinuxSyscall::Munmap,
    LinuxSyscall::Mprotect,
    LinuxSyscall::Lseek,
    LinuxSyscall::Pread64,
    LinuxSyscall::Pwrite64,
    LinuxSyscall::Fstat,
    LinuxSyscall::Statx,
    LinuxSyscall::ClockGettime,
    LinuxSyscall::Nanosleep,
    LinuxSyscall::EpollCreate1,
    LinuxSyscall::EpollCtl,
    LinuxSyscall::EpollPwait,
    LinuxSyscall::Clone,
    LinuxSyscall::Getpid,
    LinuxSyscall::Kill,
    LinuxSyscall::Setsid,
    LinuxSyscall::Pipe2,
    LinuxSyscall::Dup3,
    LinuxSyscall::Fcntl,
    LinuxSyscall::RtSigaction,
    LinuxSyscall::RtSigprocmask,
    LinuxSyscall::RtSigreturn,
    LinuxSyscall::Signalfd4,
    LinuxSyscall::Mkdirat,
    LinuxSyscall::Unlinkat,
    LinuxSyscall::Renameat2,
    LinuxSyscall::Ftruncate,
    LinuxSyscall::Fdatasync,
    LinuxSyscall::Getdents64,
    LinuxSyscall::Fchmodat,
    LinuxSyscall::Syncfs,
    LinuxSyscall::Statfs,
    LinuxSyscall::Clone3,
    LinuxSyscall::Waitid,
    LinuxSyscall::PidfdSendSignal,
];

/// How many arguments a Linux system call can take.
///
/// Six on every architecture Linux supports, because the arguments go in
/// registers and six is how many the kernel entry reserves for them. A seventh
/// has nowhere to go, so a declaration with seven parameters is refused rather
/// than lowered with one silently dropped.
pub const MAX_SYSCALL_ARGUMENTS: usize = 6;

impl LinuxSyscall {
    /// Returns the append-only serialized byte for this system call.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Decodes a system call from its serialized byte.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Read),
            1 => Some(Self::Write),
            2 => Some(Self::Mount),
            3 => Some(Self::Umount2),
            4 => Some(Self::Reboot),
            5 => Some(Self::Execve),
            6 => Some(Self::Wait4),
            7 => Some(Self::ExitGroup),
            8 => Some(Self::Sync),
            9 => Some(Self::Ppoll),
            10 => Some(Self::Chdir),
            11 => Some(Self::Chroot),
            12 => Some(Self::Openat),
            13 => Some(Self::Close),
            14 => Some(Self::Ioctl),
            15 => Some(Self::Mmap),
            16 => Some(Self::Munmap),
            17 => Some(Self::Mprotect),
            18 => Some(Self::Lseek),
            19 => Some(Self::Pread64),
            20 => Some(Self::Pwrite64),
            21 => Some(Self::Fstat),
            22 => Some(Self::Statx),
            23 => Some(Self::ClockGettime),
            24 => Some(Self::Nanosleep),
            25 => Some(Self::EpollCreate1),
            26 => Some(Self::EpollCtl),
            27 => Some(Self::EpollPwait),
            28 => Some(Self::Clone),
            29 => Some(Self::Getpid),
            30 => Some(Self::Kill),
            31 => Some(Self::Setsid),
            32 => Some(Self::Pipe2),
            33 => Some(Self::Dup3),
            34 => Some(Self::Fcntl),
            35 => Some(Self::RtSigaction),
            36 => Some(Self::RtSigprocmask),
            37 => Some(Self::RtSigreturn),
            38 => Some(Self::Signalfd4),
            39 => Some(Self::Mkdirat),
            40 => Some(Self::Unlinkat),
            41 => Some(Self::Renameat2),
            42 => Some(Self::Ftruncate),
            43 => Some(Self::Fdatasync),
            44 => Some(Self::Getdents64),
            45 => Some(Self::Fchmodat),
            46 => Some(Self::Syncfs),
            47 => Some(Self::Statfs),
            48 => Some(Self::Clone3),
            49 => Some(Self::Waitid),
            50 => Some(Self::PidfdSendSignal),
            _ => None,
        }
    }

    /// The name a declaration writes, which is the kernel's own.
    ///
    /// `exit_group`, not `exitGroup`: the name in Kira source is the name in
    /// `man 2`, so a reader can look up what the call does and what its
    /// arguments mean. Kira's own naming convention applies to the wrapper
    /// function around it, not to the identifier that selects the kernel entry.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Mount => "mount",
            Self::Umount2 => "umount2",
            Self::Reboot => "reboot",
            Self::Execve => "execve",
            Self::Wait4 => "wait4",
            Self::ExitGroup => "exit_group",
            Self::Sync => "sync",
            Self::Ppoll => "ppoll",
            Self::Chdir => "chdir",
            Self::Chroot => "chroot",
            Self::Openat => "openat",
            Self::Close => "close",
            Self::Ioctl => "ioctl",
            Self::Mmap => "mmap",
            Self::Munmap => "munmap",
            Self::Mprotect => "mprotect",
            Self::Lseek => "lseek",
            Self::Pread64 => "pread64",
            Self::Pwrite64 => "pwrite64",
            Self::Fstat => "fstat",
            Self::Statx => "statx",
            Self::ClockGettime => "clock_gettime",
            Self::Nanosleep => "nanosleep",
            Self::EpollCreate1 => "epoll_create1",
            Self::EpollCtl => "epoll_ctl",
            Self::EpollPwait => "epoll_pwait",
            Self::Clone => "clone",
            Self::Getpid => "getpid",
            Self::Kill => "kill",
            Self::Setsid => "setsid",
            Self::Pipe2 => "pipe2",
            Self::Dup3 => "dup3",
            Self::Fcntl => "fcntl",
            Self::RtSigaction => "rt_sigaction",
            Self::RtSigprocmask => "rt_sigprocmask",
            Self::RtSigreturn => "rt_sigreturn",
            Self::Signalfd4 => "signalfd4",
            Self::Mkdirat => "mkdirat",
            Self::Unlinkat => "unlinkat",
            Self::Renameat2 => "renameat2",
            Self::Ftruncate => "ftruncate",
            Self::Fdatasync => "fdatasync",
            Self::Getdents64 => "getdents64",
            Self::Fchmodat => "fchmodat",
            Self::Syncfs => "syncfs",
            Self::Statfs => "statfs",
            Self::Clone3 => "clone3",
            Self::Waitid => "waitid",
            Self::PidfdSendSignal => "pidfd_send_signal",
        }
    }

    /// Resolves a written name, or `None` when this table has no such call.
    ///
    /// A total function of the spelling: there is no near-match and no default,
    /// because a misspelled name resolved to a neighbour would call a different
    /// kernel entry with this declaration's arguments.
    pub fn parse(name: &str) -> Option<Self> {
        LINUX_SYSCALLS
            .into_iter()
            .find(|syscall| syscall.label() == name)
    }

    /// This call's number on `arch`.
    ///
    /// Total in both arguments, so no caller has to handle a name that has no
    /// number: the only way to hold a [`SyscallArch`] is
    /// [`SyscallArch::for_arch`], which is the one place an architecture without
    /// a lowering is turned away.
    pub const fn number(self, arch: SyscallArch) -> i64 {
        match arch {
            SyscallArch::Aarch64 => match self {
                Self::Read => 63,
                Self::Write => 64,
                Self::Mount => 40,
                Self::Umount2 => 39,
                Self::Reboot => 142,
                Self::Execve => 221,
                Self::Wait4 => 260,
                Self::ExitGroup => 94,
                Self::Sync => 81,
                Self::Ppoll => 73,
                Self::Chdir => 49,
                Self::Chroot => 51,
                Self::Openat => 56,
                Self::Close => 57,
                Self::Ioctl => 29,
                Self::Mmap => 222,
                Self::Munmap => 215,
                Self::Mprotect => 226,
                Self::Lseek => 62,
                Self::Pread64 => 67,
                Self::Pwrite64 => 68,
                Self::Fstat => 80,
                Self::Statx => 291,
                Self::ClockGettime => 113,
                Self::Nanosleep => 101,
                Self::EpollCreate1 => 20,
                Self::EpollCtl => 21,
                Self::EpollPwait => 22,
                Self::Clone => 220,
                Self::Getpid => 172,
                Self::Kill => 129,
                Self::Setsid => 157,
                Self::Pipe2 => 59,
                Self::Dup3 => 24,
                Self::Fcntl => 25,
                Self::RtSigaction => 134,
                Self::RtSigprocmask => 135,
                Self::RtSigreturn => 139,
                Self::Signalfd4 => 74,
                Self::Mkdirat => 34,
                Self::Unlinkat => 35,
                Self::Renameat2 => 276,
                Self::Ftruncate => 46,
                Self::Fdatasync => 83,
                Self::Getdents64 => 61,
                Self::Fchmodat => 53,
                Self::Syncfs => 267,
                Self::Statfs => 43,
                Self::Clone3 => 435,
                Self::Waitid => 95,
                Self::PidfdSendSignal => 424,
            },
            SyscallArch::X86_64 => match self {
                Self::Read => 0,
                Self::Write => 1,
                Self::Mount => 165,
                Self::Umount2 => 166,
                Self::Reboot => 169,
                Self::Execve => 59,
                Self::Wait4 => 61,
                Self::ExitGroup => 231,
                Self::Sync => 162,
                Self::Ppoll => 271,
                Self::Chdir => 80,
                Self::Chroot => 161,
                Self::Openat => 257,
                Self::Close => 3,
                Self::Ioctl => 16,
                Self::Mmap => 9,
                Self::Munmap => 11,
                Self::Mprotect => 10,
                Self::Lseek => 8,
                Self::Pread64 => 17,
                Self::Pwrite64 => 18,
                Self::Fstat => 5,
                Self::Statx => 332,
                Self::ClockGettime => 228,
                Self::Nanosleep => 35,
                Self::EpollCreate1 => 291,
                Self::EpollCtl => 233,
                Self::EpollPwait => 281,
                Self::Clone => 56,
                Self::Getpid => 39,
                Self::Kill => 62,
                Self::Setsid => 112,
                Self::Pipe2 => 293,
                Self::Dup3 => 292,
                Self::Fcntl => 72,
                Self::RtSigaction => 13,
                Self::RtSigprocmask => 14,
                Self::RtSigreturn => 15,
                Self::Signalfd4 => 289,
                Self::Mkdirat => 258,
                Self::Unlinkat => 263,
                Self::Renameat2 => 316,
                Self::Ftruncate => 77,
                Self::Fdatasync => 75,
                Self::Getdents64 => 217,
                Self::Fchmodat => 268,
                Self::Syncfs => 306,
                Self::Statfs => 137,
                Self::Clone3 => 435,
                Self::Waitid => 247,
                Self::PidfdSendSignal => 424,
            },
        }
    }

    /// Whether control comes back from this call.
    ///
    /// `exit_group` is the one that does not: the kernel ends the process, so
    /// there is no return value and no next instruction. That is worth knowing
    /// rather than ignoring for two reasons. A declaration that gives it a
    /// result type is describing a value nothing can ever produce, so it is
    /// refused. And the code generator can tell the optimizer that control stops
    /// there, which is what keeps a caller from having to write a return it can
    /// never reach.
    pub const fn returns(self) -> bool {
        !matches!(self, Self::ExitGroup)
    }

    /// Whether an interpreter can serve this call on the program's behalf.
    ///
    /// A property of what the call *does*, which is why it lives on the call
    /// rather than on the host that serves it: no host is in a position to
    /// answer it differently, because the fact underneath is the same for all
    /// of them — **under the interpreter the process is the interpreter, not
    /// the program.**
    ///
    /// The three that answer yes take a descriptor and say what they did.
    /// Serving one is indistinguishable from the program having made it, so a
    /// `--backend vm` run of a program that only reads, writes, or waits on
    /// descriptors can be compared byte for byte against a native run — which is
    /// the oracle a `@FFI.Syscall` otherwise has none of.
    ///
    /// Taking a descriptor is the whole of the test, and it is a narrower test
    /// than "acts on files". A call the program hands one of its own open
    /// descriptors to cannot reach past what the program already had; a call
    /// that takes none is bounded by nothing smaller than the machine.
    ///
    /// The rest act on the process itself or on the machine, and there the
    /// difference between the interpreter and the program is the whole failure:
    ///
    /// - `wait4` reaps the *interpreter's* children, so a program waiting for
    ///   one it started sees a process it never spawned, or blocks forever.
    /// - `exit_group` ends the interpreter in the middle of the program it is
    ///   running — under `kira test` that is the runner, and the rest of the
    ///   suite never reports.
    /// - `execve` replaces the interpreter's image, so the VM ceases to exist
    ///   and there is nothing left to hand the program's result back to.
    /// - `mount`, `umount2` and `reboot` act on the developer's real machine.
    ///   `kira run --backend vm` on an init would mount over their filesystem
    ///   and power the machine off.
    /// - `sync` is that same argument, and sat on the wrong side of it until it
    ///   flushed a real machine. It takes no descriptor: it writes back every
    ///   filesystem mounted on the box, so what it acts on is the developer's
    ///   machine rather than the program's open files. `fsync(fd)` is the call
    ///   that would belong here. Overreach is not the worst of it — over a 9p
    ///   `/mnt/c` the flush parks in the kernel uninterruptibly, so a run that
    ///   reaches it cannot be killed and `timeout` does not end it.
    ///
    /// None of that applies to the code generator's lowering, where the process
    /// really is the program — so this narrows one engine and changes nothing
    /// about `--backend llvm` or the native half of `--backend hybrid`.
    pub const fn servable_by_an_interpreter(self) -> bool {
        match self {
            Self::Read
            | Self::Write
            | Self::Ppoll
            | Self::Openat
            | Self::Close
            | Self::Ioctl
            | Self::Lseek
            | Self::Pread64
            | Self::Pwrite64
            | Self::Fstat
            | Self::Statx
            | Self::ClockGettime
            | Self::Nanosleep
            | Self::EpollCreate1
            | Self::EpollCtl
            | Self::EpollPwait
            | Self::Pipe2
            | Self::Fcntl
            | Self::Ftruncate
            | Self::Fdatasync
            | Self::Getdents64
            | Self::Syncfs => true,
            Self::Sync
            | Self::Mount
            | Self::Umount2
            | Self::Reboot
            | Self::Execve
            | Self::Wait4
            | Self::Chdir
            | Self::Chroot
            | Self::Mmap
            | Self::Munmap
            | Self::Mprotect
            | Self::Clone
            | Self::Clone3
            | Self::Waitid
            | Self::PidfdSendSignal
            | Self::Getpid
            | Self::Kill
            | Self::Setsid
            | Self::Dup3
            | Self::RtSigaction
            | Self::RtSigprocmask
            | Self::RtSigreturn
            | Self::Signalfd4
            | Self::Mkdirat
            | Self::Unlinkat
            | Self::Renameat2
            | Self::Fchmodat
            | Self::Statfs
            | Self::ExitGroup => false,
        }
    }

    /// Why an interpreter refuses this call, as one clause naming the effect.
    ///
    /// Written once here rather than at each place that reports a refusal, so
    /// the compile-time message and the runtime one cannot come to say different
    /// things about the same call. Empty for a call that is served, which no
    /// caller has a reason to ask about.
    pub const fn interpreter_refusal(self) -> &'static str {
        match self {
            Self::Read
            | Self::Write
            | Self::Ppoll
            | Self::Openat
            | Self::Close
            | Self::Ioctl
            | Self::Lseek
            | Self::Pread64
            | Self::Pwrite64
            | Self::Fstat
            | Self::Statx
            | Self::ClockGettime
            | Self::Nanosleep
            | Self::EpollCreate1
            | Self::EpollCtl
            | Self::EpollPwait
            | Self::Pipe2
            | Self::Fcntl
            | Self::Ftruncate
            | Self::Fdatasync
            | Self::Getdents64
            | Self::Syncfs => "",
            Self::Sync => {
                "would flush every filesystem mounted on the machine running the interpreter, \
                 taking no descriptor that could bound it to the program's own files"
            }
            Self::Mount => "would mount a filesystem on the machine running the interpreter",
            Self::Umount2 => "would unmount a filesystem of the machine running the interpreter",
            Self::Reboot => "would restart, halt, or power off the machine running the interpreter",
            Self::Execve => {
                "would replace the interpreter's own image, so the VM would cease to exist \
                 mid-program"
            }
            Self::Wait4 => "would reap the interpreter's children rather than the program's",
            Self::ExitGroup => {
                "would end the interpreter itself, in the middle of the program it is running"
            }
            Self::Chdir => {
                "would move the interpreter's own working directory, so every relative path \
                 the interpreter resolves afterwards would resolve somewhere else"
            }
            Self::Chroot => {
                "would confine the interpreter itself to the program's new root, leaving it \
                 unable to reach the rest of the developer's machine"
            }
            Self::Mmap => {
                "would map into the interpreter's own address space, where the program has no way to reach it and the interpreter did not ask for it"
            }
            Self::Munmap => "would unmap part of the interpreter's own address space",
            Self::Mprotect => "would change the permissions of the interpreter's own memory",
            Self::Clone => {
                "would fork the interpreter, leaving two of them running the same program"
            }
            Self::Clone3 => {
                "would fork the interpreter, leaving two of them running the same program"
            }
            Self::Waitid => {
                "would reap a child of the interpreter, which is not the program's to reap"
            }
            Self::PidfdSendSignal => {
                "would signal a process of the developer's machine through a descriptor the interpreter owns"
            }
            Self::Getpid => {
                "would answer with the interpreter's identifier, which is not the program's"
            }
            Self::Kill => {
                "would signal a process of the developer's machine, chosen by a number the program made up"
            }
            Self::Setsid => "would detach the interpreter from its own terminal",
            Self::Dup3 => {
                "would rewrite the interpreter's own descriptor table, where a number the program picked may be the interpreter's output"
            }
            Self::RtSigaction => {
                "would install a handler in the interpreter, which is the process the signal would reach"
            }
            Self::RtSigprocmask => {
                "would block signals for the interpreter rather than for the program"
            }
            Self::RtSigreturn => "would return from a handler the interpreter never entered",
            Self::Signalfd4 => {
                "would take delivery of the interpreter's signals, which are not the program's to consume"
            }
            Self::Mkdirat => "would create a directory on the developer's machine",
            Self::Unlinkat => "would remove a file from the developer's machine",
            Self::Renameat2 => "would move a file on the developer's machine",
            Self::Fchmodat => "would change permissions on the developer's machine",
            Self::Statfs => "would answer about a filesystem of the developer's machine",
        }
    }
}

/// Why a system call a Kira program made did not reach the kernel.
///
/// Reserved for the call never happening. A call the kernel *refused* is not an
/// error here: it answers `-errno` in its result register exactly as it does in
/// a native build, and decoding that is the program's own job — which is what
/// makes the two engines comparable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum SyscallError {
    /// This host does not enter a kernel at all.
    ///
    /// The default answer, and the only one on `wasm32`, on a non-Linux host,
    /// and on a machine Kira has no register sequence for.
    #[error(
        "this host does not enter a kernel, so it cannot make the system call this program asked \
         for"
    )]
    NoKernelHost,
    /// The host enters a kernel, but not for this call.
    ///
    /// See [`LinuxSyscall::servable_by_an_interpreter`] for what separates the
    /// two sets, and why it is not a property of the host.
    #[error(
        "`{}` cannot be served by an interpreter: it {}",
        call.label(),
        call.interpreter_refusal()
    )]
    Unservable {
        /// The call the interpreter will not make.
        call: LinuxSyscall,
    },
    /// More argument words than the kernel entry has registers for.
    ///
    /// The frontend refuses a seventh parameter, so this is a table and a
    /// compiler that disagree rather than something an author can write.
    #[error("a system call takes at most {MAX_SYSCALL_ARGUMENTS} arguments, and {0} were supplied")]
    TooManyArguments(usize),
    /// A declared position is not something a register can carry.
    ///
    /// Also unreachable from source: `syscall_word_of` in the frontend refuses a
    /// float, an aggregate, and a `CString` result. Reaching it means an import
    /// table was built by something that did not apply those rules.
    #[error("a `@FFI.Syscall` position that no register can carry reached the kernel seam")]
    NotARegisterWord,
}

/// An architecture Kira emits system calls for.
///
/// Holding one of these *is* the proof that a lowering exists, which is why it
/// carries the registers rather than answering questions about them: there is
/// no `SyscallArch` for a machine whose registers are unknown, so no caller has
/// an unsupported case to handle.
///
/// macOS is deliberately absent even on the same processors. Its system-call
/// numbers are not a stable interface — Apple's supported entry is libSystem,
/// the numbers move between releases, and a program that called them directly
/// would break on an OS update with no diagnostic. Kira refuses the target
/// instead of shipping numbers it cannot stand behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyscallArch {
    /// 64-bit ARM: `svc #0`, number in `x8`, result in `x0`.
    Aarch64,
    /// 64-bit x86: `syscall`, number in `rax`, result in `rax`.
    X86_64,
}

/// The operating system whose system calls this table describes, spelled the way
/// a target triple's OS component is.
///
/// One constant rather than a comparison written out at each gate, so the
/// frontend's refusal and the code generator's assumption cannot come to
/// disagree about which spelling counts.
pub const SYSCALL_OS: &str = "linux";

impl SyscallArch {
    /// The architecture Kira lowers system calls on for `arch`, or `None` when
    /// it has no lowering for that machine.
    ///
    /// `arch` is a target triple's architecture component. Only 64-bit AArch64
    /// and x86-64 answer: the 32-bit entries take their arguments in different
    /// registers and carry a third numbering of their own, and guessing at one
    /// would emit a call that lands somewhere else in the kernel's table.
    pub fn for_arch(arch: &str) -> Option<Self> {
        match arch {
            "aarch64" => Some(Self::Aarch64),
            "x86_64" => Some(Self::X86_64),
            _ => None,
        }
    }

    /// This architecture's triple spelling, for a diagnostic that lists what is
    /// supported.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Aarch64 => "aarch64",
            Self::X86_64 => "x86_64",
        }
    }

    /// The instruction that enters the kernel.
    pub const fn instruction(self) -> &'static str {
        match self {
            Self::Aarch64 => "svc #0",
            Self::X86_64 => "syscall",
        }
    }

    /// The register the call number goes in.
    pub const fn number_register(self) -> &'static str {
        match self {
            Self::Aarch64 => "x8",
            Self::X86_64 => "rax",
        }
    }

    /// The register the kernel's answer comes back in.
    pub const fn result_register(self) -> &'static str {
        match self {
            Self::Aarch64 => "x0",
            Self::X86_64 => "rax",
        }
    }

    /// The registers the arguments go in, in declaration order.
    ///
    /// x86-64 uses `r10` where the C calling convention uses `rcx`, because the
    /// `syscall` instruction overwrites `rcx` with the return address. An
    /// argument placed there by analogy with a C call would be destroyed by the
    /// instruction that was supposed to pass it.
    pub const fn argument_registers(self) -> &'static [&'static str] {
        match self {
            Self::Aarch64 => &["x0", "x1", "x2", "x3", "x4", "x5"],
            Self::X86_64 => &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
        }
    }

    /// The registers the kernel entry destroys besides the result.
    ///
    /// Empty on AArch64: `svc` preserves everything but `x0`. On x86-64 the
    /// `syscall` instruction itself writes the return address into `rcx` and the
    /// saved flags into `r11`, so a value the caller left in either is gone —
    /// and a code generator that was not told keeps using it.
    pub const fn clobbered_registers(self) -> &'static [&'static str] {
        match self {
            Self::Aarch64 => &[],
            Self::X86_64 => &["rcx", "r11"],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned bytes: an import table carrying these travels into a `.kbc`
    /// module, so a renumbering would make an old module name a different call.
    #[test]
    fn the_serialized_tags_are_the_ones_already_written() {
        assert_eq!(LinuxSyscall::Read.tag(), 0);
        assert_eq!(LinuxSyscall::Write.tag(), 1);
        assert_eq!(LinuxSyscall::Mount.tag(), 2);
        assert_eq!(LinuxSyscall::Umount2.tag(), 3);
        assert_eq!(LinuxSyscall::Reboot.tag(), 4);
        assert_eq!(LinuxSyscall::Execve.tag(), 5);
        assert_eq!(LinuxSyscall::Wait4.tag(), 6);
        assert_eq!(LinuxSyscall::ExitGroup.tag(), 7);
        assert_eq!(LinuxSyscall::Sync.tag(), 8);
        assert_eq!(LinuxSyscall::Ppoll.tag(), 9);
        assert_eq!(LinuxSyscall::Chdir.tag(), 10);
        assert_eq!(LinuxSyscall::Chroot.tag(), 11);
        assert_eq!(LinuxSyscall::Openat.tag(), 12);
        assert_eq!(LinuxSyscall::Close.tag(), 13);
    }

    #[test]
    fn every_call_round_trips_through_its_tag_and_its_name() {
        for syscall in LINUX_SYSCALLS {
            assert_eq!(LinuxSyscall::from_tag(syscall.tag()), Some(syscall));
            assert_eq!(LinuxSyscall::parse(syscall.label()), Some(syscall));
        }
        // One past the last tag, derived rather than written: tags are assigned
        // densely from zero, so the table's length *is* the first unassigned
        // one. A literal here was 48, which stopped meaning "unassigned" the
        // day the table grew past it and turned this into a test that the
        // newest call does not exist.
        let first_unassigned =
            u8::try_from(LINUX_SYSCALLS.len()).expect("the table is far below the tag width");
        assert_eq!(LinuxSyscall::from_tag(first_unassigned), None);
    }

    /// A name this table does not carry resolves to nothing at all. The near
    /// miss is the case that matters: `exitGroup` is what a Kira author would
    /// write by habit, and resolving it to `exit_group` would mean the spelling
    /// in source stops being the spelling in `man 2`.
    #[test]
    fn a_name_this_table_does_not_carry_resolves_to_nothing() {
        assert_eq!(LinuxSyscall::parse("exitGroup"), None);
        assert_eq!(LinuxSyscall::parse("bpf"), None);
        assert_eq!(LinuxSyscall::parse(""), None);
        assert_eq!(LinuxSyscall::parse("WRITE"), None);
    }

    /// The measured numbers, both architectures. These are the whole reason the
    /// compiler owns the table: `write` is 64 on one machine and 1 on the other,
    /// so one number written in Kira source would be wrong on one of them.
    #[test]
    fn the_numbers_are_the_kernels_own_on_each_architecture() {
        let aarch64 = SyscallArch::Aarch64;
        let x86_64 = SyscallArch::X86_64;
        assert_eq!(LinuxSyscall::Write.number(aarch64), 64);
        assert_eq!(LinuxSyscall::Write.number(x86_64), 1);
        assert_eq!(LinuxSyscall::Read.number(aarch64), 63);
        assert_eq!(LinuxSyscall::Read.number(x86_64), 0);
        assert_eq!(LinuxSyscall::Mount.number(aarch64), 40);
        assert_eq!(LinuxSyscall::Mount.number(x86_64), 165);
        assert_eq!(LinuxSyscall::Umount2.number(aarch64), 39);
        assert_eq!(LinuxSyscall::Umount2.number(x86_64), 166);
        assert_eq!(LinuxSyscall::Reboot.number(aarch64), 142);
        assert_eq!(LinuxSyscall::Reboot.number(x86_64), 169);
        assert_eq!(LinuxSyscall::Execve.number(aarch64), 221);
        assert_eq!(LinuxSyscall::Execve.number(x86_64), 59);
        assert_eq!(LinuxSyscall::Wait4.number(aarch64), 260);
        assert_eq!(LinuxSyscall::Wait4.number(x86_64), 61);
        assert_eq!(LinuxSyscall::ExitGroup.number(aarch64), 94);
        assert_eq!(LinuxSyscall::ExitGroup.number(x86_64), 231);
        assert_eq!(LinuxSyscall::Sync.number(aarch64), 81);
        assert_eq!(LinuxSyscall::Sync.number(x86_64), 162);
    }

    /// Two architectures answer and everything else is turned away here, which
    /// is what makes [`LinuxSyscall::number`] total.
    #[test]
    fn only_the_architectures_with_a_lowering_answer() {
        assert_eq!(SyscallArch::for_arch("aarch64"), Some(SyscallArch::Aarch64));
        assert_eq!(SyscallArch::for_arch("x86_64"), Some(SyscallArch::X86_64));
        assert_eq!(SyscallArch::for_arch("x86"), None);
        assert_eq!(SyscallArch::for_arch("arm"), None);
        assert_eq!(SyscallArch::for_arch("riscv64"), None);
        assert_eq!(SyscallArch::for_arch("wasm32"), None);
    }

    /// Six argument registers on both, because six is what the kernel entry
    /// reserves — the constant the frontend refuses a seventh parameter against
    /// is the length of these lists and not a number written twice.
    #[test]
    fn each_architecture_names_exactly_the_arguments_the_kernel_reads() {
        for arch in [SyscallArch::Aarch64, SyscallArch::X86_64] {
            assert_eq!(arch.argument_registers().len(), MAX_SYSCALL_ARGUMENTS);
        }
    }

    /// x86-64 must not pass an argument in `rcx`: the `syscall` instruction
    /// writes the return address there, so an argument placed in it is destroyed
    /// by the instruction meant to deliver it.
    #[test]
    fn x86_64_keeps_its_arguments_out_of_the_registers_the_instruction_destroys() {
        let arch = SyscallArch::X86_64;
        for clobbered in arch.clobbered_registers() {
            assert!(
                !arch.argument_registers().contains(clobbered),
                "`{clobbered}` is both an argument register and destroyed by `syscall`"
            );
        }
        assert_eq!(arch.clobbered_registers(), &["rcx", "r11"]);
        assert!(arch.argument_registers().contains(&"r10"));
        assert!(!arch.argument_registers().contains(&"rcx"));
    }

    /// `exit_group` is the one call with no return, and the table says so rather
    /// than every caller special-casing the name.
    #[test]
    fn exit_group_is_the_one_call_that_does_not_come_back() {
        for syscall in LINUX_SYSCALLS {
            assert_eq!(syscall.returns(), syscall != LinuxSyscall::ExitGroup);
        }
    }

    /// The split, pinned. Three calls take a descriptor the program already
    /// holds and are therefore the same call whoever's process makes them; the
    /// other seven act on the process or the machine, which under the
    /// interpreter is not the program's.
    ///
    /// `sync` is among the seven deliberately. It is the one that reads as
    /// file-shaped and is not: no descriptor bounds it, so it reaches every
    /// mount on the machine. Pinning it here is what keeps a later reading of
    /// "acts on files" from putting it back.
    #[test]
    fn an_interpreter_serves_the_calls_that_act_only_on_descriptors() {
        for syscall in [LinuxSyscall::Read, LinuxSyscall::Write, LinuxSyscall::Ppoll] {
            assert!(syscall.servable_by_an_interpreter(), "{}", syscall.label());
        }
        for syscall in [
            LinuxSyscall::Sync,
            LinuxSyscall::Mount,
            LinuxSyscall::Umount2,
            LinuxSyscall::Reboot,
            LinuxSyscall::Execve,
            LinuxSyscall::Wait4,
            LinuxSyscall::ExitGroup,
        ] {
            assert!(!syscall.servable_by_an_interpreter(), "{}", syscall.label());
        }
    }

    /// Every refused call says what it would do, and no served one carries a
    /// reason it will never be asked for. A refusal naming nothing is what sends
    /// a reader looking for a flag to pass instead of an engine to change.
    #[test]
    fn every_refused_call_carries_the_effect_that_refuses_it() {
        for syscall in LINUX_SYSCALLS {
            assert_eq!(
                syscall.interpreter_refusal().is_empty(),
                syscall.servable_by_an_interpreter(),
                "{}",
                syscall.label()
            );
        }
        assert_eq!(
            SyscallError::Unservable {
                call: LinuxSyscall::Reboot
            }
            .to_string(),
            "`reboot` cannot be served by an interpreter: it would restart, halt, or power off \
             the machine running the interpreter"
        );
    }
}
