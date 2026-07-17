//! Parity for strings, integers, arithmetic, traps, and booleans.

use crate::assert_parity;

#[test]
fn prints_a_string_literal() {
    assert_parity(r#"@Main function main() { print("hello from Kira") return }"#);
}

#[test]
fn prints_integers_including_the_extremes() {
    assert_parity(
        r#"@Main function main() {
            print(0)
            print(1)
            print(-1)
            print(42)
            print(-9223372036854775807 - 1)
            print(9223372036854775807)
            return
        }"#,
    );
}

#[test]
fn integer_arithmetic_wraps_like_the_vm() {
    assert_parity(
        r#"@Main function main() {
            print(9223372036854775807 + 1)
            print(-9223372036854775807 - 2)
            print(9223372036854775807 * 2)
            print((-9223372036854775807 - 1) / -1)
            print((-9223372036854775807 - 1) % -1)
            print(7 / 2)
            print(-7 / 2)
            print(7 % 3)
            print(-7 % 3)
            return
        }"#,
    );
}

#[test]
fn division_by_zero_traps_the_same_way() {
    assert_parity(
        r#"@Main function main() {
            print("before")
            print(1 / 0)
            return
        }"#,
    );
}

#[test]
fn remainder_by_zero_traps_the_same_way() {
    assert_parity(
        r#"@Main function main() {
            print("before")
            print(1 % 0)
            return
        }"#,
    );
}

#[test]
fn prints_booleans_and_comparisons() {
    assert_parity(
        r#"@Main function main() {
            print(true)
            print(false)
            print(1 < 2)
            print(2 <= 2)
            print(3 > 4)
            print(!true)
            print(true && false)
            print(true || false)
            print("a" == "a")
            print("a" != "b")
            return
        }"#,
    );
}

#[test]
fn short_circuit_operators_skip_their_right_operand() {
    // The right operand traps, so a backend that evaluated it eagerly would
    // disagree with the VM by dying instead of printing `false`.
    assert_parity(
        r#"@Main function main() {
            print(false && (1 / 0) == 0)
            print(true || (1 / 0) == 0)
            return
        }"#,
    );
}

#[test]
fn concatenates_and_compares_strings() {
    assert_parity(
        r#"@Main function main() {
            let greeting = "hello"
            let subject = "kira"
            print(greeting + " " + subject)
            print(banner())
            print(greeting == "hello")
            print(greeting == subject)
            print("")
            print("" + "")
            return
        }
        function banner() -> String { return "one source" + ", many backends" }"#,
    );
}

#[test]
fn runs_loops_and_mutation() {
    assert_parity(
        r#"@Main function main() {
            var i = 0
            var sum = 0
            while i < 10 {
                sum = sum + i
                i = i + 1
            }
            print(sum)
            var countdown = 3
            while countdown > 0 {
                print(countdown)
                countdown = countdown - 1
            }
            print(sum > 40 && sum < 50)
            return
        }"#,
    );
}

#[test]
fn runs_recursion() {
    assert_parity(
        r#"@Main function main() {
            print(fib(10))
            print(fib(20))
            return
        }
        function fib(n: Int) -> Int {
            if n < 2 { return n }
            return fib(n - 1) + fib(n - 2)
        }"#,
    );
}
