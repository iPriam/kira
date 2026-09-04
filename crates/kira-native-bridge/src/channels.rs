//! The native half of the channel table: one symbol over one table.
//!
//! Generated native code reaches the table through `kira_rt_channel_op` and
//! nothing else, because the ordering *policy* is not here: it is generated
//! Kira the IR synthesizes, compiled into the same executable as the rest of
//! the program. What this file owns is the channel table, and it owns it by
//! delegating every question to [`kira_runtime_abi::ChannelExecutor`], the
//! same type the VM holds. That is the whole parity argument: one table
//! implementation, two engines that only carry it.

use std::cell::RefCell;

use kira_runtime_abi::{ChannelExecutor, ChannelPrim};

thread_local! {
    /// The channels this thread's program created.
    ///
    /// Thread-local rather than global because an end handle is an index into
    /// one table: two threads sharing it would let one thread's end name the
    /// other's channel. The scoping is the invariant, not the count.
    static CHANNELS: RefCell<ChannelExecutor> = RefCell::new(ChannelExecutor::new());
}

/// Starts a native channel scope with the same empty table a VM run receives.
///
/// Native code can outlive one entrypoint when it is loaded as a hybrid
/// library, so a thread-local table cannot be allowed to turn into process
/// state. The host calls this before every run; the generated process entry
/// calls it for the executable path.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_channel_reset() {
    CHANNELS.with_borrow_mut(|channels| *channels = ChannelExecutor::new());
}

/// Carries out one channel primitive.
///
/// `prim` is a [`ChannelPrim`] wire byte and the three operands are the
/// primitive's arguments, zero where it takes fewer. The answer is the
/// primitive's `Int` result.
///
/// A trap here is the same trap the VM raises for the same program, so the two
/// engines fail on exactly the same programs. It exits non-zero with a message
/// rather than unwinding, the way every other native trap does.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_channel_op(prim: i64, a: i64, b: i64, c: i64) -> i64 {
    let Some(prim) = u8::try_from(prim).ok().and_then(ChannelPrim::from_byte) else {
        // Only generated code writes this byte, so an unknown one means the
        // executable and this archive disagree, which is a link-time problem
        // reported at run time rather than a program error.
        eprintln!("kira: runtime trap: unknown channel primitive {prim}");
        std::process::exit(1);
    };
    match CHANNELS.with_borrow_mut(|channels| channels.perform(prim, a, b, c)) {
        Ok(answer) => answer,
        Err(trap) => {
            eprintln!("kira: runtime trap: {trap}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One primitive through the symbol, as generated code calls it.
    fn op(prim: ChannelPrim, a: i64, b: i64) -> i64 {
        kira_rt_channel_op(i64::from(prim.as_byte()), a, b, 0)
    }

    /// The receiver end sharing a sender's channel.
    ///
    /// Generated code derives it the same way: the two ends differ only in the
    /// end bit of the slot field, which is bit 0 of the handle's low half.
    fn receiver_of(sender: i64) -> i64 {
        sender + 1
    }

    #[test]
    fn values_cross_the_symbol_in_send_order() {
        kira_rt_channel_reset();
        let sender = op(ChannelPrim::Create, 0, 0);
        let receiver = receiver_of(sender);
        op(ChannelPrim::Send, sender, 11);
        op(ChannelPrim::Send, sender, 22);
        assert_eq!(op(ChannelPrim::Poll, receiver, 0), 1);
        assert_eq!(op(ChannelPrim::Take, receiver, 0), 11);
        assert_eq!(op(ChannelPrim::Take, receiver, 0), 22);
        assert_eq!(op(ChannelPrim::Poll, receiver, 0), 0);
    }

    #[test]
    fn a_closed_channel_polls_closed_once_drained() {
        kira_rt_channel_reset();
        let sender = op(ChannelPrim::Create, 0, 0);
        let receiver = receiver_of(sender);
        op(ChannelPrim::Send, sender, 7);
        op(ChannelPrim::CloseSender, sender, 0);
        assert_eq!(op(ChannelPrim::Poll, receiver, 0), 1);
        assert_eq!(op(ChannelPrim::Take, receiver, 0), 7);
        assert_eq!(op(ChannelPrim::Poll, receiver, 0), 2);
    }

    #[test]
    fn resetting_a_scope_drops_old_ends_and_starts_over() {
        kira_rt_channel_reset();
        let first = op(ChannelPrim::Create, 0, 0);
        kira_rt_channel_reset();
        assert_eq!(
            op(ChannelPrim::Create, 0, 0),
            first,
            "a fresh scope hands out the same first handle"
        );
    }
}
