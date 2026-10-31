use std::collections::{HashMap, VecDeque};
/// Owner-side read combiner: collapses N fallback reads on one slot into 1 snapshot (Fixes H2).
#[derive(Default)]
pub struct ReadCombiner {
    waiters: HashMap<u32, Vec<u64>>,
    pub snapshots_taken: u64,
    pub requests_served: u64,
}

impl ReadCombiner {
    pub fn enqueue(&mut self, slot: u32, req_id: u64) {
        self.waiters.entry(slot).or_default().push(req_id);
    }

    pub fn flush<R: Clone>(
        &mut self,
        mut snap: impl FnMut(u32) -> R,
        mut reply: impl FnMut(u64, R),
    ) {
        for (slot, reqs) in self.waiters.drain() {
            if reqs.is_empty() {
                continue;
            }
            let v = snap(slot);
            self.snapshots_taken += 1;
            for id in reqs {
                self.requests_served += 1;
                reply(id, v.clone());
            }
        }
    }
}

// =============================================================================
// 2. NON-BLOCKING CREDIT-CONTROLLED SPSC MESH (Fixes P0-1b & H1)
// =============================================================================

pub trait Lane<T> {
    fn try_push(&mut self, v: T) -> Result<(), T>;
}

impl<T> Lane<T> for (VecDeque<T>, usize) {
    fn try_push(&mut self, v: T) -> Result<(), T> {
        if self.0.len() >= self.1 {
            Err(v)
        } else {
            self.0.push_back(v);
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    Deferred,
    ShedAdmission,
}

pub struct OutboundFlow<T> {
    credits: usize,
    sent: u64,
    acknowledged: u64,
    overflow: VecDeque<T>,
    cap: usize,
    pub overflow_hwm: usize,
    lane_capacity: usize,
    pub excess_credits: usize,
}

impl<T> OutboundFlow<T> {
    pub fn new(lane_capacity: usize, overflow_cap: usize) -> Self {
        OutboundFlow {
            credits: lane_capacity,
            sent: 0,
            acknowledged: 0,
            overflow: VecDeque::with_capacity(overflow_cap),
            cap: overflow_cap,
            overflow_hwm: 0,
            lane_capacity,
            excess_credits: 0,
        }
    }

    pub fn send(&mut self, lane: &mut impl Lane<T>, v: T) -> Result<SendOutcome, T> {
        if self.overflow.len() >= self.cap && !(self.overflow.is_empty() && self.credits > 0) {
            return Err(v);
        }
        if self.overflow.is_empty() && self.credits > 0 {
            match lane.try_push(v) {
                Ok(()) => {
                    self.credits -= 1;
                    self.sent = self
                        .sent
                        .checked_add(1)
                        .expect("lane sequence exhausted; restart required");
                    return Ok(SendOutcome::Sent);
                }
                Err(v) => {
                    if self.overflow.len() >= self.cap {
                        return Err(v);
                    }
                    self.overflow.push_back(v);
                }
            }
        } else {
            self.overflow.push_back(v);
        }
        self.overflow_hwm = self.overflow_hwm.max(self.overflow.len());
        if self.overflow.len() >= self.cap {
            Ok(SendOutcome::ShedAdmission)
        } else {
            Ok(SendOutcome::Deferred)
        }
    }

    /// Cumulative consumed index: duplicate or reordered acknowledgements are idempotent.
    pub fn acknowledge(&mut self, consumed_upto: u64) -> Result<bool, &'static str> {
        if consumed_upto <= self.acknowledged {
            return Ok(false);
        }
        if consumed_upto > self.sent {
            self.excess_credits = self.excess_credits.saturating_add(1);
            return Err("credit acknowledges unsent message");
        }
        self.acknowledged = consumed_upto;
        self.credits = self.lane_capacity - (self.sent - self.acknowledged) as usize;
        Ok(true)
    }
    pub fn pump(&mut self, lane: &mut impl Lane<T>) -> usize {
        let mut sent = 0;
        while self.credits > 0 {
            let Some(v) = self.overflow.pop_front() else {
                break;
            };
            match lane.try_push(v) {
                Ok(()) => {
                    self.credits -= 1;
                    self.sent = self
                        .sent
                        .checked_add(1)
                        .expect("lane sequence exhausted; restart required");
                    sent += 1;
                }
                Err(v) => {
                    self.overflow.push_front(v);
                    break;
                }
            }
        }
        sent
    }

    pub fn backlog(&self) -> usize {
        self.overflow.len()
    }
}

// =============================================================================
// 3. SPECTRE-SAFE ROUTE TABLE, SLOT HANDLES & CHECKED EXTENT LOCATORS
// =============================================================================
