// SPDX-License-Identifier: BSD-2-Clause
//! Local one-outstanding-RPC guard, NOT a GSP wire parser.
//!
//! A versioned decoder must FIRST snapshot DMA bytes and validate both message
//! headers, length/page counts, checksum, status and payload/continuations.
//! Notifications never renew a deadline. Local tags are NOT firmware sequence
//! numbers: NOVA has distinct outer seqNum and inner rpc.sequence fields, and
//! does not establish a request/reply sequence-echo contract at this revision.
use crate::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ticket {
    tag: u64,
    function: u32,
}
impl Ticket {
    pub fn tag(self) -> u64 {
        self.tag
    }
    pub fn function(self) -> u32 {
        self.function
    }
}
#[derive(Clone, Copy, Debug)]
struct Pending {
    ticket: Ticket,
    deadline_ns: u64,
    last_now_ns: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Matched,
    Unrelated,
}

/// On timeout/clock failure, permanently unusable. Constructing a replacement
/// does NOT authorize channel reuse: only verified hardware teardown can do so.
pub struct RpcTracker {
    next_tag: u64,
    pending: Option<Pending>,
    poisoned: bool,
}
impl Default for RpcTracker {
    fn default() -> Self {
        Self::new()
    }
}
impl RpcTracker {
    pub const fn new() -> Self {
        Self {
            next_tag: 1,
            pending: None,
            poisoned: false,
        }
    }
    pub fn start(&mut self, function: u32, now_ns: u64, timeout_ns: u64) -> Result<Ticket, Error> {
        if self.poisoned {
            return Err(Error::Quarantined);
        }
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        let deadline_ns = now_ns
            .checked_add(timeout_ns)
            .filter(|_| timeout_ns != 0)
            .ok_or(Error::InvalidRange)?;
        let next = self
            .next_tag
            .checked_add(1)
            .ok_or(Error::SequenceExhausted)?;
        let ticket = Ticket {
            tag: self.next_tag,
            function,
        };
        self.next_tag = next;
        self.pending = Some(Pending {
            ticket,
            deadline_ns,
            last_now_ns: now_ns,
        });
        Ok(ticket)
    }
    pub fn poll(&mut self, now_ns: u64) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::Quarantined);
        }
        let pending = self.pending.as_mut().ok_or(Error::NoPending)?;
        let error = if now_ns < pending.last_now_ns {
            Some(Error::ClockRegressed)
        } else if now_ns >= pending.deadline_ns {
            Some(Error::Timeout)
        } else {
            None
        };
        if let Some(error) = error {
            self.poisoned = true;
            return Err(error);
        }
        pending.last_now_ns = now_ns;
        Ok(())
    }
    /// Completion of a local receive operation after full wire validation.
    /// The caller supplies its saved request ticket, NOT rpc.sequence/seqNum.
    /// This checks stale LOCAL callbacks; it cannot reject replayed wire data.
    /// Transport replay/order validation is still an unimplemented port task.
    pub fn reply(&mut self, ticket: Ticket, function: u32, now_ns: u64) -> Result<Reply, Error> {
        self.poll(now_ns)?;
        let pending = self.pending.as_ref().ok_or(Error::NoPending)?;
        if pending.ticket != ticket || ticket.function != function {
            return Ok(Reply::Unrelated);
        }
        self.pending = None;
        Ok(Reply::Matched)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ticket_and_function_match_and_only_one_outstanding() {
        let mut rpc = RpcTracker::new();
        let t = rpc.start(10, 100, 100).unwrap();
        assert_eq!(rpc.start(20, 100, 100), Err(Error::Busy));
        assert_eq!(rpc.reply(t, 20, 110), Ok(Reply::Unrelated));
        assert_eq!(rpc.reply(t, 10, 120), Ok(Reply::Matched));
        let second = rpc.start(10, 130, 100).unwrap();
        assert_ne!(t.tag(), second.tag());
        assert_eq!(rpc.reply(t, 10, 140), Ok(Reply::Unrelated));
        assert_eq!(rpc.reply(second, 10, 150), Ok(Reply::Matched));
    }
    #[test]
    fn unrelated_traffic_cannot_extend_timeout_or_allow_reuse() {
        let mut rpc = RpcTracker::new();
        let t = rpc.start(10, 100, 100).unwrap();
        for now in 101..200 {
            assert_eq!(rpc.reply(t, 90, now), Ok(Reply::Unrelated));
        }
        assert_eq!(rpc.reply(t, 10, 200), Err(Error::Timeout));
        assert_eq!(rpc.start(10, 201, 100), Err(Error::Quarantined));
        assert_eq!(rpc.reply(t, 10, 202), Err(Error::Quarantined));
    }
    #[test]
    fn no_wrap_and_no_clock_regression() {
        let mut rpc = RpcTracker {
            next_tag: u64::MAX,
            ..RpcTracker::new()
        };
        assert_eq!(rpc.start(1, 0, 1), Err(Error::SequenceExhausted));
        let mut rpc = RpcTracker::new();
        assert_eq!(rpc.start(1, 0, 0), Err(Error::InvalidRange));
        assert_eq!(rpc.start(1, u64::MAX, 1), Err(Error::InvalidRange));
        rpc.start(1, 100, 100).unwrap();
        rpc.poll(120).unwrap();
        assert_eq!(rpc.poll(119), Err(Error::ClockRegressed));
        assert_eq!(rpc.poll(121), Err(Error::Quarantined));
    }
}
