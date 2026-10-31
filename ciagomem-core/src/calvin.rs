//! Core-local, message-tagged Calvin voting. All changes require exclusive owner access.
pub const MAX_PARTICIPANTS: u32 = 64;
const EMPTY: u64 = u64::MAX;

/// Minimal patch for the spec's constructor, if nothing else changes.
#[inline(always)]
pub const fn full_mask(n: u32) -> Option<u64> {
    if n == 0 || n > MAX_PARTICIPANTS {
        None
    } else {
        Some(u64::MAX >> (64 - n))
    }
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
        if n == 0 || n > MAX_PARTICIPANTS {
            None
        } else {
            Some(Participants(n as u8))
        }
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
    Decided {
        commit: bool,
    },
    /// Same participant voted twice (redelivery). Ignored.
    Duplicate,
    Conflict,
    /// Slot does not belong to this `tx_seq` (late or misrouted message).
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// Previous occupant not closed yet: sequencer back-pressure.
    Busy,
}

#[derive(Debug)]
struct TxSlot {
    tag: u64,
    mask: u64,
    voted: u64,
    nays: u64,
}
#[derive(Debug)]
pub struct VoteTable {
    slots: Box<[TxSlot]>,
    last_opened: Option<u64>,
}
impl VoteTable {
    pub fn new(slots: usize) -> Self {
        assert!(slots.is_power_of_two());
        Self {
            slots: (0..slots)
                .map(|_| TxSlot {
                    tag: EMPTY,
                    mask: 0,
                    voted: 0,
                    nays: 0,
                })
                .collect(),
            last_opened: None,
        }
    }
    fn slot(&mut self, tx: u64) -> &mut TxSlot {
        let i = tx as usize & (self.slots.len() - 1);
        &mut self.slots[i]
    }
    pub fn open(&mut self, tx: u64, n: Participants) -> Result<(), OpenError> {
        if tx == EMPTY || self.last_opened.is_some_and(|last| tx <= last) {
            return Err(OpenError::Busy);
        }
        let s = self.slot(tx);
        if s.tag != EMPTY {
            return Err(OpenError::Busy);
        }
        *s = TxSlot {
            tag: tx,
            mask: n.mask(),
            voted: 0,
            nays: 0,
        };
        self.last_opened = Some(tx);
        Ok(())
    }
    pub fn vote(&mut self, tx: u64, idx: u32, ok: bool) -> Vote {
        let s = self.slot(tx);
        if s.tag != tx || idx >= 64 || (s.mask >> idx) & 1 == 0 {
            return Vote::Stale;
        }
        let bit = 1u64 << idx;
        if s.voted & bit != 0 {
            return if ((s.nays & bit) == 0) == ok {
                Vote::Duplicate
            } else {
                Vote::Conflict
            };
        }
        if !ok {
            s.nays |= bit
        }
        s.voted |= bit;
        if s.voted == s.mask {
            Vote::Decided {
                commit: s.nays == 0,
            }
        } else {
            Vote::Pending
        }
    }
    pub fn aborted_hint(&self, tx: u64) -> bool {
        let s = &self.slots[tx as usize & (self.slots.len() - 1)];
        s.tag == tx && s.nays != 0
    }
    pub fn close(&mut self, tx: u64) {
        let s = self.slot(tx);
        if s.tag == tx {
            s.tag = EMPTY
        }
    }
}
