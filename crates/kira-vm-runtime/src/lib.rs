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
//!
//! # Two ways in, and why there are two
//!
//! [`execute`] and [`Program`] run a *program*: each call gets its own heap, and
//! that heap is gone when the call ends. [`Instance`] runs a *library*: one heap
//! for the instance's whole life, with a root table naming the objects the
//! consumer still holds. A library needs the second because Kira has no globals,
//! so an object returned by one call has nowhere else to live until the next
//! one. See [`instance`] for what "balanced" means once a heap outlives a call.

pub mod debug;
pub mod error;
pub mod instance;
pub mod interp;
pub mod profile;
pub mod value;

pub use debug::{
    KIRA_VM_DEBUG_ACTIVE, KiraVmDebugFrame, KiraVmDebugState, KiraVmDebugValue, VmLldbBreakpoint,
    VmLldbObserver, format_debug_state, kira_vm_debug_dump, kira_vm_debug_probe,
};
pub use error::{NativeStateOperation, VmError};
pub use instance::{Instance, RootId};
pub use interp::{Program, RunOutcome, execute, execute_with_debug};
pub use value::{Heap, HeapStats, StrId, Value};

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod compiler_tests;

#[cfg(test)]
#[path = "capacity_tests.rs"]
mod capacity_tests;

#[cfg(test)]
#[path = "foreign_tests.rs"]
mod foreign_tests;

#[cfg(test)]
#[path = "frame_cache_tests.rs"]
mod frame_cache_tests;

#[cfg(test)]
#[path = "native_state_tests.rs"]
mod native_state_tests;

#[cfg(test)]
#[path = "release_tests.rs"]
mod release_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::{VmDebugAction, VmDebugEvent, VmDebugObserver};
    use kira_bytecode::module::{FuncProto, Module};
    use kira_bytecode::op::Instruction as I;
    use kira_runtime_abi::CapturingHost;

    fn run(module: &Module) -> (Vec<String>, RunOutcome) {
        let mut host = CapturingHost::new();
        let outcome = execute(module, &mut host).expect("clean run");
        (host.lines().to_vec(), outcome)
    }

    fn func(name: &str, params: u64, locals: u64, code: Vec<I>) -> FuncProto {
        FuncProto {
            name: name.to_owned(),
            param_count: params,
            local_count: locals,
            execution: kira_runtime_abi::Execution::Runtime,
            code,
            releases: kira_bytecode::FrameRelease::EveryLocal,
        }
    }

    #[derive(Default)]
    struct DebugProbe {
        events: Vec<DebugSnapshot>,
    }

    struct DebugSnapshot {
        function_name: String,
        pc: usize,
        locals: Vec<Value>,
        stack: Vec<Value>,
        backtrace: Vec<(u32, usize)>,
    }

    impl VmDebugObserver for DebugProbe {
        fn before_instruction(&mut self, event: VmDebugEvent<'_>) -> VmDebugAction {
            self.events.push(DebugSnapshot {
                function_name: event.function_name.to_owned(),
                pc: event.pc,
                locals: event.locals.to_vec(),
                stack: event.stack.to_vec(),
                backtrace: event
                    .backtrace
                    .iter()
                    .map(|frame| (frame.function_id, frame.pc))
                    .collect(),
            });
            VmDebugAction::Continue
        }
    }

    #[test]
    fn debugger_events_expose_the_pre_instruction_frame_state() {
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            functions: vec![func(
                "main",
                0,
                1,
                vec![I::ConstInt(7), I::StoreLocal(0), I::LoadLocal(0), I::Return],
            )],
            main: Some(0),
            strings: Vec::new(),
        };
        let mut host = CapturingHost::new();
        let mut probe = DebugProbe::default();
        let outcome = execute_with_debug(&module, &mut host, &mut probe).expect("debug run");

        assert_eq!(outcome.result, Value::Int(7));
        assert_eq!(probe.events.len(), 4);
        assert_eq!(probe.events[0].function_name, "main");
        assert_eq!(probe.events[0].pc, 0);
        assert_eq!(probe.events[0].locals, [Value::Void]);
        assert!(probe.events[0].stack.is_empty());
        assert_eq!(probe.events[0].backtrace, [(0, 0)]);
        assert_eq!(probe.events[2].locals, [Value::Int(7)]);
        assert!(probe.events[2].stack.is_empty());
        assert_eq!(probe.events[3].stack, [Value::Int(7)]);
        assert_eq!(probe.events[3].locals, [Value::Int(7)]);
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
        ) -> Result<kira_runtime_abi::NativeReturn, kira_runtime_abi::NativeCallError> {
            use kira_runtime_abi::{NativeArg, NativeResult, NativeReturn};
            self.seen.push(format!("{function_id}{args:?}"));
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
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

    /// The scalar conversion instructions, pinned at the instruction level: the
    /// float-to-int cast truncates toward zero, saturates past `i64`'s range,
    /// and maps NaN to zero, and the int-to-float cast is exact. These are the
    /// endpoints the parity suite exercises through source, proven here directly
    /// so a change to either instruction fails a small, fast test too.
    #[test]
    fn scalar_conversions_truncate_saturate_and_convert() {
        // A magnitude past `i64::MAX` (~9.2e18), produced by a literal.
        let far = 100_000_000_000_000_000_000.0_f64;
        let main = func(
            "main",
            0,
            0,
            vec![
                I::ConstFloat(2.9),
                I::ConvertFloatToInt,
                I::Print,
                I::Pop,
                I::ConstFloat(-2.9),
                I::ConvertFloatToInt,
                I::Print,
                I::Pop,
                I::ConstFloat(far),
                I::ConvertFloatToInt,
                I::Print,
                I::Pop,
                I::ConstFloat(-far),
                I::ConvertFloatToInt,
                I::Print,
                I::Pop,
                I::ConstFloat(f64::NAN),
                I::ConvertFloatToInt,
                I::Print,
                I::Pop,
                I::ConstInt(7),
                I::ConvertIntToFloat,
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            functions: vec![main],
            main: Some(0),
            strings: Vec::new(),
        };
        let (lines, outcome) = run(&module);
        assert_eq!(
            lines,
            [
                "2",
                "-2",
                "9223372036854775807",
                "-9223372036854775808",
                "0",
                "7",
            ]
        );
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            functions: vec![main],
            main: Some(0),
            strings: vec![],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["10"]);
        assert_eq!(outcome.heap.current, 0);
    }

    #[test]
    fn signed_integer_comparisons_keep_their_ordering() {
        let main = func(
            "main",
            0,
            0,
            vec![
                I::ConstInt(4),
                I::ConstInt(4),
                I::EqInt,
                I::Print,
                I::Pop,
                I::ConstInt(4),
                I::ConstInt(4),
                I::NeInt,
                I::Print,
                I::Pop,
                I::ConstInt(3),
                I::ConstInt(4),
                I::LeInt,
                I::Print,
                I::Pop,
                I::ConstInt(5),
                I::ConstInt(4),
                I::GtInt,
                I::Print,
                I::Pop,
                I::ConstInt(4),
                I::ConstInt(4),
                I::GeInt,
                I::Print,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            functions: vec![main],
            main: Some(0),
            strings: vec![],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["true", "false", "true", "true", "true"]);
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
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
                exports: Default::default(),
                foreign_imports: Vec::new(),
                foreign_aggregates: Default::default(),
                foreign_callbacks: Vec::new(),
                functions: vec![func("main", 0, 0, vec![])],
                main: Some(0),
                strings: vec![],
            },
            // Falls off the end (not return-terminated).
            Module {
                exports: Default::default(),
                foreign_imports: Vec::new(),
                foreign_aggregates: Default::default(),
                foreign_callbacks: Vec::new(),
                functions: vec![func("main", 0, 0, vec![I::ConstInt(1)])],
                main: Some(0),
                strings: vec![],
            },
            // ConstStr into an empty pool.
            Module {
                exports: Default::default(),
                foreign_imports: Vec::new(),
                foreign_aggregates: Default::default(),
                foreign_callbacks: Vec::new(),
                functions: vec![func("main", 0, 0, vec![I::ConstStr(3), I::ReturnVoid])],
                main: Some(0),
                strings: vec![],
            },
            // LoadLocal beyond local_count.
            Module {
                exports: Default::default(),
                foreign_imports: Vec::new(),
                foreign_aggregates: Default::default(),
                foreign_callbacks: Vec::new(),
                functions: vec![func("main", 0, 1, vec![I::LoadLocal(9), I::ReturnVoid])],
                main: Some(0),
                strings: vec![],
            },
            // More parameters than locals (fill_params would underflow slots).
            Module {
                exports: Default::default(),
                foreign_imports: Vec::new(),
                foreign_aggregates: Default::default(),
                foreign_callbacks: Vec::new(),
                functions: vec![func("main", 2, 0, vec![I::ReturnVoid])],
                main: Some(0),
                strings: vec![],
            },
            // Entrypoint out of range.
            Module {
                exports: Default::default(),
                foreign_imports: Vec::new(),
                foreign_aggregates: Default::default(),
                foreign_callbacks: Vec::new(),
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
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            functions: vec![main],
            main: Some(0),
            strings: vec!["a".to_owned(), "bb".to_owned()],
        };
        let (lines, outcome) = run(&module);
        assert_eq!(lines, ["bb"]);
        assert_eq!(outcome.heap.current, 0);
    }
}
