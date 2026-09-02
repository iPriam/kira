//! Every rule a `@FFI.Syscall` declaration is held to.
//!
//! Each refusal is checked by code and, where the program is otherwise clean,
//! proved to be the *only* diagnostic reported — so a rule is never mistaken for
//! a cascade. The target-dependent rules are checked against a named machine
//! rather than the host, because the whole point of them is that the answer
//! differs per target and the machine running the tests is only one of them.

use super::*;
use crate::BuildMachine;

/// An `aarch64` Linux target, which every accepted declaration below is checked
/// against so the result does not depend on the machine running the tests.
fn linux_aarch64() -> BuildMachine {
    BuildMachine::new("linux", "aarch64")
}

/// The diagnostics of a single-file program analyzed for `machine`.
fn machine_diagnostics(text: &str, machine: BuildMachine) -> Vec<Diagnostic> {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(
        &db,
        text.to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        BuildKind::Application,
        PrecompiledShaders::default(),
        machine,
        // Not a lint run.
        false,
    );
    analyzed::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulator| accumulator.0.clone())
        .collect()
}

/// The diagnostic codes a program produced for `machine`, in order.
fn codes(text: &str, machine: BuildMachine) -> Vec<String> {
    machine_diagnostics(text, machine)
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect()
}

/// The single diagnostic message a program produced, for the cases where the
/// wording is the whole answer an author gets.
fn message(text: &str, machine: BuildMachine) -> String {
    let diagnostics = machine_diagnostics(text, machine);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    diagnostics[0].message.clone()
}

/// A `write` declaration plus a `@Main` that calls it — the shape every accepted
/// case below is a variation on.
const WRITE: &str = "@FFI.Syscall { name: write }\n\
     function sysWrite(fd: Int, buffer: CString, count: U64) -> Int\n\
     @Main function main() { let n = sysWrite(1, \"hi\", U64(2)) return }";

/// The same declaration with nothing calling it, for the target refusals.
///
/// A refused declaration is never recorded, so a call to one is also an undefined
/// function — a true second diagnostic, and one that would hide whether the
/// target rule reported once or twice.
const WRITE_UNCALLED: &str = "@FFI.Syscall { name: write }\n\
     function sysWrite(fd: Int, buffer: CString, count: U64) -> Int\n\
     @Main function main() { return }";

#[test]
fn a_syscall_declaration_and_a_call_to_it_are_accepted() {
    assert!(codes(WRITE, linux_aarch64()).is_empty(), "{WRITE}");
}

/// The recorded row names the kernel rather than a library, and carries the
/// system-call ABI — which is what every step downstream reads to decide that
/// nothing needs binding.
#[test]
fn an_accepted_syscall_is_recorded_as_entering_the_kernel() {
    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(
        &db,
        WRITE.to_owned(),
        "test.kira".to_owned(),
        Vec::new(),
        BuildKind::Application,
        PrecompiledShaders::default(),
        linux_aarch64(),
        false,
    );
    let program = analyzed(&db, source);
    assert_eq!(program.foreign.len(), 1);
    let import = &program.foreign[0];
    assert_eq!(import.kira_name, "sysWrite");
    assert_eq!(import.symbol, "write");
    assert_eq!(import.library, "");
    assert_eq!(import.abi, kira_runtime_abi::ForeignAbi::LinuxSyscall);
    assert!(!import.abi.binds_a_library_symbol());
}

/// Every argument shape the kernel entry carries, at both ends of the arity
/// range: none at all, and the full six.
#[test]
fn a_call_with_no_arguments_and_one_with_six_are_both_accepted() {
    let text = "@FFI.Syscall { name: sync }\n\
         function sysSync()\n\
         @FFI.Syscall { name: mount }\n\
         function sysMount(source: CString, target: CString, kind: CString, flags: U64, \
         data: RawPtr, extra: Int) -> Int\n\
         @Main function main() { sysSync() return }";
    assert!(codes(text, linux_aarch64()).is_empty(), "{text}");
}

/// The refusal the whole design turns on: the program names the call, so a name
/// the compiler has no number for is refused here rather than emitted as a
/// number that means something else in the kernel's table.
#[test]
fn a_syscall_name_the_compiler_has_no_number_for_is_refused() {
    let text = "@FFI.Syscall { name: openat }\n\
         function sysOpenat(dirfd: Int, path: CString, flags: Int) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM279"]);
    let reported = message(text, linux_aarch64());
    assert!(
        reported.contains("`openat` is not a system call"),
        "{reported}"
    );
    // The message lists what *is* available, so the fix does not need a source
    // dive to find.
    assert!(reported.contains("write"), "{reported}");
    assert!(reported.contains("exit_group"), "{reported}");
}

/// A Kira author's habitual spelling is not the kernel's, and it is refused
/// rather than mapped: the name in source is the name in `man 2`, which is what
/// makes a declaration something a reader can look up.
#[test]
fn the_kira_spelling_of_a_kernel_name_is_not_accepted_for_it() {
    let text = "@FFI.Syscall { name: exitGroup }\n\
         function sysExit(status: Int)\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM279"]);
}

/// A target that cannot reach the Linux kernel is refused by name, at the
/// declaration, with the reason macOS is excluded rather than merely absent.
#[test]
fn a_target_that_is_not_linux_is_refused_by_name() {
    let machine = BuildMachine::new("macos", "aarch64");
    assert_eq!(codes(WRITE_UNCALLED, machine.clone()), vec!["KSEM280"]);
    let reported = message(WRITE_UNCALLED, machine);
    assert!(
        reported.contains("this build targets `macos`"),
        "{reported}"
    );
    assert!(reported.contains("libSystem"), "{reported}");

    let windows = BuildMachine::new("windows", "x86_64");
    assert_eq!(codes(WRITE_UNCALLED, windows), vec!["KSEM280"]);
}

/// Linux on a processor Kira emits no kernel entry sequence for is a separate
/// refusal from the wrong operating system, because it has a separate fix.
#[test]
fn a_linux_target_on_an_architecture_with_no_lowering_is_refused_by_name() {
    let machine = BuildMachine::new("linux", "riscv64");
    assert_eq!(codes(WRITE_UNCALLED, machine.clone()), vec!["KSEM281"]);
    let reported = message(WRITE_UNCALLED, machine);
    assert!(reported.contains("architecture `riscv64`"), "{reported}");
    assert!(reported.contains("aarch64"), "{reported}");
    assert!(reported.contains("x86_64"), "{reported}");
}

/// Both halves of the target are wrong, and both are said — an author fixing one
/// and rebuilding to discover the other is the failure the unconditional checks
/// avoid.
#[test]
fn a_wrong_operating_system_and_a_wrong_architecture_are_both_reported() {
    let machine = BuildMachine::new("windows", "x86");
    assert_eq!(codes(WRITE_UNCALLED, machine), vec!["KSEM280", "KSEM281"]);
}

/// x86-64 is the other architecture with a lowering, and it is accepted on its
/// own terms rather than by being the host.
#[test]
fn the_other_supported_architecture_is_accepted_too() {
    assert!(
        codes(WRITE, BuildMachine::new("linux", "x86_64")).is_empty(),
        "{WRITE}"
    );
}

#[test]
fn a_seventh_argument_has_no_register_to_go_in_and_is_refused() {
    let text = "@FFI.Syscall { name: mount }\n\
         function sysMount(a: Int, b: Int, c: Int, d: Int, e: Int, f: Int, g: Int) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM282"]);
    let reported = message(text, linux_aarch64());
    assert!(reported.contains("at most 6 arguments"), "{reported}");
}

/// A float has no register in the kernel entry sequence at all, and an aggregate
/// has no single word — so neither is spilled somewhere the kernel will not look.
#[test]
fn a_type_that_is_not_a_machine_word_is_refused() {
    let float = "@FFI.Syscall { name: write }\n\
         function sysWrite(fd: Float) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(float, linux_aarch64()), vec!["KSEM283"]);

    let boolean = "@FFI.Syscall { name: write }\n\
         function sysWrite(fd: Bool) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(boolean, linux_aarch64()), vec!["KSEM283"]);

    let aggregate = "struct Pair { var a: Int\n var b: Int }\n\
         @FFI.Syscall { name: write }\n\
         function sysWrite(pair: Pair) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(aggregate, linux_aarch64()), vec!["KSEM283"]);
}

/// A `String` carries its length and its bytes are not NUL-terminated, so it is
/// not what a register holds; the message names the two things that are.
#[test]
fn a_string_argument_is_refused_and_names_what_to_write_instead() {
    let text = "@FFI.Syscall { name: write }\n\
         function sysWrite(fd: Int, buffer: String, count: U64) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM283"]);
    let reported = message(text, linux_aarch64());
    assert!(reported.contains("`CString`"), "{reported}");
    assert!(reported.contains("`RawPtr`"), "{reported}");
}

/// A `CString` crosses inbound only: the kernel answers a machine word, and
/// nothing in that word says it addresses text.
#[test]
fn a_cstring_result_is_refused_although_a_cstring_argument_is_not() {
    let text = "@FFI.Syscall { name: read }\n\
         function sysRead(fd: Int, buffer: RawPtr, count: U64) -> CString\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM283"]);
}

/// `exit_group` ends the process, so a declaration naming a result describes a
/// value nothing can produce.
#[test]
fn a_result_on_a_call_that_never_returns_is_refused() {
    let text = "@FFI.Syscall { name: exit_group }\n\
         function sysExit(status: Int) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM284"]);
    let reported = message(text, linux_aarch64());
    assert!(reported.contains("does not return"), "{reported}");
}

#[test]
fn the_same_call_declared_without_a_result_is_accepted() {
    let text = "@FFI.Syscall { name: exit_group }\n\
         function sysExit(status: Int)\n\
         @Main function main() { sysExit(0) return }";
    assert!(codes(text, linux_aarch64()).is_empty(), "{text}");
}

/// The block carries one field. `library`, `symbol`, and `abi` are refused rather
/// than ignored: an author who wrote `library: libc` believes the call goes
/// through libc, and it does not.
#[test]
fn a_field_a_system_call_has_no_use_for_is_refused_rather_than_ignored() {
    let text = "@FFI.Syscall { name: write, library: libc }\n\
         function sysWrite(fd: Int) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM178"]);
    let reported = message(text, linux_aarch64());
    assert!(reported.contains("the only field is `name`"), "{reported}");
}

#[test]
fn a_missing_name_field_is_refused() {
    let text = "@FFI.Syscall { }\n\
         function sysWrite(fd: Int) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM180"]);
}

#[test]
fn a_name_field_written_twice_is_refused() {
    let text = "@FFI.Syscall { name: write, name: read }\n\
         function sysWrite(fd: Int) -> Int\n\
         @Main function main() { return }";
    assert_eq!(codes(text, linux_aarch64()), vec!["KSEM179"]);
}

/// The shared annotation refusal, with the wording that names the form actually
/// written: told about `@FFI.Extern`, a reader looks for a declaration they never
/// wrote.
#[test]
fn a_syscall_that_is_also_an_entrypoint_or_an_engine_choice_is_refused_by_its_own_name() {
    let main = "@FFI.Syscall { name: sync }\n\
         @Main function sysSync()\n";
    let reported = machine_diagnostics(main, linux_aarch64());
    assert!(
        reported.iter().any(|d| d.code_text() == Some("KSEM177")
            && d.message.contains("an `@FFI.Syscall` function")),
        "{reported:?}"
    );

    let native = "@FFI.Syscall { name: sync }\n\
         @Native function sysSync()\n\
         @Main function main() { return }";
    assert_eq!(codes(native, linux_aarch64()), vec!["KSEM177"]);
    assert!(
        message(native, linux_aarch64()).contains("an `@FFI.Syscall` function"),
        "{native}"
    );
}

/// A syscall shares the call namespace with every other callable, so its name
/// cannot repeat one — and the refusal names the form that introduced it.
#[test]
fn a_syscall_name_cannot_repeat_a_function_name() {
    let text = "function sysSync() { return }\n\
         @FFI.Syscall { name: sync }\n\
         function sysSync()\n\
         @Main function main() { return }";
    let reported = machine_diagnostics(text, linux_aarch64());
    assert!(
        reported
            .iter()
            .any(|d| d.code_text() == Some("KSEM184")
                && d.message.contains("an `@FFI.Syscall` name")),
        "{reported:?}"
    );
}

/// A refused declaration is never recorded, so a call to it is an undefined
/// function rather than a call against a contract nothing checked.
#[test]
fn a_refused_declaration_leaves_no_callable_behind() {
    let text = "@FFI.Syscall { name: openat }\n\
         function sysOpenat(path: CString) -> Int\n\
         @Main function main() { let n = sysOpenat(\"/x\") return }";
    let reported = codes(text, linux_aarch64());
    assert!(reported.contains(&"KSEM279".to_owned()), "{reported:?}");
    assert!(reported.contains(&"KSEM061".to_owned()), "{reported:?}");
}

/// An `@FFI.Extern` beside a `@FFI.Syscall` is unaffected: the two forms share a
/// call namespace and a signature check, and neither borrows the other's rules.
#[test]
fn an_extern_and_a_syscall_coexist_in_one_program() {
    let text = "@FFI.Extern { library: ffimath, symbol: ffi_add, abi: c }\n\
         function add(a: I32, b: I32) -> I32\n\
         @FFI.Syscall { name: write }\n\
         function sysWrite(fd: Int, buffer: CString, count: U64) -> Int\n\
         @Main function main() { let n = sysWrite(1, \"hi\", U64(2)) let s = add(1, 2) return }";
    assert!(codes(text, linux_aarch64()).is_empty(), "{text}");
}
