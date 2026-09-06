//! The seam a hybrid program runs on, in both directions: the VM reaching a
//! native callee through the host, and a host calling back into bytecode.

use super::*;
use crate::vm_test_support::func;
use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

/// A host with a native half: answers `shout(n, s)` with `s` repeated `n`
/// times, and records what it was handed.
#[derive(Default)]
struct NativeHost {
    lines: Vec<String>,
    seen: Vec<String>,
    fail: bool,
}

impl kira_runtime_abi::HostCapabilities for NativeHost {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[kira_runtime_abi::NativeArg<'_>],
    ) -> Result<kira_runtime_abi::NativeReturn, kira_runtime_abi::NativeCallError> {
        use kira_runtime_abi::{NativeArg, NativeResult, NativeReturn};
        self.seen.push(format!("{function_id}{args:?}"));
        if self.fail {
            return Err(kira_runtime_abi::NativeCallError::UnboundFunction(
                function_id,
            ));
        }
        match args {
            [NativeArg::Int(count), NativeArg::Str(text)] => Ok(NativeReturn::plain(
                NativeResult::Str(text.repeat(*count as usize)),
            )),
            _ => Err(kira_runtime_abi::NativeCallError::UnboundFunction(
                function_id,
            )),
        }
    }
}

/// The seam a hybrid program runs on: the VM reaches a native callee, hands
/// the host safe Rust values, and pushes back what it returns — without the
/// VM itself touching any FFI.
#[test]
fn call_native_marshals_through_the_host_and_back() {
    // main: print(shout(2, "hi"))  — shout's body lives in the native half.
    // The leading marker stays below the call's arguments. It proves the
    // seam consumes exactly its declared arguments rather than the whole
    // operand-stack suffix.
    let main = func(
        "main",
        0,
        0,
        vec![
            I::ConstInt(99),
            I::ConstInt(2),
            I::ConstStr(0),
            I::CallNative(1),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let mut shout = func("shout", 2, 2, vec![]);
    shout.execution = kira_runtime_abi::Execution::Native;
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main, shout],
        main: Some(0),
        strings: vec!["hi".to_owned()],
    };

    let mut host = NativeHost::default();
    let outcome = execute(&module, &mut host).expect("clean run");
    assert_eq!(host.lines, ["hihi"]);
    // The string crossed as a borrow of the VM's own heap, not a copy.
    assert!(host.seen[0].contains("Str(\"hi\")"), "{:?}", host.seen);
    // Every argument the VM lent out is still reclaimed by exit.
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn a_native_host_error_reclaims_arguments_on_a_persistent_heap() {
    let main = func(
        "main",
        0,
        0,
        vec![I::ConstStr(0), I::CallNative(1), I::Return],
    );
    let mut native = func("shout", 1, 1, vec![]);
    native.execution = kira_runtime_abi::Execution::Native;
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main, native],
        main: Some(0),
        strings: vec!["transient".to_owned()],
    };
    let mut instance = Instance::load(module).expect("the native module validates");
    let mut host = NativeHost {
        fail: true,
        ..NativeHost::default()
    };
    assert_eq!(
        instance.call(&mut host, 0, &[]),
        Err(VmError::NativeCall(
            kira_runtime_abi::NativeCallError::UnboundFunction(1)
        ))
    );
    assert_eq!(instance.stats().current, 0);
}

/// The other direction of the same seam: a host calls one function by id.
/// This is what the native half of a hybrid program reaches back through.
#[test]
fn a_host_calls_one_function_by_id_and_gets_an_owned_result() {
    use kira_runtime_abi::{NativeArg, NativeResult};

    // greet(name) { return "hi, " + name }
    let greet = func(
        "greet",
        1,
        1,
        vec![I::ConstStr(0), I::LoadLocal(0), I::ConcatStr, I::Return],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![func("main", 0, 0, vec![I::ReturnVoid]), greet],
        main: Some(0),
        strings: vec!["hi, ".to_owned()],
    };

    let program = Program::load(module).expect("a valid module");
    let mut host = CapturingHost::new();
    // The argument is lent, not given: `name` is still the caller's after.
    let name = String::from("kira");
    let result = program
        .call(&mut host, 1, &[NativeArg::Str(&name)])
        .expect("clean call");
    assert_eq!(result, NativeResult::Str("hi, kira".to_owned()));
    assert_eq!(name, "kira");

    // Calls are independent: nothing from the first survives into the next.
    let again = program
        .call(&mut host, 1, &[NativeArg::Str("again")])
        .expect("clean call");
    assert_eq!(again, NativeResult::Str("hi, again".to_owned()));
}

/// A handle argument is refused by name rather than resolved into whatever
/// object its word happens to land on.
///
/// Each `call` runs on a heap it drops at the end, so a handle — which names
/// an object across calls — has nothing here to denote. The persistent
/// instance is what gives one a home; until then this is the honest answer.
#[test]
fn a_handle_argument_is_refused_because_this_call_has_no_lasting_heap() {
    use kira_runtime_abi::NativeArg;

    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![
            func("main", 0, 0, vec![I::ReturnVoid]),
            func("takes_one", 1, 1, vec![I::ReturnVoid]),
        ],
        main: Some(0),
        strings: Vec::new(),
    };
    let program = Program::load(module).expect("a valid module");
    let mut host = CapturingHost::new();
    let error = program
        .call(&mut host, 1, &[NativeArg::Handle(7)])
        .expect_err("a handle has no VM representation yet");
    assert_eq!(error, VmError::HandleAtSeam { function: 1 });
    assert!(
        error.to_string().contains("handle"),
        "the refusal names what it refused: {error}"
    );
}

/// A host that asks the VM to enter a `@Native` function is refused by name.
///
/// A native function carries a signature and no body, so entering one used
/// to read past the end of empty code. Bytecode has never been allowed to
/// `Call` one — validation rejects that — and an embedder is now held to the
/// same rule, through the same check both entry points share.
#[test]
fn a_host_cannot_enter_a_native_function() {
    let mut native = func("hot", 1, 1, vec![]);
    native.execution = kira_runtime_abi::Execution::Native;
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![func("main", 0, 0, vec![I::ReturnVoid]), native],
        main: Some(0),
        strings: Vec::new(),
    };
    let program = Program::load(module).expect("a valid module");
    let mut host = CapturingHost::new();
    assert_eq!(
        program.call(&mut host, 1, &[kira_runtime_abi::NativeArg::Int(1)]),
        Err(VmError::NativeEntry { function: 1 })
    );
}

/// A host driving the VM from an artifact that disagrees with this module
/// is a typed rejection, never a panic or a misread frame.
#[test]
fn a_host_call_with_the_wrong_arity_is_rejected() {
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![
            func("main", 0, 0, vec![I::ReturnVoid]),
            func("takes_one", 1, 1, vec![I::ReturnVoid]),
        ],
        main: Some(0),
        strings: Vec::new(),
    };
    let program = Program::load(module).expect("a valid module");
    let mut host = CapturingHost::new();
    assert_eq!(
        program.call(&mut host, 1, &[]),
        Err(VmError::ArityMismatch {
            function: 1,
            expected: 1,
            got: 0,
        })
    );
    assert_eq!(
        program.call(&mut host, 9, &[]),
        Err(VmError::UnknownFunction(9))
    );
}

/// A host with no native half must refuse rather than invent a value: a
/// program reaching this was built for the wrong backend.
#[test]
fn a_vm_only_host_refuses_a_native_call() {
    let main = func("main", 0, 0, vec![I::CallNative(1), I::Pop, I::ReturnVoid]);
    let mut native = func("gone", 0, 0, vec![]);
    native.execution = kira_runtime_abi::Execution::Native;
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main, native],
        main: Some(0),
        strings: vec![],
    };
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).unwrap_err();
    assert_eq!(
        error,
        VmError::NativeCall(kira_runtime_abi::NativeCallError::NoNativeHalf)
    );
}

/// A native function's body lives elsewhere, so a bytecode `Call` must not
/// target one — it would push a frame over an empty body.
#[test]
fn a_bytecode_call_to_a_native_function_is_rejected() {
    let main = func("main", 0, 0, vec![I::Call(1), I::Pop, I::ReturnVoid]);
    let mut native = func("hot", 0, 0, vec![]);
    native.execution = kira_runtime_abi::Execution::Native;
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main, native],
        main: Some(0),
        strings: vec![],
    };
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).unwrap_err();
    assert!(matches!(error, VmError::Module(_)), "{error:?}");
}
