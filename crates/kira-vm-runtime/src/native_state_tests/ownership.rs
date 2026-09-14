use super::*;

#[test]
fn a_mutable_native_call_writes_a_recovered_view_back_to_state() {
    let module = hybrid_module(
        vec![
            I::ConstInt(7),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData { shared: false },
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
            I::TakeLocal(0),
            I::NativeStateRelease,
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
            I::NativeUserData { shared: false },
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
            I::TakeLocal(0),
            I::NativeStateRelease,
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
fn wrong_recovery_type_traps_and_an_affine_release_is_exactly_once() {
    let wrong = module(
        vec![
            I::ConstInt(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::NativeUserData { shared: false },
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

    // The affine owner is released exactly once: an explicit release *consumes*
    // the owner (a move out of the local), so the frame's own release finds the
    // slot empty and does not release a second time. A double free is refused
    // at compile time as a use after move, not defended against here — so this
    // proves the single release destroys the state and balances the store.
    let released = module(
        vec![
            I::ConstInt(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::NativeUserData { shared: false },
            I::StoreLocal(0),
            I::TakeLocal(0),
            I::NativeStateRelease,
            I::Pop,
            I::ReturnVoid,
        ],
        1,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    execute(&released, &mut host).expect("the owner releases cleanly");
    assert_eq!(host.store().live(), 0);
}

/// An affine owner is counted per holder: copying the handle into a second
/// local is a second owner, each local's frame release gives one back, and the
/// last release destroys the state — so a clean store is the proof the copies
/// balanced.
#[test]
fn copies_of_a_handle_are_counted_and_all_releases_destroy_the_state() {
    let module = module(
        vec![
            I::ConstInt(5),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            // The temporary handle's reference becomes local 0's.
            I::NativeUserData { shared: false },
            I::StoreLocal(0),
            // A copy into local 1 is a second owner of the same state.
            I::LoadLocal(0),
            I::StoreLocal(1),
            // Reading through either owner reaches the same value.
            I::LoadLocal(1),
            I::NativeRecover(STATE_TYPE.as_word()),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
        2,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    execute(&module, &mut host).expect("both owners release with the frame");
    assert_eq!(host.inner().lines(), ["5"]);
    // Both locals released with the frame; the last release destroyed the state.
    assert_eq!(host.store().live(), 0);
}

/// A handle a frame still holds when it returns gives up its reference with
/// the frame, while an owning userdata export handed back to the caller keeps
/// the state alive on its own.
///
/// `shared: false` is the affine-owner export. The local load contributes the
/// reference transferred into the returned handle, so local 0 releases its own
/// reference when the frame drops and exactly one owner survives.
#[test]
fn a_handle_dropped_with_its_frame_releases_one_owner() {
    let module = module(
        vec![
            I::ConstInt(3),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData { shared: false },
            I::Return,
        ],
        1,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    let outcome = execute(&module, &mut host).expect("the owner is returned");
    let crate::Value::NativeState(token) = outcome.result else {
        panic!("the exported userdata must return as an affine owner");
    };
    assert_eq!(host.store().owners(token), Ok(1));
    assert_eq!(host.store().live(), 1);
    host.native_state_release(token)
        .expect("the owner releases");
    assert_eq!(host.store().live(), 0);
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
            I::NativeUserData { shared: false },
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(2),
            // ... then rebind that same local to a view of the second.
            I::LoadLocal(1),
            I::NativeUserData { shared: false },
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(2),
            // The local now names the second state, and the first is untouched.
            I::LoadLocal(2),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeUserData { shared: false },
            I::NativeRecover(STATE_TYPE.as_word()),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::TakeLocal(0),
            I::NativeStateRelease,
            I::Pop,
            I::TakeLocal(1),
            I::NativeStateRelease,
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
            I::TakeLocal(1),
            I::NativeStateRelease,
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
        constants: Vec::new(),
        types: Vec::new(),
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
