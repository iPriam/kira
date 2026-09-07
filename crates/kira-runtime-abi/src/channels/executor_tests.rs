//! What the channel table does, and what it refuses.

use super::{ChannelExecutor, ChannelPrim, ChannelReceive, ChannelTrap};

/// Creates a channel and returns its ends.
fn channel(executor: &mut ChannelExecutor) -> (i64, i64) {
    executor
        .create(false)
        .expect("table has room for one channel")
}

#[test]
fn the_two_ends_name_one_channel_without_aliasing() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    assert_ne!(sender, receiver);
    assert_eq!(executor.live(), 1);
    assert_eq!(
        executor.receive(receiver),
        Ok(ChannelReceive::Empty),
        "an untouched channel is open and empty"
    );
}

#[test]
fn values_arrive_in_send_order() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    executor.send(sender, 1).unwrap();
    executor.send(sender, 2).unwrap();
    executor.send(sender, 3).unwrap();
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Value(1)));
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Value(2)));
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Value(3)));
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Empty));
}

#[test]
fn a_drained_closed_channel_reports_closure_rather_than_trapping() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    executor.send(sender, 7).unwrap();
    executor.close_sender(sender).unwrap();
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Value(7)));
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Closed));
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Closed));
}

#[test]
fn a_closed_empty_channel_is_closed_at_once() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    executor.close_sender(sender).unwrap();
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Closed));
}

#[test]
fn sending_after_the_receiver_is_gone_traps() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    executor.close_receiver(receiver).unwrap();
    assert_eq!(executor.send(sender, 1), Err(ChannelTrap::ReceiverGone));
}

#[test]
fn an_end_used_in_the_wrong_direction_traps() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    assert_eq!(executor.send(receiver, 1), Err(ChannelTrap::WrongDirection));
    assert_eq!(executor.receive(sender), Err(ChannelTrap::WrongDirection));
}

#[test]
fn reclaiming_both_ends_stales_both_handles() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    executor.close_sender(sender).unwrap();
    executor.close_receiver(receiver).unwrap();
    assert_eq!(executor.live(), 0);
    assert_eq!(executor.send(sender, 1), Err(ChannelTrap::UnknownHandle));
    assert_eq!(executor.receive(receiver), Err(ChannelTrap::UnknownHandle));
}

#[test]
fn a_reused_slot_has_a_new_generation() {
    let mut executor = ChannelExecutor::new();
    let (stale_sender, stale_receiver) = channel(&mut executor);
    executor.close_sender(stale_sender).unwrap();
    executor.close_receiver(stale_receiver).unwrap();
    let (sender, receiver) = channel(&mut executor);
    assert_ne!((sender, receiver), (stale_sender, stale_receiver));
    assert_eq!(
        executor.receive(stale_receiver),
        Err(ChannelTrap::UnknownHandle)
    );
    assert_eq!(executor.receive(receiver), Ok(ChannelReceive::Empty));
}

#[test]
fn zero_names_no_channel_end() {
    let mut executor = ChannelExecutor::new();
    assert_eq!(executor.receive(0), Err(ChannelTrap::UnknownHandle));
    assert_eq!(executor.send(0, 1), Err(ChannelTrap::UnknownHandle));
}

/// The trap codes are a wire contract, so they are pinned like the
/// primitive bytes are, and zero stays reserved for "no trap".
#[test]
fn the_trap_wire_codes_are_pinned() {
    assert_eq!(ChannelTrap::UnknownHandle.as_code(), 1);
    assert_eq!(ChannelTrap::ReceiverGone.as_code(), 2);
    assert_eq!(ChannelTrap::WrongDirection.as_code(), 3);
    assert_eq!(ChannelTrap::NotReady.as_code(), 4);
    assert_eq!(ChannelTrap::Deadlock.as_code(), 5);
    assert_eq!(ChannelTrap::from_code(0), None);
}

#[test]
fn every_trap_round_trips_through_its_code() {
    for trap in ChannelTrap::ALL {
        assert_eq!(ChannelTrap::from_code(trap.as_code()), Some(trap));
    }
    assert_eq!(ChannelTrap::from_code(-1), None);
    assert_eq!(ChannelTrap::from_code(6), None);
}

#[test]
fn the_primitive_wire_bytes_are_pinned() {
    // Spelled out literally: a reorder here silently redirects every
    // already-compiled module, so it has to fail a test instead.
    assert_eq!(ChannelPrim::Create.as_byte(), 0);
    assert_eq!(ChannelPrim::Send.as_byte(), 1);
    assert_eq!(ChannelPrim::Poll.as_byte(), 2);
    assert_eq!(ChannelPrim::Take.as_byte(), 3);
    assert_eq!(ChannelPrim::CloseSender.as_byte(), 4);
    assert_eq!(ChannelPrim::CloseReceiver.as_byte(), 5);
    assert_eq!(ChannelPrim::Deadlock.as_byte(), 6);
}

#[test]
fn every_primitive_round_trips_through_its_byte() {
    for prim in ChannelPrim::ALL {
        assert_eq!(ChannelPrim::from_byte(prim.as_byte()), Some(prim));
    }
}

#[test]
fn an_unknown_byte_names_no_primitive() {
    assert_eq!(ChannelPrim::from_byte(ChannelPrim::ALL.len() as u8), None);
    assert_eq!(ChannelPrim::from_byte(u8::MAX), None);
}

#[test]
fn poll_and_take_agree_without_consuming_early() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    assert_eq!(executor.perform(ChannelPrim::Poll, receiver, 0, 0), Ok(0));
    executor.perform(ChannelPrim::Send, sender, 11, 0).unwrap();
    assert_eq!(executor.perform(ChannelPrim::Poll, receiver, 0, 0), Ok(1));
    assert_eq!(
        executor.perform(ChannelPrim::Poll, receiver, 0, 0),
        Ok(1),
        "polling twice must not consume the waiting value"
    );
    assert_eq!(executor.perform(ChannelPrim::Take, receiver, 0, 0), Ok(11));
    assert_eq!(
        executor.perform(ChannelPrim::Take, receiver, 0, 0),
        Err(ChannelTrap::NotReady)
    );
}

#[test]
fn a_closed_channel_polls_closed_once_drained() {
    let mut executor = ChannelExecutor::new();
    let (sender, receiver) = channel(&mut executor);
    executor
        .perform(ChannelPrim::CloseSender, sender, 0, 0)
        .unwrap();
    assert_eq!(executor.perform(ChannelPrim::Poll, receiver, 0, 0), Ok(2));
}

#[test]
fn the_deadlock_primitive_always_traps() {
    let mut executor = ChannelExecutor::new();
    assert_eq!(
        executor.perform(ChannelPrim::Deadlock, 0, 0, 0),
        Err(ChannelTrap::Deadlock)
    );
}
