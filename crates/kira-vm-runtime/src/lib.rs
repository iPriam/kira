//! The Kira VM: bytecode interpreter and runtime.
//!
//! Layer 4 of the Kira package graph.
//!
//! - **Portable core.** No filesystem, process, thread, or dynamic-loading
//!   calls; the crate compiles for `wasm32-unknown-unknown`. It consumes a
//!   [`Module`](kira_bytecode::Module) and talks to the world only through the
//!   [`HostCapabilities`](kira_runtime_abi::HostCapabilities) trait supplied by
//!   the embedder.
//! - **Affine strings.** Strings live on a heap with drop accounting; a clean
//!   run reclaims every allocation ([`HeapStats::current`] is 0 at exit).
//! - **Match-in-loop.** Dispatch is a single `match` over decoded instructions.

pub mod error;
pub mod interp;
pub mod value;

pub use error::VmError;
pub use interp::{Program, RunOutcome, execute};
pub use value::{Heap, HeapStats, StrId, Value};

#[cfg(test)]
mod tests {
    use super::*;
    use kira_bytecode::module::{FuncProto, Module};
    use kira_bytecode::op::Instruction as I;
    use kira_runtime_abi::CapturingHost;

    fn run(module: &Module) -> (Vec<String>, RunOutcome) {
        let mut host = CapturingHost::new();
        let outcome = execute(module, &mut host).expect("clean run");
        (host.lines().to_vec(), outcome)
    }

    fn func(name: &str, params: u16, locals: u16, code: Vec<I>) -> FuncProto {
        FuncProto {
            name: name.to_owned(),
            param_count: params,
            local_count: locals,
            execution: kira_runtime_abi::Execution::Runtime,
            code,
        }
    }

    /// A host with a native half: answers `shout(n, s)` with `s` repeated `n`
    /// times, and records what it was handed.
    #[derive(Default)]
    struct NativeHost {
        lines: Vec<String>,
        seen: Vec<String>,
    }

    impl kira_runtime_abi::HostCapabilities for NativeHost {
        fn write_line(&mut self, text: &str) {
            self.lines.push(text.to_owned());
        }

        fn call_native(
            &mut self,
            function_id: u32,
            args: &[kira_runtime_abi::NativeArg<'_>],
        ) -> Result<kira_runtime_abi::NativeResult, kira_runtime_abi::NativeCallError> {
            use kira_runtime_abi::{NativeArg, NativeResult};
            self.seen.push(format!("{function_id}{args:?}"));
            match args {
                [NativeArg::Int(count), NativeArg::Str(text)] => {
                    Ok(NativeResult::Str(text.repeat(*count as usize)))
                }
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
        let main = func(
            "main",
            0,
            0,
            vec![
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

    /// A host driving the VM from an artifact that disagrees with this module
    /// is a typed rejection, never a panic or a misread frame.
    #[test]
    fn a_host_call_with_the_wrong_arity_is_rejected() {
        let module = Module {
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
            functions: vec![main, native],
            main: Some(0),
            strings: vec![],
        };
        let mut host = CapturingHost::new();
        let error = execute(&module, &mut host).unwrap_err();
        assert!(matches!(error, VmError::Module(_)), "{error:?}");
    }

    #[test]
    fn arithmetic_and_print() {
        // print(17 % 5); print(-4); print(20 / 4 / 2)
        let main = func(
            "main",
            0,
            0,
            vec![
                I::ConstInt(17),
                I::ConstInt(5),
                I::RemInt,
                I::Print,
                I::Pop,
                I::ConstInt(4),
                I::NegInt,
                I::Print,
                I::Pop,
                I::ConstInt(20),
                I::ConstInt(4),
                I::DivInt,
                I::ConstInt(2),
                I::DivInt,
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            functions: vec![main],
            main: Some(0),
            strings: vec![],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["2", "-4", "2"]);
        assert_eq!(outcome.heap.current, 0);
    }

    #[test]
    fn while_loop_accumulates() {
        // var i = 0; var sum = 0; while i < 5 { sum = sum + i; i = i + 1 } print(sum)
        // locals: 0 = i, 1 = sum
        let main = func(
            "main",
            0,
            2,
            vec![
                I::ConstInt(0),
                I::StoreLocal(0),
                I::ConstInt(0),
                I::StoreLocal(1),
                // loop start (index 4)
                I::LoadLocal(0),
                I::ConstInt(5),
                I::LtInt,
                I::JumpIfFalse(17),
                I::LoadLocal(1),
                I::LoadLocal(0),
                I::AddInt,
                I::StoreLocal(1),
                I::LoadLocal(0),
                I::ConstInt(1),
                I::AddInt,
                I::StoreLocal(0),
                I::Jump(4),
                I::LoadLocal(1), // index 17: loop exit
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            functions: vec![main],
            main: Some(0),
            strings: vec![],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["10"]);
        assert_eq!(outcome.heap.current, 0);
    }

    /// Builds a two-function module computing and printing `fib(n)`.
    fn fib_module(n: i64) -> Module {
        // main (index 0): print(fib(n))
        let main = func(
            "main",
            0,
            0,
            vec![I::ConstInt(n), I::Call(1), I::Print, I::Pop, I::ReturnVoid],
        );
        // fib (index 1): if n < 2 { return n } return fib(n-1) + fib(n-2)
        let fib = func(
            "fib",
            1,
            1,
            vec![
                I::LoadLocal(0),
                I::ConstInt(2),
                I::LtInt,
                I::JumpIfFalse(6),
                I::LoadLocal(0),
                I::Return,
                I::LoadLocal(0), // index 6
                I::ConstInt(1),
                I::SubInt,
                I::Call(1),
                I::LoadLocal(0),
                I::ConstInt(2),
                I::SubInt,
                I::Call(1),
                I::AddInt,
                I::Return,
            ],
        );
        Module {
            functions: vec![main, fib],
            main: Some(0),
            strings: vec![],
        }
    }

    #[test]
    fn recursion_computes_fib_20() {
        let (lines, outcome) = run(&fib_module(20));
        assert_eq!(lines, ["6765"]);
        assert_eq!(outcome.heap.current, 0);
    }

    #[test]
    fn strings_concatenate_and_fully_reclaim() {
        // let s = "foo"; let t = "bar"; print(s + t)  — locals 0 = s, 1 = t
        let main = func(
            "main",
            0,
            2,
            vec![
                I::ConstStr(0),
                I::StoreLocal(0),
                I::ConstStr(1),
                I::StoreLocal(1),
                I::LoadLocal(0),
                I::LoadLocal(1),
                I::ConcatStr,
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            functions: vec![main],
            main: Some(0),
            strings: vec!["foo".to_owned(), "bar".to_owned()],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["foobar"]);
        // Every string (two literals, two clones on read, one concat result)
        // is reclaimed by exit.
        assert_eq!(outcome.heap.current, 0);
        assert!(outcome.heap.allocated >= 3);
        assert_eq!(outcome.heap.allocated, outcome.heap.freed);
    }

    #[test]
    fn division_by_zero_traps() {
        let main = func(
            "main",
            0,
            0,
            vec![
                I::ConstInt(10),
                I::ConstInt(0),
                I::DivInt,
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            functions: vec![main],
            main: Some(0),
            strings: vec![],
        };
        let mut host = CapturingHost::new();
        let error = execute(&module, &mut host).unwrap_err();
        assert_eq!(error, VmError::DivideByZero);
    }

    #[test]
    fn malformed_modules_are_rejected_typed_not_panicking() {
        // Each of these once panicked the interpreter; validation now rejects
        // them with a typed VmError before execution starts.
        let malformed = vec![
            // Empty code.
            Module {
                functions: vec![func("main", 0, 0, vec![])],
                main: Some(0),
                strings: vec![],
            },
            // Falls off the end (not return-terminated).
            Module {
                functions: vec![func("main", 0, 0, vec![I::ConstInt(1)])],
                main: Some(0),
                strings: vec![],
            },
            // ConstStr into an empty pool.
            Module {
                functions: vec![func("main", 0, 0, vec![I::ConstStr(3), I::ReturnVoid])],
                main: Some(0),
                strings: vec![],
            },
            // LoadLocal beyond local_count.
            Module {
                functions: vec![func("main", 0, 1, vec![I::LoadLocal(9), I::ReturnVoid])],
                main: Some(0),
                strings: vec![],
            },
            // More parameters than locals (fill_params would underflow slots).
            Module {
                functions: vec![func("main", 2, 0, vec![I::ReturnVoid])],
                main: Some(0),
                strings: vec![],
            },
            // Entrypoint out of range.
            Module {
                functions: vec![func("main", 0, 0, vec![I::ReturnVoid])],
                main: Some(7),
                strings: vec![],
            },
        ];
        for module in malformed {
            let mut host = CapturingHost::new();
            let error = execute(&module, &mut host).unwrap_err();
            assert!(
                matches!(error, VmError::Module(_)),
                "expected typed module rejection, got {error:?}"
            );
        }
    }

    #[test]
    fn string_local_reassignment_frees_the_old_value() {
        // var s = "a"; s = "bb"; print(s)  — local 0 = s
        let main = func(
            "main",
            0,
            1,
            vec![
                I::ConstStr(0),
                I::StoreLocal(0),
                I::ConstStr(1),
                I::StoreLocal(0),
                I::LoadLocal(0),
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            functions: vec![main],
            main: Some(0),
            strings: vec!["a".to_owned(), "bb".to_owned()],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["bb"]);
        assert_eq!(outcome.heap.current, 0);
    }
}
