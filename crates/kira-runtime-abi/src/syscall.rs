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
}

/// Every system call this table knows, in tag order.
///
/// A total list rather than a search: the frontend prints it when it refuses an
/// unknown name, and a name that is in the enum but missing from here would be
/// a call the author cannot discover.
pub const LINUX_SYSCALLS: [LinuxSyscall; 10] = [
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
    }

    #[test]
    fn every_call_round_trips_through_its_tag_and_its_name() {
        for syscall in LINUX_SYSCALLS {
            assert_eq!(LinuxSyscall::from_tag(syscall.tag()), Some(syscall));
            assert_eq!(LinuxSyscall::parse(syscall.label()), Some(syscall));
        }
        assert_eq!(LinuxSyscall::from_tag(10), None);
    }

    /// A name this table does not carry resolves to nothing at all. The near
    /// miss is the case that matters: `exitGroup` is what a Kira author would
    /// write by habit, and resolving it to `exit_group` would mean the spelling
    /// in source stops being the spelling in `man 2`.
    #[test]
    fn a_name_this_table_does_not_carry_resolves_to_nothing() {
        assert_eq!(LinuxSyscall::parse("exitGroup"), None);
        assert_eq!(LinuxSyscall::parse("openat"), None);
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
}
