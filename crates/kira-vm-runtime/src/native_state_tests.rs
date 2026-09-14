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
        constants: Vec::new(),
        types: Vec::new(),
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
        constants: Vec::new(),
        types: Vec::new(),
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
            I::NativeUserData { shared: false },
            I::NativeRecover(STATE_TYPE.as_word()),
            I::CallNative(1),
            I::Print,
            I::Pop,
            I::TakeLocal(0),
            I::NativeStateRelease,
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
            I::NativeUserData { shared: false },
            I::NativeRecover(STATE_TYPE.as_word()),
            I::GetField(0),
            I::ArrayElements(kira_runtime_abi::ForeignType::F32),
            I::StoreLocal(1),
            I::TakeLocal(0),
            I::NativeStateRelease,
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
            I::NativeUserData { shared: false },
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
        // Slot 0 is a borrowed `NativeState` owner the caller keeps; only a
        // `let` binding is a release candidate, so the plan names the non-param
        // slots and leaves the borrow to the caller — exactly as `scope_releases`
        // does for a `borrow NativeState<T>` parameter.
        releases: kira_bytecode::FrameRelease::Planned(vec![1, 2]),
    };
    let free_state = FuncProto {
        name: "freeState".to_owned(),
        param_count: 1,
        local_count: 1,
        execution: Execution::Runtime,
        code: vec![
            I::TakeLocal(0),
            I::NativeStateRelease,
            I::Pop,
            I::ReturnVoid,
        ],
        releases: kira_bytecode::FrameRelease::EveryLocal,
    };
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![make_state, read_state, free_state],
        main: None,
        strings: Vec::new(),
    };
    let mut host = NativeStateHost::new(CapturingHost::new());
    let mut instance = crate::Instance::load(module).expect("the transition module validates");
    let NativeResult::NativeState(token) = instance
        .call(&mut host, 0, &[])
        .expect("the first entry creates callback state")
    else {
        panic!("the state token must cross as an affine native-state owner");
    };

    for _ in 0..2 {
        assert_eq!(
            instance
                .call(&mut host, 1, &[NativeArg::NativeState(token)])
                .expect("the state array remains readable"),
            NativeResult::Int(11)
        );
    }

    instance
        .call(&mut host, 2, &[NativeArg::NativeState(token)])
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
            I::NativeUserData { shared: false },
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
        // A borrowed `NativeState` owner sits in the param slot; the caller keeps
        // it, so the plan releases only the non-param slots.
        releases: kira_bytecode::FrameRelease::Planned(vec![1, 2]),
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
        // A borrowed `NativeState` owner sits in the param slot; the caller keeps
        // it, so the plan releases only the non-param slots.
        releases: kira_bytecode::FrameRelease::Planned(vec![1, 2]),
    };
    let transition_module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![make_state, read_state, read_state_stack],
        main: None,
        strings: Vec::new(),
    };
    let mut host = NativeStateHost::new(CapturingHost::new());
    let mut instance = crate::Instance::load(transition_module).expect("the trap module validates");
    let NativeResult::NativeState(token) = instance
        .call(&mut host, 0, &[])
        .expect("the first entry creates callback state")
    else {
        panic!("the state token must cross as an affine native-state owner");
    };
    assert!(matches!(
        instance.call(&mut host, 1, &[NativeArg::NativeState(token)]),
        Err(VmError::NotAnArray)
    ));
    assert!(matches!(
        instance.call(&mut host, 2, &[NativeArg::NativeState(token)]),
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

#[path = "native_state_tests/ownership.rs"]
mod ownership;
