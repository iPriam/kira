//! Parity for `type Name = Target` aliases.
//!
//! An alias is resolved away in the frontend, so what these prove is a
//! *negative*: no backend ever sees one. The programs below are written twice
//! over — once through aliases and once spelling the targets out — and every
//! backend must produce the same answer for both, which is the only way to show
//! the alias changed nothing but the spelling.

use crate::assert_parity;

#[test]
fn an_alias_stands_in_for_a_builtin() {
    assert_parity(
        r#"type Count = Int
        @Main function main() {
            let n: Count = 7
            print(n + 1)
            return
        }"#,
    );
}

#[test]
fn an_alias_names_an_array_and_chains_through_another_alias() {
    assert_parity(
        r#"type Count = Int
        type Buffer = [Count]
        type Matrix = [Buffer]
        function total(rows: borrow Matrix) -> Int {
            var sum = 0
            for row in rows {
                for value in row {
                    sum = sum + value
                }
                sum = sum + row.count
            }
            return sum
        }
        @Main function main() {
            var first: Buffer = []
            first.append(1)
            first.append(2)
            var second: Buffer = []
            second.append(3)
            var rows: Matrix = []
            rows.append(move first)
            rows.append(move second)
            print(total(rows))
            return
        }"#,
    );
}

#[test]
fn an_alias_reaches_a_struct_field_a_parameter_and_a_return_type() {
    assert_parity(
        r#"type Count = Int
        type Buffer = [Count]
        struct Packet {
            var payload: Buffer
        }
        function build(size: Count) -> Packet {
            var payload: Buffer = []
            var i = 0
            while i < size {
                payload.append(i * 2)
                i = i + 1
            }
            return Packet { payload: move payload }
        }
        function sum(buffer: borrow Buffer) -> Count {
            var total = 0
            for value in buffer {
                total = total + value
            }
            return total
        }
        @Main function main() {
            let packet = build(4)
            print(sum(packet.payload))
            print(packet.payload.count)
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
        function describe(tone: Mood) -> Int {
            match tone {
                Quiet -> return 0
                Named(text) -> return text.count
            }
        }
        @Main function main() {
            let quiet: Mood = .Quiet
            print(describe(quiet))
            print(describe(.Named("loud")))
            return
        }"#,
    );
}

#[test]
fn an_aliased_program_matches_the_same_program_written_out() {
    let aliased = assert_parity(
        r#"type Count = Int
        type Buffer = [Count]
        @Main function main() {
            var xs: Buffer = []
            xs.append(10)
            xs.append(20)
            let n: Count = xs.count
            print(xs[0] + xs[1] + n)
            return
        }"#,
    );
    let spelled_out = assert_parity(
        r#"@Main function main() {
            var xs: [Int] = []
            xs.append(10)
            xs.append(20)
            let n: Int = xs.count
            print(xs[0] + xs[1] + n)
            return
        }"#,
    );
    assert_eq!(
        aliased, spelled_out,
        "an alias changed the program's meaning, not just its spelling",
    );
}

#[test]
fn an_aliased_index_still_traps_out_of_bounds_on_every_backend() {
    crate::assert_trap_parity(
        r#"type Buffer = [Int]
        @Main function main() {
            var xs: Buffer = []
            xs.append(1)
            print(xs[0])
            print(xs[1])
            return
        }"#,
        "1\n",
    );
}
