//! The native half of the `Number` operations.
//!
//! A `Number` crosses native code as a two-word value, not a handle: its
//! [`Decimal`](kira_runtime_abi::Decimal) mantissa and scale are packed into an
//! `i128` the generated code holds in the slot and passes in a register pair.
//! There is no allocation, no share count, and no clone or free — a `Number` is
//! copied by its bits like an `Int`. Every helper answers exactly what the VM's
//! `perform_number_op` answers for the same input, because both compute through
//! the one [`Decimal`].

use kira_runtime_abi::{Decimal, DecimalError};

use crate::runtime::{KStr, bytes_of, bytes_to_handle, drop_handle, print_trap_backtrace};

/// A `Number` as generated code holds it: the mantissa in the low 64 bits, the
/// scale in the high 64. Opaque to the backend, which only moves it around and
/// hands it back to these helpers.
pub type KNumber = i128;

/// Packs a decimal into the two-word value.
pub(crate) fn pack(decimal: Decimal) -> KNumber {
    let mantissa = decimal.mantissa() as u64 as u128;
    let scale = u128::from(decimal.scale()) << 64;
    (scale | mantissa) as i128
}

/// Unpacks the two-word value into a decimal.
pub(crate) fn unpack(value: KNumber) -> Decimal {
    let bits = value as u128;
    Decimal::from_parts(bits as u64 as i64, (bits >> 64) as u32)
}

/// Ends the program on a decimal error, the same trap the VM raises.
fn trap(error: DecimalError) -> ! {
    eprintln!("kira: runtime trap: {}", error.message());
    print_trap_backtrace();
    std::process::exit(1);
}

/// Packs a fallible decimal result, trapping on error.
fn pack_or_trap(result: Result<Decimal, DecimalError>) -> KNumber {
    match result {
        Ok(decimal) => pack(decimal),
        Err(error) => trap(error),
    }
}

/// An `Int` as an exact `Number`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_from_int(value: i64) -> KNumber {
    pack(Decimal::from_i64(value))
}

/// A decimal string as a `Number`, freeing the string and trapping on text that
/// does not read as a decimal.
///
/// # Safety
/// `text` must be null or a live string handle; it is freed here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_number_from_string(text: KStr) -> KNumber {
    // SAFETY: caller passes a live (or null) handle that outlives this read.
    let parsed = Decimal::parse(&String::from_utf8_lossy(unsafe { bytes_of(text) }));
    // SAFETY: the same handle, consumed exactly once here.
    unsafe { drop_handle(text) };
    pack_or_trap(parsed)
}

/// A `Float` as the nearest `Number`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_from_float(value: f64) -> KNumber {
    pack_or_trap(Decimal::from_f64(value))
}

/// The `Number` as its shortest exact decimal text.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_to_string(value: KNumber) -> KStr {
    bytes_to_handle(unpack(value).to_decimal_string().into_bytes())
}

/// The `Number` truncated toward zero to an `Int`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_to_int(value: KNumber) -> i64 {
    match unpack(value).to_i64() {
        Ok(value) => value,
        Err(error) => trap(error),
    }
}

/// The `Number` as the nearest `Float`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_to_float(value: KNumber) -> f64 {
    unpack(value).to_f64()
}

/// One `Number` negated, trapping when the value has no negative in range.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_negate(value: KNumber) -> KNumber {
    pack_or_trap(unpack(value).negate())
}

/// Builds a binary arithmetic helper that traps on a decimal error.
macro_rules! binary_arithmetic {
    ($name:ident, $method:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(left: KNumber, right: KNumber) -> KNumber {
            pack_or_trap(unpack(left).$method(unpack(right)))
        }
    };
}

binary_arithmetic!(kira_rt_number_add, add);
binary_arithmetic!(kira_rt_number_subtract, subtract);
binary_arithmetic!(kira_rt_number_multiply, multiply);
binary_arithmetic!(kira_rt_number_divide, divide);

/// Builds a comparison helper answering a `bool`.
macro_rules! comparison {
    ($name:ident, $predicate:expr) => {
        #[unsafe(no_mangle)]
        pub extern "C" fn $name(left: KNumber, right: KNumber) -> bool {
            match unpack(left).compare(unpack(right)) {
                Ok(ordering) => ($predicate)(ordering),
                Err(error) => trap(error),
            }
        }
    };
}

comparison!(kira_rt_number_less, |o: core::cmp::Ordering| o.is_lt());
comparison!(kira_rt_number_less_or_equal, |o: core::cmp::Ordering| o.is_le());
comparison!(kira_rt_number_greater, |o: core::cmp::Ordering| o.is_gt());
comparison!(kira_rt_number_greater_or_equal, |o: core::cmp::Ordering| o.is_ge());

/// Whether two `Number`s are numerically equal.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_number_equal(left: KNumber, right: KNumber) -> bool {
    match unpack(left).equals(unpack(right)) {
        Ok(equal) => equal,
        Err(error) => trap(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::kira_rt_str_new;

    #[test]
    fn packing_round_trips() {
        for decimal in [
            Decimal::from_i64(0),
            Decimal::from_i64(-1),
            Decimal::from_parts(123_456, 3),
            Decimal::from_parts(i64::MAX, 18),
            Decimal::from_parts(i64::MIN, 0),
        ] {
            assert_eq!(unpack(pack(decimal)), decimal);
        }
    }

    #[test]
    fn the_tenths_add_exactly_through_the_values() {
        // SAFETY: fresh string handles, consumed by from_string.
        let a = unsafe { kira_rt_number_from_string(kira_rt_str_new(c"0.1".as_ptr().cast(), 3)) };
        let b = unsafe { kira_rt_number_from_string(kira_rt_str_new(c"0.2".as_ptr().cast(), 3)) };
        let sum = kira_rt_number_add(a, b);
        let text = kira_rt_number_to_string(sum);
        // SAFETY: a live string handle.
        let bytes = unsafe { bytes_of(text) }.to_vec();
        // SAFETY: consumed once.
        unsafe { drop_handle(text) };
        assert_eq!(bytes, b"0.3");
    }

    #[test]
    fn hundred_over_four_is_twenty_five() {
        let q = kira_rt_number_divide(kira_rt_number_from_int(100), kira_rt_number_from_int(4));
        assert_eq!(kira_rt_number_to_int(q), 25);
    }
}
