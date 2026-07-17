//! Differential tests: the VM and the generated wasm module must not disagree.
//!
//! Parity is proven, not asserted. Each case compiles one program from one
//! [`IrProgram`] to bytecode and to wasm, runs the bytecode on the VM and the
//! module on a wasm engine, and requires identical output. The VM is the
//! reference: it is the simplest of the engines and the one the others are
//! defined to mirror, so a disagreement names the side that drifted.
//!
//! Both devices are compared on every case. `wasm32` and `wasm64` run the same
//! lowering through different address widths, and a program cannot tell which
//! memory it is in — so a case that passes on one and fails on the other has
//! found a bug in the width axis, which is exactly what it is here to do.
//!
//! # Why an engine, and why this one
//!
//! Checking the emitted bytes against expected bytes would test the encoder
//! against itself: the claim is that a Kira program *runs*, and only an engine
//! can answer that. The engine is `wasmi` — an interpreter that is a
//! dependency, not a program on the machine. Everything follows from that: it
//! runs in-process, it runs on every machine that can build this workspace, and
//! there is no path where a missing tool turns into a green build that proved
//! nothing.
//!
//! It also reads the spec independently of this crate. An encoder bug that
//! happened to match a mistaken reading here would have to match wasmi's too,
//! which is the point of testing against someone else's implementation.
//!
//! What this does *not* cover is the generated page: the browser host, `fetch`,
//! and `instantiateStreaming` are the browser's, and no wasm engine exercises
//! them. That surface is thin by design — the host supplies two imports over
//! bytes the module already rendered — and it is checked by opening the page.

use kira_wasm_runtime::WasmDevice;
use wasmi::{Caller, Config, Engine, Linker, Module, Store};

/// Collects what a program printed, standing in for the CLI's stdout host.
#[derive(Default)]
struct Collector {
    lines: Vec<String>,
}

impl kira_runtime_abi::HostCapabilities for Collector {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }
}

/// Compiles `source` to IR through the same frontend the CLI drives.
fn lower(source: &str) -> kira_ir::IrProgram {
    let db = salsa::DatabaseImpl::new();
    let program =
        kira_semantics::SourceProgram::new(&db, source.to_owned(), "test.kira".to_owned());
    let analyzed = kira_semantics::analyzed(&db, program);
    kira_ir::lower(&analyzed).expect("a runnable program")
}

/// Runs `source` on the VM, returning its output lines.
fn run_on_vm(source: &str) -> Result<Vec<String>, String> {
    let ir = lower(source);
    let module = kira_bytecode::compile(&ir).map_err(|error| error.to_string())?;
    let mut host = Collector::default();
    match kira_vm_runtime::execute(&module, &mut host) {
        Ok(_) => Ok(host.lines),
        Err(trap) => Err(trap.to_string()),
    }
}

/// What the module told the host, as the host saw it.
#[derive(Default)]
struct Host {
    lines: Vec<String>,
    /// The message from a `trap` call, if the program raised one.
    ///
    /// A Kira trap arrives as a call and *then* an `unreachable`, so the engine
    /// error that follows is the stop, not the reason. The reason is this.
    trap: Option<String>,
}

/// Reads a Kira string's bytes out of the module's exported memory.
fn read_text(caller: &Caller<'_, Host>, pointer: u64, length: u32) -> String {
    let memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .expect("the module exports its memory");
    let data = memory.data(caller);
    let start = pointer as usize;
    let end = start + length as usize;
    String::from_utf8_lossy(&data[start..end]).into_owned()
}

/// Runs a compiled module, returning its output lines or why it stopped.
///
/// The two imports are the same two the browser supplies, over the same bytes:
/// this host writes them into a `Vec` where the page writes them into the DOM.
fn execute(bytes: &[u8], device: WasmDevice) -> Result<Vec<String>, String> {
    let mut config = Config::default();
    // Off by default in wasmi, and a `wasm64` module is rejected outright
    // without it.
    config.wasm_memory64(true);
    let engine = Engine::new(&config);

    let module = Module::new(&engine, bytes).map_err(|error| format!("invalid module: {error}"))?;
    let mut store = Store::new(&engine, Host::default());
    let mut linker = Linker::new(&engine);

    // A pointer is an `i32` under Memory32 and an `i64` under Memory64, so the
    // host's own signatures move with the device — exactly as the browser's do.
    match device {
        WasmDevice::Wasm32 => {
            linker
                .func_wrap(
                    "kira",
                    "print",
                    |caller: Caller<'_, Host>, pointer: i32, length: i32| {
                        let text = read_text(&caller, pointer as u32 as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().lines.push(text);
                    },
                )
                .map_err(|error| error.to_string())?;
            linker
                .func_wrap(
                    "kira",
                    "trap",
                    |caller: Caller<'_, Host>, pointer: i32, length: i32| {
                        let text = read_text(&caller, pointer as u32 as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().trap = Some(text);
                    },
                )
                .map_err(|error| error.to_string())?;
        }
        WasmDevice::Wasm64 => {
            linker
                .func_wrap(
                    "kira",
                    "print",
                    |caller: Caller<'_, Host>, pointer: i64, length: i32| {
                        let text = read_text(&caller, pointer as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().lines.push(text);
                    },
                )
                .map_err(|error| error.to_string())?;
            linker
                .func_wrap(
                    "kira",
                    "trap",
                    |caller: Caller<'_, Host>, pointer: i64, length: i32| {
                        let text = read_text(&caller, pointer as u64, length as u32);
                        let mut caller = caller;
                        caller.data_mut().trap = Some(text);
                    },
                )
                .map_err(|error| error.to_string())?;
        }
    }

    let instance = linker
        .instantiate_and_start(&mut store, &module)
        .map_err(|error| format!("cannot instantiate: {error}"))?;
    let main = instance
        .get_typed_func::<(), ()>(&store, "kira_main")
        .map_err(|error| format!("no entrypoint: {error}"))?;

    match main.call(&mut store, ()) {
        Ok(()) => Ok(store.data().lines.clone()),
        Err(error) => Err(match &store.data().trap {
            // A Kira trap: report what Kira said, the way the CLI does.
            Some(reason) => format!("kirac: runtime trap: {reason}"),
            // Anything else is the engine refusing the module, which is a bug
            // in this backend and must not be mistaken for a program's trap.
            None => format!("the module faulted with no Kira trap: {error}"),
        }),
    }
}

/// Runs `source` as a wasm module on `device`, returning its output lines.
fn run_on_wasm(source: &str, device: WasmDevice) -> Result<Vec<String>, String> {
    let ir = lower(source);
    let bytes = kira_wasm_runtime::compile(&ir, device).map_err(|error| error.to_string())?;
    execute(&bytes, device)
}

/// Asserts the VM and both wasm devices agree on `source`.
///
/// The three engines are independent, so they run at once: each case is a
/// module built and interpreted twice over, and waiting for one width before
/// starting the other is the case paying twice for nothing. The harness
/// parallelises across cases; this is what parallelises inside one.
fn assert_parity(source: &str) {
    let (expected, actuals) = std::thread::scope(|scope| {
        let wasm: Vec<_> = [WasmDevice::Wasm32, WasmDevice::Wasm64]
            .map(|device| (device, scope.spawn(move || run_on_wasm(source, device))))
            .into_iter()
            .collect();
        // The VM is the reference and the cheapest of the three, so it runs
        // here rather than paying for a thread of its own.
        let expected = run_on_vm(source);
        let actuals: Vec<_> = wasm
            .into_iter()
            .map(|(device, worker)| (device, worker.join().expect("the wasm engine ran")))
            .collect();
        (expected, actuals)
    });

    for (device, actual) in actuals {
        let actual = &actual;
        match (&expected, &actual) {
            (Ok(vm), Ok(wasm)) => assert_eq!(
                vm,
                wasm,
                "the vm and {} disagree on output for:\n{source}",
                device.label()
            ),
            (Err(vm), Err(wasm)) => assert!(
                wasm.contains(vm.as_str()),
                "the vm trapped with `{vm}` but {} reported:\n{wasm}\nfor:\n{source}",
                device.label()
            ),
            (Ok(vm), Err(wasm)) => panic!(
                "the vm printed {vm:?} but {} failed:\n{wasm}\nfor:\n{source}",
                device.label()
            ),
            (Err(vm), Ok(wasm)) => panic!(
                "the vm trapped with `{vm}` but {} printed {wasm:?} for:\n{source}",
                device.label()
            ),
        }
    }
}

#[test]
fn prints_a_string_literal() {
    assert_parity(r#"@Main function main() { print("hello from Kira") return }"#);
}

#[test]
fn prints_integers_including_the_extremes() {
    assert_parity(
        r#"@Main function main() {
            print(0)
            print(1)
            print(-1)
            print(42)
            print(-9223372036854775807 - 1)
            print(9223372036854775807)
            return
        }"#,
    );
}

#[test]
fn integer_arithmetic_wraps_like_the_vm() {
    assert_parity(
        r#"@Main function main() {
            print(9223372036854775807 + 1)
            print(-9223372036854775807 - 2)
            print(9223372036854775807 * 2)
            print((-9223372036854775807 - 1) / -1)
            print((-9223372036854775807 - 1) % -1)
            print(7 / 2)
            print(-7 / 2)
            print(7 % 3)
            print(-7 % 3)
            return
        }"#,
    );
}

#[test]
fn division_by_zero_traps_the_same_way() {
    assert_parity(
        r#"@Main function main() {
            print("before")
            print(1 / 0)
            return
        }"#,
    );
}

#[test]
fn remainder_by_zero_traps_the_same_way() {
    assert_parity(
        r#"@Main function main() {
            print("before")
            print(1 % 0)
            return
        }"#,
    );
}

#[test]
fn prints_booleans_and_comparisons() {
    assert_parity(
        r#"@Main function main() {
            print(true)
            print(false)
            print(1 < 2)
            print(2 <= 2)
            print(3 > 4)
            print(!true)
            print(true && false)
            print(true || false)
            print("a" == "a")
            print("a" != "b")
            return
        }"#,
    );
}

#[test]
fn short_circuit_operators_skip_their_right_operand() {
    // The right operand traps, so a backend that evaluated it eagerly would
    // disagree with the VM by dying instead of printing `false`.
    assert_parity(
        r#"@Main function main() {
            print(false && (1 / 0) == 0)
            print(true || (1 / 0) == 0)
            return
        }"#,
    );
}

#[test]
fn concatenates_and_compares_strings() {
    assert_parity(
        r#"@Main function main() {
            let greeting = "hello"
            let subject = "kira"
            print(greeting + " " + subject)
            print(banner())
            print(greeting == "hello")
            print(greeting == subject)
            print("")
            print("" + "")
            return
        }
        function banner() -> String { return "one source" + ", many backends" }"#,
    );
}

#[test]
fn runs_loops_and_mutation() {
    assert_parity(
        r#"@Main function main() {
            var i = 0
            var sum = 0
            while i < 10 {
                sum = sum + i
                i = i + 1
            }
            print(sum)
            var countdown = 3
            while countdown > 0 {
                print(countdown)
                countdown = countdown - 1
            }
            print(sum > 40 && sum < 50)
            return
        }"#,
    );
}

#[test]
fn runs_recursion() {
    assert_parity(
        r#"@Main function main() {
            print(fib(10))
            print(fib(20))
            return
        }
        function fib(n: Int) -> Int {
            if n < 2 { return n }
            return fib(n - 1) + fib(n - 2)
        }"#,
    );
}

#[test]
fn prints_floats_the_way_rust_displays_them() {
    // Every one of these is a place a hand-rolled formatter goes wrong: whole
    // floats that must not show a point, values JavaScript would render in
    // exponent notation, and a negative zero whose sign is observable.
    assert_parity(
        r#"@Main function main() {
            print(0.0)
            print(-0.0)
            print(1.0)
            print(2.5)
            print(-3.75)
            print(0.1)
            print(0.2)
            print(0.1 + 0.2)
            print(1.0 / 3.0)
            print(2.0 / 3.0)
            print(100.0)
            print(0.5)
            print(0.05)
            print(1.0 / 0.0)
            print(-1.0 / 0.0)
            print(0.0 / 0.0)
            return
        }"#,
    );
}

#[test]
fn prints_floats_at_the_extremes_of_the_format() {
    assert_parity(
        r#"@Main function main() {
            print(1.7976931348623157e308)
            print(-1.7976931348623157e308)
            print(5.0e-324)
            print(2.2250738585072014e-308)
            print(1.0e21)
            print(1.0e-7)
            print(123456789.123456789)
            print(4.9406564584124654e-324)
            return
        }"#,
    );
}

#[test]
fn float_arithmetic_agrees_with_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var x = 0.0
            var i = 0
            while i < 10 {
                x = x + 0.1
                i = i + 1
            }
            print(x)
            print(x == 1.0)
            print(-x)
            print(x * 3.0)
            print(x / 7.0)
            return
        }"#,
    );
}

#[test]
fn passes_arguments_and_returns_values_of_every_type() {
    assert_parity(
        r#"@Main function main() {
            print(double(21))
            print(half(5.0))
            print(negate(true))
            print(shout("kira"))
            return
        }
        function double(value: Int) -> Int { return value + value }
        function half(value: Float) -> Float { return value / 2.0 }
        function negate(value: Bool) -> Bool { return !value }
        function shout(value: String) -> String { return value + "!" }"#,
    );
}

/// Builds `@Main function main() { print(v0) print(v1) ... return }` directly,
/// so the values under test are exact bit patterns rather than whatever the
/// lexer makes of a decimal literal.
fn program_printing(values: &[f64]) -> kira_ir::IrProgram {
    let mut exprs = la_arena::Arena::new();
    let mut body = Vec::with_capacity(values.len() + 1);
    for value in values {
        let literal = exprs.alloc(kira_ir::IrExpr::Float(*value));
        let call = exprs.alloc(kira_ir::IrExpr::Call {
            callee: kira_ir::IrCallee::Print,
            args: vec![literal],
            result: kira_semantics_model::Type::Void,
        });
        body.push(kira_ir::IrStmt::Eval { expr: call });
    }
    body.push(kira_ir::IrStmt::Return { value: None });

    kira_ir::IrProgram {
        functions: vec![kira_ir::IrFunction {
            name: "main".to_owned(),
            param_count: 0,
            locals: Vec::new(),
            return_type: kira_semantics_model::Type::Void,
            execution: kira_runtime_abi::Execution::Inherited,
            body,
        }],
        main: 0,
        exprs,
    }
}

/// Runs a hand-built program on a device, returning its output lines.
fn run_ir_on_wasm(program: &kira_ir::IrProgram, device: WasmDevice) -> Result<Vec<String>, String> {
    let bytes = kira_wasm_runtime::compile(program, device).map_err(|error| error.to_string())?;
    execute(&bytes, device)
}

/// A deterministic bit-pattern source.
///
/// Seeded and reproducible on purpose: a fuzz failure that cannot be replayed
/// is a rumour, not a bug report.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        // SplitMix64: small, well-distributed, and no dependency to add.
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[test]
fn the_float_formatter_agrees_with_rust_on_random_values() {
    // Dragon4's whole point is that the shortest round-tripping digits are the
    // ones Rust prints. Handwritten cases only cover what was thought of; the
    // values that break a float formatter are the ones nobody would think to
    // write down. Rust's own `to_string` is the oracle, on the same bits.
    let mut random = Random(0x5eed_1234_5678_9abc);
    let mut values = Vec::new();
    while values.len() < 5000 {
        let candidate = f64::from_bits(random.next());
        // Every finite value is fair game — subnormals, huge exponents, and the
        // ordinary middle. The non-finite ones are covered by name elsewhere.
        if candidate.is_finite() {
            values.push(candidate);
        }
    }
    // Values with few significant digits are where the boundary tests and the
    // rounding tie-break actually bite, so they are not left to chance.
    for scale in 0..40u32 {
        values.push(f64::from(scale) / 10.0);
        values.push(f64::from(scale) * 1e10);
        values.push(f64::from(scale).powi(3) / 7.0);
    }

    check_against_rust(&values);
}

/// Asserts both devices print every one of `values` exactly as Rust does.
///
/// Split across threads because this is one test doing one long piece of work:
/// the harness parallelises across test functions, so a single monolithic case
/// is the one thing it cannot speed up, and this is the case that grows every
/// time the formatter is doubted. Each thread compiles and runs its own module,
/// so they share nothing.
fn check_against_rust(values: &[f64]) {
    let threads = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4);
    // Oversubscribed on purpose, and split per device as well as per chunk:
    // what a value costs to format swings by orders of magnitude with its
    // exponent, so equal-sized chunks are not equal-sized work. Small units let
    // the fast ones fill in around the slow ones.
    let chunk = values.len().div_ceil(threads * 4).max(1);

    std::thread::scope(|scope| {
        let workers: Vec<_> = values
            .chunks(chunk)
            .flat_map(|values| {
                [WasmDevice::Wasm32, WasmDevice::Wasm64].map(|device| {
                    scope.spawn(move || {
                        let expected: Vec<String> =
                            values.iter().map(|value| value.to_string()).collect();
                        let program = program_printing(values);
                        let actual = run_ir_on_wasm(&program, device).expect("the module runs");
                        assert_eq!(
                            actual.len(),
                            expected.len(),
                            "{} lost lines",
                            device.label()
                        );
                        for ((value, want), got) in values.iter().zip(&expected).zip(&actual) {
                            assert_eq!(
                                want,
                                got,
                                "{} printed `{got}` for the f64 with bits {:#018x}, which Rust \
                                 prints as `{want}`",
                                device.label(),
                                value.to_bits(),
                            );
                        }
                    })
                })
            })
            .collect();

        // A failing chunk already printed which value broke it; joining is what
        // makes the test fail rather than the thread quietly dying.
        for worker in workers {
            worker.join().expect("a chunk of values checks out");
        }
    });
}

#[test]
fn a_program_whose_literals_outgrow_a_page_still_instantiates() {
    // Literals are written by a data segment at instantiation, before any code
    // runs, so they cannot grow the memory they need the way the heap can. A
    // module that reserved one page for them was refused by the engine outright
    // — the program never started, whatever it did.
    let mut source = String::from("@Main function main() {\n");
    let mut expected = Vec::new();
    for index in 0..2000 {
        // Distinct, so none of them dedup away, and 40 bytes each: past one
        // page in total and nowhere near it individually.
        let line = format!("literal number {index:04} padding padding pad");
        source.push_str(&format!("    print(\"{line}\")\n"));
        expected.push(line);
    }
    source.push_str("    return\n}");

    for device in [WasmDevice::Wasm32, WasmDevice::Wasm64] {
        let actual = run_on_wasm(&source, device).expect("the module instantiates and runs");
        assert_eq!(actual, expected, "{} lost literals", device.label());
    }
}

#[test]
fn a_concatenating_loop_outgrows_the_first_page() {
    // The allocator never frees, so this is what makes it grow memory: a module
    // that could not grow would trap partway through instead of printing.
    assert_parity(
        r#"@Main function main() {
            var text = ""
            var i = 0
            while i < 2000 {
                text = text + "0123456789012345678901234567890123456789"
                i = i + 1
            }
            print(text == "")
            print(i)
            return
        }"#,
    );
}
