//! The `@FFI.Syscall` seam: turning a bodyless declaration that names a Linux
//! system call into a validated [`HirForeign`] row.
//!
//! # Why this is a sibling of `@FFI.Extern` and not a separate concept
//!
//! Both forms declare the same thing — a function Kira calls but does not
//! contain — and they differ in exactly one place: what has to be named to reach
//! it. An extern names a library and a symbol in it; a syscall names the call
//! and the compiler supplies the number. Everything after that point is the same
//! question with the same answer — the arity check, the argument coercion, the
//! call's result type, the refusal of a body, the shared call namespace — so it
//! is asked once, in [`crate::foreign`], and both forms land in
//! [`kira_semantics_model::hir::HirProgram::foreign`] with a
//! [`kira_runtime_abi::ForeignAbi`] that says which mechanism reaches them.
//!
//! # Why the refusals are here and not in the backend
//!
//! Three of them are about the machine: the operating system must be Linux, the
//! architecture must be one Kira lowers on, and the call must be one this
//! compiler has a number for. A backend could ask all three, and it would be
//! asking too late — `kira check` would pass and the build would fail, and on a
//! target with no lowering there is no honest object to emit at all. Asking here
//! means the answer arrives with a span, pointing at the name that has no
//! number.
//!
//! The type rules are here for the older reason [`crate::foreign`] states: a
//! syscall argument is a machine word, and that is true of every backend.

use kira_runtime_abi::syscall::{
    LINUX_SYSCALLS, LinuxSyscall, MAX_SYSCALL_ARGUMENTS, SYSCALL_OS, SyscallArch,
};
use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType, ForeignTypeSpec};
use kira_semantics_model::hir::HirForeign;
use kira_semantics_model::{IntSpelling, Type};
use kira_source::Span;
use kira_syntax_model::ast::{ForeignMark, Function, TypeRefId};

use crate::analyze::Analyzer;

/// Whether a position in a syscall signature is the result or an argument.
///
/// The two differ on one type: nothing is passed to the kernel as `Void`, and a
/// declaration that returns nothing is written with no result type at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// One of the up-to-six argument registers.
    Argument,
    /// The result register.
    Result,
}

impl Analyzer<'_> {
    /// Validates one `@FFI.Syscall` declaration, returning its row when every
    /// check passes and `None` — with diagnostics emitted — when any fails.
    ///
    /// Every check runs whatever the others answered, so an author sees each
    /// mistake once rather than one per rebuild. `annotations_ok` carries the
    /// result of the shared annotation check that already ran.
    pub(crate) fn validate_syscall(
        &mut self,
        function: &Function,
        name: &str,
        mark: &ForeignMark,
        annotations_ok: bool,
    ) -> Option<HirForeign> {
        let syscall = self.parse_syscall_fields(mark);
        let target_ok = self.check_syscall_target(mark);
        let signature = self.map_syscall_signature(function, syscall);
        match (annotations_ok, target_ok, syscall, signature) {
            (true, true, Some(syscall), Some(signature)) => Some(HirForeign {
                kira_name: name.to_owned(),
                // No library: the kernel is not one. Nothing reads this field for
                // a syscall import — every step that would is guarded by
                // `ForeignAbi::binds_a_library_symbol` — and the symbol is the
                // kernel's own name for the call, which is what the emitting
                // backend resolves to a number for the machine it emits for.
                library: String::new(),
                symbol: syscall.label().to_owned(),
                abi: ForeignAbi::LinuxSyscall,
                signature,
                // The C seam's per-position marshalling has nothing to do here.
                // A syscall argument is a machine word: there is no aggregate to
                // lay out, no pointer whose target has members, and no handle
                // struct to unwrap, because none of those fits in a register the
                // kernel reads.
                param_wrappers: vec![None; function.params.len()].into(),
                param_pointees: vec![None; function.params.len()].into(),
                result_pointee: None,
                result_wrapper: None,
                name_span: function.name_span,
            }),
            _ => None,
        }
    }

    /// Reads the one field an `@FFI.Syscall` block carries — `name` — and
    /// resolves it to a system call this compiler has numbers for.
    ///
    /// There is no `library`, `symbol`, or `abi` to read. A block that writes one
    /// is refused rather than ignored: an author who wrote `library: libc` means
    /// something this declaration does not do, and silently dropping the field
    /// would leave them believing the call goes through libc.
    fn parse_syscall_fields(&mut self, mark: &ForeignMark) -> Option<LinuxSyscall> {
        let mut named: Option<(String, Span)> = None;
        let mut ok = true;
        for field in &mark.fields {
            let key = self.interner.resolve(field.key).to_owned();
            let value = self.interner.resolve(field.value).to_owned();
            if key != "name" {
                self.emit(
                    field.key_span,
                    "KSEM178",
                    format!(
                        "unknown `@FFI.Syscall` field `{key}` (the only field is `name`; a system \
                         call has no library to load, no symbol to look up, and one calling \
                         convention)"
                    ),
                );
                ok = false;
                continue;
            }
            if named.is_some() {
                self.emit(
                    field.key_span,
                    "KSEM179",
                    "`@FFI.Syscall` field `name` is set twice",
                );
                ok = false;
                continue;
            }
            named = Some((value, field.value_span));
        }
        let Some((written, span)) = named else {
            self.emit(
                mark.block_span,
                "KSEM180",
                "`@FFI.Syscall` block is missing its required `name` field",
            );
            return None;
        };
        let Some(syscall) = LinuxSyscall::parse(&written) else {
            let known: Vec<&str> = LINUX_SYSCALLS.iter().map(|call| call.label()).collect();
            self.emit(
                span,
                "KSEM279",
                format!(
                    "`{written}` is not a system call Kira has a number for; the calls it knows \
                     are {}. A name is written the way `man 2` writes it, because the number it \
                     resolves to differs per architecture and the compiler supplies it.",
                    known.join(", ")
                ),
            );
            return None;
        };
        ok.then_some(syscall)
    }

    /// Refuses a target that cannot reach the Linux kernel, or one whose
    /// architecture Kira has no system-call lowering for.
    ///
    /// Both are refused by name at the declaration rather than reported by the
    /// backend, because the fix is in the source or on the command line and
    /// neither is visible from an object file. The two are separate diagnostics
    /// because they have separate fixes: one is the wrong operating system, the
    /// other is the right one on a processor Kira does not emit the entry
    /// sequence for.
    fn check_syscall_target(&mut self, mark: &ForeignMark) -> bool {
        let platform = self.machine.platform().to_owned();
        let architecture = self.machine.architecture().to_owned();
        let mut ok = true;
        if platform != SYSCALL_OS {
            self.emit(
                mark.span,
                "KSEM280",
                format!(
                    "`@FFI.Syscall` names a Linux system call, and this build targets `{platform}`. \
                     Only Linux numbers its calls as a stable interface — macOS supports libSystem \
                     rather than its own numbers, and they move between releases — so Kira has \
                     none to emit here."
                ),
            );
            ok = false;
        }
        if SyscallArch::for_arch(&architecture).is_none() {
            self.emit(
                mark.span,
                "KSEM281",
                format!(
                    "`@FFI.Syscall` has no lowering for architecture `{architecture}`; Kira emits \
                     the kernel entry sequence for `{}` and `{}`",
                    SyscallArch::Aarch64.label(),
                    SyscallArch::X86_64.label()
                ),
            );
            ok = false;
        }
        ok
    }

    /// Maps a syscall declaration's written signature to the register words it
    /// crosses as, or `None` when the arity or any position is one the kernel
    /// entry cannot carry.
    fn map_syscall_signature(
        &mut self,
        function: &Function,
        syscall: Option<LinuxSyscall>,
    ) -> Option<ForeignSignature> {
        let mut ok = true;
        if function.params.len() > MAX_SYSCALL_ARGUMENTS {
            self.emit(
                function.name_span,
                "KSEM282",
                format!(
                    "a system call takes at most {MAX_SYSCALL_ARGUMENTS} arguments, and this \
                     declaration writes {}: the arguments go in registers, and there is no \
                     seventh for one to go in",
                    function.params.len()
                ),
            );
            ok = false;
        }
        let mut params = Vec::with_capacity(function.params.len());
        for param in &function.params {
            match self.syscall_word_of(param.ty, Position::Argument) {
                Some(word) => params.push(ForeignTypeSpec::Scalar(word)),
                None => ok = false,
            }
        }
        let result = match function.return_type {
            None => ForeignType::Void,
            Some(type_ref) => match self.syscall_word_of(type_ref, Position::Result) {
                Some(word) => word,
                None => {
                    ok = false;
                    ForeignType::Void
                }
            },
        };
        // A call that does not come back has no result to read, and a declaration
        // that names one describes a value nothing can ever produce. Refused
        // rather than accepted-and-undefined: the alternative is a caller reading
        // whatever happened to be in the result register of a process that no
        // longer exists.
        if let Some(syscall) = syscall
            && !syscall.returns()
            && let Some(type_ref) = function.return_type
        {
            self.emit(
                self.tree.type_ref(type_ref).span(),
                "KSEM284",
                format!(
                    "`{}` does not return, so its declaration cannot name a result type; write no \
                     `->` at all",
                    syscall.label()
                ),
            );
            ok = false;
        }
        ok.then(|| ForeignSignature::new(params, result))
    }

    /// The register word a written type crosses as, reporting the refusal when
    /// it has none.
    ///
    /// The kernel reads its arguments out of registers, so what crosses is
    /// exactly what fits in one: an integer of any width, a pointer, and — as an
    /// argument only — a `CString`, whose transient NUL-terminated copy is what
    /// the pointer register receives. A float has no register in the sequence at
    /// all, and an aggregate has no single word, so both are refused rather than
    /// silently spilled somewhere the kernel will not look.
    fn syscall_word_of(&mut self, type_ref: TypeRefId, position: Position) -> Option<ForeignType> {
        let span = self.tree.type_ref(type_ref).span();
        let ty = self.resolve_foreign_type(type_ref);
        match ty {
            // Whatever produced an `Error` type already spoke.
            Type::Error => None,
            Type::Int(spelling) => Some(int_word(spelling)),
            Type::RawPtr | Type::ForeignPtr(_) => Some(ForeignType::RawPtr),
            Type::CString if position == Position::Argument => Some(ForeignType::CString),
            Type::CString => {
                self.emit(
                    span,
                    "KSEM283",
                    "a system call cannot return a `CString`: the kernel answers a machine word, \
                     and nothing in that word says it addresses text or how much of it",
                );
                None
            }
            Type::Void => {
                self.emit(
                    span,
                    "KSEM283",
                    "`Void` is not a system-call argument: it names no value to put in a register. \
                     A call that answers nothing is written with no `->` at all.",
                );
                None
            }
            Type::String => {
                self.emit(
                    span,
                    "KSEM283",
                    "a `String` is not a machine word: use `CString` for a NUL-terminated argument \
                     the kernel reads, or `RawPtr` and a length for raw bytes",
                );
                None
            }
            _ => {
                self.emit(
                    span,
                    "KSEM283",
                    format!(
                        "`{}` cannot be a system-call {}: the kernel reads its arguments out of \
                         registers, so what crosses is an integer, a pointer, or a `CString`",
                        self.type_name(ty),
                        match position {
                            Position::Argument => "argument",
                            Position::Result => "result",
                        }
                    ),
                );
                None
            }
        }
    }
}

/// The fixed-width register word an integer spelling occupies.
///
/// The same mapping the C seam uses, and deliberately so: a declaration written
/// `I32` means a 32-bit value in both places, and two answers to that would mean
/// the same Kira source produced different bits depending on which annotation it
/// carried.
fn int_word(spelling: IntSpelling) -> ForeignType {
    match spelling {
        IntSpelling::Plain => ForeignType::I64,
        IntSpelling::I8 => ForeignType::I8,
        IntSpelling::I16 => ForeignType::I16,
        IntSpelling::I32 => ForeignType::I32,
        IntSpelling::U8 => ForeignType::U8,
        IntSpelling::U16 => ForeignType::U16,
        IntSpelling::U32 => ForeignType::U32,
        IntSpelling::U64 => ForeignType::U64,
    }
}
