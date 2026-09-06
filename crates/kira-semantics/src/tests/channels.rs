//! What the analyzer accepts and refuses in the channel surface.

use super::{codes, diagnostics};

/// A `@Main` around one body, with `import Foundation` for `attempt`'s shapes.
fn program(body: &str) -> String {
    format!("@Main function main() {{ {body} return }}")
}

#[test]
fn a_channel_yields_a_sender_whose_receiver_is_read_off_it() {
    assert!(
        diagnostics(&program(
            "let tx = Channel<Int>() let rx = tx.receiver tx.send(1) tx.close()"
        ))
        .is_empty()
    );
}

/// The payload is the type argument, so the call takes none.
#[test]
fn a_channel_takes_its_payload_as_a_type_argument_only() {
    assert_eq!(codes(&program("let tx = Channel<Int>(1)")), vec!["KSEM364"]);
    assert_eq!(codes(&program("let tx = Channel()")), vec!["KSEM364"]);
}

/// A queue slot is one machine word, so a payload with no word is refused by
/// name rather than queued as a pointer into the sender's heap.
#[test]
fn a_payload_with_no_machine_word_is_refused() {
    assert_eq!(
        codes(&program("let tx = Channel<String>()")),
        vec!["KSEM365"]
    );
}

/// The two ends are two types. A direction used the wrong way is a type error
/// here rather than a trap at run time.
#[test]
fn an_end_has_only_its_own_direction() {
    assert_eq!(
        codes(&program(
            "let tx = Channel<Int>() let rx = tx.receiver rx.send(1)"
        )),
        vec!["KSEM367"]
    );
    assert_eq!(
        codes(&program("let tx = Channel<Int>() let v = tx.receive()")),
        vec!["KSEM367"]
    );
    assert_eq!(
        codes(&program(
            "let tx = Channel<Int>() let rx = tx.receiver let again = rx.receiver"
        )),
        vec!["KSEM367"]
    );
}

/// An end is opaque. `.raw` would hand a program the table index and let it
/// forge an end, so it is refused even though the row is a `distinct`.
#[test]
fn an_end_does_not_expose_the_handle_underneath() {
    assert_eq!(
        codes(&program("let tx = Channel<Int>() print(tx.raw)")),
        vec!["KSEM367"]
    );
}

/// `send` carries exactly one value, checked against the payload.
#[test]
fn send_takes_one_value_of_the_payload_type() {
    assert_eq!(
        codes(&program("let tx = Channel<Int>() tx.send()")),
        vec!["KSEM366"]
    );
    assert_eq!(
        codes(&program("let tx = Channel<Int>() tx.send(1, 2)")),
        vec!["KSEM366"]
    );
    assert_eq!(
        codes(&program("let tx = Channel<Int>() tx.send(true)")),
        vec!["KSEM063"]
    );
}

/// `receive` and `close` take nothing.
#[test]
fn the_no_argument_operations_take_no_arguments() {
    assert_eq!(
        codes(&program("let tx = Channel<Int>() tx.close(1)")),
        vec!["KSEM366"]
    );
}

/// A receive is a fallible step, so it is written under `try` inside an
/// `attempt` like every other one, and its failure is covered by name.
#[test]
fn a_receive_is_a_fallible_step_with_one_failure() {
    let covered = "@Main function main() { \
                   let tx = Channel<Int>() let rx = tx.receiver \
                   attempt { let v = try rx.receive() print(v) } \
                   handle { Closed { print(0) } } return }";
    assert!(
        diagnostics(covered).is_empty(),
        "{:?}",
        diagnostics(covered)
    );

    // Uncovered, so the `attempt` does not handle everything it can fail with.
    let uncovered = "@Main function main() { \
                     let tx = Channel<Int>() let rx = tx.receiver \
                     attempt { let v = try rx.receive() print(v) } \
                     handle { } return }";
    assert_eq!(codes(uncovered), vec!["KSEM139"]);
}

/// An end reaches the task that uses it: it is one word, which is what a task
/// argument slot holds, and a `distinct` is erased before the slot exists.
#[test]
fn an_end_crosses_into_a_task() {
    let text = "async function chxFill(tx: Sender<Int>) -> Int { tx.send(1) return 0 }\n\
                @Main function main() { \
                let tx = Channel<Int>() var t = Task { chxFill(tx) } print(t.await) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A program may declare its own `Sender`, and it is not the compiler's.
#[test]
fn a_program_may_name_its_own_sender() {
    let text = "struct Sender { var n: Int }\n\
                @Main function main() { let own = Sender { n: 1 } print(own.n) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A payload leaves the context that sent it, which is the boundary `Send`
/// describes. The rule runs before the representation rule, exactly as the
/// task-slot pair runs it: what a value *is* is decided before what it is made
/// of, so a function type is refused for what it kept and not for its width.
#[test]
fn a_payload_that_cannot_move_between_contexts_is_refused() {
    let items = diagnostics(&program("let tx = Channel<() -> Void>()"));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM312"))
        .unwrap_or_else(|| panic!("expected a KSEM312, got {items:?}"));
    assert!(refusal.message.contains("cannot cross"), "{refusal:?}");
}

/// A channel over nothing carries nothing: `Void` is a scalar with no value in
/// it, so there is no `send` to write.
#[test]
fn a_channel_over_void_is_refused() {
    assert_eq!(codes(&program("let tx = Channel<Void>()")), vec!["KSEM365"]);
}

/// An end written as an annotation is the same channel, so a payload no
/// channel may carry is refused there under the same code rather than falling
/// through to "`Sender` is not generic".
///
/// Once, not once per pass: the annotation is one mistake, and the count is
/// asserted because type resolution reaches a written annotation more than
/// once.
#[test]
fn an_end_annotation_refuses_the_payloads_a_channel_refuses() {
    let text = "function take(rx: Receiver<String>) -> Int { return 0 }\n\
                @Main function main() { print(1) return }";
    let refusals = codes(text).iter().filter(|code| *code == "KSEM365").count();
    assert_eq!(refusals, 1, "{:?}", diagnostics(text));
}

/// The fall-through the owner-filed rows exist for: a program that declares
/// its own generic `Sender` keeps it, and the compiler's payload rule has no
/// say over a type that is not a channel end.
#[test]
fn a_program_may_name_its_own_generic_sender() {
    let text = "struct Sender<T> { let value: T }\n\
                @Main function main() { let own = Sender<String>(value: \"a\") \
                print(own.value) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}
