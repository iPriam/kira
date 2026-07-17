//! Parity for float arithmetic and formatting.

use crate::assert_parity;

#[test]
fn prints_floats_the_way_rust_displays_them() {
    // Every one of these is a place a hand-rolled formatter goes wrong: whole
    // floats that must not show a point, values JavaScript would render in
    // exponent notation, and a negative zero whose sign is observable.
    assert_parity(
        r#"@Main function main() {
            print(0.0)
            print(-0.0)
            print(1.0)
            print(2.5)
            print(-3.75)
            print(0.1)
            print(0.2)
            print(0.1 + 0.2)
            print(1.0 / 3.0)
            print(2.0 / 3.0)
            print(100.0)
            print(0.5)
            print(0.05)
            print(1.0 / 0.0)
            print(-1.0 / 0.0)
            print(0.0 / 0.0)
            return
        }"#,
    );
}

#[test]
fn prints_floats_at_the_extremes_of_the_format() {
    assert_parity(
        r#"@Main function main() {
            print(1.7976931348623157e308)
            print(-1.7976931348623157e308)
            print(5.0e-324)
            print(2.2250738585072014e-308)
            print(1.0e21)
            print(1.0e-7)
            print(123456789.123456789)
            print(4.9406564584124654e-324)
            return
        }"#,
    );
}

#[test]
fn float_arithmetic_agrees_with_the_vm() {
    assert_parity(
        r#"@Main function main() {
            var x = 0.0
            var i = 0
            while i < 10 {
                x = x + 0.1
                i = i + 1
            }
            print(x)
            print(x == 1.0)
            print(-x)
            print(x * 3.0)
            print(x / 7.0)
            return
        }"#,
    );
}
