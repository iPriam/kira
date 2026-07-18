//! Parity for argument passing, return values, and float formatting at scale.

use kira_wasm_runtime::WasmDevice;

use crate::{assert_parity, execute};

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
        types: Default::default(),
        main: Some(0),
        exports: Vec::new(),
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
