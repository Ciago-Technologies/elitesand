//! Calvin participant vote table.
//!
//! Spec code:
//! ```text
//! let mask = (1u64 << num_participants) - 1;          // (a)
//! let bit = if ok { 1 << idx } else { TX_ABORT_BIT | (1 << idx) };  // (b) TX_ABORT_BIT = 1 << 63
//! if (word & expected_mask) == expected_mask { commit }               // (c)
//! ```
//!
//! Bugs:
//! * (a) `num_participants == 64`: `1u64 << 64` is an overflow. Debug builds panic;
//!   release builds compute `1 << (64 & 63) = 1`, so `mask = 0` and (c) is true for
//!   *any* word: the transaction commits before a single participant voted.
//!   `num_participants == 0` also yields mask 0 (vacuous commit). `num_participants > 64`
//!   and `participant_idx >= 64` overflow the same way.
//! * (b) Participant 63's "yes" bit *is* `TX_ABORT_BIT`, so with >= 64 participants a
//!   valid vote aborts the transaction; and an abort from participant k is
//!   indistinguishable from "k voted yes + participant 63 voted yes".
//! * No slot tag: a status word reused for a later `tx_seq` can absorb a late vote from
//!   the earlier transaction (ABA).
//! * Nobody is "the decider": every participant polls the word, so either all spin
//!   (core stalls) or the outcome must be broadcast by someone who cannot know they
//!   were last.
//!
//! Fix:
//! * Participants are *cores*, deduplicated and sorted by the sequencer, so N <= cores
//!   <= 64 and every participant has a stable index `0..N`.
//! * `full_mask(n) = u64::MAX >> (64 - n)` for `n` in `1..=64`: no shift ever reaches 64.
//! * Two words: `voted` (one bit per participant that has voted) and `nays` (bit per
//!   "no" vote). The abort flag no longer shares a bit with a participant.
//! * Each voter writes `nays` (if voting no) *before* `voted` with Release ordering; the
//!   voter whose `fetch_or` on `voted` completes the mask is the unique decider and
//!   reads `nays` after an AcqRel RMW on `voted`, which happens-after every other
//!   voter's `nays` write (release sequence on `voted`). Outcome: commit iff `nays == 0`.
//! * Duplicate votes (redelivered message) are detected, not double counted.
//! * Slots are tagged with `tx_seq`; votes for a different `tx_seq` are `Stale`.

use crate::CachePadded;
use core::sync::atomic::{AtomicU64, Ordering::{AcqRel, Acquire, Relaxed, Release}};

pub const MAX_PARTICIPANTS: u32 = 64;
const EMPTY: u64 = u64::MAX;

/// Minimal patch for the spec's constructor, if nothing else changes.
#[inline(always)]
pub const fn full_mask(n: u32) -> Option<u64> {
    if n == 0 || n > MAX_PARTICIPANTS { None } else { Some(u64::MAX >> (64 - n)) }
}

/// The spec's formula, kept only to demonstrate the bug in tests/examples.
#[inline(never)]
pub fn spec_mask_release_semantics(n: u8) -> u64 {
    // What `(1u64 << n) - 1` compiles to in release on x86-64/AArch64: shift count is
    // masked to 6 bits by the hardware (and by `wrapping_shl`).
    1u64.wrapping_shl(n as u32).wrapping_sub(1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Participants(u8);

impl Participants {
    pub const fn new(n: u32) -> Option<Participants> {
        if n == 0 || n > MAX_PARTICIPANTS { None } else { Some(Participants(n as u8)) }
    }
    pub const fn get(self) -> u32 {
        self.0 as u32
    }
    pub const fn mask(self) -> u64 {
        u64::MAX >> (64 - self.0 as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    /// Others have not voted yet: park this transaction (not the core) until the
    /// decider's outcome message arrives on the mesh.
    Pending,
    /// This vote completed the set: broadcast `commit` to the other participants.
    Decided { commit: bool },
    /// Same participant voted twice (redelivery). Ignored.
    Duplicate,
    /// Slot does not belong to this `tx_seq` (late or misrouted message).
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Previous occupant not closed yet: sequencer back-pressure.
    Busy,
}

#[derive(Debug)]
#[repr(C, align(64))]
struct TxSlot {
    tag: AtomicU64,
    mask: AtomicU64,
    voted: AtomicU64,
    nays: AtomicU64,
}

/// Power-of-two ring of vote slots indexed by `tx_seq`. The sequencer admits at most
/// `slots` multi-core transactions in flight, so `tx_seq & (len-1)` never collides
/// with a live transaction.
#[derive(Debug)]
pub struct VoteTable {
    slots: Box<[CachePadded<TxSlot>]>,
}

impl VoteTable {
    pub fn new(slots: usize) -> Self {
        assert!(slots.is_power_of_two());
        VoteTable {
            slots: (0..slots)
                .map(|_| CachePadded(TxSlot { tag: AtomicU64::new(EMPTY), mask: AtomicU64::new(0), voted: AtomicU64::new(0), nays: AtomicU64::new(0) }))
                .collect(),
        }
    }

    #[inline(always)]
    fn slot(&self, tx_seq: u64) -> &TxSlot {
        &self.slots[(tx_seq as usize) & (self.slots.len() - 1)].0
    }

    /// Sequencer, before admitting the tx to participant lanes.
    pub fn open(&self, tx_seq: u64, n: Participants) -> Result<(), OpenError> {
        assert_ne!(tx_seq, EMPTY);
        let s = self.slot(tx_seq);
        if s.tag.load(Acquire) != EMPTY {
            return Err(OpenError::Busy);
        }
        s.mask.store(n.mask(), Relaxed);
        s.voted.store(0, Relaxed);
        s.nays.store(0, Relaxed);
        s.tag.store(tx_seq, Release); // publishes mask/voted/nays
        Ok(())
    }

    /// Participant core `idx` (0-based, < n) votes after local validation.
    pub fn vote(&self, tx_seq: u64, idx: u32, ok: bool) -> Vote {
        let s = self.slot(tx_seq);
        if s.tag.load(Acquire) != tx_seq {
            return Vote::Stale;
        }
        let mask = s.mask.load(Relaxed);
        if idx >= MAX_PARTICIPANTS || (mask >> idx) & 1 == 0 {
            return Vote::Stale;
        }
        let bit = 1u64 << idx; // idx <= 63: never overflows
        if !ok {
            s.nays.fetch_or(bit, Release);
        }
        let before = s.voted.fetch_or(bit, AcqRel);
        if before & bit != 0 {
            return Vote::Duplicate;
        }
        if (before | bit) & mask == mask {
            // Unique decider. AcqRel on `voted` synchronizes with every other voter's
            // release RMW on `voted`, which is sequenced after its `nays` write.
            let commit = s.nays.load(Acquire) & mask == 0;
            Vote::Decided { commit }
        } else {
            Vote::Pending
        }
    }

    /// Fast-path hint: some participant already voted no (skip work early). Never use
    /// it to decide; only the decider decides.
    pub fn aborted_hint(&self, tx_seq: u64) -> bool {
        let s = self.slot(tx_seq);
        s.tag.load(Acquire) == tx_seq && s.nays.load(Relaxed) != 0
    }

    /// Decider, after its outcome is durable in its log and broadcast.
    pub fn close(&self, tx_seq: u64) {
        let _ = self.slot(tx_seq).tag.compare_exchange(tx_seq, EMPTY, AcqRel, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn spec_mask_is_zero_for_64_and_fix_is_full() {
        assert_eq!(spec_mask_release_semantics(64), 0, "spec bug: zero mask commits with no votes");
        assert_eq!(spec_mask_release_semantics(0), 0);
        assert_eq!(full_mask(64), Some(u64::MAX));
        assert_eq!(full_mask(63), Some(u64::MAX >> 1));
        assert_eq!(full_mask(1), Some(1));
        assert_eq!(full_mask(0), None);
        assert_eq!(full_mask(65), None);
        for n in 1..=64u32 {
            assert_eq!(full_mask(n).unwrap().count_ones(), n);
        }
    }

    #[test]
    fn sixty_four_participants_need_all_votes_and_bit63_is_a_normal_vote() {
        let t = VoteTable::new(8);
        let n = Participants::new(64).unwrap();
        t.open(100, n).unwrap();
        for i in 0..63 {
            assert_eq!(t.vote(100, i, true), Vote::Pending, "must not decide before vote 64");
        }
        assert_eq!(t.vote(100, 5, true), Vote::Duplicate);
        assert_eq!(t.vote(100, 63, true), Vote::Decided { commit: true }, "participant 63 yes != abort");
        t.close(100);

        t.open(101, n).unwrap();
        assert_eq!(t.vote(101, 63, false), Vote::Pending);
        assert!(t.aborted_hint(101));
        for i in 0..62 {
            assert_eq!(t.vote(101, i, true), Vote::Pending);
        }
        assert_eq!(t.vote(101, 62, true), Vote::Decided { commit: false });
    }

    #[test]
    fn stale_and_busy() {
        let t = VoteTable::new(4);
        let n = Participants::new(2).unwrap();
        t.open(8, n).unwrap();
        assert_eq!(t.open(12, n), Err(OpenError::Busy), "same slot, previous not closed");
        assert_eq!(t.vote(12, 0, true), Vote::Stale);
        assert_eq!(t.vote(8, 2, true), Vote::Stale, "index outside participant set");
        assert_eq!(t.vote(8, 0, true), Vote::Pending);
        assert_eq!(t.vote(8, 1, true), Vote::Decided { commit: true });
        t.close(8);
        t.open(12, n).unwrap();
        assert_eq!(t.vote(8, 0, true), Vote::Stale, "late vote for closed tx is rejected (no ABA)");
    }

    #[test]
    fn exactly_one_decider_under_contention() {
        for round in 0..if cfg!(miri) { 4 } else { 200u64 } {
            let t = Arc::new(VoteTable::new(2));
            let n = 16;
            t.open(round, Participants::new(n).unwrap()).unwrap();
            let no_voter = (round % 17) as u32; // 16 => everyone votes yes
            let hs: Vec<_> = (0..n)
                .map(|i| {
                    let t = t.clone();
                    std::thread::spawn(move || t.vote(round, i, i != no_voter))
                })
                .collect();
            let res: Vec<Vote> = hs.into_iter().map(|h| h.join().unwrap()).collect();
            let deciders: Vec<_> = res.iter().filter_map(|v| if let Vote::Decided { commit } = v { Some(*commit) } else { None }).collect();
            assert_eq!(deciders.len(), 1);
            assert_eq!(deciders[0], no_voter >= n);
        }
    }
}
