//! The native side of the hybrid boundary: how native code calls back into the
//! VM.
//!
//! In a hybrid program the two engines call each other in both directions.
//! Runtime-to-native is straightforward — the host resolves a trampoline symbol
//! out of the shared library and calls it. Native-to-runtime is the hard
//! direction, because the shared library is loaded *by* the host and cannot
//! link against it.
//!
//! Kira solves that by **installing an invoker** rather than by leaving an
//! undefined symbol. This crate is linked into the shared library, so the
//! library carries [`kira_hybrid_call_runtime`] (which generated native code
//! calls) and [`kira_hybrid_install_runtime_invoker`] (which the host calls once
//! after loading, handing over a function pointer). The library therefore has no
//! unresolved symbols and needs no `--export-dynamic` / `-undefined
//! dynamic_lookup` arrangement with the host — it is a plain, self-contained
//! dylib that happens to accept a callback.
//!
//! # Wire contract
//!
//! These symbols and signatures are shared with generated code and with the
//! host, and are append-only.

use std::sync::atomic::{AtomicPtr, Ordering};

use kira_runtime_abi::BridgeValue;

/// The host callback that runs a runtime (`@Runtime`) function.
///
/// # Safety
/// `args` must point at `count` readable [`BridgeValue`]s (or be null when
/// `count` is 0), and `out` must point at one writable [`BridgeValue`].
pub type RuntimeInvoker = unsafe extern "C" fn(
    function_id: u32,
    args: *const BridgeValue,
    count: u32,
    out: *mut BridgeValue,
);

/// The installed host invoker, or null before the host installs one.
///
/// An atomic rather than a `static mut`: the pointer is written by the host and
/// read by generated code, and a plain `static mut` would make every read a
/// data race the compiler is entitled to assume never happens.
static RUNTIME_INVOKER: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

/// Installs the host's runtime invoker; passing `None` clears it.
///
/// The host calls this once, right after loading the library and before running
/// anything.
///
/// # Safety
/// `invoker` must remain callable for as long as the library may call back —
/// that is, until it is cleared or the process exits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_hybrid_install_runtime_invoker(invoker: Option<RuntimeInvoker>) {
    let pointer = match invoker {
        Some(invoker) => invoker as *mut (),
        None => std::ptr::null_mut(),
    };
    RUNTIME_INVOKER.store(pointer, Ordering::Release);
}

/// Calls a runtime function from native code.
///
/// Generated native code calls this wherever a `@Native` function calls a
/// `@Runtime` one.
///
/// # Safety
/// `args` must point at `count` readable [`BridgeValue`]s (or be null when
/// `count` is 0), and `out` must point at one writable [`BridgeValue`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_hybrid_call_runtime(
    function_id: u32,
    args: *const BridgeValue,
    count: u32,
    out: *mut BridgeValue,
) {
    let pointer = RUNTIME_INVOKER.load(Ordering::Acquire);
    if pointer.is_null() {
        // Reaching a runtime call with no host is a broken hybrid build, not a
        // recoverable condition: the alternative is calling a null pointer.
        eprintln!(
            "kira: native code called runtime function {function_id} before a host \
             installed a runtime invoker"
        );
        std::process::abort();
    }
    // SAFETY: the pointer was installed from a `RuntimeInvoker` in
    // `kira_hybrid_install_runtime_invoker` and is only ever that type; the
    // caller upholds the args/out contract, which is the invoker's own.
    let invoker: RuntimeInvoker =
        unsafe { std::mem::transmute::<*mut (), RuntimeInvoker>(pointer) };
    // SAFETY: forwarding the caller's own guarantees unchanged.
    unsafe { invoker(function_id, args, count, out) };
}

/// Whether a host invoker is currently installed.
///
/// Exposed for the host's own assertions; generated code never calls it.
pub fn runtime_invoker_installed() -> bool {
    !RUNTIME_INVOKER.load(Ordering::Acquire).is_null()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::BridgeData;
    use std::sync::atomic::{AtomicU32, Ordering as TestOrdering};

    /// Records the last function id it was asked to run, and answers with the
    /// first argument doubled.
    static LAST_CALLED: AtomicU32 = AtomicU32::new(u32::MAX);

    unsafe extern "C" fn test_invoker(
        function_id: u32,
        args: *const BridgeValue,
        count: u32,
        out: *mut BridgeValue,
    ) {
        LAST_CALLED.store(function_id, TestOrdering::Release);
        // SAFETY: the caller passes `count` readable args and one writable out.
        unsafe {
            let doubled = match (count > 0).then(|| (*args).decode()).flatten() {
                Some(BridgeData::Int(value)) => BridgeData::Int(value * 2),
                _ => BridgeData::Void,
            };
            *out = BridgeValue::encode(doubled);
        }
    }

    #[test]
    fn an_installed_invoker_receives_the_call_and_answers() {
        // SAFETY: `test_invoker` is a `'static` function, callable for the whole
        // test, and it is cleared before returning.
        unsafe {
            kira_hybrid_install_runtime_invoker(Some(test_invoker));
            assert!(runtime_invoker_installed());

            let args = [BridgeValue::encode(BridgeData::Int(21))];
            let mut out = BridgeValue::VOID;
            kira_hybrid_call_runtime(7, args.as_ptr(), 1, &mut out);

            assert_eq!(LAST_CALLED.load(TestOrdering::Acquire), 7);
            assert_eq!(out.decode(), Some(BridgeData::Int(42)));

            kira_hybrid_install_runtime_invoker(None);
            assert!(!runtime_invoker_installed());
        }
    }
}
