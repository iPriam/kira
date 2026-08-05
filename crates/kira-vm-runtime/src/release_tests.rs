//! What a returning frame releases, and where that answer comes from.
//!
//! The VM does not decide it. `kira_ir::mid` decides it for both engines and
//! the compiler writes the answer into the module, so these tests are about one
//! thing: that the module's answer is the one obeyed. Each builds a frame
//! holding a string and varies nothing but the plan, so a difference in heap
//! accounting can only have come from reading it.

use super::*;
use kira_bytecode::module::{FrameRelease, FuncProto, Module};
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

/// `main`: allocate a string into slot 0 and return unit.
///
/// Slot 0 is the frame's alone — nothing is written back and nothing is
/// returned — so at the moment of return the string is live exactly when the
/// plan does not name it.
fn holding_a_string(releases: FrameRelease) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        main: Some(0),
        strings: vec!["held".to_owned()],
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 1,
            execution: kira_runtime_abi::Execution::Runtime,
            code: vec![I::ConstStr(0), I::StoreLocal(0), I::ReturnVoid],
            releases,
        }],
    }
}

fn run(module: &Module) -> u64 {
    let mut host = CapturingHost::new();
    execute(module, &mut host).expect("clean run").heap.current
}

#[test]
fn a_planned_slot_is_released() {
    assert_eq!(run(&holding_a_string(FrameRelease::Planned(vec![0]))), 0);
}

/// The decisive one. The frame still holds the string, and the plan is the only
/// thing that changed — so a VM walking its locals would report a balanced heap
/// here too, and reporting an unbalanced one is the proof it read the plan.
#[test]
fn a_slot_the_plan_omits_is_left_alone() {
    assert!(
        run(&holding_a_string(FrameRelease::Planned(Vec::new()))) > 0,
        "an empty plan released a slot it did not name"
    );
}

/// A module carrying no plan — one built by hand, or written before the release
/// section existed — keeps the frame discipline the VM has always had.
#[test]
fn a_module_with_no_plan_releases_the_whole_frame() {
    assert_eq!(run(&holding_a_string(FrameRelease::EveryLocal)), 0);
}

/// A plan naming a slot the frame does not have is refused before the module
/// runs, rather than skipped while it does.
#[test]
fn a_plan_naming_no_local_is_refused_by_validation() {
    let module = holding_a_string(FrameRelease::Planned(vec![7]));
    assert_eq!(
        module.validate(),
        Err(kira_bytecode::ModuleValidateError::ReleaseSlotOutOfRange {
            function: "main".to_owned(),
            slot: 7,
        })
    );
}

/// A plan naming one slot twice would free it twice, so it is refused too.
#[test]
fn a_plan_repeating_a_slot_is_refused_by_validation() {
    let module = holding_a_string(FrameRelease::Planned(vec![0, 0]));
    assert_eq!(
        module.validate(),
        Err(kira_bytecode::ModuleValidateError::ReleasePlanUnordered {
            function: "main".to_owned(),
            slot: 0,
        })
    );
}
