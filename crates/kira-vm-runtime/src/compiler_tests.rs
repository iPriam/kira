//! VM execution tests for the compiler instruction.
//!
//! The VM sits below the compiler and holds none of its own, so what these pin
//! down is the *refusal*: a host that was never handed a compiler says so by
//! name, and never answers with an empty diagnostic list — which a program
//! would read as "it compiled".

use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::{CompilerOp, Instruction as I};
use kira_runtime_abi::{
    CapturingHost, CheckDiagnostic, CheckRequest, CheckSeverity, CompilerError, Execution,
    HostCapabilities,
};

use crate::{VmError, execute};

fn module(code: Vec<I>, strings: Vec<String>) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 1,
            execution: Execution::Runtime,
            code,
            releases: kira_bytecode::FrameRelease::EveryLocal,
        }],
        main: Some(0),
        strings,
    }
}

/// Builds a program that checks a one-package request and prints what it got.
fn checking_program(request: &CheckRequest) -> Module {
    let strings = request.encode();
    let mut code: Vec<I> = (0..strings.len())
        .map(|index| I::ConstStr(index as u64))
        .collect();
    code.push(I::NewArray(strings.len() as u64));
    code.push(I::Compiler(CompilerOp::CheckPackages));
    code.push(I::ArrayLen);
    code.push(I::Print);
    code.push(I::Pop);
    code.push(I::ReturnVoid);
    module(code, strings)
}

fn one_package_request() -> CheckRequest {
    CheckRequest {
        root: "App".to_owned(),
        packages: vec![kira_runtime_abi::CheckPackage {
            manifest: "Package App {\n    let kind = .App\n}\n".to_owned(),
            files: vec![kira_runtime_abi::CheckFile {
                path: "app/main.kira".to_owned(),
                text: "@Main function main() { return }".to_owned(),
            }],
        }],
    }
}

/// A host that was never handed a compiler refuses, by name.
///
/// This is the whole guarantee behind "the portable core reaches nothing on its
/// own": the trap says a compiler is missing, so a program that needed one
/// fails loudly rather than being told its package is clean.
#[test]
fn a_host_with_no_compiler_refuses_by_name() {
    let module = checking_program(&one_package_request());
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).expect_err("no compiler was installed");
    assert_eq!(error, VmError::Compiler(CompilerError::NoCompilerHost));
    assert_eq!(
        error.to_string(),
        "compiler operation failed: this host does not provide a compiler"
    );
    assert!(host.lines().is_empty(), "nothing was printed");
}

/// A host that *does* answer is asked exactly what the program built, and its
/// answer comes back as the array the program reads.
#[test]
fn a_host_with_a_compiler_is_asked_what_the_program_built() {
    /// Records the request it was asked and answers with one diagnostic.
    struct RecordingHost {
        asked: Option<CheckRequest>,
    }

    impl HostCapabilities for RecordingHost {
        fn write_line(&mut self, _text: &str) {}

        fn compiler(
            &mut self,
            request: &CheckRequest,
        ) -> Result<Vec<CheckDiagnostic>, CompilerError> {
            self.asked = Some(request.clone());
            Ok(vec![CheckDiagnostic {
                code: "KSEM060".to_owned(),
                severity: CheckSeverity::Error,
                file: "app/main.kira".to_owned(),
                title: "undefined name".to_owned(),
                message: "undefined name `missing`".to_owned(),
            }])
        }
    }

    let request = one_package_request();
    let module = checking_program(&request);
    let mut host = RecordingHost { asked: None };
    execute(&module, &mut host).expect("the host answered");
    assert_eq!(host.asked.as_ref(), Some(&request));
}

/// A request the VM cannot read is a trap, not a silent empty answer.
#[test]
fn an_unreadable_request_traps_rather_than_answering() {
    let module = module(
        vec![
            I::NewArray(0),
            I::Compiler(CompilerOp::CheckPackages),
            I::Pop,
            I::ReturnVoid,
        ],
        Vec::new(),
    );
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).expect_err("an empty request names no root");
    assert!(
        matches!(error, VmError::Compiler(CompilerError::Wire(_))),
        "{error:?}"
    );
}
