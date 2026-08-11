//! The environment capability: what the process was started with.
//!
//! An environment variable is not a filesystem read and not a compiler service,
//! but it reaches a Kira program the same way both of those do — as an
//! intrinsic the VM routes through its host and native code routes through a
//! `kira_rt_*` symbol. It gets that treatment for one reason: the alternative
//! is every caller declaring `getenv` as an `@FFI.Extern`, and a library that
//! does binds a C symbol through whichever native library it happened to name.
//! The UI Foundation did exactly that — it read `KIRA_UI_*` variables through
//! `library: kira_metal`, so a compositor that wanted to know whether a debug
//! flag was set could not link on a machine with no Metal.
//!
//! # Nothing here fails
//!
//! An unset variable is not an error, it is an answer: [`EnvOp::Text`] gives
//! back the empty string and [`EnvOp::IsSet`] gives back `false`. Both are
//! offered, because "" and unset are different states and a program that can
//! only ask for the text cannot tell them apart — `KIRA_UI_TRACE=` is a
//! deliberate empty setting, not an absent one.
//!
//! # Read, never written
//!
//! There is no set operation, and adding one would need a reason this does not
//! have. The environment is process-wide mutable state shared with every
//! library in the address space, and `setenv` is not thread-safe against a
//! concurrent `getenv` on any platform Kira targets.

use std::ffi::OsString;

/// Which process/environment operation one `EnvOp` instruction performs.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `EnvOp` bytecode instruction, so they are **append-only** — a new
/// operation takes the next free number and no existing one ever moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EnvOp {
    /// The variable's value, empty when it is unset.
    Text = 0,
    /// Whether the variable is set at all, however empty its value.
    IsSet = 1,
    /// The number of user arguments passed to the process, excluding its path.
    ArgumentCount = 2,
    /// One user argument by zero-based index, or an empty string out of range.
    Argument = 3,
    /// Pause the current process for a number of milliseconds.
    Sleep = 4,
}

impl EnvOp {
    /// Every operation, in wire order.
    ///
    /// The one place the set is written down: decoding indexes this rather than
    /// repeating a match, so a new operation cannot be added to the enum and
    /// forgotten by the decoder.
    pub const ALL: [EnvOp; 5] = [
        EnvOp::Text,
        EnvOp::IsSet,
        EnvOp::ArgumentCount,
        EnvOp::Argument,
        EnvOp::Sleep,
    ];

    /// The wire byte this operation travels as.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no operation.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// How many operands this operation pops, in source order.
    pub const fn arity(self) -> usize {
        match self {
            EnvOp::Text | EnvOp::IsSet => 1,
            EnvOp::ArgumentCount => 0,
            EnvOp::Argument | EnvOp::Sleep => 1,
        }
    }

    /// The Kira intrinsic name that compiles to this operation.
    pub const fn intrinsic_name(self) -> &'static str {
        match self {
            EnvOp::Text => "envText",
            EnvOp::IsSet => "envIsSet",
            EnvOp::ArgumentCount => "envArgumentCount",
            EnvOp::Argument => "envArgument",
            EnvOp::Sleep => "processSleep",
        }
    }

    /// The `kira_rt_*` symbol native code calls to perform this operation.
    ///
    /// Derived from the operation rather than written twice, so the backend's
    /// declaration and the runtime's definition cannot drift apart.
    pub const fn runtime_symbol(self) -> &'static str {
        match self {
            EnvOp::Text => "kira_rt_env_text",
            EnvOp::IsSet => "kira_rt_env_is_set",
            EnvOp::ArgumentCount => "kira_rt_env_argument_count",
            EnvOp::Argument => "kira_rt_env_argument",
            EnvOp::Sleep => "kira_rt_process_sleep",
        }
    }

    /// Resolves a Kira intrinsic name to its operation, or `None`.
    pub fn from_intrinsic_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.intrinsic_name() == name)
    }
}

/// The environment keys used to carry arguments through an embedded run.
///
/// A native executable gets its arguments from the operating system normally.
/// The VM and Hybrid engines, however, often run inside a launcher process
/// whose own flags are not the Kira program's arguments. The CLI installs this
/// scoped override so both copies of the native bridge — including one linked
/// into a Hybrid shared library — observe the same argument list.
const ARGUMENT_COUNT_KEY: &str = "KIRA_RUNTIME_ARGUMENT_COUNT";
const ARGUMENT_KEY_PREFIX: &str = "KIRA_RUNTIME_ARGUMENT_";

/// Runs `body` with `arguments` as the process arguments visible to Kira code.
///
/// This changes process environment variables for the duration of the body so
/// a separately linked native bridge can observe the same list as the VM. The
/// caller must ensure no other thread reads or mutates the process environment
/// during the scope; that is why this API is `unsafe`. The CLI satisfies that
/// contract around its single-threaded launch/run boundary.
///
/// The previous values are restored even when `body` unwinds.
///
/// # Safety
///
/// No other thread may access or mutate the process environment for the whole
/// duration of `body`, including while a native bridge is executing.
pub unsafe fn with_arguments<R>(arguments: &[String], body: impl FnOnce() -> R) -> R {
    let guard = ArgumentOverride::install(arguments);
    let result = body();
    drop(guard);
    result
}

/// Restores the environment after [`with_arguments`] returns or unwinds.
struct ArgumentOverride {
    previous: Vec<(String, Option<OsString>)>,
}

impl ArgumentOverride {
    fn install(arguments: &[String]) -> Self {
        let mut previous = Vec::with_capacity(arguments.len() + 1);
        remember_and_set(
            &mut previous,
            ARGUMENT_COUNT_KEY,
            &arguments.len().to_string(),
        );
        for (index, argument) in arguments.iter().enumerate() {
            let key = format!("{ARGUMENT_KEY_PREFIX}{index}");
            remember_and_set(&mut previous, &key, argument);
        }
        Self { previous }
    }
}

impl Drop for ArgumentOverride {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            // SAFETY: `ArgumentOverride` can only be installed by the unsafe
            // scoped API. Its caller promised that no other thread accesses
            // the process environment while the guard is alive.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(&key, value),
                    None => std::env::remove_var(&key),
                }
            }
        }
    }
}

fn remember_and_set(previous: &mut Vec<(String, Option<OsString>)>, key: &str, value: &str) {
    previous.push((key.to_owned(), std::env::var_os(key)));
    // SAFETY: the caller of `with_arguments` guarantees exclusive access to
    // process environment for the lifetime of the override guard.
    unsafe { std::env::set_var(key, value) };
}

/// Reads `name` from the process environment.
///
/// One definition for every engine: the VM calls it from its interpreter and
/// native code reaches it through `kira_rt_env_text`, so a program cannot get
/// one answer on one backend and another on the next.
///
/// A name holding an interior NUL, or a value that is not UTF-8, reads as
/// unset — neither is something a Kira `String` can carry, and inventing a
/// lossy answer would be worse than saying there is nothing there.
#[must_use]
pub fn text(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Whether `name` is set, however empty its value.
#[must_use]
pub fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

/// The number of user arguments passed to the process, excluding its path.
#[must_use]
pub fn argument_count() -> i64 {
    if let Some(count) = overridden_argument_count() {
        return count as i64;
    }
    std::env::args().skip(1).count() as i64
}

/// One user argument by zero-based index, or an empty string out of range.
#[must_use]
pub fn argument(index: i64) -> String {
    if index < 0 {
        return String::new();
    }
    if let Some(count) = overridden_argument_count() {
        if (index as usize) >= count {
            return String::new();
        }
        return std::env::var(format!("{ARGUMENT_KEY_PREFIX}{index}")).unwrap_or_default();
    }
    std::env::args()
        .skip(1)
        .nth(index as usize)
        .unwrap_or_default()
}

/// Pause the current process for a number of milliseconds.
pub fn sleep(milliseconds: i64) {
    if milliseconds <= 0 {
        return;
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::sleep(std::time::Duration::from_millis(milliseconds as u64));
}

fn overridden_argument_count() -> Option<usize> {
    std::env::var(ARGUMENT_COUNT_KEY)
        .ok()
        .and_then(|count| count.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_bytes_are_pinned() {
        // These travel in an operand byte that artifacts already hold.
        assert_eq!(EnvOp::Text.as_byte(), 0);
        assert_eq!(EnvOp::IsSet.as_byte(), 1);
        assert_eq!(EnvOp::ArgumentCount.as_byte(), 2);
        assert_eq!(EnvOp::Argument.as_byte(), 3);
        assert_eq!(EnvOp::Sleep.as_byte(), 4);
        assert_eq!(EnvOp::from_byte(0), Some(EnvOp::Text));
        assert_eq!(EnvOp::from_byte(1), Some(EnvOp::IsSet));
        assert_eq!(EnvOp::from_byte(2), Some(EnvOp::ArgumentCount));
        assert_eq!(EnvOp::from_byte(3), Some(EnvOp::Argument));
        assert_eq!(EnvOp::from_byte(4), Some(EnvOp::Sleep));
        assert_eq!(EnvOp::from_byte(5), None);
    }

    #[test]
    fn every_operation_round_trips_through_its_names() {
        for op in EnvOp::ALL {
            assert_eq!(EnvOp::from_intrinsic_name(op.intrinsic_name()), Some(op));
            assert_eq!(EnvOp::from_byte(op.as_byte()), Some(op));
            let symbol = op.runtime_symbol();
            assert!(symbol.starts_with("kira_rt_env_") || symbol == "kira_rt_process_sleep");
        }
    }

    #[test]
    fn an_unset_variable_is_an_answer_rather_than_a_failure() {
        let absent = "KIRA_TEST_ENV_DEFINITELY_UNSET";
        assert_eq!(text(absent), "");
        assert!(!is_set(absent));
    }

    #[test]
    fn a_variable_set_to_nothing_is_still_set() {
        // The distinction the two operations exist to draw: `KIRA_UI_TRACE=` is
        // a deliberate empty setting, not an absent one.
        let name = "KIRA_TEST_ENV_SET_TO_EMPTY";
        // SAFETY: this process's own variable, named for this test alone, and
        // read back on this thread before anything else can observe it.
        unsafe { std::env::set_var(name, "") };
        assert_eq!(text(name), "");
        assert!(is_set(name));
        // SAFETY: as above — removing what this test just set.
        unsafe { std::env::remove_var(name) };
    }

    #[test]
    fn a_scoped_argument_override_is_visible_and_restored() {
        let arguments = vec!["first".to_owned(), "second value".to_owned()];
        let previous_count = std::env::var_os(ARGUMENT_COUNT_KEY);
        // SAFETY: this test performs no concurrent environment access while
        // the scoped override is installed.
        unsafe {
            with_arguments(&arguments, || {
                assert_eq!(argument_count(), 2);
                assert_eq!(argument(0), "first");
                assert_eq!(argument(1), "second value");
                assert_eq!(argument(2), "");
            });
        }
        assert_eq!(std::env::var_os(ARGUMENT_COUNT_KEY), previous_count);
    }
}
