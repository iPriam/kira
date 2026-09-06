//! What a debug observer is shown, and when: the frame state as it stands
//! *before* an instruction runs, and the constant table's one fill.

use super::*;
use crate::debug::{VmDebugAction, VmDebugEvent, VmDebugObserver};
use crate::vm_test_support::{func, run};
use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

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
        constants: Vec::new(),
        types: Vec::new(),
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

#[test]
fn module_constants_fill_once_before_main_and_release_after() {
    // Slot 0 is a string built by an init that also prints, so the run's
    // output proves the init ran exactly once, before `main`, however many
    // times the slot is read; the outcome's heap accounting proves the
    // slot's storage went back at the end.
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: vec![1],
        types: Vec::new(),
        functions: vec![
            func(
                "main",
                0,
                0,
                vec![
                    I::LoadConstant(0),
                    I::Print,
                    I::Pop,
                    I::LoadConstant(0),
                    I::Print,
                    I::Pop,
                    I::ReturnVoid,
                ],
            ),
            func(
                "greeting$constant",
                0,
                0,
                vec![I::ConstStr(1), I::Print, I::Pop, I::ConstStr(0), I::Return],
            ),
        ],
        main: Some(0),
        strings: vec!["hello".to_owned(), "init-ran".to_owned()],
    };
    let (lines, outcome) = run(&module);
    assert_eq!(lines, ["init-ran", "hello", "hello"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn a_constant_read_ahead_of_the_table_is_a_typed_trap() {
    // Validation bounds `LoadConstant` by the module's table, so the only
    // way to read an unfilled slot is a table whose *own* init reads a
    // later slot — bytecode ahead of the compiler's dependency order.
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: vec![1, 2],
        types: Vec::new(),
        functions: vec![
            func("main", 0, 0, vec![I::ReturnVoid]),
            func("first$constant", 0, 0, vec![I::LoadConstant(1), I::Return]),
            func("second$constant", 0, 0, vec![I::ConstInt(2), I::Return]),
        ],
        main: Some(0),
        strings: Vec::new(),
    };
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).unwrap_err();
    assert_eq!(error, VmError::ConstantUninitialized(1));
}
