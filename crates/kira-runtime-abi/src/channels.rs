//! The channel table both engines share, for ordered handoffs between contexts.
//!
//! A channel is a FIFO queue with two owned ends. `Channel::create` answers a
//! sender handle and a receiver handle; values sent through the sender arrive
//! in order, and dropping the sender closes the channel so a receiver that
//! drains the queue learns the closure as a typed answer rather than a trap.
//! Storage reclamation mirrors the task table: a slot is reused only after
//! both ends are gone, and the generation advances so a stale end traps
//! instead of naming the next channel.
//!
//! This type only owns the table. Suspension, parking, and the scheduler
//! policy live in generated Kira above it, exactly as the task executor owns
//! the task table and the synthesized scheduler drives it. Values travel as
//! `i64` words for the same reason task arguments do: the word is a heap index
//! or a scalar the engines already agree on, so neither backend needs its own
//! reading of what a queued value means.

use std::collections::VecDeque;

/// What one end of a channel is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelEnd {
    /// Sends values into the queue.
    Sender,
    /// Takes values out of the queue.
    Receiver,
}

/// Why a channel operation could not be carried out.
///
/// Every variant is a program error the language defines as a runtime trap.
/// A closed channel with a drained queue is deliberately absent: that is not
/// an error but the [`ChannelReceive::Closed`] answer, which generated code
/// routes to a handler instead of trapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChannelTrap {
    /// An end handle that names no channel reached a primitive.
    #[error("channel end handle is not live")]
    UnknownHandle,
    /// A value was sent after the receiver was gone.
    #[error("channel receiver is gone, so no value sent to it can arrive")]
    ReceiverGone,
    /// A value was taken through a sender, or sent through a receiver.
    #[error("channel end used in the wrong direction")]
    WrongDirection,
}

/// What a non-blocking receive answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelReceive {
    /// The oldest queued value.
    Value(i64),
    /// The queue is empty and the sender is still live.
    Empty,
    /// The queue is empty and the sender is gone.
    Closed,
}

/// One channel.
#[derive(Debug)]
struct Channel {
    /// Values in arrival order.
    queue: VecDeque<i64>,
    /// Whether the sender end is still live.
    sender_live: bool,
    /// Whether the receiver end is still live.
    receiver_live: bool,
}

/// One reusable channel-table position.
#[derive(Debug, Default)]
struct ChannelSlot {
    /// Incremented whenever the channel in this position is reclaimed.
    generation: u32,
    /// The live channel, or `None` when the position is free.
    channel: Option<Channel>,
}

/// The channel table one running program owns.
///
/// A handle packs a generation in bits 32..63 and a 1-based end slot in bits
/// 0..31. The end slot packs the table index shifted left one with the end bit
/// in bit 0 (sender `0`, receiver `1`), so the two ends of one channel name
/// the same storage without aliasing each other, while `0` remains free to
/// mean "no channel".
#[derive(Debug, Default)]
pub struct ChannelExecutor {
    /// Channel storage by table index.
    channels: Vec<ChannelSlot>,
    /// Table indexes ready for reuse.
    free: Vec<usize>,
}

impl ChannelExecutor {
    /// A table with no channels in it.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many channels have live storage.
    pub fn live(&self) -> usize {
        self.channels.iter().filter(|slot| slot.channel.is_some()).count()
    }

    /// Creates a channel, answering its `(sender, receiver)` handles.
    pub fn create(&mut self) -> Result<(i64, i64), ChannelTrap> {
        let index = match self.free.pop() {
            Some(index) => index,
            None => {
                let index = self.channels.len();
                if index >= (u32::MAX >> 1) as usize {
                    return Err(ChannelTrap::UnknownHandle);
                }
                self.channels.push(ChannelSlot::default());
                index
            }
        };
        let slot = self
            .channels
            .get_mut(index)
            .ok_or(ChannelTrap::UnknownHandle)?;
        slot.channel = Some(Channel {
            queue: VecDeque::new(),
            sender_live: true,
            receiver_live: true,
        });
        let generation = slot.generation;
        Ok((
            Self::handle(index, generation, ChannelEnd::Sender)?,
            Self::handle(index, generation, ChannelEnd::Receiver)?,
        ))
    }

    /// Queues `value` behind every value already waiting.
    pub fn send(&mut self, sender: i64, value: i64) -> Result<(), ChannelTrap> {
        let (index, generation, end) = Self::parts(sender)?;
        if end != ChannelEnd::Sender {
            return Err(ChannelTrap::WrongDirection);
        }
        let slot = self
            .channels
            .get_mut(index)
            .ok_or(ChannelTrap::UnknownHandle)?;
        if slot.generation != generation {
            return Err(ChannelTrap::UnknownHandle);
        }
        let channel = slot.channel.as_mut().ok_or(ChannelTrap::UnknownHandle)?;
        if !channel.sender_live {
            return Err(ChannelTrap::UnknownHandle);
        }
        if !channel.receiver_live {
            return Err(ChannelTrap::ReceiverGone);
        }
        channel.queue.push_back(value);
        Ok(())
    }

    /// Answers the oldest queued value without blocking.
    pub fn receive(&mut self, receiver: i64) -> Result<ChannelReceive, ChannelTrap> {
        let (index, generation, end) = Self::parts(receiver)?;
        if end != ChannelEnd::Receiver {
            return Err(ChannelTrap::WrongDirection);
        }
        let slot = self
            .channels
            .get_mut(index)
            .ok_or(ChannelTrap::UnknownHandle)?;
        if slot.generation != generation {
            return Err(ChannelTrap::UnknownHandle);
        }
        let channel = slot.channel.as_mut().ok_or(ChannelTrap::UnknownHandle)?;
        if !channel.receiver_live {
            return Err(ChannelTrap::UnknownHandle);
        }
        if let Some(value) = channel.queue.pop_front() {
            return Ok(ChannelReceive::Value(value));
        }
        if channel.sender_live {
            Ok(ChannelReceive::Empty)
        } else {
            Ok(ChannelReceive::Closed)
        }
    }

    /// Drops the sender end, closing the channel once the queue drains.
    pub fn close_sender(&mut self, sender: i64) -> Result<(), ChannelTrap> {
        let (index, generation, end) = Self::parts(sender)?;
        if end != ChannelEnd::Sender {
            return Err(ChannelTrap::WrongDirection);
        }
        let slot = self
            .channels
            .get_mut(index)
            .ok_or(ChannelTrap::UnknownHandle)?;
        if slot.generation != generation {
            return Err(ChannelTrap::UnknownHandle);
        }
        let channel = slot.channel.as_mut().ok_or(ChannelTrap::UnknownHandle)?;
        if !channel.sender_live {
            return Err(ChannelTrap::UnknownHandle);
        }
        channel.sender_live = false;
        if !channel.receiver_live {
            self.reclaim(index, generation)?;
        }
        Ok(())
    }

    /// Drops the receiver end, discarding whatever is still queued.
    pub fn close_receiver(&mut self, receiver: i64) -> Result<(), ChannelTrap> {
        let (index, generation, end) = Self::parts(receiver)?;
        if end != ChannelEnd::Receiver {
            return Err(ChannelTrap::WrongDirection);
        }
        let slot = self
            .channels
            .get_mut(index)
            .ok_or(ChannelTrap::UnknownHandle)?;
        if slot.generation != generation {
            return Err(ChannelTrap::UnknownHandle);
        }
        let channel = slot.channel.as_mut().ok_or(ChannelTrap::UnknownHandle)?;
        if !channel.receiver_live {
            return Err(ChannelTrap::UnknownHandle);
        }
        channel.receiver_live = false;
        channel.queue.clear();
        if !channel.sender_live {
            self.reclaim(index, generation)?;
        }
        Ok(())
    }

    /// Reclaims one channel whose ends are both gone and advances its generation.
    fn reclaim(&mut self, index: usize, generation: u32) -> Result<(), ChannelTrap> {
        let slot = self
            .channels
            .get_mut(index)
            .ok_or(ChannelTrap::UnknownHandle)?;
        if slot.generation != generation || slot.channel.take().is_none() {
            return Err(ChannelTrap::UnknownHandle);
        }
        if let Some(next) = slot
            .generation
            .checked_add(1)
            .filter(|next| *next <= i32::MAX as u32)
        {
            slot.generation = next;
            self.free.push(index);
        }
        Ok(())
    }

    /// Packs one stable end handle.
    fn handle(index: usize, generation: u32, end: ChannelEnd) -> Result<i64, ChannelTrap> {
        let end_bit = match end {
            ChannelEnd::Sender => 0u32,
            ChannelEnd::Receiver => 1u32,
        };
        let slot_field = (u32::try_from(index).map_err(|_| ChannelTrap::UnknownHandle)? << 1)
            | end_bit;
        let slot_field = slot_field
            .checked_add(1)
            .ok_or(ChannelTrap::UnknownHandle)?;
        let word = (u64::from(generation) << 32) | u64::from(slot_field);
        i64::try_from(word).map_err(|_| ChannelTrap::UnknownHandle)
    }

    /// Unpacks and validates a nonzero end handle.
    fn parts(handle: i64) -> Result<(usize, u32, ChannelEnd), ChannelTrap> {
        let word = u64::try_from(handle).map_err(|_| ChannelTrap::UnknownHandle)?;
        let slot_field =
            u32::try_from(word & u64::from(u32::MAX)).map_err(|_| ChannelTrap::UnknownHandle)?;
        let base = slot_field
            .checked_sub(1)
            .ok_or(ChannelTrap::UnknownHandle)?;
        let end = if base & 1 == 0 {
            ChannelEnd::Sender
        } else {
            ChannelEnd::Receiver
        };
        let index =
            usize::try_from(base >> 1).map_err(|_| ChannelTrap::UnknownHandle)?;
        Ok((index, (word >> 32) as u32, end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a channel and returns its ends.
    fn channel(executor: &mut ChannelExecutor) -> (i64, i64) {
        executor.create().expect("table has room for one channel")
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
        assert_eq!(
            executor.send(sender, 1),
            Err(ChannelTrap::ReceiverGone)
        );
    }

    #[test]
    fn an_end_used_in_the_wrong_direction_traps() {
        let mut executor = ChannelExecutor::new();
        let (sender, receiver) = channel(&mut executor);
        assert_eq!(
            executor.send(receiver, 1),
            Err(ChannelTrap::WrongDirection)
        );
        assert_eq!(
            executor.receive(sender),
            Err(ChannelTrap::WrongDirection)
        );
    }

    #[test]
    fn reclaiming_both_ends_stales_both_handles() {
        let mut executor = ChannelExecutor::new();
        let (sender, receiver) = channel(&mut executor);
        executor.close_sender(sender).unwrap();
        executor.close_receiver(receiver).unwrap();
        assert_eq!(executor.live(), 0);
        assert_eq!(
            executor.send(sender, 1),
            Err(ChannelTrap::UnknownHandle)
        );
        assert_eq!(
            executor.receive(receiver),
            Err(ChannelTrap::UnknownHandle)
        );
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
        assert_eq!(
            executor.receive(0),
            Err(ChannelTrap::UnknownHandle)
        );
        assert_eq!(executor.send(0, 1), Err(ChannelTrap::UnknownHandle));
    }
}
