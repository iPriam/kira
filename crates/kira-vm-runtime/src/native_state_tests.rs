//! VM execution tests for opaque callback-state instructions.

use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::{FieldPath, Instruction as I, PlacePath, WritebackTarget};
use kira_runtime_abi::{
    CapturingHost, Execution, HostCapabilities, NativeArg, NativeCallError, NativeResult,
    NativeReturn, NativeStateError, NativeStateHost, NativeStateTypeId, NativeStateValue,
};

use crate::{VmError, execute};

const STATE_TYPE: NativeStateTypeId = NativeStateTypeId::new(0x0500_0000_0000_0000);

fn module(code: Vec<I>, locals: u64) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: locals,
            execution: Execution::Runtime,
            code,
            releases: kira_bytecode::FrameRelease::EveryLocal,
        }],
        main: Some(0),
        strings: Vec::new(),
    }
}

fn hybrid_module(code: Vec<I>, locals: u64) -> Module {
    let main = FuncProto {
        name: "main".to_owned(),
        param_count: 0,
        local_count: locals,
        execution: Execution::Runtime,
        code,
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let native = FuncProto {
        name: "nativeViewFunction".to_owned(),
        param_count: 1,
        local_count: 1,
        execution: Execution::Native,
        code: Vec::new(),
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        functions: vec![main, native],
        main: Some(0),
        strings: Vec::new(),
    }
}

#[derive(Default)]
struct NativeViewHost {
    calls: usize,
    lines: Vec<String>,
    mutate: bool,
}

impl HostCapabilities for NativeViewHost {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        self.calls += 1;
        let [NativeArg::Aggregate(tree)] = args else {
            return Err(NativeCallError::UnboundFunction(function_id));
        };
        let NativeStateValue::Struct(fields) = tree else {
            return Err(NativeCallError::UnboundFunction(function_id));
        };
        let Some(NativeStateValue::Int(value)) = fields.first() else {
            return Err(NativeCallError::UnboundFunction(function_id));
        };
        if !self.mutate {
            return Ok(NativeReturn::plain(NativeResult::Int(*value)));
        }

        let mut fields = fields.as_ref().clone();
        fields[0] = NativeStateValue::Int(*value + 1);
        Ok(NativeReturn {
            result: NativeResult::Void,
            writebacks: vec![(
                0,
                NativeResult::Aggregate(NativeStateValue::struct_of(fields)),
            )],
        })
    }
}

#[test]
fn a_recovered_view_is_snapshotted_for_a_native_call() {
    let module = hybrid_module(
        vec![
            I::ConstInt(7),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word()),
            I::CallNative(1),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::ReturnVoid,
        ],
        1,
    );
    let mut host = NativeStateHost::new(NativeViewHost::default());
    let outcome = execute(&module, &mut host).expect("a native call can read a view");
    assert_eq!(host.inner().calls, 1);
    assert_eq!(host.inner().lines, ["7"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn a_recovered_array_crosses_the_array_elements_seam() {
    let module = module(
        vec![
            I::ConstFloat(1.25),
            I::ConstFloat(2.5),
            I::NewArray(2),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word()),
            I::GetField(0),
            I::ArrayElements(kira_runtime_abi::ForeignType::F32),
            I::StoreLocal(1),
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::LoadLocal(1),
            I::Return,
        ],
        2,
    );
    let mut host = NativeStateHost::new(CapturingHost::default());
    let outcome = execute(&module, &mut host).expect("a snapshot array reaches the C seam");
    // The flattened elements are an *owned* block now, not process-lifetime
    // storage: the run returns one and the exit drop frees it, so a clean
    // account is the proof the seam no longer leaks per crossing. The bytes
    // the block holds are pinned by `write_seam_scalar`'s own tests.
    assert!(
        matches!(outcome.result, crate::Value::CBlock(_)),
        "array elements must return an owned C block"
    );
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn a_native_state_array_read_survives_the_next_vm_entry() {
    let make_state = FuncProto {
        name: "makeState".to_owned(),
        param_count: 0,
        local_count: 0,
        execution: Execution::Runtime,
        code: vec![
            I::ConstInt(11),
            I::ConstInt(22),
            I::NewArray(2),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::NativeUserData,
            I::Return,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let read_state = FuncProto {
        name: "readState".to_owned(),
        param_count: 1,
        local_count: 3,
        execution: Execution::Runtime,
        code: vec![
            I::LoadLocal(0),
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(1),
            I::LoadLocal(1),
            I::GetField(0),
            I::StoreLocal(2),
            I::LoadLocal(2),
            I::ConstInt(0),
            I::ArrayGet,
            I::Return,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let free_state = FuncProto {
        name: "freeState".to_owned(),
        param_count: 1,
        local_count: 1,
        execution: Execution::Runtime,
        code: vec![I::LoadLocal(0), I::NativeStateFree, I::Pop, I::ReturnVoid],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        functions: vec![make_state, read_state, free_state],
        main: None,
        strings: Vec::new(),
    };
    let mut host = NativeStateHost::new(CapturingHost::new());
    let mut instance = crate::Instance::load(module).expect("the transition module validates");
    let NativeResult::RawPtr(token) = instance
        .call(&mut host, 0, &[])
        .expect("the first entry creates callback state")
    else {
        panic!("the state token must cross as a raw pointer");
    };

    for _ in 0..2 {
        assert_eq!(
            instance
                .call(&mut host, 1, &[NativeArg::RawPtr(token)])
                .expect("the state array remains readable"),
            NativeResult::Int(11)
        );
    }

    instance
        .call(&mut host, 2, &[NativeArg::RawPtr(token)])
        .expect("the state token is released");
    assert_eq!(instance.stats().current, 0);
}

#[test]
fn array_index_preserves_snapshot_type_and_bounds_traps() {
    let make_state = FuncProto {
        name: "makeState".to_owned(),
        param_count: 0,
        local_count: 0,
        execution: Execution::Runtime,
        code: vec![
            I::ConstInt(7),
            I::NewStruct(1),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::NativeUserData,
            I::Return,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let read_state = FuncProto {
        name: "readState".to_owned(),
        param_count: 1,
        local_count: 3,
        execution: Execution::Runtime,
        code: vec![
            I::LoadLocal(0),
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(1),
            I::LoadLocal(1),
            I::GetField(0),
            I::StoreLocal(2),
            I::ConstInt(0),
            I::ArrayGetLocal(2),
            I::Return,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let read_state_stack = FuncProto {
        name: "readStateStack".to_owned(),
        param_count: 1,
        local_count: 3,
        execution: Execution::Runtime,
        code: vec![
            I::LoadLocal(0),
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(1),
            I::LoadLocal(1),
            I::GetField(0),
            I::StoreLocal(2),
            I::LoadLocal(2),
            I::ConstInt(0),
            I::ArrayGet,
            I::Return,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let transition_module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        functions: vec![make_state, read_state, read_state_stack],
        main: None,
        strings: Vec::new(),
    };
    let mut host = NativeStateHost::new(CapturingHost::new());
    let mut instance = crate::Instance::load(transition_module).expect("the trap module validates");
    let NativeResult::RawPtr(token) = instance
        .call(&mut host, 0, &[])
        .expect("the first entry creates callback state")
    else {
        panic!("the state token must cross as a raw pointer");
    };
    assert!(matches!(
        instance.call(&mut host, 1, &[NativeArg::RawPtr(token)]),
        Err(VmError::NotAnArray)
    ));
    assert!(matches!(
        instance.call(&mut host, 2, &[NativeArg::RawPtr(token)]),
        Err(VmError::NotAnArray)
    ));

    let wrong_type = module(
        vec![
            I::ConstInt(7),
            I::NewStruct(1),
            I::StoreLocal(0),
            I::ConstInt(0),
            I::ArrayGetLocal(0),
            I::Return,
        ],
        1,
    );
    assert!(matches!(
        execute(&wrong_type, &mut NativeStateHost::new(CapturingHost::new())),
        Err(VmError::NotAnArray)
    ));

    let out_of_bounds = module(
        vec![
            I::ConstInt(7),
            I::NewArray(1),
            I::StoreLocal(0),
            I::ConstInt(1),
            I::ArrayGetLocal(0),
            I::Return,
        ],
        1,
    );
    assert!(matches!(
        execute(
            &out_of_bounds,
            &mut NativeStateHost::new(CapturingHost::new())
        ),
        Err(VmError::IndexOutOfBounds)
    ));

    let negative = module(
        vec![
            I::ConstInt(7),
            I::NewArray(1),
            I::StoreLocal(0),
            I::ConstInt(-1),
            I::ArrayGetLocal(0),
            I::Return,
        ],
        1,
    );
    assert!(matches!(
        execute(&negative, &mut NativeStateHost::new(CapturingHost::new())),
        Err(VmError::NegativeIndex)
    ));
}

#[test]
fn a_mutable_native_call_writes_a_recovered_view_back_to_state() {
    let module = hybrid_module(
        vec![
            I::ConstInt(7),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(1),
            I::LoadLocal(1),
            I::CallNativeWriteback {
                func: 1,
                targets: vec![WritebackTarget {
                    param: 0,
                    slot: 1,
                    path: PlacePath::new(Vec::new()),
                }],
            },
            I::Pop,
            I::LoadLocal(1),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::ReturnVoid,
        ],
        2,
    );
    let mut host = NativeStateHost::new(NativeViewHost {
        mutate: true,
        ..NativeViewHost::default()
    });
    let outcome = execute(&module, &mut host).expect("a native call can write a view");
    assert_eq!(host.inner().calls, 1);
    assert_eq!(host.inner().lines, ["8"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn boxes_recovers_mutates_observes_and_frees_state() {
    let field_zero = FieldPath::new(vec![0]);
    let module = module(
        vec![
            I::ConstInt(0),
            I::ConstInt(0),
            I::NewStruct(2),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData,
            I::StoreLocal(1),
            I::LoadLocal(1),
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(2),
            I::LoadLocal(2),
            I::GetField(0),
            I::ConstInt(1),
            I::AddInt,
            I::StoreField {
                slot: 2,
                path: field_zero,
            },
            I::LoadLocal(2),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::ReturnVoid,
        ],
        3,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    let outcome = execute(&module, &mut host).expect("state flow executes");
    assert_eq!(host.inner().lines(), ["1"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn wrong_recovery_type_and_double_free_are_typed_traps() {
    let wrong = module(
        vec![
            I::ConstInt(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word() + 1),
            I::ReturnVoid,
        ],
        0,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    assert!(matches!(
        execute(&wrong, &mut host),
        Err(VmError::NativeState(NativeStateError::WrongType { .. }))
    ));

    let double_free = module(
        vec![
            I::ConstInt(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::ReturnVoid,
        ],
        1,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    assert!(matches!(
        execute(&double_free, &mut host),
        Err(VmError::NativeState(NativeStateError::UnknownToken(_)))
    ));
}

/// A local that already holds a recovered view may be REBOUND to another view.
///
/// Storing into such a local ordinarily writes through it into the callback
/// state, which is what makes `state.field = x` work — but a view has no boxed
/// form, so treating a rebind as a write-back trapped every program that
/// recovered twice into one slot. Rendering the UI editor on the VM did exactly
/// that and never reached its first frame.
#[test]
fn rebinding_a_recovered_local_to_another_view_is_not_a_write_back() {
    let module = module(
        vec![
            // Two independent states, boxed and kept in locals 0 and 1.
            I::ConstInt(7),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::ConstInt(9),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(1),
            // Recover the first into local 2 ...
            I::LoadLocal(0),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(2),
            // ... then rebind that same local to a view of the second.
            I::LoadLocal(1),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(2),
            // The local now names the second state, and the first is untouched.
            I::LoadLocal(2),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word()),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::LoadLocal(1),
            I::NativeStateFree,
            I::Pop,
            I::ReturnVoid,
        ],
        3,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    let outcome = execute(&module, &mut host).expect("rebinding a view executes");
    assert_eq!(host.inner().lines(), ["9", "7"]);
    assert_eq!(outcome.heap.current, 0);
}

/// A capture cell inside callback state is one box, and the share the state
/// holds comes back when the state is freed.
///
/// The balance at the end is the whole point: the tree's share is released by
/// code that has no heap to release against, so it is recorded and drained
/// ([`crate::value::Heap::drain_released_cells`]). A drain that never ran would
/// leave the box live and this count above zero; a release that ran twice would
/// have freed it under the local still holding it.
#[test]
fn a_capture_cell_in_state_is_shared_and_its_share_comes_back() {
    let module = module(
        vec![
            // local 0: a cell holding 7, as a `var` capture becomes.
            I::ConstInt(7),
            I::NewCell,
            I::StoreLocal(0),
            // local 1: state boxing a struct that holds the same cell.
            I::LoadLocal(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(1),
            // The cell is still the frame's to write through.
            I::ConstInt(9),
            I::CellSet(0),
            I::CellGet(0),
            I::Print,
            I::Pop,
            I::LoadLocal(1),
            I::NativeStateFree,
            I::Pop,
            // …and still readable after the state that shared it is gone.
            I::CellGet(0),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
        2,
    );
    let mut host = NativeStateHost::new(CapturingHost::default());
    let outcome = execute(&module, &mut host).expect("state may hold a capture cell");
    assert_eq!(host.inner().lines(), ["9", "9"]);
    assert_eq!(outcome.heap.current, 0, "no heap was leaked");
}

#[derive(Default)]
struct ActiveReentryHost {
    calls: usize,
}

impl HostCapabilities for ActiveReentryHost {
    fn write_line(&mut self, _text: &str) {}

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        self.calls += 1;
        if function_id != 2 {
            return Err(NativeCallError::UnboundFunction(function_id));
        }
        crate::interp::call_active(1, args, &[0])
            .ok_or(NativeCallError::NoNativeHalf)?
            .map_err(|_| NativeCallError::MalformedResult(function_id))
    }
}

#[test]
fn a_reentered_vm_returns_an_array_writeback_to_the_suspended_frame() {
    let entry = FuncProto {
        name: "entry".to_owned(),
        param_count: 0,
        local_count: 1,
        execution: Execution::Runtime,
        code: vec![
            I::ConstInt(10),
            I::ConstInt(20),
            I::NewArray(2),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::CallNativeWriteback {
                func: 2,
                targets: vec![WritebackTarget {
                    param: 0,
                    slot: 0,
                    path: PlacePath::new(Vec::new()),
                }],
            },
            I::Pop,
            I::LoadLocal(0),
            I::ConstInt(0),
            I::ArrayGet,
            I::Return,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let callback = FuncProto {
        name: "callback".to_owned(),
        param_count: 1,
        local_count: 1,
        execution: Execution::Runtime,
        code: vec![
            I::LoadLocal(0),
            I::ConstInt(0),
            I::ArrayGet,
            I::Pop,
            I::ReturnVoid,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let native = FuncProto {
        name: "native".to_owned(),
        param_count: 1,
        local_count: 1,
        execution: Execution::Native,
        code: Vec::new(),
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        functions: vec![entry, callback, native],
        main: None,
        strings: Vec::new(),
    };
    let mut instance = crate::Instance::load(module).expect("the reentry module validates");
    let mut host = ActiveReentryHost::default();
    for _ in 0..2 {
        assert_eq!(
            instance
                .call(&mut host, 0, &[])
                .expect("the suspended VM accepts the callback writeback"),
            NativeResult::Int(10)
        );
    }
    assert_eq!(host.calls, 2);
    assert_eq!(instance.stats().current, 0);
}
