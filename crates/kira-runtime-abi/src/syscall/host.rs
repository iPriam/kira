//! The host-side kernel entry: one performer both engines' hosts share.
//!
//! The mirror of [`crate::file_system::host`], and it exists for the same
//! reason. A program's system call is machine code when the backend emitted it
//! and a request to the embedder when the interpreter reached it, and those two
//! must not be two implementations of the kernel ABI — so the numbers and the
//! registers come from [`super`] on both paths and only the notation differs.
//!
//! Nothing here is reachable from the portable VM core: the capability's
//! default refuses, and a program gets this only from a host that answered
//! [`HostCapabilities::syscall`] by calling [`perform`].
//!
//! # Why inline assembly and not `libc::syscall`
//!
//! `@FFI.Syscall` exists so that Kira's kernel calls do not go through a C
//! library: a program built from `packages/linux` is PID 1 in an initramfs with
//! no `libc.so` to load and no loader to find one. Serving the interpreter's
//! half through `libc::syscall` would put the C library back in on one engine
//! only, which is worse than not serving it at all — the two engines would stop
//! being comparable at exactly the point this capability exists to compare
//! them, and a `errno`-setting wrapper answers `-1` where the kernel answers
//! `-EBADF`.
//!
//! # Where this refuses
//!
//! Everywhere but Linux on the two architectures Kira lowers on. There is no
//! fallback and no emulation: a host that cannot enter the kernel says
//! [`SyscallError::NoKernelHost`], which is what `wasm32-unknown-unknown`,
//! macOS, and Windows all get.

use super::{LinuxSyscall, MAX_SYSCALL_ARGUMENTS, SyscallError};
use crate::{
    ForeignArg, ForeignCallError, ForeignResult, ForeignSignature, ForeignType, ForeignTypeSpec,
    HostCapabilities,
};

/// Enters the kernel with `args` in the argument registers, answering the word
/// the result register holds.
///
/// The kernel's own encoding, undecoded: a non-negative value or a small
/// negative `-errno`. Reading that convention belongs to the Kira program —
/// `packages/linux` does it in `linuxDecode` — and doing it here would make the
/// interpreted call answer something the emitted one does not.
///
/// This is *not* a policy gate. It performs whatever it is handed, because the
/// same function serves an embedder that legitimately owns its process. The
/// interpreter's policy is [`LinuxSyscall::servable_by_an_interpreter`], applied
/// by [`call`] and by the CLI before a VM run starts.
///
/// # Safety
///
/// Any argument the declaration typed as a pointer must address memory the
/// calling program owns, for the length the call is given. Linux answers
/// `EFAULT` for an address outside this process, so a wild word is an ordinary
/// failure — but an address that happens to be *inside* the interpreter is one
/// the kernel will write through, and nothing below can tell that from the
/// buffer the program meant. This is the same obligation
/// `ForeignLibrary::call` carries at the `@FFI.Extern` seam, for the same
/// reason: the word came from a Kira program that is allowed to build one.
pub unsafe fn perform(syscall: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
    if args.len() > MAX_SYSCALL_ARGUMENTS {
        return Err(SyscallError::TooManyArguments(args.len()));
    }
    let mut words = [0i64; MAX_SYSCALL_ARGUMENTS];
    words[..args.len()].copy_from_slice(args);
    // SAFETY: forwarded from this function's own contract — the caller
    // guarantees every pointer word addresses memory the program owns.
    unsafe { enter(syscall, words) }
}

/// Serves one `@FFI.Syscall` import for a host running an interpreted program.
///
/// The whole of the interpreted path: it applies the interpreter's policy,
/// lowers the declaration's arguments to the register words the kernel reads,
/// asks `host` — so an embedder that overrode
/// [`HostCapabilities::syscall`] is the one that answers — and reads the result
/// register back as the declared result type.
///
/// The policy is applied here rather than in [`perform`] because this is the
/// function that means "a host is making this call *on a program's behalf*".
/// The CLI refuses a non-servable call by name before the program starts, which
/// is where an author should ever meet it; this is the backstop that keeps a
/// module reaching the seam another way from powering the machine off.
pub fn call<H: HostCapabilities + ?Sized>(
    host: &mut H,
    syscall: LinuxSyscall,
    signature: &ForeignSignature,
    args: &[ForeignArg<'_>],
) -> Result<ForeignResult, ForeignCallError> {
    if !syscall.servable_by_an_interpreter() {
        return Err(SyscallError::Unservable { call: syscall }.into());
    }
    let expected = signature.parameters().len();
    if args.len() != expected {
        return Err(ForeignCallError::ArgumentCount {
            expected,
            actual: args.len(),
        });
    }
    // A `CString` argument crosses as a pointer to a NUL-terminated copy, and
    // the copy has to outlive the call rather than the loop that made it. This
    // is the transient half of the rule `c_storage` states: the kernel reads
    // the bytes while it runs and never keeps the pointer, so this storage is
    // freed when the vector drops instead of being leaked for the process.
    let mut owned: Vec<std::ffi::CString> = Vec::new();
    let mut words = Vec::with_capacity(args.len());
    for (index, argument) in args.iter().enumerate() {
        words.push(word_of(argument, index, &mut owned)?);
    }
    let answer = host.syscall(syscall, &words)?;
    // Held until the answer is back, not merely until the words were built: the
    // kernel reads the bytes during the call, so releasing them any earlier
    // would hand it a pointer into freed storage.
    drop(owned);
    result_of(signature.result(), answer)
}

/// Lowers one declared argument to the word its register receives.
///
/// Sign for a signed width and zero for an unsigned one, because that is what
/// the declaration asked for: a `fd: I32` holding -1 has to arrive as a
/// sign-extended -1, not as 4294967295, which the kernel would read as an
/// enormous descriptor number. The emitted lowering makes the same distinction,
/// and the harness pins it on both.
fn word_of(
    argument: &ForeignArg<'_>,
    index: usize,
    owned: &mut Vec<std::ffi::CString>,
) -> Result<i64, ForeignCallError> {
    Ok(match *argument {
        ForeignArg::I8(value) => i64::from(value),
        ForeignArg::I16(value) => i64::from(value),
        ForeignArg::I32(value) => i64::from(value),
        ForeignArg::I64(value) => value,
        ForeignArg::U8(value) => i64::from(value),
        ForeignArg::U16(value) => i64::from(value),
        ForeignArg::U32(value) => i64::from(value),
        ForeignArg::U64(value) => value as i64,
        ForeignArg::RawPtr(value) => value as i64,
        ForeignArg::CString(text) => {
            let text = std::ffi::CString::new(text)
                .map_err(|_| ForeignCallError::InteriorNul { index })?;
            let word = text.as_ptr() as usize as i64;
            owned.push(text);
            word
        }
        ForeignArg::Void
        | ForeignArg::Bool(_)
        | ForeignArg::F32(_)
        | ForeignArg::F64(_)
        | ForeignArg::Aggregate { .. } => return Err(SyscallError::NotARegisterWord.into()),
    })
}

/// Reads the result register back as the type the declaration named.
///
/// Truncating rather than refusing a narrow width, which is what the register
/// holds: the kernel writes a full word and a declaration that wrote `I32` said
/// which part of it means something.
fn result_of(spec: ForeignTypeSpec, answer: i64) -> Result<ForeignResult, ForeignCallError> {
    let ForeignTypeSpec::Scalar(scalar) = spec else {
        return Err(SyscallError::NotARegisterWord.into());
    };
    Ok(match scalar {
        ForeignType::Void => ForeignResult::Void,
        ForeignType::I8 => ForeignResult::I8(answer as i8),
        ForeignType::I16 => ForeignResult::I16(answer as i16),
        ForeignType::I32 => ForeignResult::I32(answer as i32),
        ForeignType::I64 => ForeignResult::I64(answer),
        ForeignType::U8 => ForeignResult::U8(answer as u8),
        ForeignType::U16 => ForeignResult::U16(answer as u16),
        ForeignType::U32 => ForeignResult::U32(answer as u32),
        ForeignType::U64 => ForeignResult::U64(answer as u64),
        ForeignType::RawPtr => ForeignResult::RawPtr(answer as u64),
        ForeignType::Bool | ForeignType::F32 | ForeignType::F64 | ForeignType::CString => {
            return Err(SyscallError::NotARegisterWord.into());
        }
    })
}

/// The AArch64 entry: number in `x8`, arguments in `x0`–`x5`, result in `x0`.
///
/// `svc` preserves every register but `x0`, which is why nothing else is
/// declared clobbered — see [`SyscallArch::clobbered_registers`](
/// super::SyscallArch::clobbered_registers).
///
/// # Safety
///
/// As [`perform`]: every pointer word must address memory the program owns.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
unsafe fn enter(
    syscall: LinuxSyscall,
    words: [i64; MAX_SYSCALL_ARGUMENTS],
) -> Result<i64, SyscallError> {
    let number = syscall.number(super::SyscallArch::Aarch64);
    let answer: i64;
    // SAFETY: the registers are the ones the AArch64 kernel entry reads, taken
    // from the same table the code generator emits from, and the pointer
    // obligation is the caller's. `nostack` holds because `svc` touches no
    // user stack.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") number,
            inlateout("x0") words[0] => answer,
            in("x1") words[1],
            in("x2") words[2],
            in("x3") words[3],
            in("x4") words[4],
            in("x5") words[5],
            options(nostack),
        );
    }
    Ok(answer)
}

/// The x86-64 entry: number in `rax`, arguments in `rdi, rsi, rdx, r10, r8,
/// r9`, result in `rax`.
///
/// `rcx` and `r11` are declared destroyed because the `syscall` instruction
/// itself writes the return address into one and the saved flags into the
/// other. A compiler that was not told keeps using whatever it left there.
///
/// # Safety
///
/// As [`perform`]: every pointer word must address memory the program owns.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe fn enter(
    syscall: LinuxSyscall,
    words: [i64; MAX_SYSCALL_ARGUMENTS],
) -> Result<i64, SyscallError> {
    let number = syscall.number(super::SyscallArch::X86_64);
    let answer: i64;
    // SAFETY: the registers are the ones the x86-64 kernel entry reads, taken
    // from the same table the code generator emits from, and the pointer
    // obligation is the caller's. `rcx` and `r11` are surrendered because the
    // instruction destroys them; `nostack` holds because it touches no user
    // stack.
    unsafe {
        core::arch::asm!(
            "syscall",
            inlateout("rax") number => answer,
            in("rdi") words[0],
            in("rsi") words[1],
            in("rdx") words[2],
            in("r10") words[3],
            in("r8") words[4],
            in("r9") words[5],
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    Ok(answer)
}

/// Every other host: there is no kernel here to enter.
///
/// `wasm32-unknown-unknown` is the one that matters — `kira-runtime-abi` and
/// everything above it through `kira-vm-runtime` must keep compiling for it —
/// but macOS, Windows, and a Linux machine on a third architecture all land
/// here, and all of them say so rather than emulating an answer a Kira program
/// would then decode as a real one.
///
/// # Safety
///
/// Trivially satisfied: nothing is dereferenced. The signature matches the
/// entries above so [`perform`] has one call site.
#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "aarch64", target_arch = "x86_64")
)))]
unsafe fn enter(
    syscall: LinuxSyscall,
    words: [i64; MAX_SYSCALL_ARGUMENTS],
) -> Result<i64, SyscallError> {
    let _ = (syscall, words);
    Err(SyscallError::NoKernelHost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapturingHost;

    /// The policy is enforced before anything is lowered, so a refused call
    /// never reaches a register — and the message says what it would have done.
    #[test]
    fn a_call_no_interpreter_serves_is_refused_before_it_is_lowered() {
        let mut host = CapturingHost::new();
        let signature = ForeignSignature::scalars(vec![ForeignType::I64], ForeignType::I64);
        let error = call(
            &mut host,
            LinuxSyscall::ExitGroup,
            &signature,
            &[ForeignArg::I64(0)],
        )
        .expect_err("an interpreter does not end itself");
        assert_eq!(
            error,
            ForeignCallError::Syscall(SyscallError::Unservable {
                call: LinuxSyscall::ExitGroup,
            })
        );
        assert!(
            error
                .to_string()
                .contains("would end the interpreter itself"),
            "{error}"
        );
    }

    /// A host that never granted the capability refuses a call it *would*
    /// otherwise serve, rather than answering a value the program would decode.
    #[test]
    fn a_host_with_no_kernel_refuses_a_servable_call() {
        let mut host = CapturingHost::new();
        let signature = ForeignSignature::scalars(
            vec![ForeignType::I64, ForeignType::CString, ForeignType::U64],
            ForeignType::I64,
        );
        assert_eq!(
            call(
                &mut host,
                LinuxSyscall::Write,
                &signature,
                &[
                    ForeignArg::I64(1),
                    ForeignArg::CString("x"),
                    ForeignArg::U64(1),
                ],
            ),
            Err(ForeignCallError::Syscall(SyscallError::NoKernelHost))
        );
    }

    /// The declaration's arity is checked here rather than trusted, because the
    /// words are positional: one missing argument would shift every later one
    /// into the wrong register and the kernel would read it as something else.
    #[test]
    fn a_call_site_that_does_not_match_the_declaration_is_refused() {
        let mut host = CapturingHost::new();
        let signature =
            ForeignSignature::scalars(vec![ForeignType::I64, ForeignType::U64], ForeignType::I64);
        assert_eq!(
            call(
                &mut host,
                LinuxSyscall::Read,
                &signature,
                &[ForeignArg::I64(1)]
            ),
            Err(ForeignCallError::ArgumentCount {
                expected: 2,
                actual: 1,
            })
        );
    }

    /// A written width is what reaches the register: signed widths sign-extend
    /// and unsigned ones do not. `-1` in an `I32` has to arrive as a 64-bit -1,
    /// which is the case a plain `as i64` on the raw bits gets wrong.
    #[test]
    fn a_declared_width_decides_how_its_value_fills_the_register() {
        let mut owned = Vec::new();
        assert_eq!(word_of(&ForeignArg::I32(-1), 0, &mut owned), Ok(-1));
        assert_eq!(
            word_of(&ForeignArg::U32(u32::MAX), 0, &mut owned),
            Ok(4_294_967_295)
        );
        assert_eq!(word_of(&ForeignArg::I8(-2), 0, &mut owned), Ok(-2));
        assert_eq!(word_of(&ForeignArg::U64(u64::MAX), 0, &mut owned), Ok(-1));
        assert!(owned.is_empty());
    }

    /// A `CString` argument is a pointer to a NUL-terminated copy that lives
    /// until the call returns, and a string carrying an interior NUL is refused
    /// rather than handed over truncated.
    #[test]
    fn a_c_string_argument_becomes_a_pointer_to_terminated_bytes() {
        let mut owned = Vec::new();
        let word = word_of(&ForeignArg::CString("hi"), 0, &mut owned).expect("a pointer word");
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].as_ptr() as usize as i64, word);
        assert_eq!(owned[0].as_bytes_with_nul(), b"hi\0");
        assert_eq!(
            word_of(&ForeignArg::CString("a\0b"), 3, &mut owned),
            Err(ForeignCallError::InteriorNul { index: 3 })
        );
    }

    /// The result register is read back as the declared type, undecoded: a
    /// failure is the kernel's own `-errno` and a Kira program decodes it. The
    /// engines agree only because neither side interprets it.
    #[test]
    fn the_result_register_is_read_back_as_the_declared_type() {
        assert_eq!(
            result_of(ForeignTypeSpec::Scalar(ForeignType::I64), -9),
            Ok(ForeignResult::I64(-9))
        );
        assert_eq!(
            result_of(ForeignTypeSpec::Scalar(ForeignType::Void), 0),
            Ok(ForeignResult::Void)
        );
        assert_eq!(
            result_of(ForeignTypeSpec::Scalar(ForeignType::I32), -9),
            Ok(ForeignResult::I32(-9))
        );
        assert_eq!(
            result_of(ForeignTypeSpec::Scalar(ForeignType::F64), 0),
            Err(ForeignCallError::Syscall(SyscallError::NotARegisterWord))
        );
    }

    /// Seven arguments have nowhere to go, so the performer refuses rather than
    /// dropping one. The frontend refuses a seventh parameter too; this is what
    /// keeps a table built elsewhere from getting past it.
    #[test]
    fn more_arguments_than_registers_is_refused_rather_than_truncated() {
        // SAFETY: refused on the length before any register is written.
        let answer = unsafe { perform(LinuxSyscall::Write, &[0; MAX_SYSCALL_ARGUMENTS + 1]) };
        assert_eq!(answer, Err(SyscallError::TooManyArguments(7)));
    }

    /// The real kernel, on the machines Kira lowers on: `write` to a descriptor
    /// nothing is open on answers `-EBADF` in its result register, which is the
    /// whole convention this seam has to preserve.
    #[test]
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    fn the_kernel_answers_its_own_negative_errno() {
        let bytes = std::ffi::CString::new("x").expect("no interior NUL");
        // SAFETY: the pointer addresses a live NUL-terminated buffer this test
        // owns, for the one byte the count names.
        let answer = unsafe {
            perform(
                LinuxSyscall::Write,
                &[-1, bytes.as_ptr() as usize as i64, 1],
            )
        };
        assert_eq!(answer, Ok(-9));
    }

    /// A call that succeeds comes back as itself: `write` of no bytes takes
    /// none and says zero, the same success path the harness uses because it
    /// puts nothing in anybody's output.
    #[test]
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    fn a_successful_call_answers_the_kernels_own_count() {
        let bytes = std::ffi::CString::new("").expect("no interior NUL");
        // SAFETY: as above, and the count is zero, so nothing is read at all.
        let answer =
            unsafe { perform(LinuxSyscall::Write, &[1, bytes.as_ptr() as usize as i64, 0]) };
        assert_eq!(answer, Ok(0));
    }
}
