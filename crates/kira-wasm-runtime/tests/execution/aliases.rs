//! Parity for `type Name = Target` aliases on wasm32 and wasm64.
//!
//! The wasm lowering has no alias case and never will: analysis resolves the
//! name before the IR exists. These cases are what makes that claim checkable
//! rather than asserted — an aliased program has to run identically on the VM
//! and in both address widths, which it can only do if the alias vanished.

use crate::assert_parity;

#[test]
fn an_alias_stands_in_for_a_builtin() {
    assert_parity(
        r#"type Count = Int
        @Main function main() {
            let n: Count = 7
            print(n * 6)
            return
        }"#,
    );
}

#[test]
fn a_chained_alias_names_a_nested_array() {
    assert_parity(
        r#"type Count = Int
        type Buffer = [Count]
        type Matrix = [Buffer]
        @Main function main() {
            var first: Buffer = []
            first.append(1)
            first.append(2)
            var rows: Matrix = []
            rows.append(move first)
            var sum = 0
            for row in rows {
                for value in row {
                    sum = sum + value
                }
                sum = sum + row.count
            }
            print(sum)
            return
        }"#,
    );
}

#[test]
fn an_alias_reaches_a_struct_field_and_a_signature() {
    assert_parity(
        r#"type Count = Int
        type Buffer = [Count]
        struct Packet {
            var payload: Buffer
        }
        function sum(buffer: borrow Buffer) -> Count {
            var total = 0
            for value in buffer {
                total = total + value
            }
            return total
        }
        @Main function main() {
            var payload: Buffer = []
            payload.append(4)
            payload.append(5)
            let packet = Packet { payload: move payload }
            print(sum(packet.payload))
            return
        }"#,
    );
}

#[test]
fn an_alias_names_an_enum_and_its_payload() {
    assert_parity(
        r#"type Label = String
        enum Tone {
            Quiet
            Named(Label)
        }
        type Mood = Tone
        @Main function main() {
            let quiet: Mood = .Quiet
            match quiet {
                Quiet -> print(0)
                Named(text) -> print(text.count)
            }
            let loud: Mood = .Named("loud")
            match loud {
                Quiet -> print(0)
                Named(text) -> print(text.count)
            }
            return
        }"#,
    );
}
