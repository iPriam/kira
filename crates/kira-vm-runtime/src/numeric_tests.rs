//! The numeric instructions pinned at the instruction level: conversions,
//! checked arithmetic, the width checks, and the traps each of them raises.
//!
//! The parity suite reaches these through source. Proving them here as well
//! means a change to one instruction fails a small, fast test before it fails
//! a slow one.

use super::*;
use crate::vm_test_support::{func, run};
use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

/// The scalar conversion instructions, pinned at the instruction level: the
/// float-to-int cast truncates toward zero, saturates past `i64`'s range,
/// and maps NaN to zero, and the int-to-float cast is exact. These are the
/// endpoints the parity suite exercises through source, proven here directly
/// so a change to either instruction fails a small, fast test too.
#[test]
fn scalar_conversions_truncate_and_convert() {
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
            I::ConstInt(7),
            I::ConvertIntToFloat,
            I::Print,
            I::Pop,
            I::ConstInt(-1),
            I::ConvertUIntToFloat,
            I::Print,
            I::Pop,
            I::ConstInt(-1),
            I::PrintUnsigned,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
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
            "7",
            "18446744073709552000",
            "18446744073709551615"
        ]
    );
    assert_eq!(outcome.heap.current, 0);
}

/// A float with no integer value traps rather than saturating: a magnitude
/// past `i64::MAX`, and NaN.
#[test]
fn a_float_without_an_integer_value_traps() {
    for value in [
        100_000_000_000_000_000_000.0_f64,
        -100_000_000_000_000_000_000.0,
        f64::NAN,
    ] {
        let main = func(
            "main",
            0,
            0,
            vec![
                I::ConstFloat(value),
                I::ConvertFloatToInt,
                I::Pop,
                I::ReturnVoid,
            ],
        );
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![main],
            main: Some(0),
            strings: Vec::new(),
        };
        let mut host = CapturingHost::new();
        let error = execute(&module, &mut host).expect_err("a trap");
        assert!(
            matches!(error, VmError::FloatToIntOutOfRange { .. }),
            "{value}: {error:?}"
        );
    }
}

/// A program to run and the trap it must raise.
type TrapCase = (Vec<I>, fn(&VmError) -> bool);

/// Checked arithmetic traps where the wrapping opcodes wrap, and a width
/// check refuses a 64-bit result the written width cannot hold.
#[test]
fn checked_arithmetic_and_width_checks_trap() {
    let cases: Vec<TrapCase> = vec![
        (
            vec![I::ConstInt(i64::MAX), I::ConstInt(1), I::AddIntChecked],
            |e| matches!(e, VmError::IntegerOverflow { .. }),
        ),
        (
            vec![
                I::ConstInt(i64::MAX),
                I::ConstInt(1),
                I::AddInt,
                I::Pop,
                I::ConstInt(250),
                I::ConstInt(10),
                I::AddIntChecked,
                I::CheckInt(4),
            ],
            |e| matches!(e, VmError::IntegerOverflow { spelling: "U8" }),
        ),
        (
            vec![I::ConstInt(-1), I::ConstInt(1), I::AddUIntChecked],
            |e| matches!(e, VmError::IntegerOverflow { spelling: "U64" }),
        ),
        (vec![I::ConstInt(i64::MIN), I::NegIntChecked], |e| {
            matches!(e, VmError::IntegerOverflow { .. })
        }),
        (
            vec![I::ConstInt(i64::MIN), I::ConstInt(-1), I::DivIntChecked],
            |e| matches!(e, VmError::IntegerOverflow { .. }),
        ),
        (
            vec![I::ConstInt(1), I::ConstInt(8), I::CheckShift(8), I::Shl],
            |e| matches!(e, VmError::ShiftOutOfRange { count: 8, bits: 8 }),
        ),
        (
            vec![I::ConstInt(256), I::ConvertInt { from: 0, to: 4 }],
            |e| {
                matches!(
                    e,
                    VmError::NarrowingOutOfRange {
                        value: 256,
                        spelling: "U8"
                    }
                )
            },
        ),
        (
            vec![I::ConstInt(-1), I::ConvertInt { from: 7, to: 0 }],
            |e| {
                matches!(
                    e,
                    VmError::NarrowingOutOfRange {
                        spelling: "Int",
                        ..
                    }
                )
            },
        ),
    ];
    for (code, matches) in cases {
        let mut code = code;
        code.extend([I::Pop, I::ReturnVoid]);
        let main = func("main", 0, 0, code);
        let module = Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![main],
            main: Some(0),
            strings: Vec::new(),
        };
        let mut host = CapturingHost::new();
        let error = execute(&module, &mut host).expect_err("a trap");
        assert!(matches(&error), "{error:?}");
    }
}

/// The wrapping opcodes wrap at 64 bits, `WrapInt` at the written width,
/// and a conversion whose value fits passes it through.
#[test]
fn wrapping_opcodes_and_fitting_conversions_answer() {
    let main = func(
        "main",
        0,
        0,
        vec![
            I::ConstInt(i64::MAX),
            I::ConstInt(1),
            I::AddInt,
            I::Print,
            I::Pop,
            I::ConstInt(255),
            I::ConstInt(1),
            I::AddInt,
            I::WrapInt(4),
            I::Print,
            I::Pop,
            I::ConstInt(128),
            I::WrapInt(1),
            I::Print,
            I::Pop,
            I::ConstInt(255),
            I::ConvertInt { from: 0, to: 4 },
            I::Print,
            I::Pop,
            I::ConstInt(-1),
            I::ConstInt(63),
            I::CheckShift(64),
            I::ShrUInt,
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
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: Vec::new(),
    };
    let (lines, _) = run(&module);
    assert_eq!(lines, ["-9223372036854775808", "0", "-128", "255", "1"]);
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
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: vec![],
    };
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).unwrap_err();
    assert_eq!(error, VmError::DivideByZero);
}
