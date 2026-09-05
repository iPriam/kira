//! Parity for the deferred async task spine.
//!
//! The spine's scheduler is generated IR rather than runtime code, so what
//! these prove is that the generation *reaches* all three engines: the VM
//! interprets `TaskOp`, native code calls `kira_rt_task_op`, and a hybrid build
//! puts the whole spine in one half. A divergence here would mean one engine
//! carries the table differently, which is exactly the failure the single
//! shared `TaskExecutor` exists to make impossible.
//!
//! Interleaving is deliberately observable in the cases below: `asyncStepper`
//! yields twice and three of them run at once, so a scheduler that ran tasks in
//! a different order would still add up the same — which is why the yielding
//! cases also *print* from inside the bodies, where order shows.

use crate::{assert_parity, assert_parity_with_heap_balance, assert_trap_parity};

/// The async bodies every case below spawns.
const BODIES: &str = r#"
async function tskSum(a: Int, b: Int) -> Int {
    return a + b
}
function tskFactorialOf(n: Int) -> Int {
    if n <= 1 {
        return 1
    }
    return n * tskFactorialOf(n - 1)
}
async function tskFactorial(n: Int) -> Int {
    return tskFactorialOf(n)
}
async function tskNoop(n: Int) {
    let unused = n + 1
    return
}
"#;

/// Builds a program from [`BODIES`] plus a `@Main` body.
fn program(body: &str) -> String {
    format!("{BODIES}\n@Main function main() {{\n{body}\n    return\n}}")
}

#[test]
fn an_async_function_runs_as_a_task_and_joins_for_its_result() {
    let output = assert_parity(&program(
        "    let sum = Task { tskSum(19, 23) }\n    let product = Task { tskFactorial(5) }\n    print(sum.await)\n    print(product.await)",
    ));
    assert_eq!(output, "42\n120\n");
}

#[test]
fn a_spawned_task_runs_at_its_join_and_not_before() {
    let output = assert_parity(&program(
        r#"    let handle = Task { tskFactorial(5) }
    print(1)
    print(handle.await)"#,
    ));
    // `1` first: the body ran at the `.await`, not at the spawn.
    assert_eq!(output, "1\n120\n");
}

#[test]
fn a_literal_task_body_joins_as_that_literal() {
    let output = assert_parity(&program(
        "    let handle = Task { 41 }\n    print(handle.await + 1)",
    ));
    assert_eq!(output, "42\n");
}

#[test]
fn a_void_task_joins_as_zero() {
    let output = assert_parity(&program(
        "    let handle = Task { tskNoop(5) }\n    print(handle.await)",
    ));
    assert_eq!(output, "0\n");
}

#[test]
fn a_cancelled_task_never_runs_and_leaves_a_sibling_join_intact() {
    let output = assert_parity(&format!(
        r#"async function tskLoud(n: Int) -> Int {{
            print(n)
            return n
        }}
        {BODIES}
        @Main function main() {{
            let joined = Task {{ tskSum(100, 40) }}
            let cancelled = Task {{ tskLoud(7) }}
            cancelled.requestCancel()
            print(joined.await + 2)
            return
        }}"#
    ));
    // `7` is absent: a task cancelled before its first drive never runs.
    assert_eq!(output, "142\n");
}

#[test]
fn a_detached_task_runs_and_discards_its_result() {
    let output = assert_parity(&format!(
        r#"async function tskLoud(n: Int) -> Int {{
            print(n)
            return n
        }}
        {BODIES}
        @Main function main() {{
            let detached = Task {{ tskLoud(7) }}
            detached.detach()
            print(1)
            return
        }}"#
    ));
    assert_eq!(output, "7\n1\n");
}

#[test]
fn a_yield_hands_the_next_queued_task_a_turn_in_spawn_order() {
    let output = assert_parity(&format!(
        r#"async function tskStep(tag: Int) -> Int {{
            print(tag)
            taskYield()
            print(tag + 1)
            return tag
        }}
        {BODIES}
        @Main function main() {{
            let first = Task {{ tskStep(10) }}
            let second = Task {{ tskStep(20) }}
            let third = Task {{ tskStep(30) }}
            print(first.await + second.await + third.await)
            return
        }}"#
    ));
    // Joining `first` drives it; its yield hands `second` a turn, whose yield
    // hands `third` one, and each finishes as the stack unwinds. Every engine
    // has to agree on that order, not just on the sum.
    assert_eq!(output, "10\n20\n30\n31\n21\n11\n60\n");
}

#[test]
fn a_yield_outside_a_task_body_is_a_no_op() {
    let output = assert_parity(&program("    taskYield()\n    print(1)"));
    assert_eq!(output, "1\n");
}

#[test]
fn a_sleep_parks_and_resumes_with_its_locals_intact() {
    let output = assert_parity(&format!(
        r#"async function tskNap(ms: Int, value: Int) -> Int {{
            taskSleep(ms)
            return value + 2
        }}
        {BODIES}
        @Main function main() {{
            let slow = Task {{ tskNap(3, 40) }}
            let quick = Task {{ tskNap(1, 2) }}
            print(slow.await * 10 + quick.await)
            return
        }}"#
    ));
    assert_eq!(output, "424\n");
}

#[test]
fn a_join_inside_a_task_body_completes_its_target_first() {
    let output = assert_parity(&format!(
        r#"async function tskInner(base: Int) -> Int {{
            taskYield()
            return base + 10
        }}
        async function tskOuter(base: Int) -> Int {{
            let worker = Task {{ tskInner(base) }}
            return worker.await + 1
        }}
        {BODIES}
        @Main function main() {{
            let outer = Task {{ tskOuter(100) }}
            let bystander = Task {{ tskSum(1, 2) }}
            print(outer.await + bystander.await)
            return
        }}"#
    ));
    assert_eq!(output, "114\n");
}

#[test]
fn a_float_task_joins_as_a_float() {
    let output = assert_parity(&format!(
        r#"async function tskScale(value: Float) -> Float {{
            return value * 2.0
        }}
        {BODIES}
        @Main function main() {{
            let handle = Task {{ tskScale(1.5) }}
            print(handle.await)
            return
        }}"#
    ));
    assert_eq!(output, "3\n");
}

#[test]
fn awaiting_a_cancelled_task_traps_on_every_engine() {
    assert_trap_parity(
        &program(
            r#"    let handle = Task { tskSum(40, 2) }
    handle.requestCancel()
    print(handle.await)"#,
        ),
        "",
    );
}

#[test]
fn joining_twice_traps_on_every_engine() {
    // The first join succeeded, so the trap is the second one and nothing else.
    assert_trap_parity(
        &program(
            r#"    let handle = Task { tskSum(40, 2) }
    print(handle.await)
    print(handle.await)"#,
        ),
        "42\n",
    );
}

#[test]
fn joining_after_a_detach_traps_on_every_engine() {
    assert_trap_parity(
        &program(
            r#"    let handle = Task { tskSum(40, 2) }
    handle.detach()
    print(handle.await)"#,
        ),
        "",
    );
}

/// A channel orders two contexts against each other identically on every
/// backend, with the heap balanced.
///
/// The receive is the point: it comes back after the task that fills it has
/// run, because an empty live channel hands the next runnable task a turn
/// rather than spinning. That policy is one synthesized IR function both
/// engines run, so this is the two of them agreeing by construction.
#[test]
fn a_channel_orders_two_contexts_on_every_backend() {
    let output = assert_parity_with_heap_balance(
        r#"
import Foundation

async function fill(tx: Sender<Int>) -> Int {
    tx.send(41)
    tx.send(1)
    tx.close()
    return 7
}

@Main
function main() {
    let tx = Channel<Int>()
    let rx = tx.receiver
    var pending = Task { fill(tx) }
    attempt {
        let first = try rx.receive()
        let second = try rx.receive()
        print(first + second)
        let past = try rx.receive()
        print(past)
    } handle {
        Closed { print(0 - 1) }
    }
    print(pending.await)
    return
}
"#,
    );
    assert_eq!(output, "42\n-1\n7\n");
}

/// A receive nothing can ever answer traps identically on every backend,
/// rather than hanging.
///
/// The queue is empty, the sender is live, and no other work is runnable, so no
/// future turn can change the answer. Waiting forever is a hang, and answering
/// `Closed` would tell the program the sender went away when it did not.
#[test]
fn a_receive_nothing_can_answer_traps_on_every_backend() {
    assert_trap_parity(
        r#"
import Foundation

@Main
function main() {
    print(1)
    let tx = Channel<Int>()
    let rx = tx.receiver
    attempt {
        let value = try rx.receive()
        print(value)
    } handle {
        Closed { print(77) }
    }
    return
}
"#,
        "1\n",
    );
}
