//! The native half of the deferred-task spine: one symbol over one table.
//!
//! Generated native code reaches the executor through `kira_rt_task_op` and
//! nothing else, because the *scheduler* is not here — it is generated Kira the
//! IR synthesizes, compiled into the same executable as the rest of the
//! program. What this file owns is the task table, and it owns it by delegating
//! every question to [`kira_runtime_abi::TaskExecutor`], the same type the VM
//! holds. That is the whole parity argument: one table implementation, one
//! scheduler, two engines that only carry them.

use std::cell::RefCell;

use kira_runtime_abi::{TaskExecutor, TaskPrim};

thread_local! {
    /// The tasks this thread's program spawned.
    ///
    /// Thread-local rather than global because a handle is an index into one
    /// table: two threads sharing it would let one thread's handle name the
    /// other's task. A Kira program runs on one thread today, so this is one
    /// table in practice — but the scoping is the invariant, not the count.
    static TASKS: RefCell<TaskExecutor> = RefCell::new(TaskExecutor::new());
}

/// Starts a native task scope with the same empty table a VM run receives.
///
/// Native code can outlive one entrypoint when it is loaded as a hybrid
/// library, so a thread-local table cannot be allowed to turn into process
/// state. The host calls this before every run; the generated process entry
/// calls it for the executable path.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_task_reset() {
    TASKS.with_borrow_mut(|tasks| *tasks = TaskExecutor::new());
}

/// Carries out one deferred-task primitive.
///
/// `prim` is a [`TaskPrim`] wire byte and the three operands are the primitive's
/// arguments, zero where it takes fewer. The answer is the primitive's `Int`
/// result.
///
/// A trap here is the same trap the VM raises for the same program — awaiting a
/// cancelled task, joining twice — so the two engines fail on exactly the same
/// programs. It exits non-zero with a message rather than unwinding, the way
/// every other native trap does.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_task_op(prim: i64, a: i64, b: i64, c: i64) -> i64 {
    let Some(prim) = u8::try_from(prim).ok().and_then(TaskPrim::from_byte) else {
        // Only generated code writes this byte, so an unknown one means the
        // executable and this archive disagree — which is a link-time problem
        // reported at run time, not a program error.
        eprintln!("kira: runtime trap: unknown task primitive {prim}");
        std::process::exit(1);
    };
    match TASKS.with_borrow_mut(|tasks| tasks.perform(prim, a, b, c)) {
        Ok(answer) => answer,
        Err(trap) => {
            eprintln!("kira: runtime trap: {trap}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawn_round_trips_its_target_and_arguments_through_the_symbol() {
        let handle = kira_rt_task_op(i64::from(TaskPrim::Spawn.as_byte()), 2, 0, 0);
        kira_rt_task_op(i64::from(TaskPrim::SetArg.as_byte()), handle, 0, 41);
        assert_eq!(
            kira_rt_task_op(i64::from(TaskPrim::TargetOf.as_byte()), handle, 0, 0),
            2
        );
        assert_eq!(
            kira_rt_task_op(i64::from(TaskPrim::SlotGet.as_byte()), handle, 0, 0),
            41
        );
    }

    #[test]
    fn resetting_a_scope_drops_old_handles_and_starts_at_one() {
        let first = kira_rt_task_op(i64::from(TaskPrim::Spawn.as_byte()), 2, 0, 0);
        assert_eq!(first, 1);
        kira_rt_task_reset();
        assert_eq!(
            kira_rt_task_op(i64::from(TaskPrim::Spawn.as_byte()), 3, 0, 0),
            1
        );
    }

    #[test]
    fn a_join_drives_once_and_hands_back_the_completed_value() {
        let handle = kira_rt_task_op(i64::from(TaskPrim::Spawn.as_byte()), 1, 0, 0);
        assert_eq!(
            kira_rt_task_op(i64::from(TaskPrim::BeginJoin.as_byte()), handle, 0, 0),
            1
        );
        kira_rt_task_op(i64::from(TaskPrim::Complete.as_byte()), handle, 42, 0);
        assert_eq!(
            kira_rt_task_op(i64::from(TaskPrim::TakeResult.as_byte()), handle, 0, 0),
            42
        );
    }
}
