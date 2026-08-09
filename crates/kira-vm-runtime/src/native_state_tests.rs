//! VM execution tests for opaque callback-state instructions.

use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::{FieldPath, Instruction as I, PlacePath, WritebackTarget};
use kira_runtime_abi::{
    CapturingHost, Execution, HostCapabilities, NativeArg, NativeCallError, NativeResult,
    NativeReturn, NativeStateError, NativeStateHost, NativeStateTypeId, NativeStateValue,
};

use crate::{VmError, execute};

const STATE_TYPE: NativeStateTypeId = NativeStateTypeId::new(0x0500_0000_0000_0000);

fn module(code: Vec<I>, locals: u16) -> Module {
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

fn hybrid_module(code: Vec<I>, locals: u16) -> Module {
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
                    path: PlacePath::new(Vec::new()).expect("an empty writeback path"),
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
    let field_zero = FieldPath::new(vec![0]).expect("one field");
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
