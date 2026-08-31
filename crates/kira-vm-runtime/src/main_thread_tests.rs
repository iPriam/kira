use super::*;
use kira_bytecode::FrameRelease;
use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::Instruction;
use kira_runtime_abi::{CapturingHost, Execution, HostCapabilities};

fn function(name: &str, locals: u64, code: Vec<Instruction>) -> FuncProto {
    FuncProto {
        name: name.to_owned(),
        param_count: locals,
        local_count: locals,
        execution: Execution::Runtime,
        code,
        releases: FrameRelease::EveryLocal,
    }
}

#[test]
fn invocation_runs_the_target_on_the_caller_loop() {
    let module = Module {
        functions: vec![
            function(
                "main",
                0,
                vec![
                    Instruction::ConstInt(4),
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::Invoke,
                        function: 1,
                        args: 1,
                    },
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
            function(
                "ui",
                1,
                vec![
                    Instruction::LoadLocal(0),
                    Instruction::ConstInt(1),
                    Instruction::AddInt,
                    Instruction::Return,
                ],
            ),
        ],
        main: Some(0),
        strings: Vec::new(),
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
    };
    let mut host = CapturingHost::new();
    let outcome = execute_with_main_thread(&module, &mut host).expect("main-thread run");
    assert_eq!(outcome.heap.current, 0);
    assert_eq!(host.lines(), ["5"]);
}

#[test]
fn a_main_thread_target_can_reenter_the_same_loop() {
    let module = Module {
        functions: vec![
            function(
                "main",
                0,
                vec![
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::Invoke,
                        function: 1,
                        args: 0,
                    },
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
            function(
                "outer",
                0,
                vec![
                    Instruction::ConstInt(4),
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::Invoke,
                        function: 2,
                        args: 1,
                    },
                    Instruction::Return,
                ],
            ),
            function(
                "inner",
                1,
                vec![
                    Instruction::LoadLocal(0),
                    Instruction::ConstInt(1),
                    Instruction::AddInt,
                    Instruction::Return,
                ],
            ),
        ],
        main: Some(0),
        strings: Vec::new(),
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
    };
    let mut host = CapturingHost::new();
    execute_with_main_thread(&module, &mut host).expect("main-thread run");
    assert_eq!(host.lines(), ["5"]);
}

#[test]
fn spawn_and_post_keep_the_helper_and_loop_ordered() {
    let module = Module {
        functions: vec![
            function(
                "main",
                1,
                vec![
                    Instruction::ConstInt(10),
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::Spawn,
                        function: 1,
                        args: 1,
                    },
                    Instruction::StoreLocal(0),
                    Instruction::ConstInt(20),
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::Post,
                        function: 2,
                        args: 1,
                    },
                    Instruction::Pop,
                    Instruction::LoadLocal(0),
                    Instruction::MainThreadJoin,
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
            function(
                "spawned",
                1,
                vec![
                    Instruction::LoadLocal(0),
                    Instruction::ConstInt(1),
                    Instruction::AddInt,
                    Instruction::Return,
                ],
            ),
            function(
                "posted",
                1,
                vec![
                    Instruction::LoadLocal(0),
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
        ],
        main: Some(0),
        strings: Vec::new(),
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
    };
    let mut host = CapturingHost::new();
    execute_with_main_thread(&module, &mut host).expect("main-thread run");
    assert_eq!(host.lines(), ["20", "11"]);
}

#[test]
fn the_helper_and_main_loop_use_different_threads() {
    #[derive(Default)]
    struct ThreadHost {
        lines: Vec<(String, thread::ThreadId)>,
    }

    impl HostCapabilities for ThreadHost {
        fn write_line(&mut self, text: &str) {
            self.lines.push((text.to_owned(), thread::current().id()));
        }
    }

    let module = Module {
        functions: vec![
            function(
                "main",
                0,
                vec![
                    Instruction::ConstInt(1),
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ConstInt(2),
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::Invoke,
                        function: 1,
                        args: 1,
                    },
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
            function(
                "main_thread_target",
                1,
                vec![
                    Instruction::LoadLocal(0),
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::LoadLocal(0),
                    Instruction::Return,
                ],
            ),
        ],
        main: Some(0),
        strings: Vec::new(),
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
    };
    let mut host = ThreadHost::default();
    execute_with_main_thread(&module, &mut host).expect("main-thread run");
    assert_eq!(
        host.lines
            .iter()
            .map(|(line, _)| line.as_str())
            .collect::<Vec<_>>(),
        ["1", "2", "2"]
    );
    assert_eq!(host.lines[0].1, host.lines[2].1);
    assert_ne!(host.lines[0].1, host.lines[1].1);
}

#[test]
fn a_lifecycle_runs_on_the_callers_main_thread_beside_the_entrypoint() {
    #[derive(Default)]
    struct ThreadHost {
        thread: Option<thread::ThreadId>,
    }

    impl HostCapabilities for ThreadHost {
        fn write_line(&mut self, _text: &str) {
            self.thread = Some(thread::current().id());
        }
    }

    let module = Module {
        // The entrypoint keeps the helper thread and writes nothing, so the
        // only line recorded is the one the lifecycle wrote.
        functions: vec![
            function(
                "main",
                0,
                vec![
                    Instruction::MainThreadCall {
                        operation: MainThreadOp::LifecycleStart,
                        function: 1,
                        args: 0,
                    },
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
            function(
                "ui",
                0,
                vec![
                    Instruction::MainThreadLifecycle,
                    Instruction::ConstInt(1),
                    Instruction::Print,
                    Instruction::Pop,
                    Instruction::ReturnVoid,
                ],
            ),
        ],
        main: Some(0),
        strings: Vec::new(),
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
    };
    let caller = thread::current().id();
    let mut host = ThreadHost::default();
    execute_with_main_thread(&module, &mut host).expect("lifecycle run");
    assert_eq!(host.thread, Some(caller));
}
