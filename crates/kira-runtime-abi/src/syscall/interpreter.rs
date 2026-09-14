//! The interpreter-serving policy for a Linux system call.
//!
//! Split from the call table in the parent module: whether the VM can serve a
//! call on the program behalf, and the one clause reason it gives when it cannot.

use super::LinuxSyscall;

impl LinuxSyscall {
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
            | Self::Syncfs
            | Self::Socket
            | Self::Socketpair
            | Self::Listen
            | Self::Accept4
            | Self::Connect
            | Self::Sendmsg
            | Self::Recvmsg
            | Self::Shutdown
            | Self::Getsockname
            | Self::Setsockopt
            | Self::Getsockopt => true,
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
            | Self::Bind
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
            | Self::Syncfs
            | Self::Socket
            | Self::Socketpair
            | Self::Listen
            | Self::Accept4
            | Self::Connect
            | Self::Sendmsg
            | Self::Recvmsg
            | Self::Shutdown
            | Self::Getsockname
            | Self::Setsockopt
            | Self::Getsockopt => "",
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
            Self::Bind => {
                "would create a socket name on the developer's filesystem, which outlives the run \
                 and which the next one would find already there"
            }
        }
    }
}
