//! Entering the Linux kernel: the inline-assembly callee a `@FFI.Syscall`
//! import is called through.
//!
//! # Why inline assembly and not a call
//!
//! There is nothing to call. A system call is one instruction — `svc #0` on
//! AArch64, `syscall` on x86-64 — with the call number and the arguments in
//! agreed registers. Routing it through libc's wrapper would work and would
//! defeat the point: the program this exists for is PID 1 in an initramfs, in an
//! image with no libc and no dynamic loader, and a call that resolves `write`
//! through a shared object is a call that never starts.
//!
//! # Why the constraint string is built rather than written out
//!
//! One asm callee is needed per architecture and per arity — seven arities on
//! two architectures. Written out, that is fourteen strings differing by one
//! register each, and the failure a wrong one produces is not a compile error:
//! the kernel reads whatever was in the register the string forgot to name. So
//! the registers come from [`kira_runtime_abi::syscall`], which owns the ABI,
//! and this assembles them into the notation LLVM takes.
//!
//! Three parts of that notation are load-bearing:
//!
//! * The `sideeffect` flag. Without it the call has no result the optimizer can
//!   see a use for whenever the result is discarded — `exit_group`, `sync` — and
//!   the whole instruction is deleted as dead.
//! * The `~{memory}` clobber. A `read` writes through the buffer pointer it was
//!   given, and without this LLVM is entitled to assume nothing behind that
//!   pointer changed and to keep serving loads from before the call.
//! * On x86-64, `~{rcx}` and `~{r11}`. The `syscall` instruction itself writes
//!   the return address into `rcx` and the saved flags into `r11`, so a value
//!   the surrounding code left in either is gone — and code that was not told
//!   goes on using it.

use kira_runtime_abi::syscall::SyscallArch;
use llvm_sys::LLVMInlineAsmDialect;
use llvm_sys::core::*;

use super::Codegen;
use super::plan::CodegenTarget;
use super::types::Callable;

impl CodegenTarget {
    /// The architecture whose instructions this module is being lowered for, or
    /// `None` for a target that is not a processor Kira names one for.
    ///
    /// The host arm answers about the machine running the compiler, which is the
    /// same machine the module is for: a host build's objects run here. That is
    /// also why a system call reached through the interpreter is correct — the
    /// adapter it goes through is compiled for the machine the interpreter is
    /// running on, and enters that machine's kernel with that machine's numbers.
    pub(crate) fn architecture(&self) -> Option<&str> {
        match self {
            Self::Native(target) => match target.cross() {
                None => Some(std::env::consts::ARCH),
                Some(cross) => Some(cross.triple().arch()),
            },
            Self::Wasm(_) => None,
        }
    }
}

impl Codegen<'_> {
    /// The system-call ABI of the machine this module is being lowered for.
    ///
    /// `None` for a machine Kira has no entry sequence for. The frontend already
    /// refused a `@FFI.Syscall` on such a target by name, so a `None` reaching a
    /// syscall lowering is a compiler fault rather than a program's mistake, and
    /// it is reported as one.
    pub(super) fn syscall_arch(&self) -> Option<SyscallArch> {
        SyscallArch::for_arch(self.target.architecture()?)
    }

    /// The inline-assembly callee that enters the kernel with `arity` arguments.
    ///
    /// Its LLVM type is `i64 (i64, i64 * arity)`: the first parameter is the call
    /// number and the rest are the arguments, each already widened to a full
    /// register word. The result is the kernel's own answer — a non-negative
    /// value or a negated `errno` — and a caller whose declaration named no
    /// result simply discards it, which keeps one constraint string per arity
    /// instead of two.
    pub(super) fn syscall_callee(&self, arch: SyscallArch, arity: usize) -> Callable {
        let mut parameters = vec![self.types.i64; arity + 1];
        let instruction = super::ffi::c_string(arch.instruction());
        let constraints = super::ffi::c_string(&syscall_constraints(arch, arity));
        // SAFETY: `i64` belongs to this module's context, `parameters` outlives
        // the type call, and both strings outlive the `LLVMGetInlineAsm` call
        // that copies them.
        unsafe {
            let ty = LLVMFunctionType(
                self.types.i64,
                parameters.as_mut_ptr(),
                parameters.len() as u32,
                0,
            );
            let value = LLVMGetInlineAsm(
                ty,
                instruction.as_ptr(),
                arch.instruction().len(),
                constraints.as_ptr(),
                constraints.as_bytes().len(),
                // Has side effects: entering the kernel is the effect, and
                // without this a call whose result is unused is deleted.
                1,
                // The stack needs no realignment: the entry sequence pushes
                // nothing and calls nothing.
                0,
                // GNU assembler syntax, which is what both instruction spellings
                // are written in.
                LLVMInlineAsmDialect::LLVMInlineAsmDialectATT,
                // Cannot throw: there is no unwinder on the other side of `svc`.
                0,
            );
            Callable { ty, value }
        }
    }
}

/// The LLVM constraint string for `arity` arguments on `arch`.
///
/// Output first, then the call number, then one argument register each, then the
/// registers the entry sequence destroys and the memory clobber. The order of the
/// operands is the order the call passes them, which is why the number comes
/// before the arguments here and in every caller.
fn syscall_constraints(arch: SyscallArch, arity: usize) -> String {
    let mut constraints = format!(
        "={{{}}},{{{}}}",
        arch.result_register(),
        arch.number_register()
    );
    for register in arch.argument_registers().iter().take(arity) {
        constraints.push_str(&format!(",{{{register}}}"));
    }
    for register in arch.clobbered_registers() {
        constraints.push_str(&format!(",~{{{register}}}"));
    }
    constraints.push_str(",~{memory}");
    constraints
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two strings proved at the LLVM level before this existed. They are
    /// pinned here because a wrong register in one of them is not a compile
    /// error: the kernel reads whatever was in the register the string named.
    #[test]
    fn the_constraint_strings_are_the_ones_measured_against_the_kernel() {
        assert_eq!(
            syscall_constraints(SyscallArch::Aarch64, 3),
            "={x0},{x8},{x0},{x1},{x2},~{memory}"
        );
        assert_eq!(
            syscall_constraints(SyscallArch::X86_64, 3),
            "={rax},{rax},{rdi},{rsi},{rdx},~{rcx},~{r11},~{memory}"
        );
    }

    /// A call with no arguments still names its output and the number register,
    /// and nothing else. `sync` is that call.
    #[test]
    fn a_call_with_no_arguments_names_only_the_output_and_the_number() {
        assert_eq!(
            syscall_constraints(SyscallArch::Aarch64, 0),
            "={x0},{x8},~{memory}"
        );
        assert_eq!(
            syscall_constraints(SyscallArch::X86_64, 0),
            "={rax},{rax},~{rcx},~{r11},~{memory}"
        );
    }

    /// Six arguments is the widest the kernel entry carries, and the argument
    /// registers appear in the ABI's order — `r10` in x86-64's fourth position,
    /// not `rcx`.
    #[test]
    fn the_widest_call_names_every_argument_register_in_order() {
        assert_eq!(
            syscall_constraints(SyscallArch::Aarch64, 6),
            "={x0},{x8},{x0},{x1},{x2},{x3},{x4},{x5},~{memory}"
        );
        assert_eq!(
            syscall_constraints(SyscallArch::X86_64, 6),
            "={rax},{rax},{rdi},{rsi},{rdx},{r10},{r8},{r9},~{rcx},~{r11},~{memory}"
        );
    }

    /// Every arity names exactly as many argument registers as it passes
    /// arguments, which is the property a hand-written table gets wrong.
    #[test]
    fn every_arity_names_one_register_per_argument() {
        for arch in [SyscallArch::Aarch64, SyscallArch::X86_64] {
            for arity in 0..=kira_runtime_abi::syscall::MAX_SYSCALL_ARGUMENTS {
                let constraints = syscall_constraints(arch, arity);
                let inputs = constraints
                    .split(',')
                    .filter(|part| part.starts_with('{'))
                    .count();
                // The number register plus one per argument.
                assert_eq!(
                    inputs,
                    arity + 1,
                    "{arch:?} at arity {arity}: {constraints}"
                );
                assert!(constraints.ends_with(",~{memory}"));
            }
        }
    }
}
