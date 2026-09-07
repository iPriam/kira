//! Which session is this thread's, and the invoker the native half calls back
//! through.
//!
//! The two belong together: the invoker is installed for exactly as long as a
//! session is the active one, so the library never holds a callback that
//! outlives the session it would reach.

use super::*;

/// Marks a session as this thread's for as long as it is alive.
///
/// Installs the invoker on the way in and clears it on the way out, so the
/// library never holds a callback that outlives the session it would reach.
pub(super) struct ActiveSession<'a> {
    pub(super) session: &'a Session,
    pub(super) previous: *const Session,
}

impl<'a> ActiveSession<'a> {
    pub(super) fn install(session: &'a Session) -> ActiveSession<'a> {
        let previous = ACTIVE_SESSION.replace(session);
        // SAFETY: `invoke_runtime` is a `'static` function, so it stays
        // callable for the process's life; `Drop` clears it before this
        // session's borrow ends regardless.
        unsafe { session.library.install_invoker(Some(invoke_runtime)) };
        ActiveSession { session, previous }
    }

    /// Marks only the current thread as active without replacing the loaded
    /// library's process-wide invoker.
    ///
    /// A split main-thread run has one session guard on the caller thread and
    /// may enter native code on the helper thread. The invoker is already
    /// installed by the caller guard; this guard gives callbacks on the helper
    /// the same session pointer without racing the installation lifetime.
    pub(super) fn bind(session: &'a Session) -> BoundSession {
        BoundSession {
            previous: ACTIVE_SESSION.replace(session),
        }
    }
}

impl Drop for ActiveSession<'_> {
    fn drop(&mut self) {
        // Clearing this session's invoker is not enough on its own. `previous`
        // says whether an outer session is still running, and if one is, it has
        // to be left able to call back: when both sessions share a library, the
        // clear below has just removed the outer session's invoker too, and the
        // next `@Native` function to reach a `@Runtime` one would abort on an
        // invoker that was installed all along.
        //
        // So clear ours, then re-install for the outer session's own library.
        // That is right whether the two share a library — the re-install undoes
        // the clear — or hold different ones, where the inner library is left
        // unable to call back, which is what it should be once its session ends.
        //
        // Nested sessions are what a `@Native` function that runs another
        // program produces. Rare, and the failure it causes is the loud kind.
        //
        // SAFETY: `run` has returned for this session, so no native code of its
        // library is on the stack and nothing can be mid-callback. The outer
        // session is still borrowed by the frame that installed it, so its
        // library is live, and `invoke_runtime` is `'static`.
        unsafe { self.session.library.install_invoker(None) };
        // SAFETY: `previous` is either null — handled by `as_ref` — or the
        // session the frame above this one installed, which is borrowed for
        // longer than this guard and so is still live.
        let previous = unsafe { self.previous.as_ref() };
        if let Some(previous) = previous {
            // SAFETY: that session is live per the borrow above, and
            // `invoke_runtime` is a `'static` function, so it stays callable for
            // as long as the library can reach it.
            unsafe { previous.library.install_invoker(Some(invoke_runtime)) };
        }
        ACTIVE_SESSION.set(self.previous);
    }
}

/// A thread-local session binding that does not touch the native invoker.
pub(super) struct BoundSession {
    previous: *const Session,
}

impl Drop for BoundSession {
    fn drop(&mut self) {
        ACTIVE_SESSION.set(self.previous);
    }
}

/// The native-to-runtime direction: what the library calls back through.
///
/// # Safety
/// `args` must point at `count` readable [`BridgeValue`]s (or be null when
/// `count` is 0), every string handle among them must be transferred to this
/// call, and `out` must point at one writable [`BridgeValue`].
unsafe extern "C" fn invoke_runtime(
    function_id: u32,
    args: *mut BridgeValue,
    count: u32,
    out: *mut BridgeValue,
) {
    let pointer = ACTIVE_SESSION.get();
    if pointer.is_null() {
        // A hybrid program's native half calling back from a thread the host
        // never entered is out of scope for v0. Say so and stop, rather than
        // running against nothing.
        fatal(&format!(
            "native code called runtime function {function_id} from a thread with no \
             hybrid session; v0 supports callbacks only on the thread that started \
             the program"
        ));
    }
    // SAFETY: the pointer is non-null, so an `ActiveSession` guard is alive on
    // this thread and is borrowing the session it points at for at least as
    // long as this call — the guard lives across the whole `run`, and this
    // call is reached from inside it.
    let session = unsafe { &*pointer };

    let values: &[BridgeValue] = if count == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `count` readable values at `args`.
        unsafe { std::slice::from_raw_parts(args, count as usize) }
    };

    // SAFETY: the caller transfers every string handle among the arguments;
    // `take_args` frees each exactly once.
    let owned = match unsafe { marshal::take_args(&session.library, values) } {
        Ok(owned) => owned,
        Err(error) => fatal(&format!(
            "native code called runtime function {function_id} with an argument this \
             runtime cannot read: {error}"
        )),
    };
    let mut owned = owned;
    for argument in &mut owned {
        if let OwnedArg::Aggregate(value) = argument {
            session.rewrite_vm_cell_proxies(value);
        }
    }
    let borrowed: Vec<NativeArg<'_>> = owned.iter().map(OwnedArg::borrow).collect();

    // The parameters this function writes through, read off the manifest — the
    // same row the native caller's own signature was generated from.
    let capture: Vec<u32> = session
        .manifest
        .functions
        .get(function_id as usize)
        .map(|function| function.params.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter(|(_, param)| param.ownership.is_mutable())
        .map(|(slot, _)| slot as u32)
        .collect();

    let mut host = Host { session };
    let (program, generation, callback_ids) = session.current_program();
    let Some(&current_function_id) = callback_ids.get(function_id as usize) else {
        fatal(&format!(
            "native code called runtime function {function_id}, but the live module has no identity for it"
        ));
    };
    let returned = match kira_vm_runtime::interp::call_active_with_host(
        current_function_id,
        &borrowed,
        &capture,
        &mut host,
    ) {
        Some(result) => result,
        None => program.call_capturing(&mut host, current_function_id, &borrowed, &capture),
    };
    match returned {
        Ok(returned) => {
            session.observe_generation(generation);
            // Each written-through parameter's final value replaces the argument
            // that arrived in its slot, exactly as a trampoline does going the
            // other way. The argument's own handle was consumed by `take_args`,
            // so the slot holds nothing to free.
            for (slot, value) in returned.writebacks {
                // The slot is checked before the replacement is lowered:
                // `lower_result` allocates a fresh handle or node, and an
                // out-of-range slot would strand it with nobody to free it.
                if (slot as usize) >= values.len() {
                    continue;
                }
                let replacement = marshal::lower_result(&session.library, value);
                // SAFETY: the slot is within `count`, which the caller
                // guarantees is writable — the manifest's parameter list
                // and the call's arity are proven equal by bundle
                // validation, and the bound is re-checked above regardless.
                unsafe { *args.add(slot as usize) = replacement };
            }
            // A returned string is a fresh handle the native caller frees.
            let value = marshal::lower_result(&session.library, returned.result);
            // SAFETY: the caller guarantees `out` is one writable value.
            unsafe { *out = value };
        }
        // A trap has nowhere to go from here: unwinding out of an `extern "C"`
        // frame aborts, and the native caller has no error channel. Report and
        // exit as the native runtime's own traps do, so a trap reached through
        // native code and one reached directly look the same to a user.
        Err(trap) => fatal(&format!("runtime trap: {trap}")),
    }
}
