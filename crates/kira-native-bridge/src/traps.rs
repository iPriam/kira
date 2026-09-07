//! The integer-width traps native code raises.
//!
//! Each is the native mirror of a VM error: the same condition, the same
//! message, and — like [`crate::runtime::kira_rt_trap_div_zero`] — no return,
//! so a program that overflows fails the same way on both engines.

use kira_runtime_abi::IntWidth;

/// The spelling a width code names, for a message; a bad code is a compiler
/// bug, named as such rather than hidden.
fn spelling(code: i32) -> &'static str {
    u8::try_from(code)
        .ok()
        .and_then(IntWidth::from_code)
        .map_or("an unknown width", IntWidth::name)
}

/// Integer arithmetic left the range of its spelling.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_overflow(width: i32) -> ! {
    eprintln!(
        "kira: runtime trap: integer overflow: the result does not fit `{}`",
        spelling(width)
    );
    std::process::exit(1);
}

/// A shift count outside `0..bits`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_shift(count: i64, bits: i32) -> ! {
    eprintln!("kira: runtime trap: shift count {count} is outside 0..{bits}");
    std::process::exit(1);
}

/// An integer conversion whose value the destination spelling cannot hold.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_narrow(word: i64, from: i32, to: i32) -> ! {
    let value = u8::try_from(from)
        .ok()
        .and_then(IntWidth::from_code)
        .map_or(i128::from(word), |from| from.value_of(word));
    eprintln!(
        "kira: runtime trap: integer conversion: {value} does not fit `{}`",
        spelling(to)
    );
    std::process::exit(1);
}

/// A float-to-integer conversion of NaN, an infinity, or a value outside the
/// integer range.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_float_to_int(value: f64) -> ! {
    eprintln!("kira: runtime trap: float to integer conversion: {value} has no integer value");
    std::process::exit(1);
}

/// An `as` cast of an erased value that holds a different type.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_cast(actual: i64, expected: i64) -> ! {
    eprintln!(
        "kira: runtime trap: type cast: the `Any` holds type identity {actual:#x}, not {expected:#x}"
    );
    std::process::exit(1);
}
