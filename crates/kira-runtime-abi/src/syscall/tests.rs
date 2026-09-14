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
